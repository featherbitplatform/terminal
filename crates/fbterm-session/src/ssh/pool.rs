//! Shared SSH connections: one per profile, reached through its jump chain, carrying one shell
//! channel per tab.
//!
//! - [`SshPool::open_shell`] reuses the profile's live connection, or joins a connect already
//!   in flight for it (the connect future is shared, so a profile is never dialled twice at
//!   once), or dials a new one. A failed connect is not remembered: the next call retries.
//!   Only a connection on the profile's current route (the SSH settings of the profile and of
//!   its jump chain) is shared: after an edit the next caller dials afresh, and whoever holds
//!   the old connection (a running tunnel) keeps it until it lets go.
//! - A [`Connection`] owns the target's [`Handle`] and every hop's; it is kept alive by the
//!   sessions using it (their [`SessionHandle`]s and channel tasks) and closes, hops included,
//!   when the last of them is gone. The pool only holds a [`Weak`] reference.
//! - A connection whose transport has ended (server drop, keepalive timeout) is never reused.
//!   Each of its shells reports [`SessionEvent::Disconnected`] once and keeps its terminal
//!   content; a shell that ends normally reports [`SessionEvent::Exited`].
//! - [`SshPool::acquire`] hands out the connection itself (shared with the shells, dialled the
//!   same way) for tunnels: holding it keeps the connection open without any shell, and
//!   [`Connection::closed`] says when it ended.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use fbterm_config::{Profile, ProfileId, ProfileKind, ProfileStore, SshProfile, ValidationError};
use fbterm_term::{GridSize, SharedTerminal, Terminal};
use futures_util::future::{BoxFuture, FutureExt, Shared};
use parking_lot::Mutex;
use russh::client::{Handle, Msg};
use russh::{Channel, ChannelMsg};
use tokio::sync::{mpsc, watch};

use super::{
    ClientHandler, Forwards, HOP_TIMEOUT, Hop, Prompter, SshContext, SshError, connect_hop, in_hop,
    ssh_profile, tcp_connect, timed_out,
};
use crate::{AuthWarning, Command, EventSink, Feeder, SessionEvent, SessionHandle, SessionId};

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
        let terminal = Arc::new(Mutex::new(Terminal::new(size, scrollback)));
        self.open_shell_into(store, profile, terminal, events).await
    }

    /// Like [`SshPool::open_shell`], but the shell's output continues in an existing terminal
    /// (at its current size), below whatever it already shows; its scrollback is kept.
    pub async fn open_shell_into(
        &self,
        store: &ProfileStore,
        profile: &ProfileId,
        terminal: SharedTerminal,
        events: EventSink,
    ) -> Result<SessionHandle, SshError> {
        let prompter = self.ctx.prompter.clone();
        self.open_shell_prompting(store, profile, terminal, events, prompter)
            .await
    }

    /// Like [`SshPool::open_shell_into`], with the questions of this call's connect (host key,
    /// secrets, for every hop) going to `prompter` instead of the pool's own. When the call
    /// joins a connect already in flight for the profile, that connect keeps asking whoever
    /// started it; a connect that cannot ask anyone (an automatic reconnect with
    /// [`super::NoPrompt`]) is never joined by a prompter that can: this call dials its own.
    pub async fn open_shell_prompting(
        &self,
        store: &ProfileStore,
        profile: &ProfileId,
        terminal: SharedTerminal,
        events: EventSink,
        prompter: Arc<dyn Prompter>,
    ) -> Result<SessionHandle, SshError> {
        let can_ask = prompter.can_ask();
        let (route, dialer) = self.dialer(store, profile, prompter)?;
        self.slots
            .open_with(
                *profile,
                route,
                can_ask,
                Connection::is_alive,
                Connection::mark_dead,
                dialer,
                move |connection| {
                    open_shell_channel(connection, terminal.clone(), events.clone()).boxed()
                },
            )
            .await
    }

    /// The profile's connection itself, for tunnels: its live one (shared with its shells),
    /// or the connect in flight for it, or a new connect, exactly as a shell would get it.
    /// The connection stays open while the returned reference (or any shell) is held.
    ///
    /// With a prompter that cannot ask ([`super::NoPrompt`], automatic reconnects), a connect
    /// that needed the user fails with [`SshError::NeedsInput`]; such a call never opens a
    /// prompt of its own, but when it joins a connect already in flight that another caller
    /// started with a prompter that can ask, it waits for that connect, including a prompt
    /// that caller opened. A call whose prompter can ask never joins a connect that cannot
    /// ask: it dials its own, as [`SshPool::open_shell_prompting`] does.
    pub async fn acquire(
        &self,
        store: &ProfileStore,
        profile: &ProfileId,
        prompter: Arc<dyn Prompter>,
    ) -> Result<Arc<Connection>, SshError> {
        let can_ask = prompter.can_ask();
        let (route, dialer) = self.dialer(store, profile, prompter)?;
        self.slots
            .open_with(
                *profile,
                route,
                can_ask,
                Connection::is_alive,
                Connection::mark_dead,
                dialer,
                |connection| async move { Ok(connection) }.boxed(),
            )
            .await
            .map_err(|e| e.without_prompts(can_ask))
    }

    /// The route of `profile` and its dial step (its jump chain, then the profile), asking
    /// `prompter`.
    #[allow(clippy::type_complexity)]
    fn dialer(
        &self,
        store: &ProfileStore,
        profile: &ProfileId,
        prompter: Arc<dyn Prompter>,
    ) -> Result<
        (
            Route,
            impl Fn() -> BoxFuture<'static, Result<Connection, SshError>> + use<>,
        ),
        SshError,
    > {
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
        let ctx = SshContext {
            prompter,
            ..self.ctx.clone()
        };
        let route = route_of(&chain, &target);
        Ok((route, move || {
            dial(chain.clone(), target.clone(), ctx.clone()).boxed()
        }))
    }

    /// Profiles with a live connection (one that is still in use and has not ended).
    pub fn connected_profiles(&self) -> Vec<ProfileId> {
        self.slots.live(Connection::is_alive)
    }

    /// Whether `connection` is the one the pool hands out for `profile`. Not so for the
    /// result of a no-prompt dial that an interactive caller replaced while it ran: that one
    /// works, but nothing else shares it, so a tunnel should acquire again instead.
    pub(crate) fn holds(&self, profile: &ProfileId, connection: &Arc<Connection>) -> bool {
        self.slots.holds(*profile, connection)
    }
}

