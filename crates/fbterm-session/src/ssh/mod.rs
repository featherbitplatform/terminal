//! SSH connections: transport, host-key trust and authentication for one hop.
//!
//! [`connect_hop`] runs SSH over any byte stream (a TCP socket for the first hop, a
//! `direct-tcpip` channel for later ones), checks the server's host key against
//! `known_hosts` and authenticates with the profile's [`AuthMethod`]. Questions for the user
//! (trust this host? which password?) go through a [`Prompter`]; saved secrets come from a
//! [`SecretStore`].
//!
//! Security rules enforced here:
//! - a changed host key is refused without asking, and `known_hosts` is left untouched;
//! - an unknown host key is trusted only when the user confirms its SHA-256 fingerprint;
//! - a secret is saved only after the server accepted it and the user asked to remember it,
//!   and a saved secret is never removed because it was rejected;
//! - secret values are never logged and never appear in an [`SshError`].
//!
//! Every hop is bounded: the TCP connect (or `direct-tcpip` channel open) and the SSH
//! handshake and authentication each get [`HOP_TIMEOUT`], not counting time spent waiting for
//! the user to answer a prompt. [`SshPool`] builds jump chains and shares one connection per
//! profile between all of its shells.
//!
//! Server-initiated channels are refused, with one exception: a `forwarded-tcpip` channel for
//! a remote forward this app requested on that connection (registered in its [`Forwards`])
//! reaches the code that asked for it. Hops of a jump chain have no registry and refuse them
//! all, so a remote program can never open a tunnel or a session of its own.

mod auth;
mod known_hosts;
pub(crate) mod pool;
pub mod sftp;
pub mod transfer;
pub mod tunnel;

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fbterm_config::{Profile, ProfileId, ProfileKind, SshProfile, ValidationError};
use fbterm_secrets::{SecretKind, SecretStore};
use parking_lot::Mutex;
use russh::client::{self, DisconnectReason, Handle, Msg, Session};
use russh::keys::{HashAlg, PublicKey, PublicKeyOrCertificate};
use russh::{Channel, ChannelOpenFailure};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Notify, mpsc, watch};

pub use known_hosts::{HostKeyStatus, check_host_key, learn_host_key};
pub use pool::{Connection, SshPool};

use crate::AuthWarning;

/// Whether `contents` (a private key file) holds an RSA key, decided without the passphrase:
/// the legacy PEM header names the algorithm, and an OpenSSH key file carries its public half
/// in clear even when encrypted. Anything else (other formats, garbage) is `false`.
pub fn private_key_is_rsa(contents: &str) -> bool {
    if contents
        .trim_start()
        .starts_with("-----BEGIN RSA PRIVATE KEY-----")
    {
        return true;
    }
    let is_rsa =
        |k: &russh::keys::PrivateKey| matches!(k.algorithm(), russh::keys::Algorithm::Rsa { .. });
    if russh::keys::PrivateKey::from_openssh(contents).is_ok_and(|k| is_rsa(&k)) {
        return true;
    }
    // Unencrypted PKCS#8 (`BEGIN PRIVATE KEY`). Encrypted PKCS#8 needs the passphrase to read
    // its algorithm, so it stays undetected here (the login itself still warns).
    russh::keys::decode_secret_key(contents, None).is_ok_and(|k| is_rsa(&k))
}

/// Bound on each step of reaching one hop: the TCP connect, and the SSH handshake plus
/// authentication. Time spent waiting on the user does not count.
pub const HOP_TIMEOUT: Duration = Duration::from_secs(15);

/// Seconds of silence from the server before a keepalive is sent.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
/// Unanswered keepalives after which the connection is considered dead.
const KEEPALIVE_MAX: usize = 2;

/// Asks the user the questions a connection needs answered.
#[async_trait]
pub trait Prompter: Send + Sync {
    /// Trust-on-first-use: should this unknown host key be trusted (and remembered)?
    /// `fingerprint_sha256` is OpenSSH style, `SHA256:<base64 without padding>`.
    async fn confirm_host_key(&self, host: &str, port: u16, fingerprint_sha256: &str) -> bool;

    /// Ask for a password or key passphrase. `retry` is true when a previous value (saved or
    /// typed) was rejected. `None` means the user cancelled.
    async fn ask_secret(
        &self,
        profile_name: &str,
        kind: SecretKind,
        retry: bool,
    ) -> Option<SecretAnswer>;

    /// Ask which private key file to use for a key profile that names none yet. `None`
    /// means the user cancelled (or nobody can be asked).
    async fn ask_key_file(&self, profile_name: &str) -> Option<PathBuf>;

    /// The key file the user chose for `profile` (with [`Prompter::ask_key_file`]) was
    /// accepted by the server: the app remembers it in the profile. Never called before the
    /// key logged in, nor for a key file the profile already names.
    fn key_file_accepted(&self, _profile: ProfileId, _path: &Path) {}

