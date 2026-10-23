//! Shared SSH connections: one per profile, reached through its jump chain, carrying one shell
//! channel per tab.
//!
//! - [`SshPool::open_shell`] reuses the profile's live connection, or joins a connect already
//!   in flight for it (the connect future is shared, so a profile is never dialled twice at
//!   once), or dials a new one. A failed connect is not remembered: the next call retries.
//! - A [`Connection`] owns the target's [`Handle`] and every hop's; it is kept alive by the
//!   sessions using it (their [`SessionHandle`]s and channel tasks) and closes, hops included,
//!   when the last of them is gone. The pool only holds a [`Weak`] reference.
//! - A connection whose transport has ended (server drop, keepalive timeout) is never reused.
//!   Each of its shells reports [`SessionEvent::Disconnected`] once and keeps its terminal
//!   content; a shell that ends normally reports [`SessionEvent::Exited`].

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Weak};
use std::time::Duration;

use fbterm_config::{Profile, ProfileId, ProfileStore, ValidationError};
use fbterm_term::{GridSize, Terminal};
use futures_util::future::{BoxFuture, FutureExt, Shared};
use parking_lot::Mutex;
use russh::ChannelMsg;
use russh::client::{Handle, Msg};
use tokio::sync::{mpsc, watch};

use super::{
    ClientHandler, HOP_TIMEOUT, Hop, SshContext, SshError, connect_hop, ssh_profile, tcp_connect,
    timed_out,
};
use crate::{Command, EventSink, Feeder, SessionEvent, SessionHandle, SessionId};

/// How long a closed shell spends sending its channel close.
const CLOSE_GRACE: Duration = Duration::from_secs(2);
/// How long a connection being dropped spends saying goodbye to each hop.
const DISCONNECT_GRACE: Duration = Duration::from_secs(2);

/// Opens SSH shells, sharing one connection per profile.
pub struct SshPool {
    ctx: SshContext,
    slots: Slots<Connection>,
}

impl SshPool {
    pub fn new(ctx: SshContext) -> Self {
        Self {
            ctx,
            slots: Slots::default(),
        }
    }

    /// Open a shell on `profile`, connecting (through its jump chain) unless the profile
    /// already has a live connection.
    pub async fn open_shell(
        &self,
        store: &ProfileStore,
        profile: &ProfileId,
        size: GridSize,
        scrollback: usize,
        events: EventSink,
    ) -> Result<SessionHandle, SshError> {
        let target = store
            .get(*profile)
            .ok_or_else(|| {
                SshError::Jump(ValidationError::MissingJump {
                    profile: profile.to_string(),
                    missing: *profile,
                })
            })?
            .clone();
        ssh_profile(&target)?;
        let chain: Vec<Profile> = store
            .jump_chain(*profile)
            .map_err(SshError::Jump)?
            .into_iter()
            .cloned()
            .collect();
        let ctx = self.ctx.clone();
        let connection = self
            .slots
            .get_or_dial(*profile, Connection::is_alive, move || {
                dial(chain, target, ctx).boxed()
            })
            .await?;
        open_shell_channel(connection, size, scrollback, events).await
    }

    /// Profiles with a live connection (one that is still in use and has not ended).
    pub fn connected_profiles(&self) -> Vec<ProfileId> {
        self.slots.live(Connection::is_alive)
    }
}

/// One profile's connection: the target plus every hop in front of it.
pub(crate) struct Connection {
    /// `None` only while being dropped.
    chain: Option<Chain>,
    runtime: tokio::runtime::Handle,
}

struct Chain {
    /// Hops in connection order; each carries the next one's transport.
    hops: Vec<Hop>,
    target: Hop,
}

impl Connection {
    /// Whether every SSH session of the chain is still running.
    fn is_alive(&self) -> bool {
        self.chain.as_ref().is_some_and(|c| {
            !c.target.handle.is_closed() && c.hops.iter().all(|h| !h.handle.is_closed())
        })
    }

    fn target(&self) -> &Handle<ClientHandler> {
        &self
            .chain
            .as_ref()
            .expect("a connection has its chain until dropped")
            .target
            .handle
    }

    /// Why the connection ended: the hop nearest to us that knows, since a hop going away
    /// takes everything behind it along.
    fn lost_reason(&self) -> String {
        self.chain
            .iter()
            .flat_map(|c| c.hops.iter().chain(std::iter::once(&c.target)))
            .find_map(|h| h.lost.lock().clone())
            .unwrap_or_else(|| "connection lost".to_owned())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let Some(chain) = self.chain.take() else {
            return;
        };
        // Say goodbye from the target outwards. Dropping the handles alone would also close
        // everything once their channels are gone; this just does it promptly and politely.
        self.runtime.spawn(async move {
            let Chain { mut hops, target } = chain;
            let mut order = vec![target];
            while let Some(hop) = hops.pop() {
                order.push(hop);
            }
            for hop in order {
                let _ = tokio::time::timeout(
                    DISCONNECT_GRACE,
                    hop.handle
                        .disconnect(russh::Disconnect::ByApplication, "", "en"),
                )
                .await;
            }
        });
    }
}

