//! Tunnels: local port forwards, remote port forwards and SOCKS5 proxies riding a profile's
//! shared SSH connection (its jump chain included).
//!
//! [`start_tunnel`] runs one tunnel in a task of its own and reports its [`TunnelState`] on a
//! [`TunnelSink`]: `Starting`, then `Up` with the number of live client connections (at most
//! every [`COUNT_INTERVAL`], and always on reaching zero), or `Failed` with the reason. A
//! tunnel whose connection ends reconnects on its own (see [`start_tunnel`]), reporting
//! `Reconnecting`, then `Up` again or `Failed`.
//!
//! - **Local**: listens on `bind:port` here; each client becomes a `direct-tcpip` channel to
//!   `host:host_port` as seen from the server. A channel the server refuses closes that one
//!   client; the tunnel stays up.
//! - **Remote**: asks the server to listen on `bind:port`; each `forwarded-tcpip` channel for
//!   it becomes a TCP connection to `host:host_port` from here. Only forwards this tunnel
//!   requested are accepted (see `Connection::claim_forward`).
//! - **SOCKS**: SOCKS5 with no authentication and CONNECT only; host names go to the server
//!   as typed and are resolved there, never here. BIND and UDP ASSOCIATE are refused.
//!
//! Stopping (the handle's [`TunnelHandle::stop`], or dropping it) closes the listener, cancels
//! the remote forward, ends live client connections and releases the connection, which the
//! pool then closes unless a shell or another tunnel still uses it; [`TunnelHandle::stopped`]
//! says when that is done, so a restart can wait for its port. A stopped tunnel reports
//! nothing more, so a restart under the same key never sees a stale state. Two remote tunnels
//! of one connection never share a server port: the second is refused.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;

use fbterm_config::{ProfileId, ProfileStore, TunnelDef, TunnelKind};
use parking_lot::Mutex;
use russh::client::Msg;
use russh::{Channel, ChannelOpenFailure, ChannelStream};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;

use futures_util::FutureExt;

use super::pool::{Connection, ForwardClaim};
use super::{HOP_TIMEOUT, NoPrompt, Prompter, SshError, SshPool};

/// Connection counts are reported at most this often (zero is always reported at once).
pub const COUNT_INTERVAL: Duration = Duration::from_millis(250);
/// Bound on a SOCKS client's greeting and request, the channel open included.
const SOCKS_HANDSHAKE: Duration = Duration::from_secs(30);
/// How long a stopping remote tunnel waits for the server to cancel its forward.
const CANCEL_GRACE: Duration = Duration::from_secs(2);
/// Pause after a failed `accept` (out of file descriptors, say) before accepting again.
const ACCEPT_PAUSE: Duration = Duration::from_millis(100);

/// Names one tunnel: its profile and its name within that profile.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct TunnelKey {
    pub profile: ProfileId,
    pub name: String,
}

/// What a tunnel is doing.
#[derive(Clone, PartialEq, Debug)]
pub enum TunnelState {
    Starting,
    Up { connections: usize },
    Reconnecting { attempt: u32, max: u32 },
    Failed(String),
    Off,
}

/// Where tunnels report their state changes.
pub type TunnelSink = mpsc::UnboundedSender<(TunnelKey, TunnelState)>;

/// A running tunnel. Dropping it stops the tunnel.
pub struct TunnelHandle {
    stop: watch::Sender<bool>,
    gate: Arc<Mutex<bool>>,
    port: Arc<AtomicU32>,
    /// Loses its sender when the tunnel's task has ended and released everything.
    done: watch::Receiver<()>,
}

impl TunnelHandle {
    /// A handle with no tunnel behind it, already stopped: stands in for a real one where code
    /// that keeps handles is tested without a network.
    #[doc(hidden)]
    pub fn inert() -> Self {
        let (stop, _) = watch::channel(false);
        let (_, done) = watch::channel(());
        Self {
            stop,
            gate: Arc::new(Mutex::new(false)),
            port: Arc::new(AtomicU32::new(0)),
            done,
        }
    }

    /// Whether the tunnel's task has ended and released what it held (see
    /// [`TunnelHandle::stopped`]), without waiting.
    pub fn is_stopped(&self) -> bool {
        self.done.has_changed().is_err()
    }

    /// Stop the tunnel. Nothing more is reported once this returns; the listener, the remote
    /// forward, live clients and the connection are released by the tunnel's task right after.
    pub fn stop(&self) {
        *self.gate.lock() = true;
        self.stop.send_replace(true);
    }

    /// Resolves once the tunnel's task has ended (stopped or failed) and released what it
    /// held: the listener is closed, the remote forward cancelled and its routing released,
    /// live clients ended and the connection let go. A restart of the same tunnel awaits this
    /// so the new instance finds its port free.
    pub async fn stopped(&self) {
        let mut done = self.done.clone();
        while done.changed().await.is_ok() {}
    }

    /// [`TunnelHandle::stop`], then [`TunnelHandle::stopped`].
    pub async fn stop_and_wait(&self) {
        self.stop();
        self.stopped().await;
    }

    /// The port the tunnel listens on once it is up: the local port of a local forward or
    /// SOCKS proxy, or the server's port of a remote forward (as the server chose it when the
    /// definition asked for port 0).
    pub fn port(&self) -> Option<u16> {
        u16::try_from(self.port.load(Ordering::Acquire))
            .ok()
            .filter(|p| *p != 0)
    }
}

impl Drop for TunnelHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Sends a tunnel's states until it is stopped. The gate is held while sending, so once
/// [`TunnelHandle::stop`] has returned no send of this tunnel can come after it.
struct Reporter {
    key: TunnelKey,
    sink: TunnelSink,
    gate: Arc<Mutex<bool>>,
}

impl Reporter {
    fn send(&self, state: TunnelState) {
        let stopped = self.gate.lock();
        if !*stopped {
            let _ = self.sink.send((self.key.clone(), state));
        }
    }
}