    /// Whether this prompter can reach a person at all. `false` for [`NoPrompt`]: a connect
    /// that would have needed a question then fails with [`SshError::NeedsInput`].
    fn can_ask(&self) -> bool {
        true
    }
}

/// The prompter of automatic reconnects: never asks anyone. Unknown host keys are not
/// trusted and no secret is given, so a connect that needs the user fails at once (as
/// [`SshError::NeedsInput`] from [`SshPool::acquire`]) instead of opening a prompt. That
/// includes a saved secret the server rejects: asking again for a better one is a question.
pub struct NoPrompt;

#[async_trait]
impl Prompter for NoPrompt {
    async fn confirm_host_key(&self, _host: &str, _port: u16, _fingerprint: &str) -> bool {
        false
    }

    async fn ask_secret(
        &self,
        _profile_name: &str,
        _kind: SecretKind,
        _retry: bool,
    ) -> Option<SecretAnswer> {
        None
    }

    async fn ask_key_file(&self, _profile_name: &str) -> Option<PathBuf> {
        None
    }

    fn can_ask(&self) -> bool {
        false
    }
}

/// A secret typed by the user, and whether to save it once the server accepts it.
#[derive(Clone)]
pub struct SecretAnswer {
    pub value: String,
    pub remember: bool,
}

impl std::fmt::Debug for SecretAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretAnswer")
            .field("value", &"<redacted>")
            .field("remember", &self.remember)
            .finish()
    }
}

/// Everything a connection needs besides the profile itself.
#[derive(Clone)]
pub struct SshContext {
    /// The `known_hosts` file to check and extend (normally `~/.ssh/known_hosts`).
    pub known_hosts: PathBuf,
    pub secrets: Arc<dyn SecretStore>,
    pub prompter: Arc<dyn Prompter>,
}

/// Why a connection failed. Messages never contain secret values.
#[derive(Debug, thiserror::Error)]
pub enum SshError {
    #[error(
        "the host key of {host} port {port} has changed; refusing to connect. Someone may be \
         intercepting the connection. If the server was reinstalled, remove its old entry from \
         known_hosts and connect again."
    )]
    HostKeyChanged { host: String, port: u16 },
    #[error("the host key was not trusted")]
    HostKeyRejected,
    #[error("authentication failed for {profile:?}")]
    AuthFailed { profile: String },
    #[error("cancelled")]
    Cancelled,
    #[error("could not connect: {0}")]
    Connect(std::io::Error),
    #[error("invalid jump chain: {0}")]
    Jump(ValidationError),
    #[error("ssh error: {0}")]
    Protocol(String),
    /// A connect made without prompts ([`NoPrompt`]) needed the user: an unknown host key or
    /// a secret that is not saved.
    #[error("needs your input")]
    NeedsInput,
}

impl SshError {
    /// An equal error, for handing one failed connect to every tab that waited on it.
    pub(crate) fn duplicate(&self) -> Self {
        match self {
            Self::HostKeyChanged { host, port } => Self::HostKeyChanged {
                host: host.clone(),
                port: *port,
            },
            Self::HostKeyRejected => Self::HostKeyRejected,
            Self::AuthFailed { profile } => Self::AuthFailed {
                profile: profile.clone(),
            },
            Self::Cancelled => Self::Cancelled,
            Self::Connect(e) => Self::Connect(std::io::Error::new(e.kind(), e.to_string())),
            Self::Jump(e) => Self::Jump(e.clone()),
            Self::Protocol(m) => Self::Protocol(m.clone()),
            Self::NeedsInput => Self::NeedsInput,
        }
    }

    /// The error of a connect made for a prompter that cannot ask (`can_ask == false`): the
    /// refusals that stand for an unanswered question become [`SshError::NeedsInput`].
    pub(crate) fn without_prompts(self, can_ask: bool) -> Self {
        match self {
            Self::HostKeyRejected | Self::Cancelled if !can_ask => Self::NeedsInput,
            other => other,
        }
    }
}

fn timed_out(what: String) -> SshError {
    SshError::Connect(std::io::Error::new(std::io::ErrorKind::TimedOut, what))
}

impl From<russh::Error> for SshError {
    fn from(e: russh::Error) -> Self {
        match e {
            russh::Error::IO(io) => Self::Connect(io),
            other => Self::Protocol(other.to_string()),
        }
    }
}

/// The remote forwards requested on one connection, by the address and port the server
/// listens on (as requested, with the port the server allocated for a request of port 0).
/// Each forwarded channel for a listed forward goes to its sender.
pub(crate) type Forwards = Arc<Mutex<HashMap<(String, u32), mpsc::UnboundedSender<Channel<Msg>>>>>;

/// Where a server-initiated `forwarded-tcpip` channel for `address:port` goes: the sender of
/// a forward this app requested and still listens for, or `None` (refuse it). A hop of a jump
/// chain has no registry (`forwards == None`) and refuses every one.
fn forward_route(
    forwards: Option<&Forwards>,
    address: &str,
    port: u32,
) -> Option<mpsc::UnboundedSender<Channel<Msg>>> {
    forwards?
        .lock()
        .get(&(address.to_owned(), port))
        .filter(|tx| !tx.is_closed())
        .cloned()
}