/// What a profile's connection goes through: the SSH settings (host, port, user, auth) of each
/// hop of its jump chain and then of the profile itself, tunnels left out. A connection is
/// shared only with callers whose profile still has the route it was dialled on.
type Route = Vec<SshProfile>;

fn route_of(chain: &[Profile], target: &Profile) -> Route {
    chain
        .iter()
        .chain(std::iter::once(target))
        .filter_map(|p| match &p.kind {
            ProfileKind::Ssh(s) => Some(SshProfile {
                tunnels: Vec::new(),
                ..s.clone()
            }),
            ProfileKind::Local(_) => None,
        })
        .collect()
}

/// One profile's SSH connection: the target plus every hop in front of it, shared by the
/// profile's shells and tunnels and closed (hops included) when the last of them lets go.
/// Get one from [`SshPool::acquire`].
pub struct Connection {
    /// `None` only while being dropped.
    chain: Option<Chain>,
    /// Set when using the connection failed although its transport looked alive.
    dead: AtomicBool,
    /// What authenticating any hop of the chain turned up; every shell reports it once.
    warnings: Vec<AuthWarning>,
    /// The remote forwards requested on the target; its handler routes forwarded channels
    /// only to these.
    forwards: Forwards,
    runtime: tokio::runtime::Handle,
}

struct Chain {
    /// Hops in connection order; each carries the next one's transport.
    hops: Vec<Hop>,
    target: Hop,
}

impl Connection {
    /// Whether every SSH session of the chain is still running (and the pool would still hand
    /// this connection out).
    pub fn is_alive(&self) -> bool {
        !self.dead.load(Ordering::Acquire)
            && self.chain.as_ref().is_some_and(|c| {
                !c.target.handle.is_closed() && c.hops.iter().all(|h| !h.handle.is_closed())
            })
    }

    /// Never hand this connection out again (its existing shells keep it until they end).
    fn mark_dead(&self) {
        self.dead.store(true, Ordering::Release);
    }

    pub(crate) fn target(&self) -> &Handle<ClientHandler> {
        &self
            .chain
            .as_ref()
            .expect("a connection has its chain until dropped")
            .target
            .handle
    }

    /// Every hop of the chain in connection order, the target last.
    fn hops(&self) -> impl Iterator<Item = &Hop> {
        self.chain
            .iter()
            .flat_map(|c| c.hops.iter().chain(std::iter::once(&c.target)))
    }

    /// Why the connection ended: the hop nearest to us that knows, since a hop going away
    /// takes everything behind it along.
    fn lost_reason(&self) -> String {
        lost_reason_of(self.hops().map(|h| &h.lost))
    }

    /// Resolves, with the reason, once the target or any hop of the chain has disconnected;
    /// at once if that already happened. Holds no reference to the connection, so waiting on
    /// it does not keep the connection open.
    pub fn closed(&self) -> impl Future<Output = String> + Send + 'static {
        let ended: Vec<watch::Receiver<bool>> = self.hops().map(|h| h.ended.clone()).collect();
        let lost: Vec<Arc<Mutex<Option<String>>>> = self.hops().map(|h| h.lost.clone()).collect();
        async move {
            first_ended(ended).await;
            lost_reason_of(lost.iter())
        }
    }

    /// Claim the forwarded channels for `address:port` (the address as requested, the port
    /// the server listens on): from now on they are accepted and arrive on the returned
    /// receiver. Refused (`None`) while another claim on the same forward is live, so one
    /// tunnel can never take over or remove another's routing. The claim ends when the
    /// returned guard is dropped, and only if it is still the registered one. The guard does
    /// not keep the connection open.
    pub(crate) fn claim_forward(
        &self,
        address: &str,
        port: u32,
    ) -> Option<(ForwardClaim, mpsc::UnboundedReceiver<Channel<Msg>>)> {
        claim_in(&self.forwards, address, port)
    }
}

/// Raw remote-forward calls, for the live tests only (feature `test-support`, which only the
/// crate's own tests enable). Registering here replaces a live claim instead of being refused,
/// so these are not part of the API the app builds against.
#[cfg(feature = "test-support")]
#[doc(hidden)]
impl Connection {
    /// Ask the server to listen on `address:port` (port 0: a port of its choosing) and send
    /// its connections back as `forwarded-tcpip` channels. Returns the port it listens on.
    /// Those channels are refused until [`Connection::register_forward`] claims the port.
    pub async fn request_remote_forward(&self, address: &str, port: u32) -> Result<u32, SshError> {
        let bound = self.target().tcpip_forward(address, port).await?;
        Ok(if port == 0 { bound } else { port })
    }

    /// Ask the server to stop listening on `address:port` (a forward requested earlier).
    pub async fn cancel_remote_forward(&self, address: &str, port: u32) -> Result<(), SshError> {
        Ok(self.target().cancel_tcpip_forward(address, port).await?)
    }

    /// Claim the forwarded channels for `address:port` (the address as requested, the port
    /// the server listens on): from now on they are accepted and arrive on the returned
    /// receiver. Replaces an earlier claim on the same forward. Dropping the receiver refuses
    /// them again, as does [`Connection::unregister_forward`].
    pub fn register_forward(
        &self,
        address: &str,
        port: u32,
    ) -> mpsc::UnboundedReceiver<Channel<Msg>> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.forwards.lock().insert((address.to_owned(), port), tx);
        rx
    }

    /// Stop accepting forwarded channels for `address:port`.
    pub fn unregister_forward(&self, address: &str, port: u32) {
        self.forwards.lock().remove(&(address.to_owned(), port));
    }
}

/// Claim `address:port` in `forwards` unless a live claim holds it (see
/// [`Connection::claim_forward`]).
fn claim_in(
    forwards: &Forwards,
    address: &str,
    port: u32,
) -> Option<(ForwardClaim, mpsc::UnboundedReceiver<Channel<Msg>>)> {
    let key = (address.to_owned(), port);
    let mut map = forwards.lock();
    if map.get(&key).is_some_and(|tx| !tx.is_closed()) {
        return None;
    }
    let (tx, rx) = mpsc::unbounded_channel();
    map.insert(key.clone(), tx.clone());
    let claim = ForwardClaim {
        forwards: forwards.clone(),
        key,
        tx,
    };
    Some((claim, rx))
}

/// A registered remote forward (see [`Connection::claim_forward`]); unregisters it on drop.
pub(crate) struct ForwardClaim {
    forwards: Forwards,
    key: (String, u32),
    tx: mpsc::UnboundedSender<Channel<Msg>>,
}