/// Start the tunnel `def` of `key.profile` (the definition is a snapshot: later edits do not
/// reach a running tunnel). A local or SOCKS tunnel binds its port first, so a port in use
/// fails it before anything is asked; then the connection is acquired from `pool` with
/// `prompter`, so a tunnel switched on by hand may ask the user.
///
/// When the connection is lost the tunnel reconnects on its own: it reports
/// `Reconnecting { attempt, max }`, waits the backoff (1, 2, 4, 8, 16 s, then 30 s), acquires
/// the connection again without prompts ([`NoPrompt`]), and once it has one serves again and
/// reports `Up`. `max` is read from `retries` at each attempt; when the attempts run out (at
/// once with `0`) it reports `Failed("connection lost: {reason}")` with the last reason, and a
/// reconnect that would need the user (an unknown or changed host key, a secret that is not
/// saved) fails at once with `needs your input`. A local or SOCKS
/// listener stays bound meanwhile (clients that connect are closed), so the port stays ours; a
/// remote forward is requested again on the new connection.
/// Must be called within a tokio runtime.
pub fn start_tunnel(
    pool: Arc<SshPool>,
    store: ProfileStore,
    key: TunnelKey,
    def: TunnelDef,
    prompter: Arc<dyn Prompter>,
    retries: Arc<AtomicU32>,
    events: TunnelSink,
) -> TunnelHandle {
    let (stop, stop_rx) = watch::channel(false);
    let (done_tx, done) = watch::channel(());
    let gate = Arc::new(Mutex::new(false));
    let port = Arc::new(AtomicU32::new(0));
    let reporter = Reporter {
        key: key.clone(),
        sink: events,
        gate: gate.clone(),
    };
    reporter.send(TunnelState::Starting);
    let tunnel = Tunnel {
        pool,
        store,
        profile: key.profile,
        def,
        prompter,
        retries,
        reporter,
        stop: stop_rx,
        port: port.clone(),
    };
    tokio::spawn(async move {
        tunnel.run().await;
        // Everything the task held is released by now; dropping this tells `stopped`.
        drop(done_tx);
    });
    TunnelHandle {
        stop,
        gate,
        port,
        done,
    }
}

/// One tunnel's task.
struct Tunnel {
    pool: Arc<SshPool>,
    store: ProfileStore,
    profile: ProfileId,
    def: TunnelDef,
    prompter: Arc<dyn Prompter>,
    retries: Arc<AtomicU32>,
    reporter: Reporter,
    stop: watch::Receiver<bool>,
    port: Arc<AtomicU32>,
}

/// How serving a tunnel on one connection ended.
enum Ended {
    /// The handle stopped it.
    Stopped,
    /// The connection was lost, for this reason: worth reconnecting.
    Lost(String),
    /// The tunnel cannot run (a refused forward, say): reported as is.
    Failed(String),
}

/// Resolves once the tunnel is stopped (or its handle is gone).
async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|s| *s).await;
}

/// What a tunnel offers on this side. A local or SOCKS listener lives as long as the tunnel,
/// across reconnects, so its port stays ours.
enum Front {
    Local {
        listener: TcpListener,
        host: String,
        host_port: u16,
    },
    Socks {
        listener: TcpListener,
    },
    Remote {
        bind: String,
        port: u16,
        host: String,
        host_port: u16,
    },
}

impl Front {
    fn listener(&self) -> Option<&TcpListener> {
        match self {
            Self::Local { listener, .. } | Self::Socks { listener } => Some(listener),
            Self::Remote { .. } => None,
        }
    }
}

/// A connection with what the tunnel set up on it, ready to serve.
struct Ready {
    connection: Arc<Connection>,
    /// The server's forward, for a remote tunnel.
    forward: Option<Forward>,
}

/// A remote forward the server granted, and its routing here.
struct Forward {
    bind: String,
    bound: u32,
    claim: ForwardClaim,
    channels: mpsc::UnboundedReceiver<Channel<Msg>>,
}

/// Why one reconnect attempt did not give a tunnel ready to serve.
enum Attempt {
    Stopped,
    NeedsInput,
    Failed(String),
}

impl Tunnel {
    async fn run(self) {
        let mut stop = self.stop.clone();
        // The port first: a port in use fails the tunnel before any login or question.
        let front = match self.front().await {
            Ok(front) => front,
            Err(reason) => return self.reporter.send(TunnelState::Failed(reason)),
        };
        let prompter = self.prompter.clone();
        let first = tokio::select! {
            biased;
            () = stopped(&mut stop) => return,
            c = self.connect(prompter) => c,
        };
        let connection = match first {
            Ok(c) => c,
            Err(e) => return self.reporter.send(TunnelState::Failed(e.to_string())),
        };
        // On the first start a refused forward fails the tunnel at once; only a lost
        // connection is worth reconnecting.
        let mut ready = match self.prepare(&front, connection).await {
            Ok(ready) => ready,
            Err(Ended::Stopped) => return,
            Err(Ended::Failed(reason)) => return self.reporter.send(TunnelState::Failed(reason)),
            Err(Ended::Lost(reason)) => match self.reconnect(&front, reason).await {
                Some(ready) => ready,
                None => return,
            },
        };
        loop {
            match self.serve(&front, ready).await {
                Ended::Stopped => return,
                Ended::Failed(reason) => return self.reporter.send(TunnelState::Failed(reason)),
                Ended::Lost(reason) => match self.reconnect(&front, reason).await {
                    Some(next) => ready = next,
                    None => return,
                },
            }
        }
    }

    /// Bind the listener of a local or SOCKS tunnel.
    async fn front(&self) -> Result<Front, String> {
        Ok(match self.def.kind.clone() {
            TunnelKind::Local {
                bind,
                port,
                host,
                host_port,
            } => Front::Local {
                listener: listen(&bind, port).await?,
                host,
                host_port,
            },
            TunnelKind::Socks { bind, port } => Front::Socks {
                listener: listen(&bind, port).await?,
            },
            TunnelKind::Remote {
                bind,
                port,
                host,
                host_port,
            } => Front::Remote {
                bind: listen_address(&bind).to_owned(),
                port,
                host,
                host_port,
            },
        })
    }

    /// The profile's pooled connection. A connection the pool does not hold (a reconnect's
    /// dial that a tab replaced while it ran) is let go and the pool asked once more.
    async fn connect(&self, prompter: Arc<dyn Prompter>) -> Result<Arc<Connection>, SshError> {
        let c = self
            .pool
            .acquire(&self.store, &self.profile, prompter.clone())
            .await?;
        if self.pool.holds(&self.profile, &c) {
            return Ok(c);
        }
        drop(c);
        self.pool
            .acquire(&self.store, &self.profile, prompter)
            .await
    }