/// The russh client handler of one hop: verifies the server's host key, records why the
/// connection ended and refuses channels the server opens unless they belong to a remote
/// forward this app requested.
pub struct ClientHandler {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    prompter: Arc<dyn Prompter>,
    /// The key accepted for this connection, so a re-key does not ask again.
    accepted: Option<PublicKey>,
    /// Why the connection ended, set once it has.
    lost: Arc<Mutex<Option<String>>>,
    /// Raised when the connect that created this handler was given up before it finished.
    cancel: Arc<Cancel>,
    /// Set to `true` once the connection ended (after `lost`). Dropped with the handler when
    /// russh's session task ends, which its receivers see as an end too.
    ended: watch::Sender<bool>,
    /// The remote forwards of this connection; `None` on a jump hop (refuses them all).
    forwards: Option<Forwards>,
}

/// Refuse a channel the server wanted to open. Logged at debug level only: a server can open
/// channels as fast as it likes, and must not be able to flood the log.
async fn refuse(kind: &str, reply: client::ChannelOpenHandle) {
    tracing::debug!(kind, "refused a channel opened by the server");
    reply
        .reject(ChannelOpenFailure::AdministrativelyProhibited)
        .await;
}

/// A one-way "give up" signal.
#[derive(Default)]
struct Cancel {
    raised: std::sync::atomic::AtomicBool,
    notify: Notify,
}

impl Cancel {
    fn raise(&self) {
        self.raised
            .store(true, std::sync::atomic::Ordering::Release);
        self.notify.notify_waiters();
    }

    fn is_cancelled(&self) -> bool {
        self.raised.load(std::sync::atomic::Ordering::Acquire)
    }

    async fn cancelled(&self) {
        loop {
            // Registered before the check, so a raise in between still wakes us.
            let notified = self.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

/// Raises its [`Cancel`] when dropped before [`Self::disarm`]: when the connect future that
/// owns it is dropped (aborted dial) or fails.
struct CancelOnDrop(Option<Arc<Cancel>>);

impl CancelOnDrop {
    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            cancel.raise();
        }
    }
}

impl client::Handler for ClientHandler {
    type Error = SshError;

    async fn check_server_key(
        &mut self,
        server_key: &PublicKeyOrCertificate,
    ) -> Result<bool, SshError> {
        let key = match server_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key,
            PublicKeyOrCertificate::Certificate(_) => {
                return Err(SshError::Protocol(
                    "host certificates are not supported".into(),
                ));
            }
        };
        if self
            .accepted
            .as_ref()
            .is_some_and(|a| a.key_data() == key.key_data())
        {
            return Ok(true);
        }
        match check_host_key(&self.known_hosts, &self.host, self.port, key)? {
            HostKeyStatus::Known => {}
            HostKeyStatus::Changed => {
                tracing::warn!(host = %self.host, port = self.port, "host key changed; refusing");
                return Err(SshError::HostKeyChanged {
                    host: self.host.clone(),
                    port: self.port,
                });
            }
            HostKeyStatus::Unknown => {
                let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
                // russh runs this check in a task of its own, which outlives a connect that
                // was given up (tab closed, dial aborted): the cancel signal closes the prompt
                // then, and a "yes" racing with it is ignored, so nothing is learned.
                let trusted = tokio::select! {
                    trusted = self.prompter.confirm_host_key(&self.host, self.port, &fingerprint) => trusted,
                    () = self.cancel.cancelled() => false,
                };
                if self.cancel.is_cancelled() {
                    return Err(SshError::Cancelled);
                }
                if !trusted {
                    return Err(SshError::HostKeyRejected);
                }
                // Like OpenSSH: failing to record the key is reported but does not undo the
                // user's decision for this connection.
                if let Err(e) = learn_host_key(&self.known_hosts, &self.host, self.port, key) {
                    tracing::warn!(error = %e, "could not record host key in known_hosts");
                }
            }
        }
        self.accepted = Some(key.clone());
        Ok(true)
    }