impl Drop for ForwardClaim {
    fn drop(&mut self) {
        let mut map = self.forwards.lock();
        if map
            .get(&self.key)
            .is_some_and(|tx| tx.same_channel(&self.tx))
        {
            map.remove(&self.key);
        }
    }
}

/// The first reason recorded among `lost`, in hop order.
fn lost_reason_of<'a>(lost: impl Iterator<Item = &'a Arc<Mutex<Option<String>>>>) -> String {
    lost.into_iter()
        .find_map(|l| l.lock().clone())
        .unwrap_or_else(|| "connection lost".to_owned())
}

/// Resolves once any of `ended` turns `true` or loses its sender (the hop's session task is
/// gone); never for an empty list.
async fn first_ended(ended: Vec<watch::Receiver<bool>>) {
    if ended.is_empty() {
        return std::future::pending().await;
    }
    futures_util::future::select_all(ended.into_iter().map(|mut rx| {
        async move {
            let _ = rx.wait_for(|e| *e).await;
        }
        .boxed()
    }))
    .await;
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
    let forwards = Forwards::default();
    // Only the target gets the registry: a jump hop refuses every forwarded channel.
    let registry = |is_target: bool| is_target.then(|| forwards.clone());
    let hops_before_target = chain.len();
    let mut route = chain.iter().chain(std::iter::once(&target));
    let first = route.next().expect("the route always contains the target");
    let stream = tcp_connect(ssh_profile(first)?)
        .await
        .map_err(|e| in_hop(first, e))?;
    let mut current = connect_hop(stream, first, &ctx, registry(hops_before_target == 0))
        .await
        .map_err(|e| in_hop(first, e))?;
    let mut via = first;
    let mut hops = Vec::new();
    for (index, next) in route.enumerate() {
        let is_target = index + 1 == hops_before_target;
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
            in_hop(
                via,
                timed_out(format!(
                    "no tunnel to {} port {} opened in time",
                    ssh.host, ssh.port
                )),
            )
        })?
        .map_err(|e| {
            in_hop(
                via,
                SshError::Connect(std::io::Error::other(format!(
                    "could not reach {} port {}: {e}",
                    ssh.host, ssh.port
                ))),
            )
        })?;
        let hop = connect_hop(channel.into_stream(), next, &ctx, registry(is_target))
            .await
            .map_err(|e| in_hop(next, e))?;
        hops.push(std::mem::replace(&mut current, hop));
        via = next;
    }
    let mut warnings: Vec<AuthWarning> = hops
        .iter()
        .chain(std::iter::once(&current))
        .filter_map(|h| h.warning)
        .collect();
    warnings.dedup();
    Ok(Connection {
        chain: Some(Chain {
            hops,
            target: current,
        }),
        dead: AtomicBool::new(false),
        warnings,
        forwards,
        runtime: tokio::runtime::Handle::current(),
    })
}

/// Open a shell channel on `connection` and wire it to a new session.
async fn open_shell_channel(
    connection: Arc<Connection>,
    terminal: SharedTerminal,
    events: EventSink,
) -> Result<SessionHandle, SshError> {
    let size = terminal.lock().size();
    let channel = tokio::time::timeout(HOP_TIMEOUT, connection.target().channel_open_session())
        .await
        .map_err(|_| timed_out("the server did not open a shell channel".into()))??;
    // Both requests want a reply; the reader turns a refusal into a message on the screen.
    channel
        .request_pty(
            true,
            "xterm-256color",
            u32::from(size.cols),
            u32::from(size.rows),
            0,
            0,
            &[],
        )
        .await?;
    channel.request_shell(true).await?;
    let (read, write) = channel.split();

    let id = SessionId::next();
    let dirty_pending = Arc::new(AtomicBool::new(false));
    let bell_pending = Arc::new(AtomicBool::new(false));
    let detached = Arc::new(AtomicBool::new(false));
    let ended = crate::Ended::default();
    let (tx, rx) = mpsc::unbounded_channel::<Command>();
    let (closed_tx, closed_rx) = watch::channel(false);
    let feeder = Feeder::new(
        id,
        terminal.clone(),
        events,
        dirty_pending.clone(),
        bell_pending.clone(),
        detached.clone(),
        ended.clone(),
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
        detached,
        ended,
        auth_warnings: connection.warnings.clone(),
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

/// What the reader does after a non-data channel message.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    Continue,
    /// The server refused this request (`"terminal"` or `"shell"`).
    Refused(&'static str),
    End,
}

/// How a shell channel is going, from the messages that are not output.
#[derive(Default)]
struct ShellState {
    exit_code: Option<i32>,
    /// The server ended the shell itself (exit status/signal, refusal or channel close).
    ended: bool,
    /// Replies received to our `want_reply` requests, which are answered in order.
    replies: usize,
}

/// The requests sent with `want_reply`, in the order they are answered.
const REQUESTS: [&str; 2] = ["terminal", "shell"];

impl ShellState {
    fn on(&mut self, msg: Option<ChannelMsg>) -> Step {
        match msg {
            Some(ChannelMsg::Success) => {
                self.replies += 1;
                Step::Continue
            }
            Some(ChannelMsg::Failure) => {
                let what = REQUESTS.get(self.replies).copied().unwrap_or("request");
                self.replies += 1;
                self.ended = true;
                Step::Refused(what)
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => {
                self.exit_code = i32::try_from(exit_status).ok();
                self.ended = true;
                Step::Continue
            }
            Some(ChannelMsg::ExitSignal { .. }) => {
                self.ended = true;
                Step::Continue
            }
            Some(ChannelMsg::Close) => {
                self.ended = true;
                Step::End
            }
            Some(_) => Step::Continue,
            None => Step::End,
        }
    }

    /// The event that reports the end. A channel that vanished without being ended by
    /// either side means the connection under it ended.
    fn end_event(&self, closed_by_us: bool, lost_reason: impl FnOnce() -> String) -> SessionEvent {
        if self.ended || closed_by_us {
            SessionEvent::Exited(self.exit_code)
        } else {
            SessionEvent::Disconnected(lost_reason())
        }
    }
}

/// The dim notice written to the terminal when the server refuses a request.
fn refusal_notice(what: &str) -> Vec<u8> {
    format!("\r\n\x1b[2mThe server refused the {what}.\x1b[0m\r\n").into_bytes()
}

/// Feeds output into the terminal and reports how the shell ended.
async fn read_loop(
    mut read: russh::ChannelReadHalf,
    mut feeder: Feeder,
    replies: mpsc::UnboundedSender<Command>,
    mut closed_rx: watch::Receiver<bool>,
    connection: Arc<Connection>,
) {
    let mut state = ShellState::default();
    loop {
        let msg = tokio::select! {
            msg = read.wait() => msg,
            // Closed by us: the writer closes the channel. russh forgets a channel once we
            // send its close, so the server's confirming close never reaches us (and russh
            // keeps a small entry for the channel until the connection ends). Do not wait.
            () = closed(&mut closed_rx) => break,
        };
        match msg {
            Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                for reply in feeder.feed(&data) {
                    let _ = replies.send(Command::Input(reply));
                }
            }
            other => match state.on(other) {
                Step::Continue => {}
                Step::Refused(what) => {
                    feeder.feed(&refusal_notice(what));
                    let _ = replies.send(Command::Close);
                    break;
                }
                Step::End => break,
            },
        }
    }
    let closed_by_us = *closed_rx.borrow();
    feeder.mark_dirty();
    feeder.send(state.end_event(closed_by_us, || connection.lost_reason()));
}

/// A connect in flight, shared by every caller waiting on it.
type Dial<C> = Shared<BoxFuture<'static, Result<Arc<C>, Arc<SshError>>>>;

enum Slot<C, R> {
    /// A connection and the route it was dialled on.
    Live(Weak<C>, R),
    /// The connect runs in its own task so it can be aborted when nobody waits for it any
    /// more: a tab closed while connecting must not leave hops authenticated or a prompt open.
    Dialing {
        token: u64,
        future: Dial<C>,
        /// Whether the dial's prompter can ask the user. A caller that can ask never joins a
        /// dial that cannot (it would fail for want of an answer nobody was asked for).
        can_ask: bool,
        /// The route being dialled: only callers on the same route join it.
        route: R,
        share: Arc<DialShare>,
    },
}

/// The callers of one dial, kept beside the slot so a dial replaced in the map (by an
/// interactive caller) is still aborted when its own callers give up.
struct DialShare {
    /// Changed only under the slots' map lock.
    waiters: AtomicUsize,
    task: tokio::task::AbortHandle,
}

/// Per-profile bookkeeping of the pool, generic so it can be tested without a server. `R` is
/// the route a connection was dialled on (see [`Route`]): a slot is only shared by callers on
/// the same route.
struct Slots<C, R = Route> {
    map: Mutex<HashMap<ProfileId, Slot<C, R>>>,
    next_token: AtomicU64,
}

impl<C, R> Default for Slots<C, R> {
    fn default() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            next_token: AtomicU64::new(1),
        }
    }
}