/// Connect to the first hop over TCP, tunnel to each next hop with `direct-tcpip`, and run
/// SSH (host-key check and authentication with that hop's own profile) over each tunnel.
async fn dial(
    chain: Vec<Profile>,
    target: Profile,
    ctx: SshContext,
) -> Result<Connection, SshError> {
    let mut route = chain.iter().chain(std::iter::once(&target));
    let first = route.next().expect("the route always contains the target");
    let stream = tcp_connect(ssh_profile(first)?).await?;
    let mut current = connect_hop(stream, first, &ctx).await?;
    let mut via = first;
    let mut hops = Vec::new();
    for next in route {
        let ssh = ssh_profile(next)?;
        let channel = tokio::time::timeout(
            HOP_TIMEOUT,
            current.handle.channel_open_direct_tcpip(
                ssh.host.clone(),
                u32::from(ssh.port),
                "127.0.0.1",
                0,
            ),
        )
        .await
        .map_err(|_| {
            timed_out(format!(
                "{:?} did not open a tunnel to {} port {}",
                via.name, ssh.host, ssh.port
            ))
        })?
        .map_err(|e| {
            SshError::Connect(std::io::Error::other(format!(
                "{:?} could not reach {} port {}: {e}",
                via.name, ssh.host, ssh.port
            )))
        })?;
        let hop = connect_hop(channel.into_stream(), next, &ctx).await?;
        hops.push(std::mem::replace(&mut current, hop));
        via = next;
    }
    Ok(Connection {
        chain: Some(Chain {
            hops,
            target: current,
        }),
        runtime: tokio::runtime::Handle::current(),
    })
}

/// Open a shell channel on `connection` and wire it to a new session.
async fn open_shell_channel(
    connection: Arc<Connection>,
    size: GridSize,
    scrollback: usize,
    events: EventSink,
) -> Result<SessionHandle, SshError> {
    let channel = tokio::time::timeout(HOP_TIMEOUT, connection.target().channel_open_session())
        .await
        .map_err(|_| timed_out("the server did not open a shell channel".into()))??;
    channel
        .request_pty(
            false,
            "xterm-256color",
            u32::from(size.cols),
            u32::from(size.rows),
            0,
            0,
            &[],
        )
        .await?;
    channel.request_shell(false).await?;
    let (read, write) = channel.split();

    let id = SessionId::next();
    let terminal = Arc::new(Mutex::new(Terminal::new(size, scrollback)));
    let dirty_pending = Arc::new(AtomicBool::new(false));
    let bell_pending = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::unbounded_channel::<Command>();
    let (closed_tx, closed_rx) = watch::channel(false);
    let feeder = Feeder::new(
        id,
        terminal.clone(),
        events,
        dirty_pending.clone(),
        bell_pending.clone(),
    );

    tokio::spawn(write_loop(write, rx, closed_rx.clone(), connection.clone()));
    tokio::spawn(read_loop(
        read,
        feeder,
        tx.clone(),
        closed_rx,
        connection.clone(),
    ));

    Ok(SessionHandle {
        id,
        terminal,
        commands: tx,
        dirty_pending,
        bell_pending,
        closer: Box::new(move || {
            closed_tx.send_replace(true);
        }),
        connection: Mutex::new(Some(connection)),
    })
}

/// Resolves once the session has been closed by its handle.
async fn closed(rx: &mut watch::Receiver<bool>) {
    // An error means the handle is gone, which closes the session too.
    let _ = rx.wait_for(|c| *c).await;
}

/// Sends input (typed bytes and terminal replies, in queue order) and size changes.
async fn write_loop(
    write: russh::ChannelWriteHalf<Msg>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    mut closed_rx: watch::Receiver<bool>,
    _connection: Arc<Connection>,
) {
    loop {
        let command = tokio::select! {
            biased;
            () = closed(&mut closed_rx) => break,
            c = commands.recv() => c,
        };
        let sent = match command {
            None | Some(Command::Close) => break,
            Some(Command::Input(bytes)) => tokio::select! {
                // A full window must not keep a closed session alive.
                () = closed(&mut closed_rx) => break,
                r = write.data_bytes(bytes) => r,
            },
            Some(Command::Resize(size)) => {
                write
                    .window_change(u32::from(size.cols), u32::from(size.rows), 0, 0)
                    .await
            }
        };
        if sent.is_err() {
            // The channel or the connection is gone; the reader reports why.
            return;
        }
    }
    let _ = tokio::time::timeout(CLOSE_GRACE, write.close()).await;
}