    async fn disconnected(&mut self, reason: DisconnectReason<SshError>) -> Result<(), SshError> {
        let (text, result) = match reason {
            DisconnectReason::ReceivedDisconnect(info) if info.message.trim().is_empty() => {
                ("the server closed the connection".to_owned(), Ok(()))
            }
            DisconnectReason::ReceivedDisconnect(info) => (
                format!("the server closed the connection: {}", info.message.trim()),
                Ok(()),
            ),
            DisconnectReason::Error(SshError::Connect(io)) => {
                let text = format!("connection to {} lost: {io}", self.host);
                (text, Err(SshError::Connect(io)))
            }
            DisconnectReason::Error(e) => {
                (format!("connection to {} lost: {e}", self.host), Err(e))
            }
        };
        tracing::info!(host = %self.host, port = self.port, reason = %text, "ssh connection ended");
        self.lost.lock().get_or_insert(text);
        self.ended.send_replace(true);
        result
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<Msg>,
        connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), SshError> {
        match forward_route(self.forwards.as_ref(), connected_address, connected_port) {
            Some(tx) => {
                reply.accept().await;
                // A forward stopped in between hands the channel back: close it (a raw
                // channel dropped as it is would stay open on the server).
                if let Err(unrouted) = tx.send(channel) {
                    drop(unrouted.0.into_stream());
                }
            }
            None => {
                // Debug level (a server must not flood the log) and Debug-formatted (the
                // address is server-supplied text).
                tracing::debug!(
                    host = %self.host,
                    address = ?connected_address,
                    port = connected_port,
                    "refused a forwarded connection nobody asked for"
                );
                reply
                    .reject(ChannelOpenFailure::AdministrativelyProhibited)
                    .await;
            }
        }
        Ok(())
    }

    // Every other channel the server may open is refused: russh would accept them.

    async fn server_channel_open_forwarded_streamlocal(
        &mut self,
        _channel: Channel<Msg>,
        _socket_path: &str,
        reply: client::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), SshError> {
        refuse("forwarded-streamlocal", reply).await;
        Ok(())
    }

    async fn server_channel_open_agent_forward(
        &mut self,
        _channel: Channel<Msg>,
        reply: client::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), SshError> {
        refuse("agent", reply).await;
        Ok(())
    }

    async fn server_channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: client::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), SshError> {
        refuse("session", reply).await;
        Ok(())
    }

    async fn server_channel_open_direct_tcpip(
        &mut self,
        _channel: Channel<Msg>,
        _host_to_connect: &str,
        _port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), SshError> {
        refuse("direct-tcpip", reply).await;
        Ok(())
    }

    async fn server_channel_open_direct_streamlocal(
        &mut self,
        _channel: Channel<Msg>,
        _socket_path: &str,
        reply: client::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), SshError> {
        refuse("direct-streamlocal", reply).await;
        Ok(())
    }

    async fn server_channel_open_x11(
        &mut self,
        _channel: Channel<Msg>,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), SshError> {
        refuse("x11", reply).await;
        Ok(())
    }
}

/// Measures a hop's time against [`HOP_TIMEOUT`], excluding time spent in prompts.
struct PromptClock {
    state: Mutex<ClockState>,
    changed: Notify,
}

struct ClockState {
    started: Instant,
    /// Prompt time already finished since `started`.
    paused: Duration,
    /// Open prompts and when the first of them opened.
    prompts: usize,
    prompt_since: Option<Instant>,
}

impl PromptClock {
    fn new() -> Self {
        Self {
            state: Mutex::new(ClockState {
                started: Instant::now(),
                paused: Duration::ZERO,
                prompts: 0,
                prompt_since: None,
            }),
            changed: Notify::new(),
        }
    }

    /// Start a new step: elapsed time goes back to zero.
    fn restart(&self) {
        let mut s = self.state.lock();
        let now = Instant::now();
        s.started = now;
        s.paused = Duration::ZERO;
        if s.prompt_since.is_some() {
            s.prompt_since = Some(now);
        }
    }

    /// Time spent outside prompts since the step started, and whether a prompt is open.
    fn active(&self) -> (Duration, bool) {
        let s = self.state.lock();
        let now = Instant::now();
        let open = s.prompt_since.map_or(Duration::ZERO, |t| now - t);
        (
            (now - s.started).saturating_sub(s.paused + open),
            s.prompts > 0,
        )
    }

    fn pause(&self) -> PromptGuard<'_> {
        {
            let mut s = self.state.lock();
            s.prompts += 1;
            if s.prompts == 1 {
                s.prompt_since = Some(Instant::now());
            }
        }
        self.changed.notify_waiters();
        PromptGuard(self)
    }
}

/// Resumes the clock when the prompt ends (also when the prompt is cancelled).
struct PromptGuard<'a>(&'a PromptClock);

impl Drop for PromptGuard<'_> {
    fn drop(&mut self) {
        {
            let mut s = self.0.state.lock();
            s.prompts -= 1;
            if s.prompts == 0
                && let Some(since) = s.prompt_since.take()
            {
                s.paused += since.elapsed();
            }
        }
        self.0.changed.notify_waiters();
    }
}

/// Run `fut`, failing with a timeout once it has used `limit` outside of prompts.
async fn bounded<T>(
    clock: &PromptClock,
    limit: Duration,
    what: impl Fn() -> String,
    fut: impl Future<Output = T>,
) -> Result<T, SshError> {
    tokio::pin!(fut);
    loop {
        // Registered before reading the state, so a prompt opening or closing in between
        // still wakes us.
        let changed = clock.changed.notified();
        let (active, prompting) = clock.active();
        if prompting {
            tokio::select! {
                out = &mut fut => return Ok(out),
                () = changed => continue,
            }
        }
        let left = limit.saturating_sub(active);
        if left.is_zero() {
            return Err(timed_out(what()));
        }
        tokio::select! {
            out = &mut fut => return Ok(out),
            () = changed => {}
            () = tokio::time::sleep(left) => {}
        }
    }
}