    /// Set the tunnel up on `connection`: nothing for a local or SOCKS tunnel (its listener is
    /// already bound), the server's forward for a remote one. A connection that has already
    /// ended is `Lost`.
    async fn prepare(&self, front: &Front, connection: Arc<Connection>) -> Result<Ready, Ended> {
        if let Some(reason) = connection.closed().now_or_never() {
            return Err(Ended::Lost(reason));
        }
        let forward = match front {
            Front::Remote { bind, port, .. } => Some(self.request(&connection, bind, *port).await?),
            Front::Local { .. } | Front::Socks { .. } => None,
        };
        Ok(Ready {
            connection,
            forward,
        })
    }

    /// Reconnect after the connection was lost for `reason`: attempts with backoff and no
    /// prompts until one gives a tunnel ready to serve (`Some`), the attempts run out or one
    /// needs the user (both reported as `Failed`), or the tunnel is stopped (`None` for all
    /// three). An attempt fails when the connection cannot be had, or when the remote forward
    /// is refused on it (the old session may still hold the port for a while); its reason is
    /// the one reported when the attempts run out. Clients that reach the listener meanwhile
    /// are closed at once.
    async fn reconnect(&self, front: &Front, mut reason: String) -> Option<Ready> {
        let listener = front.listener();
        let mut attempt = 0u32;
        loop {
            attempt = attempt.saturating_add(1);
            let max = self.retries.load(Ordering::Acquire);
            if attempt > max {
                self.reporter
                    .send(TunnelState::Failed(format!("connection lost: {reason}")));
                return None;
            }
            self.reporter
                .send(TunnelState::Reconnecting { attempt, max });
            // The attempt answers `stop` itself, so a remote request it interrupts can take
            // back what the server granted.
            let wait = async {
                let mut stop = self.stop.clone();
                tokio::select! {
                    biased;
                    () = stopped(&mut stop) => return Err(Attempt::Stopped),
                    () = tokio::time::sleep(backoff(attempt)) => {}
                }
                let connection = tokio::select! {
                    biased;
                    () = stopped(&mut stop) => return Err(Attempt::Stopped),
                    c = self.connect(Arc::new(NoPrompt)) => c.map_err(no_prompt_attempt)?,
                };
                self.prepare(front, connection)
                    .await
                    .map_err(|ended| match ended {
                        Ended::Stopped => Attempt::Stopped,
                        Ended::Lost(r) | Ended::Failed(r) => Attempt::Failed(r),
                    })
            };
            let result = tokio::select! {
                biased;
                result = wait => result,
                never = refuse_clients(|| accept(listener)) => match never {},
            };
            match result {
                Ok(ready) => return Some(ready),
                Err(Attempt::Stopped) => return None,
                Err(Attempt::NeedsInput) => {
                    self.reporter
                        .send(TunnelState::Failed(SshError::NeedsInput.to_string()));
                    return None;
                }
                Err(Attempt::Failed(e)) => {
                    tracing::debug!(error = %e, attempt, "tunnel reconnect failed");
                    reason = e;
                }
            }
        }
    }

    /// Run the tunnel until it is stopped or its connection is lost, then release the
    /// connection and what was served on it.
    async fn serve(&self, front: &Front, ready: Ready) -> Ended {
        let Ready {
            connection,
            forward,
        } = ready;
        match (front, forward) {
            (
                Front::Local {
                    listener,
                    host,
                    host_port,
                },
                _,
            ) => {
                let (host, host_port) = (host.clone(), *host_port);
                self.serve_listener(listener, connection, move |c, tcp, peer| {
                    forward_local(c, tcp, peer, host.clone(), host_port)
                })
                .await
            }
            (Front::Socks { listener }, _) => {
                self.serve_listener(listener, connection, serve_socks).await
            }
            (
                Front::Remote {
                    host, host_port, ..
                },
                Some(forward),
            ) => {
                let ended = self
                    .serve_forward(connection, forward, host.clone(), *host_port)
                    .await;
                // The server's port is only known while the forward is up.
                self.port.store(0, Ordering::Release);
                ended
            }
            // `prepare` sets up the forward of every remote tunnel before it is served.
            (Front::Remote { .. }, None) => {
                unreachable!("a remote tunnel served without its forward")
            }
        }
    }