/// One caller waiting on a dial. When the last one goes away before the dial finished, the
/// dial is aborted (dropping any hop already connected and any open prompt) and forgotten.
struct Waiter<'a, C, R> {
    slots: &'a Slots<C, R>,
    id: ProfileId,
    token: u64,
    share: Arc<DialShare>,
}

impl<C, R> Drop for Waiter<'_, C, R> {
    fn drop(&mut self) {
        let mut map = self.slots.map.lock();
        if self.share.waiters.fetch_sub(1, Ordering::AcqRel) == 1 {
            // Harmless when the dial already finished.
            self.share.task.abort();
            if let Some(Slot::Dialing { token, .. }) = map.get(&self.id)
                && *token == self.token
            {
                map.remove(&self.id);
            }
        }
    }
}

impl<C: Send + Sync + 'static, R: PartialEq + Clone + Send + Sync> Slots<C, R> {
    /// The profile's live connection (`true`: reused), or the result of the connect in
    /// flight for it, or of a new connect made with `dial` (`false`). `can_ask` says whether
    /// `dial`'s prompter can ask the user: such a caller does not join a dial in flight that
    /// cannot, but replaces it with its own (whose callers then get its own result); a caller
    /// that cannot ask joins any dial in flight.
    ///
    /// `route` is what the caller's profile says the connection goes through. A live
    /// connection or a dial in flight on another route (the profile was edited since) is never
    /// handed to this caller: it dials its own, which takes the slot over for later callers.
    /// Whoever holds the old connection keeps it until they let go.
    async fn get_or_dial_on<F>(
        &self,
        id: ProfileId,
        route: R,
        can_ask: bool,
        alive: fn(&C) -> bool,
        dial: F,
    ) -> Result<(Arc<C>, bool), SshError>
    where
        F: FnOnce() -> BoxFuture<'static, Result<C, SshError>>,
    {
        let (future, waiter) = {
            let mut map = self.map.lock();
            if let Some(Slot::Live(weak, on)) = map.get(&id)
                && *on == route
                && let Some(c) = weak.upgrade().filter(|c| alive(c))
            {
                return Ok((c, true));
            }
            let joinable = match map.get(&id) {
                Some(Slot::Dialing {
                    can_ask: dial_asks,
                    route: on,
                    ..
                }) => *on == route && (*dial_asks || !can_ask),
                _ => false,
            };
            if !joinable {
                let task = tokio::spawn(dial());
                let abort = task.abort_handle();
                let future: Dial<C> = async move {
                    match task.await {
                        Ok(r) => r.map(Arc::new).map_err(Arc::new),
                        Err(e) => Err(Arc::new(SshError::Protocol(format!(
                            "connect did not finish: {e}"
                        )))),
                    }
                }
                .boxed()
                .shared();
                let token = self.next_token.fetch_add(1, Ordering::Relaxed);
                map.insert(
                    id,
                    Slot::Dialing {
                        token,
                        future,
                        can_ask,
                        route: route.clone(),
                        share: Arc::new(DialShare {
                            waiters: AtomicUsize::new(0),
                            task: abort,
                        }),
                    },
                );
            }
            let Some(Slot::Dialing {
                token,
                future,
                share,
                ..
            }) = map.get(&id)
            else {
                unreachable!("a dial slot was just ensured");
            };
            share.waiters.fetch_add(1, Ordering::AcqRel);
            let waiter = Waiter {
                slots: self,
                id,
                token: *token,
                share: share.clone(),
            };
            (future.clone(), waiter)
        };
        let result = future.await;
        {
            // The first waiter to finish records the outcome; a newer dial is left alone.
            let mut map = self.map.lock();
            if let Some(Slot::Dialing { token, .. }) = map.get(&id)
                && *token == waiter.token
            {
                match &result {
                    Ok(c) => {
                        map.insert(id, Slot::Live(Arc::downgrade(c), route));
                    }
                    Err(_) => {
                        map.remove(&id);
                    }
                }
            }
        }
        drop(waiter);
        result
            .map(|c| (c, false))
            .map_err(|e| Arc::try_unwrap(e).unwrap_or_else(|e| e.duplicate()))
    }

    /// Get a connection with [`Self::get_or_dial`] and `open` something on it. If that fails
    /// on a reused connection, the link may have died before keepalives noticed: `kill` it so
    /// it is never handed out again, and try once more on a fresh connection.
    #[allow(clippy::too_many_arguments)]
    async fn open_with<T, D, O>(
        &self,
        id: ProfileId,
        route: R,
        can_ask: bool,
        alive: fn(&C) -> bool,
        kill: fn(&C),
        dial: D,
        open: O,
    ) -> Result<T, SshError>
    where
        D: Fn() -> BoxFuture<'static, Result<C, SshError>>,
        O: Fn(Arc<C>) -> BoxFuture<'static, Result<T, SshError>>,
    {
        let (c, reused) = self
            .get_or_dial_on(id, route.clone(), can_ask, alive, &dial)
            .await?;
        match open(c.clone()).await {
            Err(e) if reused => {
                tracing::info!(error = %e, "reused connection failed; reconnecting");
                kill(&c);
                drop(c);
                let (c, _) = self
                    .get_or_dial_on(id, route, can_ask, alive, &dial)
                    .await?;
                open(c).await
            }
            other => other,
        }
    }

    /// Whether `c` is the live connection recorded for `id`.
    fn holds(&self, id: ProfileId, c: &Arc<C>) -> bool {
        matches!(self.map.lock().get(&id), Some(Slot::Live(weak, _)) if std::ptr::eq(weak.as_ptr(), Arc::as_ptr(c)))
    }

    /// Profiles whose connection is still referenced and alive; forgets the others.
    fn live(&self, alive: fn(&C) -> bool) -> Vec<ProfileId> {
        let mut map = self.map.lock();
        map.retain(|_, slot| match slot {
            Slot::Live(weak, _) => weak.strong_count() > 0,
            Slot::Dialing { .. } => true,
        });
        let mut ids: Vec<ProfileId> = map
            .iter()
            .filter_map(|(id, slot)| match slot {
                Slot::Live(weak, _) => weak.upgrade().filter(|c| alive(c)).map(|_| *id),
                Slot::Dialing { .. } => None,
            })
            .collect();
        ids.sort();
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in connection whose liveness the test controls.
    struct Fake {
        alive: AtomicBool,
        serial: usize,
    }

    fn fake_alive(c: &Fake) -> bool {
        c.alive.load(Ordering::SeqCst)
    }

    fn fake_kill(c: &Fake) {
        c.alive.store(false, Ordering::SeqCst);
    }

    /// Counts dials and how many dial futures were dropped before finishing.
    #[derive(Clone, Default)]
    struct Dials {
        started: Arc<AtomicUsize>,
        dropped_unfinished: Arc<AtomicUsize>,
    }

    impl Dials {
        fn started(&self) -> usize {
            self.started.load(Ordering::SeqCst)
        }

        /// A dial that takes `delay` and then succeeds (or fails when `fail`).
        fn dialer(
            &self,
            delay: Duration,
            fail: bool,
        ) -> impl Fn() -> BoxFuture<'static, Result<Fake, SshError>> + Clone {
            let this = self.clone();
            move || {
                let this = this.clone();
                async move {
                    struct Unfinished(Arc<AtomicUsize>, bool);
                    impl Drop for Unfinished {
                        fn drop(&mut self) {
                            if !self.1 {
                                self.0.fetch_add(1, Ordering::SeqCst);
                            }
                        }
                    }
                    let serial = this.started.fetch_add(1, Ordering::SeqCst) + 1;
                    let mut guard = Unfinished(this.dropped_unfinished.clone(), false);
                    tokio::time::sleep(delay).await;
                    guard.1 = true;
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
    }

    impl<C: Send + Sync + 'static> Slots<C, ()> {
        /// [`Slots::get_or_dial_on`] for tests where every caller is on the same route.
        async fn get_or_dial<F>(
            &self,
            id: ProfileId,
            can_ask: bool,
            alive: fn(&C) -> bool,
            dial: F,
        ) -> Result<(Arc<C>, bool), SshError>
        where
            F: FnOnce() -> BoxFuture<'static, Result<C, SshError>>,
        {
            self.get_or_dial_on(id, (), can_ask, alive, dial).await
        }
    }

    const QUICK: Duration = Duration::from_millis(50);
    /// Whether the caller's prompter can ask the user (`NoPrompt` cannot).
    const ASK: bool = true;
    const NO_ASK: bool = false;

    #[tokio::test]
    async fn interactive_caller_does_not_inherit_no_prompt_dial() {
        let slots = Slots::<Fake, ()>::default();
        let id = ProfileId::new();
        let dials = Dials::default();
        let slow = Duration::from_millis(200);
        // An automatic reconnect (no prompts) is dialling and will fail for want of an answer;
        // meanwhile the user opens a tab, whose prompter can ask.
        let (automatic, by_hand) = tokio::join!(
            slots.get_or_dial(id, NO_ASK, fake_alive, dials.dialer(slow, true)),
            async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                slots
                    .get_or_dial(id, ASK, fake_alive, dials.dialer(slow, false))
                    .await
            },
        );
        // The tab dialled on its own (with its own prompter) and got through ...
        assert_eq!(dials.started(), 2);
        let (c, reused) = by_hand.expect("the interactive caller must not inherit the failure");
        assert_eq!((c.serial, reused), (2, false));
        // ... the reconnect got its own result ...
        assert!(automatic.is_err());
        // ... and the tab's connection is the one the pool keeps.
        assert_eq!(slots.live(fake_alive), vec![id]);
        let (again, reused) = slots
            .get_or_dial(id, NO_ASK, fake_alive, dials.dialer(QUICK, true))
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&c, &again) && reused);
    }

    #[tokio::test]
    async fn replaced_no_prompt_dial_is_still_aborted_when_abandoned() {
        let slots = Slots::<Fake, ()>::default();
        let id = ProfileId::new();
        let dials = Dials::default();
        // The no-prompt caller gives up while the interactive dial that replaced its slot
        // goes on: its own dial is aborted, the interactive one is not.
        let (gave_up, by_hand) = tokio::join!(
            tokio::time::timeout(
                Duration::from_millis(50),
                slots.get_or_dial(
                    id,
                    NO_ASK,
                    fake_alive,
                    dials.dialer(Duration::from_secs(30), false)
                ),
            ),
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                slots
                    .get_or_dial(
                        id,
                        ASK,
                        fake_alive,
                        dials.dialer(Duration::from_millis(150), false),
                    )
                    .await
            },
        );
        assert!(gave_up.is_err());
        let (c, _) = by_hand.unwrap();
        assert_eq!(c.serial, 2);
        eventually("the abandoned no-prompt dial to be dropped", || {
            dials.dropped_unfinished.load(Ordering::SeqCst) == 1
        })
        .await;
        assert_eq!(slots.live(fake_alive), vec![id]);
    }

    #[tokio::test]
    async fn replaced_no_prompt_dial_that_succeeds_is_not_held() {
        let slots = Slots::<Fake, ()>::default();
        let id = ProfileId::new();
        let dials = Dials::default();
        // Both dials get through; the slot keeps the interactive one, so the reconnect's own
        // connection is not the pool's and a tunnel must not keep riding it.
        let (automatic, by_hand) = tokio::join!(
            slots.get_or_dial(
                id,
                NO_ASK,
                fake_alive,
                dials.dialer(Duration::from_millis(100), false)
            ),
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                slots
                    .get_or_dial(
                        id,
                        ASK,
                        fake_alive,
                        dials.dialer(Duration::from_millis(150), false),
                    )
                    .await
            },
        );
        let ((stray, _), (kept, _)) = (automatic.unwrap(), by_hand.unwrap());
        assert_eq!((stray.serial, kept.serial), (1, 2));
        assert!(!slots.holds(id, &stray));
        assert!(slots.holds(id, &kept));
        assert!(!slots.holds(ProfileId::new(), &kept));
        // Acquiring again hands out the held one.
        let (again, reused) = slots
            .get_or_dial(id, NO_ASK, fake_alive, dials.dialer(QUICK, false))
            .await
            .unwrap();
        assert!(reused && Arc::ptr_eq(&again, &kept));
    }

    #[test]
    fn forward_claims_are_exclusive_and_release_only_their_own() {
        let forwards: Forwards = Arc::default();
        let route = |port| super::super::forward_route(Some(&forwards), "127.0.0.1", port);
        let (first, _first_rx) = claim_in(&forwards, "127.0.0.1", 4022).unwrap();
        // A second tunnel on the same forward is refused and the first keeps its routing.
        assert!(claim_in(&forwards, "127.0.0.1", 4022).is_none());
        assert!(route(4022).is_some());
        // Other ports and addresses are separate forwards.
        let (other, _other_rx) = claim_in(&forwards, "127.0.0.1", 4023).unwrap();
        let (any, _any_rx) = claim_in(&forwards, "0.0.0.0", 4022).unwrap();
        drop((other, any));
        // Released, the forward can be claimed again (a restarted tunnel).
        drop(first);
        assert!(route(4022).is_none());
        let (again, _again_rx) = claim_in(&forwards, "127.0.0.1", 4022).unwrap();
        // An entry whose receiver is gone (an old public registration) does not block a claim,
        // and a stale guard never removes the newer claim.
        let stale = ForwardClaim {
            forwards: forwards.clone(),
            key: ("127.0.0.1".to_owned(), 4022),
            tx: mpsc::unbounded_channel().0,
        };
        drop(stale);
        assert!(route(4022).is_some());
        drop(again);
        let (tx, rx) = mpsc::unbounded_channel();
        forwards.lock().insert(("127.0.0.1".to_owned(), 4022), tx);
        drop(rx);
        assert!(claim_in(&forwards, "127.0.0.1", 4022).is_some());
        assert!(forwards.lock().is_empty());
    }

    #[tokio::test]
    async fn edited_route_is_not_reused() {
        let slots = Slots::<Fake, &str>::default();
        let id = ProfileId::new();
        let dials = Dials::default();
        // A tunnel holds the connection to the old host all day.
        let (old, _) = slots
            .get_or_dial_on(id, "old-host", ASK, fake_alive, dials.dialer(QUICK, false))
            .await
            .unwrap();
        // The profile's host is edited: a new shell dials the new host instead of joining.
        let (new, reused) = slots
            .get_or_dial_on(id, "new-host", ASK, fake_alive, dials.dialer(QUICK, false))
            .await
            .unwrap();
        assert_eq!((old.serial, new.serial, reused), (1, 2, false));
        assert!(fake_alive(&old), "the old holder keeps its connection");
        assert!(slots.holds(id, &new) && !slots.holds(id, &old));
        // Later callers on the new route share the new connection.
        let (again, reused) = slots
            .get_or_dial_on(id, "new-host", ASK, fake_alive, dials.dialer(QUICK, false))
            .await
            .unwrap();
        assert!(reused && Arc::ptr_eq(&again, &new));
        // A reconnect still on the old route (a running tunnel's snapshot) gets a connection of
        // its own, never the new one ...
        let (stale, reused) = slots
            .get_or_dial_on(
                id,
                "old-host",
                NO_ASK,
                fake_alive,
                dials.dialer(QUICK, false),
            )
            .await
            .unwrap();
        assert_eq!((stale.serial, reused), (3, false));
        // ... and new callers never get its stale route.
        let (fresh, reused) = slots
            .get_or_dial_on(id, "new-host", ASK, fake_alive, dials.dialer(QUICK, false))
            .await
            .unwrap();
        assert_eq!((fresh.serial, reused), (4, false));
        assert_eq!(dials.started(), 4);
    }

    #[tokio::test]
    async fn dial_on_another_route_is_not_joined() {
        let slots = Slots::<Fake, &str>::default();
        let id = ProfileId::new();
        let dials = Dials::default();
        let (old, new) = tokio::join!(
            slots.get_or_dial_on(
                id,
                "old-host",
                ASK,
                fake_alive,
                dials.dialer(Duration::from_millis(100), false)
            ),
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                slots
                    .get_or_dial_on(
                        id,
                        "new-host",
                        NO_ASK,
                        fake_alive,
                        dials.dialer(QUICK, false),
                    )
                    .await
            },
        );
        let ((old, _), (new, _)) = (old.unwrap(), new.unwrap());
        assert_eq!((old.serial, new.serial), (1, 2));
        assert!(slots.holds(id, &new), "the newer route keeps the slot");
    }

    #[tokio::test]
    async fn no_prompt_caller_may_join_interactive_dial() {
        let slots = Slots::<Fake, ()>::default();
        let id = ProfileId::new();
        let dials = Dials::default();
        // The user's dial is in flight (possibly showing a prompt); an automatic caller joins it
        // instead of dialling a second connection.
        let (by_hand, automatic) = tokio::join!(
            slots.get_or_dial(id, ASK, fake_alive, dials.dialer(QUICK, false)),
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                slots
                    .get_or_dial(id, NO_ASK, fake_alive, dials.dialer(QUICK, false))
                    .await
            },
        );
        let ((a, _), (b, _)) = (by_hand.unwrap(), automatic.unwrap());
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(dials.started(), 1);
        // Likewise two interactive callers, or two automatic ones, share one dial.
        let other = ProfileId::new();
        let (x, y) = tokio::join!(
            slots.get_or_dial(other, NO_ASK, fake_alive, dials.dialer(QUICK, false)),
            slots.get_or_dial(other, NO_ASK, fake_alive, dials.dialer(QUICK, false)),
        );
        assert!(Arc::ptr_eq(&x.unwrap().0, &y.unwrap().0));
        assert_eq!(dials.started(), 2);
    }

    #[tokio::test]
    async fn concurrent_callers_share_one_dial() {
        let slots = Slots::<Fake, ()>::default();
        let id = ProfileId::new();
        let dials = Dials::default();
        let (a, b, c) = tokio::join!(
            slots.get_or_dial(id, ASK, fake_alive, dials.dialer(QUICK, false)),
            slots.get_or_dial(id, ASK, fake_alive, dials.dialer(QUICK, false)),
            slots.get_or_dial(id, ASK, fake_alive, dials.dialer(QUICK, false)),
        );
        let ((a, ra), (b, rb), (c, rc)) = (a.unwrap(), b.unwrap(), c.unwrap());
        assert_eq!(dials.started(), 1);
        assert!(Arc::ptr_eq(&a, &b) && Arc::ptr_eq(&b, &c));
        assert!(!ra && !rb && !rc, "joining a dial is not reuse");
        assert_eq!(slots.live(fake_alive), vec![id]);
        // Later callers reuse it without dialling.
        let (d, reused) = slots
            .get_or_dial(id, ASK, fake_alive, dials.dialer(QUICK, false))
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&a, &d) && reused);
        assert_eq!(dials.started(), 1);
    }

    #[tokio::test]
    async fn concurrent_callers_share_a_failure_and_it_is_not_cached() {
        let slots = Slots::<Fake, ()>::default();
        let id = ProfileId::new();
        let dials = Dials::default();
        let (a, b) = tokio::join!(
            slots.get_or_dial(id, ASK, fake_alive, dials.dialer(QUICK, true)),
            slots.get_or_dial(id, ASK, fake_alive, dials.dialer(QUICK, true)),
        );
        for e in [a.err().unwrap(), b.err().unwrap()] {
            match e {
                SshError::Connect(io) => {
                    assert_eq!(io.kind(), std::io::ErrorKind::ConnectionRefused)
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(dials.started(), 1);
        assert!(slots.live(fake_alive).is_empty());
        // The next call retries.
        let (c, _) = slots
            .get_or_dial(id, ASK, fake_alive, dials.dialer(QUICK, false))
            .await
            .unwrap();
        assert_eq!(c.serial, 2);
    }

    #[tokio::test]
    async fn dead_connection_is_not_reused() {
        let slots = Slots::<Fake, ()>::default();
        let id = ProfileId::new();
        let dials = Dials::default();
        let (first, _) = slots
            .get_or_dial(id, ASK, fake_alive, dials.dialer(QUICK, false))
            .await
            .unwrap();
        fake_kill(&first);
        // Still referenced (a tab shows its disconnected notice), but not listed or reused.
        assert!(slots.live(fake_alive).is_empty());
        let (second, reused) = slots
            .get_or_dial(id, ASK, fake_alive, dials.dialer(QUICK, false))
            .await
            .unwrap();
        assert_eq!((first.serial, second.serial, reused), (1, 2, false));
        assert_eq!(slots.live(fake_alive), vec![id]);
    }

    #[tokio::test]
    async fn released_connection_is_forgotten() {
        let slots = Slots::<Fake, ()>::default();
        let (x, y) = (ProfileId::new(), ProfileId::new());
        let dials = Dials::default();
        let (cx, _) = slots
            .get_or_dial(x, ASK, fake_alive, dials.dialer(QUICK, false))
            .await
            .unwrap();
        let (cy, _) = slots
            .get_or_dial(y, ASK, fake_alive, dials.dialer(QUICK, false))
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

    /// Wait (bounded) until `cond` holds.
    async fn eventually(what: &str, cond: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !cond() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }

    #[tokio::test]
    async fn abandoned_dial_is_aborted_and_next_caller_dials_fresh() {
        let slots = Slots::<Fake, ()>::default();
        let id = ProfileId::new();
        let dials = Dials::default();
        // The only caller gives up (its tab was closed while connecting) on a slow dial.
        let gave_up = tokio::time::timeout(
            Duration::from_millis(20),
            slots.get_or_dial(
                id,
                ASK,
                fake_alive,
                dials.dialer(Duration::from_secs(30), false),
            ),
        )
        .await;
        assert!(gave_up.is_err());
        // The dial is aborted, not left running (it would hold hops and prompts) ...
        eventually("the abandoned dial to be dropped", || {
            dials.dropped_unfinished.load(Ordering::SeqCst) == 1
        })
        .await;
        // ... and forgotten.
        assert!(slots.map.lock().is_empty());
        // The next caller dials afresh and gets its connection promptly.
        let (c, reused) = tokio::time::timeout(
            Duration::from_secs(2),
            slots.get_or_dial(id, ASK, fake_alive, dials.dialer(QUICK, false)),
        )
        .await
        .expect("a fresh dial must not time out")
        .unwrap();
        assert_eq!((c.serial, reused, dials.started()), (2, false, 2));
    }

    #[tokio::test]
    async fn dial_survives_while_any_waiter_remains() {
        let slots = Slots::<Fake, ()>::default();
        let id = ProfileId::new();
        let dials = Dials::default();
        let slow = Duration::from_millis(200);
        let (gave_up, stayed) = tokio::join!(
            tokio::time::timeout(
                Duration::from_millis(20),
                slots.get_or_dial(id, ASK, fake_alive, dials.dialer(slow, false)),
            ),
            slots.get_or_dial(id, ASK, fake_alive, dials.dialer(slow, false)),
        );
        assert!(gave_up.is_err());
        let (c, _) = stayed.unwrap();
        assert_eq!((c.serial, dials.started()), (1, 1));
        assert_eq!(dials.dropped_unfinished.load(Ordering::SeqCst), 0);
    }

    /// An `open` that fails on the connections listed in `failing` (by serial) and otherwise
    /// returns the connection, the way a shell keeps its connection alive.
    fn opener(
        failing: &'static [usize],
    ) -> impl Fn(Arc<Fake>) -> BoxFuture<'static, Result<Arc<Fake>, SshError>> {
        move |c: Arc<Fake>| {
            async move {
                if failing.contains(&c.serial) {
                    Err(timed_out("no channel".into()))
                } else {
                    Ok(c)
                }
            }
            .boxed()
        }
    }

    #[tokio::test]
    async fn failed_open_on_reused_connection_redials_once() {
        let slots = Slots::<Fake, ()>::default();
        let id = ProfileId::new();
        let dials = Dials::default();
        let dial = dials.dialer(QUICK, false);
        let first = slots
            .open_with(id, (), ASK, fake_alive, fake_kill, &dial, opener(&[]))
            .await
            .unwrap();
        assert_eq!(first.serial, 1);
        // Connection 1 silently died: opening on it fails, so it is killed and replaced.
        let second = slots
            .open_with(id, (), ASK, fake_alive, fake_kill, &dial, opener(&[1]))
            .await
            .unwrap();
        assert_eq!((second.serial, dials.started()), (2, 2));
        assert!(!fake_alive(&first), "the failed connection is marked dead");
        assert_eq!(slots.live(fake_alive), vec![id]);
        // Only once: if the fresh connection fails too, the error is returned.
        let err = slots
            .open_with(id, (), ASK, fake_alive, fake_kill, &dial, opener(&[2, 3]))
            .await
            .err()
            .unwrap();
        assert!(matches!(err, SshError::Connect(_)), "{err:?}");
        assert_eq!(dials.started(), 3);
    }

    #[tokio::test]
    async fn failed_open_on_fresh_connection_is_not_retried() {
        let slots = Slots::<Fake, ()>::default();
        let id = ProfileId::new();
        let dials = Dials::default();
        let err = slots
            .open_with(
                id,
                (),
                ASK,
                fake_alive,
                fake_kill,
                dials.dialer(QUICK, false),
                opener(&[1]),
            )
            .await
            .err()
            .unwrap();
        assert!(matches!(err, SshError::Connect(_)), "{err:?}");
        assert_eq!(dials.started(), 1);
    }

    #[tokio::test]
    async fn first_ended_waits_for_any_hop() {
        let (hop_tx, hop_rx) = watch::channel(false);
        let (target_tx, target_rx) = watch::channel(false);
        let waiting = tokio::spawn(first_ended(vec![hop_rx, target_rx]));
        tokio::time::sleep(QUICK).await;
        assert!(!waiting.is_finished());
        // A hop (not the target) ending is enough.
        hop_tx.send_replace(true);
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("a hop ending must resolve it")
            .unwrap();
        // Already ended: at once.
        tokio::time::timeout(
            Duration::from_secs(1),
            first_ended(vec![hop_tx.subscribe(), target_tx.subscribe()]),
        )
        .await
        .expect("an ended hop resolves at once");
        // A session task that went away without a word counts as ended too.
        let (gone_tx, gone_rx) = watch::channel(false);
        drop(gone_tx);
        tokio::time::timeout(Duration::from_secs(1), first_ended(vec![gone_rx]))
            .await
            .expect("a dropped sender resolves it");
        drop(target_tx);
    }

    #[test]
    fn lost_reason_prefers_the_nearest_hop() {
        let hop = Arc::new(Mutex::new(None));
        let target = Arc::new(Mutex::new(Some("target gone".to_owned())));
        assert_eq!(lost_reason_of([&hop, &target].into_iter()), "target gone");
        *hop.lock() = Some("hop gone".into());
        assert_eq!(lost_reason_of([&hop, &target].into_iter()), "hop gone");
        assert_eq!(lost_reason_of(std::iter::empty()), "connection lost");
    }

    #[test]
    fn shell_state_reports_refusals_in_request_order() {
        let mut s = ShellState::default();
        assert_eq!(s.on(Some(ChannelMsg::Success)), Step::Continue);
        assert_eq!(s.on(Some(ChannelMsg::Failure)), Step::Refused("shell"));
        assert_eq!(
            s.end_event(false, || unreachable!()),
            SessionEvent::Exited(None)
        );

        let mut s = ShellState::default();
        assert_eq!(s.on(Some(ChannelMsg::Failure)), Step::Refused("terminal"));
        assert_eq!(
            s.end_event(false, || unreachable!()),
            SessionEvent::Exited(None)
        );
    }

    #[test]
    fn shell_state_exit_and_disconnect() {
        let mut s = ShellState::default();
        s.on(Some(ChannelMsg::Success));
        s.on(Some(ChannelMsg::Success));
        assert_eq!(
            s.on(Some(ChannelMsg::ExitStatus { exit_status: 3 })),
            Step::Continue
        );
        assert_eq!(s.on(Some(ChannelMsg::Eof)), Step::Continue);
        assert_eq!(s.on(Some(ChannelMsg::Close)), Step::End);
        assert_eq!(
            s.end_event(false, || unreachable!()),
            SessionEvent::Exited(Some(3))
        );

        // The channel vanishes without any end: the connection was lost.
        let mut s = ShellState::default();
        s.on(Some(ChannelMsg::Success));
        assert_eq!(s.on(None), Step::End);
        assert_eq!(
            s.end_event(false, || "gone".into()),
            SessionEvent::Disconnected("gone".into())
        );
        // Unless we closed it ourselves.
        assert_eq!(
            s.end_event(true, || unreachable!()),
            SessionEvent::Exited(None)
        );
    }

    #[test]
    fn refusal_notice_is_dim_text_on_its_own_line() {
        let size = GridSize {
            cols: 60,
            rows: 5,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let mut term = Terminal::new(size, 10);
        term.feed(b"prompt");
        term.feed(&refusal_notice("shell"));
        let notice = refusal_notice("shell");
        let text = String::from_utf8(notice).unwrap();
        assert!(text.contains("\x1b[2mThe server refused the shell.\x1b[0m"));
        assert!(text.starts_with("\r\n"));
        // The notice lands on the line after the existing output.
        use feathercritty::index::{Column, Line};
        let grid = term.term().grid();
        let line: String = (0..60)
            .map(|c| grid.screen()[Line(1)][Column(c)].c)
            .collect();
        assert_eq!(line.trim_end(), "The server refused the shell.");
    }
}