/// Pauses the hop's clock while the user is being asked something.
struct ClockedPrompter {
    inner: Arc<dyn Prompter>,
    clock: Arc<PromptClock>,
}

#[async_trait]
impl Prompter for ClockedPrompter {
    async fn confirm_host_key(&self, host: &str, port: u16, fingerprint_sha256: &str) -> bool {
        let _paused = self.clock.pause();
        self.inner
            .confirm_host_key(host, port, fingerprint_sha256)
            .await
    }

    async fn ask_secret(
        &self,
        profile_name: &str,
        kind: SecretKind,
        retry: bool,
    ) -> Option<SecretAnswer> {
        let _paused = self.clock.pause();
        self.inner.ask_secret(profile_name, kind, retry).await
    }

    async fn ask_key_file(&self, profile_name: &str) -> Option<PathBuf> {
        let _paused = self.clock.pause();
        self.inner.ask_key_file(profile_name).await
    }

    fn key_file_accepted(&self, profile: ProfileId, path: &Path) {
        self.inner.key_file_accepted(profile, path);
    }

    fn can_ask(&self) -> bool {
        self.inner.can_ask()
    }
}

/// Client configuration for one hop: keepalives on, known host-key types preferred.
fn client_config(host: &str, port: u16, known_hosts: &std::path::Path) -> client::Config {
    let mut preferred = russh::Preferred::default();
    let known = known_hosts::known_algorithms(known_hosts, host, port);
    if !known.is_empty() {
        // Ask for a key type we already trust first, so a server with several host keys does
        // not look "changed" just because it offered a different type.
        let (mut order, rest): (Vec<_>, Vec<_>) = preferred
            .key
            .iter()
            .cloned()
            .partition(|a| known.contains(a));
        order.extend(rest);
        preferred.key = order.into();
    }
    client::Config {
        keepalive_interval: Some(KEEPALIVE_INTERVAL),
        keepalive_max: KEEPALIVE_MAX,
        nodelay: true,
        preferred,
        ..Default::default()
    }
}

fn ssh_profile(profile: &Profile) -> Result<&SshProfile, SshError> {
    match &profile.kind {
        ProfileKind::Ssh(ssh) => Ok(ssh),
        ProfileKind::Local(_) => Err(SshError::Protocol(format!(
            "profile {:?} is not an SSH profile",
            profile.name
        ))),
    }
}

/// Prefix a hop's connection and protocol errors with its profile name and address, so the
/// user can tell which hop of a chain failed. Other errors already say whose they are.
pub(crate) fn in_hop(profile: &Profile, e: SshError) -> SshError {
    let Ok(ssh) = ssh_profile(profile) else {
        return e;
    };
    let at = if ssh.host.contains(':') {
        format!("[{}]:{}", ssh.host, ssh.port)
    } else {
        format!("{}:{}", ssh.host, ssh.port)
    };
    let prefix = format!("{} ({at})", profile.name);
    match e {
        SshError::Connect(io) => {
            SshError::Connect(std::io::Error::new(io.kind(), format!("{prefix}: {io}")))
        }
        SshError::Protocol(m) => SshError::Protocol(format!("{prefix}: {m}")),
        other => other,
    }
}

/// One authenticated SSH connection of a chain.
pub(crate) struct Hop {
    pub handle: Handle<ClientHandler>,
    /// Why the connection ended, once it has.
    pub lost: Arc<Mutex<Option<String>>>,
    /// Turns `true` (or loses its sender) once the connection ended; `lost` is set first.
    pub ended: watch::Receiver<bool>,
    /// Set when authentication succeeded in a way the user should hear about.
    pub warning: Option<AuthWarning>,
    /// The key file the user chose for a key profile that named none (see
    /// [`Prompter::ask_key_file`]).
    pub chosen_key: Option<PathBuf>,
}

/// Run SSH over `stream` to the host of `profile`: verify its host key and authenticate.
/// `forwards` is the registry of the chain's target; jump hops pass `None`.
pub(crate) async fn connect_hop<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    stream: S,
    profile: &Profile,
    ctx: &SshContext,
    forwards: Option<Forwards>,
) -> Result<Hop, SshError> {
    connect_hop_within(stream, profile, ctx, HOP_TIMEOUT, forwards).await
}