/// Feeds output into the terminal and reports how the shell ended.
async fn read_loop(
    mut read: russh::ChannelReadHalf,
    mut feeder: Feeder,
    replies: mpsc::UnboundedSender<Command>,
    mut closed_rx: watch::Receiver<bool>,
    connection: Arc<Connection>,
) {
    let mut exit_code = None;
    // The server ended the shell itself (exit status/signal or channel close).
    let mut ended = false;
    loop {
        let msg = tokio::select! {
            msg = read.wait() => msg,
            // Closed by us: the writer closes the channel. russh does not pass the server's
            // confirmation of a close we sent on to the channel, so do not wait for it.
            () = closed(&mut closed_rx) => break,
        };
        match msg {
            Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                for reply in feeder.feed(&data) {
                    let _ = replies.send(Command::Input(reply));
                }
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => {
                exit_code = i32::try_from(exit_status).ok();
                ended = true;
            }
            Some(ChannelMsg::ExitSignal { .. }) => ended = true,
            Some(ChannelMsg::Close) => {
                ended = true;
                break;
            }
            Some(_) => {}
            None => break,
        }
    }
    let closed_by_us = *closed_rx.borrow();
    feeder.mark_dirty();
    if ended || closed_by_us {
        feeder.send(SessionEvent::Exited(exit_code));
    } else {
        // The channel vanished without a close: the connection under it ended.
        feeder.send(SessionEvent::Disconnected(connection.lost_reason()));
    }
}

/// A connect in flight, shared by every caller waiting on it.
type Dial<C> = Shared<BoxFuture<'static, Result<Arc<C>, Arc<SshError>>>>;

enum Slot<C> {
    Live(Weak<C>),
    Dialing(Dial<C>),
}

/// Per-profile bookkeeping of the pool, generic so it can be tested without a server.
struct Slots<C> {
    map: Mutex<HashMap<ProfileId, Slot<C>>>,
}

impl<C> Default for Slots<C> {
    fn default() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
        }
    }
}