    /// Accept clients on `listener`, handing each to `client` (with the connection), until
    /// stopped or the connection ends.
    async fn serve_listener<F, Fut>(
        &self,
        listener: &TcpListener,
        connection: Arc<Connection>,
        client: F,
    ) -> Ended
    where
        F: Fn(Arc<Connection>, TcpStream, SocketAddr) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        if let Ok(addr) = listener.local_addr() {
            self.port.store(u32::from(addr.port()), Ordering::Release);
        }
        let mut clients = Clients::new(&self.reporter);
        let closed = connection.closed();
        tokio::pin!(closed);
        let mut stop = self.stop.clone();
        let result = loop {
            tokio::select! {
                biased;
                () = stopped(&mut stop) => break Ended::Stopped,
                reason = &mut closed => break Ended::Lost(reason),
                () = clients.changed() => {}
                accepted = listener.accept() => match accepted {
                    Ok((tcp, peer)) => {
                        let _ = tcp.set_nodelay(true);
                        clients.spawn(client(connection.clone(), tcp, peer));
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "tunnel accept failed");
                        tokio::select! {
                            () = stopped(&mut stop) => {}
                            () = tokio::time::sleep(ACCEPT_PAUSE) => {}
                        }
                    }
                },
            }
            clients.report(&self.reporter);
        };
        clients.end().await;
        drop(connection);
        result
    }

    /// Ask the server to listen on `bind:port` and claim the routing of its connections.
    /// A refusal is `Failed` with the reason in the user's words.
    async fn request(
        &self,
        connection: &Connection,
        bind: &str,
        port: u16,
    ) -> Result<Forward, Ended> {
        let requested = u32::from(port);
        // A fixed port is claimed before it is requested, so no connection arriving right
        // after the server's answer is refused. Port 0 can only be claimed once the server
        // has said which port it chose.
        let taken = |port: u32| {
            Ended::Failed(format!(
                "server refused the forward: {bind} port {port} is already used by another tunnel"
            ))
        };
        let early = match requested {
            0 => None,
            port => Some(
                connection
                    .claim_forward(bind, port)
                    .ok_or_else(|| taken(port))?,
            ),
        };
        let mut stop = self.stop.clone();
        let bound = tokio::select! {
            biased;
            () = stopped(&mut stop) => {
                // The server may have granted it meanwhile: take it back (a port it chose is
                // unknown here and stays until the connection ends; its channels are refused).
                if requested != 0 {
                    let _ = tokio::time::timeout(
                        CANCEL_GRACE,
                        connection.target().cancel_tcpip_forward(bind, requested),
                    )
                    .await;
                }
                return Err(Ended::Stopped);
            }
            reason = connection.closed() => return Err(Ended::Lost(reason)),
            bound = request_forward(connection, bind, requested) => bound.map_err(Ended::Failed)?,
        };
        let (claim, channels) = match early {
            Some(claimed) => claimed,
            None => match connection.claim_forward(bind, bound) {
                Some(claimed) => claimed,
                None => {
                    // Another tunnel holds the routing for the port the server just gave us
                    // (possible only after a forward outside this app's control ended).
                    let _ = tokio::time::timeout(
                        CANCEL_GRACE,
                        connection.target().cancel_tcpip_forward(bind, bound),
                    )
                    .await;
                    return Err(taken(bound));
                }
            },
        };
        Ok(Forward {
            bind: bind.to_owned(),
            bound,
            claim,
            channels,
        })
    }

    /// Connect each of the server's connections on `forward` to `host:host_port` here, until
    /// stopped or the connection ends.
    async fn serve_forward(
        &self,
        connection: Arc<Connection>,
        forward: Forward,
        host: String,
        host_port: u16,
    ) -> Ended {
        let Forward {
            bind,
            bound,
            claim,
            mut channels,
        } = forward;
        self.port.store(bound, Ordering::Release);
        let mut clients = Clients::new(&self.reporter);
        let closed = connection.closed();
        tokio::pin!(closed);
        let mut stop = self.stop.clone();
        let result = loop {
            tokio::select! {
                biased;
                () = stopped(&mut stop) => break Ended::Stopped,
                reason = &mut closed => break Ended::Lost(reason),
                () = clients.changed() => {}
                Some(channel) = channels.recv() => {
                    clients.spawn(forward_remote(channel, host.clone(), host_port));
                }
            }
            clients.report(&self.reporter);
        };
        // Refuse new channels first, close the ones still queued (a raw channel dropped as it
        // is would stay open on the server), then end the live ones and the server's listener.
        drop(claim);
        channels.close();
        while let Ok(queued) = channels.try_recv() {
            drop(queued.into_stream());
        }
        clients.end().await;
        if matches!(result, Ended::Stopped) {
            let _ = tokio::time::timeout(
                CANCEL_GRACE,
                connection
                    .target()
                    .cancel_tcpip_forward(bind.as_str(), bound),
            )
            .await;
        }
        drop(connection);
        result
    }
}

/// What a reconnect's failed no-prompt connect means for the tunnel. Anything that needs the
/// user ends the reconnecting at once: an unanswered question, and a changed host key (a
/// security stop that only the user can clear, never worth retrying).
fn no_prompt_attempt(e: SshError) -> Attempt {
    match e {
        SshError::NeedsInput | SshError::HostKeyChanged { .. } => Attempt::NeedsInput,
        e => Attempt::Failed(e.to_string()),
    }
}

/// How long to wait before reconnect attempt `attempt` (counted from 1): 1, 2, 4, 8 and 16
/// seconds, then every 30 seconds.
fn backoff(attempt: u32) -> Duration {
    let secs = match attempt {
        0 | 1 => 1,
        2 => 2,
        3 => 4,
        4 => 8,
        5 => 16,
        _ => 30,
    };
    Duration::from_secs(secs)
}

/// Close every client `accept` gives at once (a tunnel that is reconnecting cannot forward
/// them yet), pausing [`ACCEPT_PAUSE`] after a failed accept (out of file descriptors, say)
/// so a lasting error does not spin. Never ends; it is raced against what it waits for.
async fn refuse_clients<T, F, Fut>(mut accept: F) -> std::convert::Infallible
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::io::Result<T>>,
{
    loop {
        match accept().await {
            Ok(client) => drop(client),
            Err(e) => {
                tracing::debug!(error = %e, "tunnel accept failed while reconnecting");
                tokio::time::sleep(ACCEPT_PAUSE).await;
            }
        }
    }
}

/// The next client of `listener`, or never without one.
async fn accept(listener: Option<&TcpListener>) -> std::io::Result<(TcpStream, SocketAddr)> {
    match listener {
        Some(l) => l.accept().await,
        None => std::future::pending().await,
    }
}

/// The address to bind or request: trimmed, and without the brackets of an IPv6 literal.
fn listen_address(bind: &str) -> &str {
    let bind = bind.trim();
    bind.strip_prefix('[')
        .and_then(|b| b.strip_suffix(']'))
        .unwrap_or(bind)
}

/// Listen on `bind:port` here, with the reason in the user's words on failure.
async fn listen(bind: &str, port: u16) -> Result<TcpListener, String> {
    TcpListener::bind((listen_address(bind), port))
        .await
        .map_err(|e| bind_error(port, &e))
}

fn bind_error(port: u16, e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::AddrInUse => format!("port {port} already in use"),
        std::io::ErrorKind::PermissionDenied => format!("port {port} needs administrator rights"),
        _ => e.to_string(),
    }
}