/// [`connect_hop`] with the handshake and the authentication each bounded by `limit`.
async fn connect_hop_within<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    stream: S,
    profile: &Profile,
    ctx: &SshContext,
    limit: Duration,
    forwards: Option<Forwards>,
) -> Result<Hop, SshError> {
    let ssh = ssh_profile(profile)?;
    let clock = Arc::new(PromptClock::new());
    let ctx = SshContext {
        prompter: Arc::new(ClockedPrompter {
            inner: ctx.prompter.clone(),
            clock: clock.clone(),
        }),
        ..ctx.clone()
    };
    let lost = Arc::new(Mutex::new(None));
    let cancel = Arc::new(Cancel::default());
    let give_up = CancelOnDrop(Some(cancel.clone()));
    let (ended_tx, ended) = watch::channel(false);
    let handler = ClientHandler {
        ended: ended_tx,
        forwards,
        cancel,
        host: ssh.host.clone(),
        port: ssh.port,
        known_hosts: ctx.known_hosts.clone(),
        prompter: ctx.prompter.clone(),
        accepted: None,
        lost: lost.clone(),
    };
    let config = client_config(&ssh.host, ssh.port, &ctx.known_hosts);
    // Limitation: if the server stalls after the banner, the timeout drops our side of the
    // handshake, but the session task russh already spawned keeps the socket until the server
    // gives up (its LoginGraceTime); russh offers no handle to abort that task.
    let mut handle = bounded(
        &clock,
        limit,
        || format!("no SSH handshake with {} port {}", ssh.host, ssh.port),
        client::connect_stream(Arc::new(config), stream, handler),
    )
    .await??;
    clock.restart();
    let auth::Authenticated {
        warning,
        chosen_key,
    } = bounded(
        &clock,
        limit,
        || {
            format!(
                "authentication with {} port {} did not finish",
                ssh.host, ssh.port
            )
        },
        auth::authenticate(&mut handle, profile, ssh, &ctx),
    )
    .await??;
    give_up.disarm();
    Ok(Hop {
        handle,
        lost,
        ended,
        warning,
        chosen_key,
    })
}

/// Open a TCP connection to the profile's host, bounded by [`HOP_TIMEOUT`].
pub(crate) async fn tcp_connect(ssh: &SshProfile) -> Result<tokio::net::TcpStream, SshError> {
    let stream = tokio::time::timeout(
        HOP_TIMEOUT,
        tokio::net::TcpStream::connect((ssh.host.as_str(), ssh.port)),
    )
    .await
    .map_err(|_| timed_out(format!("no answer from {} port {}", ssh.host, ssh.port)))?
    .map_err(SshError::Connect)?;
    if let Err(e) = stream.set_nodelay(true) {
        tracing::debug!(error = %e, "set_nodelay failed");
    }
    Ok(stream)
}