impl<C: Send + Sync + 'static> Slots<C> {
    /// The profile's live connection, or the result of the connect in flight for it, or of a
    /// new connect made with `dial`.
    async fn get_or_dial<F>(
        &self,
        id: ProfileId,
        alive: fn(&C) -> bool,
        dial: F,
    ) -> Result<Arc<C>, SshError>
    where
        F: FnOnce() -> BoxFuture<'static, Result<C, SshError>>,
    {
        let pending = {
            let mut map = self.map.lock();
            match map.get(&id) {
                Some(Slot::Live(weak)) => {
                    if let Some(c) = weak.upgrade().filter(|c| alive(c)) {
                        return Ok(c);
                    }
                    None
                }
                Some(Slot::Dialing(f)) => Some(f.clone()),
                None => None,
            }
            .unwrap_or_else(|| {
                let f: Dial<C> = dial()
                    .map(|r| r.map(Arc::new).map_err(Arc::new))
                    .boxed()
                    .shared();
                map.insert(id, Slot::Dialing(f.clone()));
                f
            })
        };
        let result = pending.clone().await;
        {
            // Whoever finishes first records the outcome; a newer dial is left alone.
            let mut map = self.map.lock();
            if let Some(Slot::Dialing(f)) = map.get(&id)
                && f.ptr_eq(&pending)
            {
                match &result {
                    Ok(c) => {
                        map.insert(id, Slot::Live(Arc::downgrade(c)));
                    }
                    Err(_) => {
                        map.remove(&id);
                    }
                }
            }
        }
        result.map_err(|e| Arc::try_unwrap(e).unwrap_or_else(|e| e.duplicate()))
    }

    /// Profiles whose connection is still referenced and alive; forgets the others.
    fn live(&self, alive: fn(&C) -> bool) -> Vec<ProfileId> {
        let mut map = self.map.lock();
        map.retain(|_, slot| match slot {
            Slot::Live(weak) => weak.strong_count() > 0,
            Slot::Dialing(_) => true,
        });
        let mut ids: Vec<ProfileId> = map
            .iter()
            .filter_map(|(id, slot)| match slot {
                Slot::Live(weak) => weak.upgrade().filter(|c| alive(c)).map(|_| *id),
                Slot::Dialing(_) => None,
            })
            .collect();
        ids.sort();
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// A stand-in connection whose liveness the test controls.
    struct Fake {
        alive: AtomicBool,
        serial: usize,
    }

    fn fake_alive(c: &Fake) -> bool {
        c.alive.load(Ordering::SeqCst)
    }

    /// A dial that counts calls, takes a while, and fails when `fail` is set.
    fn dialer(
        calls: &Arc<AtomicUsize>,
        fail: bool,
    ) -> impl FnOnce() -> BoxFuture<'static, Result<Fake, SshError>> {
        let calls = calls.clone();
        move || {
            async move {
                let serial = calls.fetch_add(1, Ordering::SeqCst) + 1;
                tokio::time::sleep(Duration::from_millis(50)).await;
                if fail {
                    Err(SshError::Connect(std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        "refused",
                    )))
                } else {
                    Ok(Fake {
                        alive: AtomicBool::new(true),
                        serial,
                    })
                }
            }
            .boxed()
        }
    }

    #[tokio::test]
    async fn concurrent_callers_share_one_dial() {
        let slots = Slots::<Fake>::default();
        let id = ProfileId::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let (a, b, c) = tokio::join!(
            slots.get_or_dial(id, fake_alive, dialer(&calls, false)),
            slots.get_or_dial(id, fake_alive, dialer(&calls, false)),
            slots.get_or_dial(id, fake_alive, dialer(&calls, false)),
        );
        let (a, b, c) = (a.unwrap(), b.unwrap(), c.unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(Arc::ptr_eq(&a, &b) && Arc::ptr_eq(&b, &c));
        assert_eq!(slots.live(fake_alive), vec![id]);
        // Later callers reuse it without dialling.
        let d = slots
            .get_or_dial(id, fake_alive, dialer(&calls, false))
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&a, &d));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn concurrent_callers_share_a_failure_and_it_is_not_cached() {
        let slots = Slots::<Fake>::default();
        let id = ProfileId::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let (a, b) = tokio::join!(
            slots.get_or_dial(id, fake_alive, dialer(&calls, true)),
            slots.get_or_dial(id, fake_alive, dialer(&calls, true)),
        );
        for e in [a.err().unwrap(), b.err().unwrap()] {
            match e {
                SshError::Connect(io) => {
                    assert_eq!(io.kind(), std::io::ErrorKind::ConnectionRefused)
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(slots.live(fake_alive).is_empty());
        // The next call retries.
        let c = slots
            .get_or_dial(id, fake_alive, dialer(&calls, false))
            .await
            .unwrap();
        assert_eq!(c.serial, 2);
    }

    #[tokio::test]
    async fn dead_connection_is_not_reused() {
        let slots = Slots::<Fake>::default();
        let id = ProfileId::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let first = slots
            .get_or_dial(id, fake_alive, dialer(&calls, false))
            .await
            .unwrap();
        first.alive.store(false, Ordering::SeqCst);
        // Still referenced (a tab shows its disconnected notice), but not listed or reused.
        assert!(slots.live(fake_alive).is_empty());
        let second = slots
            .get_or_dial(id, fake_alive, dialer(&calls, false))
            .await
            .unwrap();
        assert_eq!((first.serial, second.serial), (1, 2));
        assert_eq!(slots.live(fake_alive), vec![id]);
    }

    #[tokio::test]
    async fn released_connection_is_forgotten() {
        let slots = Slots::<Fake>::default();
        let (x, y) = (ProfileId::new(), ProfileId::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let cx = slots
            .get_or_dial(x, fake_alive, dialer(&calls, false))
            .await
            .unwrap();
        let cy = slots
            .get_or_dial(y, fake_alive, dialer(&calls, false))
            .await
            .unwrap();
        let mut both = vec![x, y];
        both.sort();
        assert_eq!(slots.live(fake_alive), both);
        drop(cx);
        assert_eq!(slots.live(fake_alive), vec![y]);
        drop(cy);
        assert!(slots.live(fake_alive).is_empty());
        assert!(slots.map.lock().is_empty());
    }

    #[tokio::test]
    async fn abandoned_dial_is_resumed_by_the_next_caller() {
        let slots = Slots::<Fake>::default();
        let id = ProfileId::new();
        let calls = Arc::new(AtomicUsize::new(0));
        // The first caller gives up (its tab was closed while connecting).
        let gave_up = tokio::time::timeout(
            Duration::from_millis(10),
            slots.get_or_dial(id, fake_alive, dialer(&calls, false)),
        )
        .await;
        assert!(gave_up.is_err());
        let c = slots
            .get_or_dial(id, fake_alive, dialer(&calls, false))
            .await
            .unwrap();
        assert_eq!(c.serial, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