/// Ask the server for the remote forward; returns the port it listens on.
async fn request_forward(connection: &Connection, bind: &str, port: u32) -> Result<u32, String> {
    let refused = |reason: String| format!("server refused the forward: {reason}");
    match tokio::time::timeout(HOP_TIMEOUT, connection.target().tcpip_forward(bind, port)).await {
        Err(_) => Err(refused("no answer in time".into())),
        Ok(Err(russh::Error::RequestDenied)) if port == 0 => {
            Err(refused(format!("listening on {bind} is not allowed")))
        }
        Ok(Err(russh::Error::RequestDenied)) => Err(refused(format!(
            "{bind} port {port} is taken or not allowed"
        ))),
        Ok(Err(e)) => Err(refused(e.to_string())),
        Ok(Ok(bound)) => Ok(if port == 0 { bound } else { port }),
    }
}

/// The live client connections of a tunnel, counted and reported coalesced.
struct Clients {
    tasks: JoinSet<()>,
    live: Arc<AtomicUsize>,
    changed: Arc<Notify>,
    counts: Coalesce,
}

/// Counts one live client; dropped when its task ends (or is aborted).
struct Live {
    live: Arc<AtomicUsize>,
    changed: Arc<Notify>,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::AcqRel);
        self.changed.notify_one();
    }
}

impl Clients {
    /// Reports `Up` with no connections.
    fn new(reporter: &Reporter) -> Self {
        reporter.send(TunnelState::Up { connections: 0 });
        Self {
            tasks: JoinSet::new(),
            live: Arc::default(),
            changed: Arc::default(),
            counts: Coalesce::default(),
        }
    }

    fn spawn(&mut self, client: impl Future<Output = ()> + Send + 'static) {
        self.live.fetch_add(1, Ordering::AcqRel);
        let live = Live {
            live: self.live.clone(),
            changed: self.changed.clone(),
        };
        self.tasks.spawn(async move {
            let _live = live;
            client.await;
        });
    }

    /// Resolves when the count changed or a delayed report is due.
    async fn changed(&mut self) {
        while self.tasks.try_join_next().is_some() {}
        match self.counts.due {
            Some(due) => tokio::select! {
                () = self.changed.notified() => {}
                () = tokio::time::sleep_until(due) => {}
            },
            None => self.changed.notified().await,
        }
    }

    fn report(&mut self, reporter: &Reporter) {
        let count = self.live.load(Ordering::Acquire);
        if let Some(connections) = self.counts.update(count, Instant::now()) {
            reporter.send(TunnelState::Up { connections });
        }
    }

    /// End every live client connection.
    async fn end(mut self) {
        self.tasks.shutdown().await;
    }
}

/// Decides when a connection count is reported: a change at most every [`COUNT_INTERVAL`]
/// (a later one waits until `due`), and a drop to zero at once.
#[derive(Default)]
struct Coalesce {
    reported: usize,
    last: Option<Instant>,
    due: Option<Instant>,
}

impl Coalesce {
    /// The count to report now, if any.
    fn update(&mut self, count: usize, now: Instant) -> Option<usize> {
        if count == self.reported {
            self.due = None;
            return None;
        }
        let ready = self.last.is_none_or(|t| now >= t + COUNT_INTERVAL);
        if count == 0 || ready {
            self.reported = count;
            self.last = Some(now);
            self.due = None;
            Some(count)
        } else {
            self.due = self.last.map(|t| t + COUNT_INTERVAL);
            None
        }
    }
}

/// Copy between a client and its channel until both sides are done.
async fn pipe(mut tcp: TcpStream, mut stream: ChannelStream<Msg>) {
    if let Err(e) = tokio::io::copy_bidirectional(&mut tcp, &mut stream).await {
        tracing::debug!(error = %e, "tunnel connection ended");
    }
}

/// Open a `direct-tcpip` channel to `host:port` on the server, as a SOCKS failure on error.
/// The channel comes as a stream at once: a stream closes the channel when dropped (a raw
/// channel would not), whichever path lets go of it.
async fn open_direct(
    connection: &Connection,
    host: String,
    port: u16,
    peer: SocketAddr,
) -> Result<ChannelStream<Msg>, OpenFailure> {
    let open = connection.target().channel_open_direct_tcpip(
        host,
        u32::from(port),
        peer.ip().to_string(),
        u32::from(peer.port()),
    );
    match tokio::time::timeout(HOP_TIMEOUT, open).await {
        Err(_) => Err(OpenFailure::Unreachable),
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "the server did not open a tunnel channel");
            Err(open_failure(&e))
        }
        Ok(Ok(channel)) => Ok(channel.into_stream()),
    }
}

/// One client of a local forward.
async fn forward_local(
    connection: Arc<Connection>,
    tcp: TcpStream,
    peer: SocketAddr,
    host: String,
    port: u16,
) {
    let opened = open_direct(&connection, host, port, peer).await;
    drop(connection);
    // A refused channel closes just this client.
    if let Ok(channel) = opened {
        pipe(tcp, channel).await;
    }
}

/// One client of a SOCKS proxy.
async fn serve_socks(connection: Arc<Connection>, mut tcp: TcpStream, peer: SocketAddr) {
    let handshake = socks_accept(&mut tcp, |host, port| {
        let connection = connection.clone();
        async move { open_direct(&connection, host, port, peer).await }
    });
    let opened = tokio::time::timeout(SOCKS_HANDSHAKE, handshake).await;
    drop(connection);
    if let Ok(Ok(Some(channel))) = opened {
        pipe(tcp, channel).await;
    }
}

/// One connection of a remote forward: the server's channel to `host:port` here.
async fn forward_remote(channel: Channel<Msg>, host: String, port: u16) {
    // As a stream the channel is closed when dropped, on every path below; the server's
    // client then sees the connection end.
    let channel = channel.into_stream();
    let tcp = tokio::time::timeout(HOP_TIMEOUT, TcpStream::connect((host.as_str(), port))).await;
    match tcp {
        Ok(Ok(tcp)) => {
            let _ = tcp.set_nodelay(true);
            pipe(tcp, channel).await;
        }
        Ok(Err(e)) => tracing::debug!(error = %e, "remote forward could not connect here"),
        Err(_) => tracing::debug!("remote forward timed out connecting here"),
    }
}

/// Why the server did not open a channel, as far as a SOCKS client needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenFailure {
    /// The server could not connect (it does not say why; mostly a closed port).
    Refused,
    /// No answer in time.
    Unreachable,
    /// The server does not allow it.
    Prohibited,
    Other,
}