/// Connect straight to the profile's host over TCP (no jump chain, no pooling).
///
/// Profiles with a jump chain are refused rather than silently connected directly; open
/// shells through [`SshPool`] instead.
pub async fn connect_direct(
    profile: &Profile,
    ctx: &SshContext,
) -> Result<Handle<ClientHandler>, SshError> {
    let ssh = ssh_profile(profile)?;
    if !ssh.jump.is_empty() {
        return Err(SshError::Protocol(format!(
            "profile {:?} has a jump chain; it cannot be connected directly",
            profile.name
        )));
    }
    let stream = tcp_connect(ssh).await.map_err(|e| in_hop(profile, e))?;
    // No registry: nothing here requests remote forwards, so every forwarded channel is refused.
    let hop = connect_hop(stream, profile, ctx, None)
        .await
        .map_err(|e| in_hop(profile, e))?;
    Ok(hop.handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rsa_private_keys() {
        // Throwaway keys made for these tests; they protect nothing.
        assert!(private_key_is_rsa(include_str!("testdata/rsa_openssh")));
        // Encrypted OpenSSH keys keep their public half readable.
        let enc = include_str!("testdata/rsa_openssh_enc");
        assert!(enc.contains("BEGIN OPENSSH PRIVATE KEY"));
        assert!(private_key_is_rsa(enc));
        // Legacy PEM: the header alone says RSA.
        assert!(private_key_is_rsa(include_str!("testdata/rsa_legacy_pem")));
        // Unencrypted PKCS#8 is read through the generic decoder.
        assert!(private_key_is_rsa(include_str!("testdata/rsa_pkcs8")));
        assert!(!private_key_is_rsa(include_str!(
            "testdata/ed25519_openssh"
        )));
        assert!(!private_key_is_rsa("not a key at all"));
        assert!(!private_key_is_rsa(""));
    }

    #[test]
    fn secret_answer_debug_redacts_value() {
        let a = SecretAnswer {
            value: "hunter2".into(),
            remember: true,
        };
        let s = format!("{a:?}");
        assert!(!s.contains("hunter2"), "{s}");
        assert!(s.contains("redacted"));
    }

    #[test]
    fn known_key_types_are_preferred() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        std::fs::write(
            &path,
            format!("[h]:2200 {}\n", known_hosts::tests_support::ECDSA),
        )
        .unwrap();
        let cfg = client_config("h", 2200, &path);
        assert_eq!(
            cfg.preferred.key[0],
            russh::keys::Algorithm::Ecdsa {
                curve: russh::keys::EcdsaCurve::NistP256
            }
        );
        assert_eq!(cfg.keepalive_interval, Some(Duration::from_secs(15)));
        assert_eq!(cfg.keepalive_max, 2);
        // Other hosts keep the default order.
        let other = client_config("other", 22, &path);
        assert_eq!(other.preferred.key[0], russh::keys::Algorithm::Ed25519);
    }

    /// A prompter that takes `delay` to answer and trusts everything.
    struct Slow(Duration);

    #[async_trait]
    impl Prompter for Slow {
        async fn ask_key_file(&self, _: &str) -> Option<PathBuf> {
            tokio::time::sleep(self.0).await;
            None
        }

        async fn confirm_host_key(&self, _: &str, _: u16, _: &str) -> bool {
            tokio::time::sleep(self.0).await;
            true
        }
        async fn ask_secret(&self, _: &str, _: SecretKind, _: bool) -> Option<SecretAnswer> {
            tokio::time::sleep(self.0).await;
            None
        }
    }

    fn is_timeout(e: &SshError) -> bool {
        matches!(e, SshError::Connect(io) if io.kind() == std::io::ErrorKind::TimedOut)
    }

    #[tokio::test]
    async fn bounded_times_out_a_stuck_step() {
        let clock = PromptClock::new();
        let started = Instant::now();
        let err = bounded(
            &clock,
            Duration::from_millis(100),
            || "stuck".into(),
            async { std::future::pending::<()>().await },
        )
        .await
        .unwrap_err();
        assert!(is_timeout(&err), "{err:?}");
        assert!(err.to_string().contains("stuck"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn bounded_does_not_count_prompt_time() {
        // 400 ms of budget; the step works 50 ms, waits 800 ms on the user, works 30 ms more.
        // Only 80 ms of work is counted, so 320 ms of margin remains for timer granularity
        // (Windows sleeps can overshoot by ~15 ms each). The 800 ms prompt alone would exceed
        // the budget if it were counted, so a pass proves prompt time is excluded.
        let clock = Arc::new(PromptClock::new());
        let prompter = ClockedPrompter {
            inner: Arc::new(Slow(Duration::from_millis(800))),
            clock: clock.clone(),
        };
        let out = bounded(&clock, Duration::from_millis(400), || "x".into(), async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let trusted = prompter.confirm_host_key("h", 22, "SHA256:x").await;
            tokio::time::sleep(Duration::from_millis(30)).await;
            trusted
        })
        .await
        .unwrap();
        assert!(out);
        // After the prompt the remaining budget still applies.
        let clock = Arc::new(PromptClock::new());
        let prompter = ClockedPrompter {
            inner: Arc::new(Slow(Duration::from_millis(150))),
            clock: clock.clone(),
        };
        let err = bounded(&clock, Duration::from_millis(100), || "x".into(), async {
            prompter.ask_secret("p", SecretKind::Password, false).await;
            std::future::pending::<()>().await
        })
        .await
        .unwrap_err();
        assert!(is_timeout(&err), "{err:?}");
    }

    #[tokio::test]
    async fn silent_server_times_out_the_handshake() {
        // Accepts the TCP connection but never says anything.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hold = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(socket);
        });
        let dir = tempfile::tempdir().unwrap();
        let ctx = SshContext {
            known_hosts: dir.path().join("known_hosts"),
            secrets: Arc::new(fbterm_secrets::MemoryStore::new()),
            prompter: Arc::new(Slow(Duration::ZERO)),
        };
        let profile = Profile {
            id: fbterm_config::ProfileId::new(),
            name: "silent".into(),
            group: None,
            accent: None,
            accent_style: Default::default(),
            color_scheme: None,
            kind: ProfileKind::Ssh(SshProfile {
                host: "127.0.0.1".into(),
                port: addr.port(),
                user: "u".into(),
                auth: fbterm_config::AuthMethod::Password,
                jump: vec![],
                tunnels: Vec::new(),
            }),
        };
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let started = Instant::now();
        let err = connect_hop_within(stream, &profile, &ctx, Duration::from_millis(200), None)
            .await
            .err()
            .unwrap();
        assert!(is_timeout(&err), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        hold.abort();
    }

    #[test]
    fn hop_timeout_is_fifteen_seconds() {
        assert_eq!(HOP_TIMEOUT, Duration::from_secs(15));
    }

    #[test]
    fn duplicate_keeps_kind_and_message() {
        let e = SshError::Connect(std::io::Error::new(std::io::ErrorKind::TimedOut, "slow"));
        match e.duplicate() {
            SshError::Connect(io) => {
                assert_eq!(io.kind(), std::io::ErrorKind::TimedOut);
                assert_eq!(io.to_string(), "slow");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            SshError::AuthFailed {
                profile: "p".into()
            }
            .duplicate()
            .to_string(),
            "authentication failed for \"p\""
        );
    }

    #[test]
    fn hop_errors_name_the_hop() {
        let mut p = Profile {
            id: fbterm_config::ProfileId::new(),
            name: "bastion-1".into(),
            group: None,
            accent: None,
            accent_style: Default::default(),
            color_scheme: None,
            kind: ProfileKind::Ssh(SshProfile {
                host: "10.0.0.5".into(),
                port: 22,
                user: "u".into(),
                auth: fbterm_config::AuthMethod::Agent,
                jump: vec![],
                tunnels: Vec::new(),
            }),
        };
        match in_hop(&p, timed_out("no answer".into())) {
            SshError::Connect(io) => {
                assert_eq!(io.kind(), std::io::ErrorKind::TimedOut);
                assert_eq!(io.to_string(), "bastion-1 (10.0.0.5:22): no answer");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            in_hop(&p, SshError::Protocol("bad packet".into())).to_string(),
            "ssh error: bastion-1 (10.0.0.5:22): bad packet"
        );
        // Errors that already say whose they are stay as they were.
        assert!(matches!(
            in_hop(&p, SshError::HostKeyRejected),
            SshError::HostKeyRejected
        ));
        if let ProfileKind::Ssh(ssh) = &mut p.kind {
            ssh.host = "fe80::1".into();
        }
        assert!(
            in_hop(&p, SshError::Protocol("x".into()))
                .to_string()
                .contains("bastion-1 ([fe80::1]:22): x")
        );
    }

    #[test]
    fn unsolicited_forwarded_channel_is_rejected() {
        let forwards: Forwards = Arc::default();
        let (tx, rx) = mpsc::unbounded_channel();
        forwards.lock().insert(("127.0.0.1".into(), 4022), tx);
        // The target routes its own forward, and only that one.
        assert!(forward_route(Some(&forwards), "127.0.0.1", 4022).is_some());
        assert!(forward_route(Some(&forwards), "127.0.0.1", 4023).is_none());
        assert!(forward_route(Some(&forwards), "0.0.0.0", 4022).is_none());
        assert!(forward_route(Some(&forwards), "localhost", 4022).is_none());
        // A jump hop has no registry and refuses everything.
        assert!(forward_route(None, "127.0.0.1", 4022).is_none());
        // A forward whose receiver is gone (stopped tunnel) refuses too.
        drop(rx);
        assert!(forward_route(Some(&forwards), "127.0.0.1", 4022).is_none());
        // So does one that was unregistered.
        let (tx, _rx) = mpsc::unbounded_channel();
        forwards.lock().insert(("127.0.0.1".into(), 4022), tx);
        forwards.lock().remove(&("127.0.0.1".to_owned(), 4022));
        assert!(forward_route(Some(&forwards), "127.0.0.1", 4022).is_none());
    }

    #[tokio::test]
    async fn no_prompt_asks_nothing_and_needs_input() {
        let p = NoPrompt;
        assert!(!p.can_ask());
        assert!(!p.confirm_host_key("h", 22, "SHA256:x").await);
        assert!(
            p.ask_secret("p", SecretKind::Password, false)
                .await
                .is_none()
        );
        // Clocked (as inside a connect) it still cannot ask.
        let clocked = ClockedPrompter {
            inner: Arc::new(NoPrompt),
            clock: Arc::new(PromptClock::new()),
        };
        assert!(!clocked.can_ask());
        assert!(Slow(Duration::ZERO).can_ask());

        // Unanswered questions become NeedsInput, but only without prompts.
        let needs = |e: SshError, can_ask| e.without_prompts(can_ask);
        assert!(matches!(
            needs(SshError::HostKeyRejected, false),
            SshError::NeedsInput
        ));
        assert!(matches!(
            needs(SshError::Cancelled, false),
            SshError::NeedsInput
        ));
        assert!(matches!(
            needs(SshError::HostKeyRejected, true),
            SshError::HostKeyRejected
        ));
        assert!(matches!(
            needs(SshError::Cancelled, true),
            SshError::Cancelled
        ));
        // A changed key or a wrong saved secret is no question: it stays what it is.
        assert!(matches!(
            needs(
                SshError::HostKeyChanged {
                    host: "h".into(),
                    port: 22
                },
                false
            ),
            SshError::HostKeyChanged { .. }
        ));
        assert!(matches!(
            needs(
                SshError::AuthFailed {
                    profile: "p".into()
                },
                false
            ),
            SshError::AuthFailed { .. }
        ));
        assert_eq!(SshError::NeedsInput.to_string(), "needs your input");
        assert!(matches!(
            SshError::NeedsInput.duplicate(),
            SshError::NeedsInput
        ));
    }

    #[tokio::test]
    async fn cancel_on_drop_raises_unless_disarmed() {
        let cancel = Arc::new(Cancel::default());
        CancelOnDrop(Some(cancel.clone())).disarm();
        assert!(!cancel.is_cancelled());

        let waiter = {
            let cancel = cancel.clone();
            tokio::spawn(async move { cancel.cancelled().await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished());
        drop(CancelOnDrop(Some(cancel.clone())));
        assert!(cancel.is_cancelled());
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("a raised cancel wakes its waiters")
            .unwrap();
    }
}