fn open_failure(e: &russh::Error) -> OpenFailure {
    match e {
        russh::Error::ChannelOpenFailure(ChannelOpenFailure::ConnectFailed) => OpenFailure::Refused,
        russh::Error::ChannelOpenFailure(ChannelOpenFailure::AdministrativelyProhibited) => {
            OpenFailure::Prohibited
        }
        _ => OpenFailure::Other,
    }
}

// SOCKS5 (RFC 1928) reply codes.
const SOCKS_OK: u8 = 0x00;
const SOCKS_GENERAL_FAILURE: u8 = 0x01;
const SOCKS_NOT_ALLOWED: u8 = 0x02;
const SOCKS_HOST_UNREACHABLE: u8 = 0x04;
const SOCKS_REFUSED: u8 = 0x05;
const SOCKS_COMMAND_NOT_SUPPORTED: u8 = 0x07;
const SOCKS_ADDRESS_NOT_SUPPORTED: u8 = 0x08;
/// "No acceptable methods" in the method selection reply.
const SOCKS_NO_METHOD: u8 = 0xFF;

impl OpenFailure {
    fn socks_reply(self) -> u8 {
        match self {
            Self::Refused => SOCKS_REFUSED,
            Self::Unreachable => SOCKS_HOST_UNREACHABLE,
            Self::Prohibited => SOCKS_NOT_ALLOWED,
            Self::Other => SOCKS_GENERAL_FAILURE,
        }
    }
}

async fn socks_reply<S: AsyncWrite + Unpin>(client: &mut S, code: u8) -> std::io::Result<()> {
    // The bound address is not ours to tell (the server connects); all zeros, as IPv4.
    client.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    client.flush().await
}

/// Serve one SOCKS5 client up to its CONNECT: method selection (only "no authentication"),
/// then the request. CONNECT opens what `open` returns for the requested host (a name passed
/// as typed, an address as its text) and port; everything else is answered with the matching
/// error code. `None` means the client was answered with an error (or was not SOCKS5).
async fn socks_accept<S, T, F, Fut>(client: &mut S, open: F) -> std::io::Result<Option<T>>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce(String, u16) -> Fut,
    Fut: Future<Output = Result<T, OpenFailure>>,
{
    let mut head = [0u8; 2];
    client.read_exact(&mut head).await?;
    if head[0] != 5 {
        return Ok(None);
    }
    let mut methods = vec![0u8; usize::from(head[1])];
    client.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        client.write_all(&[5, SOCKS_NO_METHOD]).await?;
        client.flush().await?;
        return Ok(None);
    }
    client.write_all(&[5, 0]).await?;
    client.flush().await?;

    let mut request = [0u8; 4];
    client.read_exact(&mut request).await?;
    let [version, command, _reserved, address_type] = request;
    if version != 5 {
        socks_reply(client, SOCKS_GENERAL_FAILURE).await?;
        return Ok(None);
    }
    let host = match address_type {
        1 => {
            let mut a = [0u8; 4];
            client.read_exact(&mut a).await?;
            Some(Ipv4Addr::from(a).to_string())
        }
        4 => {
            let mut a = [0u8; 16];
            client.read_exact(&mut a).await?;
            Some(Ipv6Addr::from(a).to_string())
        }
        3 => {
            let mut len = [0u8; 1];
            client.read_exact(&mut len).await?;
            let mut name = vec![0u8; usize::from(len[0])];
            client.read_exact(&mut name).await?;
            String::from_utf8(name).ok().filter(|n| !n.is_empty())
        }
        _ => {
            socks_reply(client, SOCKS_ADDRESS_NOT_SUPPORTED).await?;
            return Ok(None);
        }
    };
    let mut port = [0u8; 2];
    client.read_exact(&mut port).await?;
    let port = u16::from_be_bytes(port);
    if command != 1 {
        socks_reply(client, SOCKS_COMMAND_NOT_SUPPORTED).await?;
        return Ok(None);
    }
    let Some(host) = host else {
        socks_reply(client, SOCKS_HOST_UNREACHABLE).await?;
        return Ok(None);
    };
    match open(host, port).await {
        Ok(opened) => {
            socks_reply(client, SOCKS_OK).await?;
            Ok(Some(opened))
        }
        Err(failure) => {
            socks_reply(client, failure.socks_reply()).await?;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex};

    #[tokio::test]
    async fn inert_handle_is_already_stopped() {
        let handle = TunnelHandle::inert();
        assert!(handle.is_stopped());
        assert_eq!(handle.port(), None);
        handle.stop_and_wait().await;
    }

    /// What the SOCKS side ended with (whether it opened something) and what it was asked
    /// to open.
    type Served = tokio::task::JoinHandle<(std::io::Result<bool>, Option<(String, u16)>)>;

    /// Runs the SOCKS side on one end of a pipe; `open` records what it was asked for and
    /// answers with `result`.
    fn serve(result: Result<(), OpenFailure>) -> (DuplexStream, Served) {
        let (client, mut server) = duplex(1024);
        let task = tokio::spawn(async move {
            let mut asked = None;
            let out = socks_accept(&mut server, |host, port| {
                asked = Some((host, port));
                async move { result }
            })
            .await
            .map(|o| o.is_some());
            (out, asked)
        });
        (client, task)
    }

    async fn read_n(s: &mut DuplexStream, n: usize) -> Vec<u8> {
        let mut buf = vec![0; n];
        tokio::time::timeout(std::time::Duration::from_secs(2), s.read_exact(&mut buf))
            .await
            .expect("read timed out")
            .expect("read");
        buf
    }

    #[tokio::test]
    async fn refusing_clients_pauses_after_accept_errors() {
        // An accept that always fails (out of file descriptors, say) must not spin.
        let calls = AtomicUsize::new(0);
        let refusing = refuse_clients(|| async {
            calls.fetch_add(1, Ordering::Relaxed);
            tokio::task::yield_now().await;
            Err::<(), _>(std::io::Error::other("too many open files"))
        });
        let _ = tokio::time::timeout(Duration::from_millis(350), refusing).await;
        let calls = calls.load(Ordering::Relaxed);
        assert!((1..=5).contains(&calls), "{calls} accepts in 350 ms");
    }

    #[tokio::test]
    async fn refusing_clients_closes_each_at_once() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let refusing = tokio::spawn(async move {
            let _ = refuse_clients(|| listener.accept()).await;
        });
        for _ in 0..3 {
            let mut client = TcpStream::connect(addr).await.unwrap();
            let mut buf = [0u8; 1];
            let read = tokio::time::timeout(Duration::from_secs(2), client.read(&mut buf))
                .await
                .expect("client not closed");
            assert!(matches!(read, Ok(0) | Err(_)));
        }
        refusing.abort();
    }

    #[test]
    fn backoff_sequence() {
        let secs: Vec<u64> = (1..=8).map(|a| backoff(a).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(backoff(100), Duration::from_secs(30));
        assert_eq!(backoff(u32::MAX), Duration::from_secs(30));
    }

    #[tokio::test]
    async fn socks_greeting_rejects_auth_methods() {
        // Username/password and GSSAPI only: nothing acceptable.
        let (mut c, task) = serve(Ok(()));
        c.write_all(&[5, 2, 2, 1]).await.unwrap();
        assert_eq!(read_n(&mut c, 2).await, [5, 0xFF]);
        let (out, asked) = task.await.unwrap();
        assert!(!out.unwrap());
        assert_eq!(asked, None);

        // "No authentication" among others is chosen.
        let (mut c, task) = serve(Ok(()));
        c.write_all(&[5, 3, 2, 0, 1]).await.unwrap();
        assert_eq!(read_n(&mut c, 2).await, [5, 0]);
        drop(c);
        let (_, asked) = task.await.unwrap();
        assert_eq!(asked, None);

        // Not SOCKS5 at all: closed without a reply.
        let (mut c, task) = serve(Ok(()));
        c.write_all(&[4, 1, 0, 80, 1, 2, 3, 4, 0]).await.unwrap();
        let (out, asked) = task.await.unwrap();
        assert!(out.is_err() || !out.unwrap());
        assert_eq!(asked, None);
    }

    fn connect(atyp: u8, addr: &[u8], port: u16) -> Vec<u8> {
        let mut req = vec![5, 1, 0, atyp];
        req.extend_from_slice(addr);
        req.extend_from_slice(&port.to_be_bytes());
        req
    }

    #[tokio::test]
    async fn socks_connect_domain_is_not_resolved_locally() {
        let name = "db.internal.invalid";
        let mut addr = vec![u8::try_from(name.len()).unwrap()];
        addr.extend_from_slice(name.as_bytes());
        let (mut c, task) = serve(Ok(()));
        c.write_all(&[5, 1, 0]).await.unwrap();
        assert_eq!(read_n(&mut c, 2).await, [5, 0]);
        c.write_all(&connect(3, &addr, 5432)).await.unwrap();
        assert_eq!(read_n(&mut c, 10).await, [5, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        let (out, asked) = task.await.unwrap();
        assert!(out.unwrap());
        // The name goes to the server as typed: never looked up here.
        assert_eq!(asked, Some((name.to_owned(), 5432)));

        // IPv4 and IPv6 literals are passed as their text.
        let (mut c, task) = serve(Ok(()));
        c.write_all(&[5, 1, 0]).await.unwrap();
        read_n(&mut c, 2).await;
        c.write_all(&connect(1, &[10, 0, 0, 7], 22)).await.unwrap();
        assert_eq!(read_n(&mut c, 10).await[1], 0);
        assert_eq!(task.await.unwrap().1, Some(("10.0.0.7".to_owned(), 22)));

        let (mut c, task) = serve(Ok(()));
        c.write_all(&[5, 1, 0]).await.unwrap();
        read_n(&mut c, 2).await;
        let v6: std::net::Ipv6Addr = "fd00::5".parse().unwrap();
        c.write_all(&connect(4, &v6.octets(), 443)).await.unwrap();
        assert_eq!(read_n(&mut c, 10).await[1], 0);
        assert_eq!(task.await.unwrap().1, Some(("fd00::5".to_owned(), 443)));

        // Channel-open failures map to SOCKS reply codes.
        for (failure, code) in [
            (OpenFailure::Refused, 5),
            (OpenFailure::Unreachable, 4),
            (OpenFailure::Prohibited, 2),
            (OpenFailure::Other, 1),
        ] {
            let (mut c, task) = serve(Err(failure));
            c.write_all(&[5, 1, 0]).await.unwrap();
            read_n(&mut c, 2).await;
            c.write_all(&connect(3, &addr, 5432)).await.unwrap();
            assert_eq!(read_n(&mut c, 10).await[1], code, "{failure:?}");
            assert!(!task.await.unwrap().0.unwrap());
        }

        // An unknown address type is refused with 0x08.
        let (mut c, task) = serve(Ok(()));
        c.write_all(&[5, 1, 0]).await.unwrap();
        read_n(&mut c, 2).await;
        c.write_all(&connect(9, &[1, 2, 3, 4], 22)).await.unwrap();
        assert_eq!(read_n(&mut c, 10).await[1], 8);
        assert_eq!(task.await.unwrap().1, None);
    }

    #[test]
    fn counts_are_coalesced() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut c = Coalesce::default();
        // Unchanged: nothing to say.
        assert_eq!(c.update(0, t0), None);
        // The first change goes out at once, the next ones wait for the interval.
        assert_eq!(c.update(1, ms(0)), Some(1));
        assert_eq!(c.update(2, ms(10)), None);
        assert_eq!(c.due, Some(ms(250)));
        assert_eq!(c.update(5, ms(100)), None);
        assert_eq!(c.update(5, ms(250)), Some(5));
        assert_eq!(c.due, None);
        // Back to the reported value before the interval: nothing pending.
        assert_eq!(c.update(6, ms(260)), None);
        assert_eq!(c.update(5, ms(270)), None);
        assert_eq!(c.due, None);
        // Zero is reported at once, however soon.
        assert_eq!(c.update(3, ms(300)), None);
        assert_eq!(c.update(0, ms(301)), Some(0));
        assert_eq!(c.due, None);
        assert_eq!(c.update(1, ms(320)), None);
        assert_eq!(c.due, Some(ms(551)));
    }

    #[test]
    fn bind_addresses_are_trimmed_and_errors_name_the_port() {
        assert_eq!(listen_address(" 127.0.0.1 "), "127.0.0.1");
        assert_eq!(listen_address("[::1]"), "::1");
        assert_eq!(listen_address(" [::1] "), "::1");
        assert_eq!(listen_address("localhost"), "localhost");
        let e = |kind| std::io::Error::new(kind, "os says no");
        assert_eq!(
            bind_error(5432, &e(std::io::ErrorKind::AddrInUse)),
            "port 5432 already in use"
        );
        assert_eq!(
            bind_error(80, &e(std::io::ErrorKind::PermissionDenied)),
            "port 80 needs administrator rights"
        );
        assert_eq!(
            bind_error(80, &e(std::io::ErrorKind::AddrNotAvailable)),
            "os says no"
        );
        assert_eq!(
            open_failure(&russh::Error::ChannelOpenFailure(
                ChannelOpenFailure::ConnectFailed
            )),
            OpenFailure::Refused
        );
        assert_eq!(
            open_failure(&russh::Error::ChannelOpenFailure(
                ChannelOpenFailure::AdministrativelyProhibited
            )),
            OpenFailure::Prohibited
        );
        assert_eq!(open_failure(&russh::Error::Disconnect), OpenFailure::Other);
    }

    /// A prompter that records whether it was asked anything and always says no.
    #[derive(Default)]
    struct Asked(std::sync::atomic::AtomicBool);

    #[async_trait::async_trait]
    impl Prompter for Asked {
        async fn ask_key_file(&self, _: &str) -> Option<std::path::PathBuf> {
            self.0.store(true, Ordering::SeqCst);
            None
        }

        async fn confirm_host_key(&self, _: &str, _: u16, _: &str) -> bool {
            self.0.store(true, Ordering::SeqCst);
            false
        }

        async fn ask_secret(
            &self,
            _: &str,
            _: fbterm_secrets::SecretKind,
            _: bool,
        ) -> Option<super::super::SecretAnswer> {
            self.0.store(true, Ordering::SeqCst);
            None
        }

        async fn unlock_vault(&self) -> Option<fbterm_secrets::Zeroizing<String>> {
            self.0.store(true, Ordering::SeqCst);
            None
        }
    }

    #[tokio::test]
    async fn taken_local_port_fails_before_connecting() {
        use fbterm_config::{AuthMethod, Profile, ProfileKind, SshProfile, StartMode};
        // The server accepts TCP and never answers: connecting would take the whole connect
        // timeout (and could ask questions) before the port was ever tried.
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        let def = TunnelDef {
            name: "db".into(),
            kind: TunnelKind::Local {
                bind: "127.0.0.1".into(),
                port,
                host: "db.internal".into(),
                host_port: 5432,
            },
            start: StartMode::OnDemand,
        };
        let profile = Profile {
            kind: ProfileKind::Ssh(SshProfile {
                host: "127.0.0.1".into(),
                port: silent.local_addr().unwrap().port(),
                user: "user".into(),
                auth: AuthMethod::Password,
                jump: vec![],
                tunnels: vec![def.clone()],
            }),
            id: ProfileId::new(),
            name: "srv".into(),
            group: None,
            accent: None,
            accent_style: Default::default(),
            color_scheme: None,
        };
        let asked = Arc::new(Asked::default());
        let pool = Arc::new(SshPool::new(super::super::SshContext {
            known_hosts: std::env::temp_dir().join("fbterm-unused-known-hosts"),
            secrets: Arc::new(fbterm_secrets::MemoryStore::new()),
            prompter: asked.clone(),
        }));
        let key = TunnelKey {
            profile: profile.id,
            name: "db".into(),
        };
        let (sink, mut rx) = mpsc::unbounded_channel();
        let store = ProfileStore {
            profiles: vec![profile],
        };
        let handle = start_tunnel(
            pool,
            store,
            key,
            def,
            asked.clone(),
            Arc::new(AtomicU32::new(0)),
            sink,
        );
        let mut last = None;
        tokio::time::timeout(Duration::from_secs(3), async {
            while let Some((_, state)) = rx.recv().await {
                last = Some(state);
                if matches!(last, Some(TunnelState::Failed(_))) {
                    break;
                }
            }
        })
        .await
        .expect("the taken port fails the tunnel at once, without connecting");
        assert_eq!(
            last,
            Some(TunnelState::Failed(format!("port {port} already in use")))
        );
        assert!(!asked.0.load(Ordering::SeqCst), "nothing was asked");
        handle.stopped().await;
    }

    #[test]
    fn reconnect_needing_the_user_fails_at_once() {
        let needs = |e: SshError| matches!(no_prompt_attempt(e), Attempt::NeedsInput);
        assert!(needs(SshError::NeedsInput));
        // A changed host key is a security stop, never retried.
        assert!(needs(SshError::HostKeyChanged {
            host: "db.internal".into(),
            port: 22,
        }));
        assert!(!needs(SshError::Connect(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "refused",
        ))));
        assert!(!needs(SshError::AuthFailed {
            profile: "srv".into()
        }));
    }

    #[tokio::test]
    async fn listening_on_a_taken_port_fails_with_reason() {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        let err = listen(" 127.0.0.1 ", port).await.err().unwrap();
        assert_eq!(err, format!("port {port} already in use"));
        // Trimmed, the address binds.
        let ok = listen(" 127.0.0.1 ", 0).await.unwrap();
        assert!(ok.local_addr().unwrap().ip().is_loopback());
    }

    #[tokio::test]
    async fn socks_bind_and_udp_are_refused() {
        for cmd in [2u8, 3] {
            let (mut c, task) = serve(Ok(()));
            c.write_all(&[5, 1, 0]).await.unwrap();
            assert_eq!(read_n(&mut c, 2).await, [5, 0]);
            let mut req = connect(1, &[127, 0, 0, 1], 80);
            req[1] = cmd;
            c.write_all(&req).await.unwrap();
            assert_eq!(read_n(&mut c, 10).await[1], 7, "command {cmd}");
            let (out, asked) = task.await.unwrap();
            assert!(!out.unwrap());
            assert_eq!(asked, None, "command {cmd} must not open anything");
        }
    }
}
