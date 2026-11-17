//! SSH connections and sessions.
//!
//! A session owns a shared terminal and reports what happens to it through
//! [`SessionEvent`]s. Local PTY sessions live in [`local`]; the SSH backend
//! reuses the same handle and event types.

mod local;
pub mod ssh;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use fbterm_term::{GridSize, SharedTerminal, TermEvent, Terminal};

pub use local::{spawn_local, spawn_local_into};

/// Identifies one session for the lifetime of the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub u64);

impl SessionId {
    /// Allocate a fresh, process-unique id.
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// The terminal changed; coalesced until [`SessionHandle::ack_dirty`].
    Dirty,
    Title(String),
    Bell,
    Exited(Option<i32>),
    Disconnected(String),
    /// A session task that was closed on purpose has let go of its terminal. Sent only after a
    /// close (a program ending by itself sends nothing), once the task's own reference is
    /// dropped, so the app can tell when closing the tab freed its terminal. The app also sends
    /// it itself (any id) when the history worker let go of a closed tab's terminal last.
    Released,
}

/// Something about how a connection was authenticated that the user should hear about. It
/// never fails the connection; every session on that connection reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthWarning {
    /// Login used an RSA private key (the `rsa` crate's signing has a known timing side channel).
    RsaUserKey,
}

/// How a session ended, as last reported by its `Exited` or `Disconnected` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEnd {
    Exited(Option<i32>),
    Disconnected(String),
}

impl From<SessionEnd> for SessionEvent {
    fn from(end: SessionEnd) -> Self {
        match end {
            SessionEnd::Exited(code) => Self::Exited(code),
            SessionEnd::Disconnected(reason) => Self::Disconnected(reason),
        }
    }
}

/// Shared between a session's handle and its feeders: set when the session reports its end.
pub(crate) type Ended = Arc<parking_lot::Mutex<Option<SessionEnd>>>;

pub type EventSink = tokio::sync::mpsc::UnboundedSender<(SessionId, SessionEvent)>;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("failed to open pty: {0}")]
    Pty(String),
    #[error("failed to spawn `{program}`: {reason}")]
    Spawn { program: String, reason: String },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Commands consumed by a session's writer side, in order: input bytes and the terminal's
/// own replies share one queue so they reach the program in the order they were produced.
#[derive(Debug)]
pub(crate) enum Command {
    Input(Vec<u8>),
    Resize(GridSize),
    Close,
}

/// What a [`SessionHandle::recording`] handle was sent.
#[doc(hidden)]
pub struct InputLog(tokio::sync::mpsc::UnboundedReceiver<Command>);

impl InputLog {
    /// The input sent since the last call, one item per send.
    pub fn inputs(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Ok(command) = self.0.try_recv() {
            if let Command::Input(bytes) = command {
                out.push(bytes);
            }
        }
        out
    }
}

/// Stops a session immediately, even if its writer is blocked.
pub(crate) type Closer = Box<dyn Fn() + Send + Sync>;

/// Handle to a running session. Dropping it closes the session.
pub struct SessionHandle {
    pub id: SessionId,
    pub terminal: SharedTerminal,
    pub(crate) commands: tokio::sync::mpsc::UnboundedSender<Command>,
    pub(crate) dirty_pending: Arc<AtomicBool>,
    pub(crate) bell_pending: Arc<AtomicBool>,
    /// Set once the handle is closed: from then on the session's output is dropped instead of
    /// fed, so a lingering reader (a grandchild still holding the PTY) can never write into a
    /// terminal that a new session has taken over.
    pub(crate) detached: Arc<AtomicBool>,
    pub(crate) closer: Closer,
    pub(crate) auth_warnings: Vec<AuthWarning>,
    pub(crate) ended: Ended,
    /// SSH sessions keep their shared connection alive until closed.
    pub(crate) connection: parking_lot::Mutex<Option<Arc<ssh::pool::Connection>>>,
}

impl SessionHandle {
    /// A handle with no session behind it, writing nowhere: stands in for a connected session
    /// where code that keeps handles is tested without a program or a network.
    #[doc(hidden)]
    pub fn inert(terminal: SharedTerminal) -> Self {
        Self {
            id: SessionId::next(),
            terminal,
            commands: tokio::sync::mpsc::unbounded_channel().0,
            dirty_pending: Arc::new(AtomicBool::new(false)),
            bell_pending: Arc::new(AtomicBool::new(false)),
            detached: Arc::new(AtomicBool::new(false)),
            closer: Box::new(|| {}),
            auth_warnings: Vec::new(),
            ended: Ended::default(),
            connection: parking_lot::Mutex::new(None),
        }
    }

    /// [`SessionHandle::inert`] that keeps what is sent to it: what the user typed can be
    /// read back from the [`InputLog`].
    #[doc(hidden)]
    pub fn recording(terminal: SharedTerminal) -> (Self, InputLog) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut handle = Self::inert(terminal);
        handle.commands = tx;
        (handle, InputLog(rx))
    }

    /// Warnings from authenticating this session's connection; empty for local sessions.
    /// Read it once when the handle is installed (once per tab), not from events.
    pub fn auth_warnings(&self) -> &[AuthWarning] {
        &self.auth_warnings
    }

    /// Write bytes to the session's input.
    pub fn send(&self, bytes: Vec<u8>) {
        let _ = self.commands.send(Command::Input(bytes));
    }

    /// Resize the remote side (and the terminal, if it is not already that size).
    pub fn resize(&self, size: GridSize) {
        {
            let mut term = self.terminal.lock();
            if term.size() != size {
                term.resize(size);
            }
        }
        let _ = self.commands.send(Command::Resize(size));
    }

    /// Kill the session and stop its threads or tasks. Safe to call repeatedly.
    ///
    /// Output that arrives after this is discarded, never fed into the terminal, so the
    /// terminal can be handed to a new session right away.
    ///
    /// The child (or channel) is stopped directly, so this works even if the writer is
    /// blocked. An SSH session also lets go of its connection, which closes once its last
    /// session has.
    pub fn close(&self) {
        self.detached.store(true, Ordering::Release);
        (self.closer)();
        let _ = self.commands.send(Command::Close);
        self.connection.lock().take();
    }

    /// Re-arm redraw notifications; returns whether a `Dirty` was pending.
    ///
    /// Call this immediately BEFORE locking the terminal to draw, so output
    /// fed after the draw starts raises a fresh `Dirty`. Also re-arms `Bell`.
    pub fn take_dirty(&self) -> bool {
        self.bell_pending.store(false, Ordering::Release);
        self.dirty_pending.swap(false, Ordering::AcqRel)
    }

    /// How the session ended, once it has reported `Exited` or `Disconnected`. A handle that
    /// is installed after its session started running (an SSH shell is handed over after its
    /// reader is already going) uses this to catch an end whose event arrived too early.
    pub fn ended(&self) -> Option<SessionEnd> {
        self.ended.lock().clone()
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        self.close();
    }
}

/// What reaches a session from outside its reader thread.
///
/// [`Feeder::feed`] and the image store's worker (through the owner hooks) route terminal
/// events with this, so a kitty reply produced by a finished decode looks exactly like one
/// produced by a feed: same replies queue in the same order, same title dedupe, same coalesced
/// `Dirty`/`Bell`. It holds the terminal *weakly*: the store lives for the whole process, and
/// hooks holding it strongly would keep every closed pane's grid alive.
struct Router {
    id: SessionId,
    events: EventSink,
    /// Where replies to programs go: the session's own input queue, so they interleave with
    /// keystrokes in the order they were produced.
    replies: tokio::sync::mpsc::UnboundedSender<Command>,
    dirty_pending: Arc<AtomicBool>,
    bell_pending: Arc<AtomicBool>,
    ended: Ended,
    terminal: std::sync::Weak<parking_lot::Mutex<Terminal>>,
    /// The terminal's id in the image store ([`Terminal::image_owner`]).
    owner: u64,
    last_title: parking_lot::Mutex<Option<String>>,
}

impl Router {
    /// Pass the events one feed (or one decode result) produced on to their destinations.
    fn route(&self, term_events: Vec<TermEvent>) {
        for ev in term_events {
            match ev {
                TermEvent::PtyWrite(bytes) => {
                    let _ = self.replies.send(Command::Input(bytes));
                }
                TermEvent::Title(t) => {
                    let mut last = self.last_title.lock();
                    if last.as_deref() != Some(t.as_str()) {
                        *last = Some(t.clone());
                        drop(last);
                        self.send(SessionEvent::Title(t));
                    }
                }
                TermEvent::Bell => {
                    if !self.bell_pending.swap(true, Ordering::AcqRel) {
                        self.send(SessionEvent::Bell);
                    }
                }
                TermEvent::Exit => {}
                TermEvent::ImageReceived { key, data } => self.submit_image(key, data),
                // Idempotent by design: scrollback eviction and failed decodes release keys the
                // terminal releases again later, and a second release must stay harmless.
                TermEvent::ImageReleased(key) => {
                    fbterm_term::images::store().release(self.owner, key)
                }
            }
        }
    }

    /// Hand one image to the store, and if it pushed this owner over its cap, reclaim — the
    /// terminal's scrollback first (outside the store's lock, as the store never locks a
    /// terminal), the store's own buffers only when that ran dry.
    fn submit_image(
        &self,
        key: fbterm_term::images::ImageKey,
        data: fbterm_term::images::ImageData,
    ) {
        let store = fbterm_term::images::store();
        store.submit(self.owner, key, data);
        if !store.over_cap(self.owner) {
            return;
        }
        // Each evicted placement becomes a placeholder line and releases its image outright; the
        // terminal raises `ImageReleased` for it too, which the idempotent release absorbs.
        while store.over_cap(self.owner) {
            let Some(terminal) = self.terminal.upgrade() else {
                break; // the pane is gone: its whole budget goes with it
            };
            let Some(evicted) = terminal.lock().evict_oldest_scrollback_image() else {
                break; // scrollback holds no image left to reclaim
            };
            drop(terminal);
            store.release(self.owner, evicted);
        }
        store.enforce_caps(self.owner);
    }

    /// Raise `Dirty` unless one is already pending.
    fn mark_dirty(&self) {
        if !self.dirty_pending.swap(true, Ordering::AcqRel) {
            self.send(SessionEvent::Dirty);
        }
    }

    /// Report an event. The end of the session is recorded first, so a handle installed after
    /// the event was delivered still learns of it through [`SessionHandle::ended`].
    fn send(&self, event: SessionEvent) {
        let end = match &event {
            SessionEvent::Exited(code) => Some(SessionEnd::Exited(*code)),
            SessionEvent::Disconnected(reason) => Some(SessionEnd::Disconnected(reason.clone())),
            _ => None,
        };
        if let Some(end) = end {
            *self.ended.lock() = Some(end);
        }
        let _ = self.events.send((self.id, event));
    }
}

/// Feeds program output into a session's terminal and raises the coalesced events, the same
/// way for every kind of session.
pub(crate) struct Feeder {
    pub terminal: SharedTerminal,
    pub detached: Arc<AtomicBool>,
    /// Everything feeding and answering goes through here; the image store's worker holds a
    /// clone of it (as this terminal's owner hooks) until the terminal drops.
    router: Arc<Router>,
    /// Declared last so it drops after `terminal`: its notice then follows the release of
    /// this feeder's terminal reference.
    _released: ReleasedNotice,
}

/// Tells the app that a closed session's task dropped its terminal reference.
struct ReleasedNotice {
    id: SessionId,
    events: EventSink,
    detached: Arc<AtomicBool>,
}

impl Drop for ReleasedNotice {
    fn drop(&mut self) {
        // Only for a session the user closed (`SessionHandle::close` sets `detached`): one that
        // ended by itself, from a program or the network, reports nothing here.
        if self.detached.load(Ordering::Acquire) {
            let _ = self.events.send((self.id, SessionEvent::Released));
        }
    }
}

impl Feeder {
    /// Build a feeder for one terminal, and register its owner hooks with the image store: an
    /// image that finishes decoding later is answered and redrawn through this session. A new
    /// feeder on the same terminal (a reconnect) replaces them, so the old session stops getting
    /// its wakeups while the images themselves survive.
    #[expect(clippy::too_many_arguments)]
    pub fn new(
        id: SessionId,
        terminal: SharedTerminal,
        events: EventSink,
        dirty_pending: Arc<AtomicBool>,
        bell_pending: Arc<AtomicBool>,
        detached: Arc<AtomicBool>,
        ended: Ended,
        replies: tokio::sync::mpsc::UnboundedSender<Command>,
    ) -> Self {
        let owner = terminal.lock().image_owner();
        let router = Arc::new(Router {
            id,
            events: events.clone(),
            replies,
            dirty_pending: dirty_pending.clone(),
            bell_pending: bell_pending.clone(),
            ended: ended.clone(),
            terminal: Arc::downgrade(&terminal),
            owner,
            last_title: parking_lot::Mutex::new(None),
        });
        fbterm_term::images::store().register_owner(
            owner,
            fbterm_term::images::OwnerHooks {
                term: Arc::downgrade(&terminal),
                on_events: {
                    let router = router.clone();
                    Arc::new(move |events| router.route(events))
                },
                wake: {
                    let router = router.clone();
                    Arc::new(move || router.mark_dirty())
                },
            },
        );
        Self {
            _released: ReleasedNotice {
                id,
                events: events.clone(),
                detached: detached.clone(),
            },
            terminal,
            detached,
            router,
        }
    }

    /// Feed output, answering programs through the session's replies queue as the emulator asks.
    ///
    /// `Dirty` and `Bell` are raised at most once until [`SessionHandle::take_dirty`] re-arms
    /// them; a title is reported only when it changes. Once the handle is closed the bytes are
    /// dropped (checked under the terminal lock, so nothing lands after `close` returns). The
    /// graphics protocols' stall watchdog runs on the same lock: this app schedules no timers,
    /// so feeding output is what keeps chunked transmissions and parked replies moving.
    pub fn feed(&mut self, bytes: &[u8]) {
        let (term_events, history_jobs) = {
            let mut term = self.terminal.lock();
            if self.is_detached() {
                return;
            }
            term.feed(bytes);
            term.tick(std::time::Instant::now());
            (term.take_events(), term.has_history_jobs())
        };
        if history_jobs {
            // After the lock is released, so the worker can take the jobs at once.
            fbterm_term::history_worker::enqueue(&self.terminal);
        }
        self.router.route(term_events);
        self.mark_dirty();
    }

    /// Whether the handle was closed and output must be dropped.
    pub fn is_detached(&self) -> bool {
        self.detached.load(Ordering::Acquire)
    }

    /// Raise `Dirty` unless one is already pending.
    pub fn mark_dirty(&self) {
        self.router.mark_dirty();
    }

    /// Report an event. The end of the session is recorded first, so a handle installed after
    /// the event was delivered still learns of it through [`SessionHandle::ended`].
    pub fn send(&self, event: SessionEvent) {
        self.router.send(event);
    }
}

#[cfg(test)]
mod released_tests {
    use super::*;
    use fbterm_term::{GridSize, Terminal};

    fn feeder(
        detached: bool,
    ) -> (
        Feeder,
        std::sync::Weak<parking_lot::Mutex<Terminal>>,
        tokio::sync::mpsc::UnboundedReceiver<(SessionId, SessionEvent)>,
    ) {
        let size = GridSize {
            cols: 10,
            rows: 3,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let terminal = Arc::new(parking_lot::Mutex::new(Terminal::new(size, 10)));
        let weak = Arc::downgrade(&terminal);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (replies, _replies_rx) = tokio::sync::mpsc::unbounded_channel();
        let f = Feeder::new(
            SessionId::next(),
            terminal,
            tx,
            Arc::default(),
            Arc::default(),
            Arc::new(AtomicBool::new(detached)),
            Ended::default(),
            replies,
        );
        (f, weak, rx)
    }

    #[test]
    fn a_closed_sessions_feeder_reports_after_it_dropped_the_terminal() {
        let (f, weak, mut rx) = feeder(true);
        assert!(weak.upgrade().is_some());
        drop(f);
        assert_eq!(
            weak.strong_count(),
            0,
            "terminal gone before the notice is read"
        );
        assert_eq!(rx.try_recv().unwrap().1, SessionEvent::Released);
    }

    #[test]
    fn a_session_that_ends_by_itself_reports_no_release() {
        let (f, _weak, mut rx) = feeder(false);
        f.send(SessionEvent::Exited(Some(0)));
        drop(f);
        assert_eq!(rx.try_recv().unwrap().1, SessionEvent::Exited(Some(0)));
        assert!(rx.try_recv().is_err());
    }
}

#[cfg(test)]
mod feeder_image_tests {
    use super::*;
    use fbterm_term::images::store;

    /// Minimal standard-alphabet base64 for building kitty frames.
    fn b64(bytes: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = (u32::from(chunk[0]) << 16)
                | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
                | u32::from(*chunk.get(2).unwrap_or(&0));
            let t = |i: u32| T[i as usize] as char;
            out.push(t(n >> 18));
            out.push(t((n >> 12) & 63));
            out.push(if chunk.len() > 1 {
                t((n >> 6) & 63)
            } else {
                '='
            });
            out.push(if chunk.len() > 2 { t(n & 63) } else { '=' });
        }
        out
    }

    /// A raw-RGBA kitty transmission (no container to decode: the terminal hands pixels over and
    /// answers the client itself), as one fed frame.
    fn rgba_frame(id: u32, pixels: &[u8]) -> String {
        format!("\x1b_Gf=32,s=2,v=1,i={id},a=t;{}\x1b\\", b64(pixels),)
    }

    /// A picture a program transmits reaches the app-wide store through the feeder, keyed by the
    /// terminal's owner id; when the emulator releases it — here, by switching pictures off —
    /// the store forgets it. Replies to programs go back through the session's input queue.
    #[test]
    fn a_fed_picture_lands_in_the_store_and_its_release_frees_it() {
        let size = GridSize {
            cols: 10,
            rows: 3,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let terminal = Arc::new(parking_lot::Mutex::new(Terminal::new(size, 10)));
        let owner = terminal.lock().image_owner();
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (replies_tx, mut replies_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
        let mut feeder = Feeder::new(
            SessionId::next(),
            terminal.clone(),
            events_tx,
            Arc::default(),
            Arc::default(),
            Arc::new(AtomicBool::new(false)),
            Ended::default(),
            replies_tx,
        );

        // 2x1 RGBA: eight bytes straight into the store, no decode job.
        feeder.feed(rgba_frame(7, &[255, 0, 0, 255, 0, 255, 0, 255]).as_bytes());
        assert_eq!(store().owner_bytes(owner), 8);
        // The emulator answered the transmission itself; the feeder passed that answer on.
        let Command::Input(reply) = replies_rx
            .try_recv()
            .expect("the client's answer should be routed")
        else {
            panic!("expected a reply to write, got a close");
        };
        // `\x1b_Gi=7;OK\x1b\\`: the emulator answers raw-format transmissions itself.
        assert!(
            reply.windows(4).any(|w| w == b"Gi=7") && reply.windows(2).any(|w| w == b"OK"),
            "expected a kitty answer naming image 7, got {reply:?}",
        );
        assert!(matches!(
            events_rx.try_recv().ok().map(|e| e.1),
            Some(SessionEvent::Dirty)
        ));

        // Switching pictures off releases what the terminal holds; routed like any other event.
        terminal.lock().set_images(false, false);
        let released = terminal.lock().take_events();
        feeder.router.route(released);
        assert_eq!(
            store().owner_bytes(owner),
            0,
            "the release should have freed it"
        );

        // A second transmission while pictures are off: nothing new is stored.
        feeder.feed(rgba_frame(8, &[9, 9, 9, 9, 9, 9, 9, 9]).as_bytes());
        assert_eq!(store().owner_bytes(owner), 0);
    }

    /// A reconnect builds a new feeder on the same terminal; the store then answers through the
    /// new session's hooks, and dropping the terminal frees its whole share.
    #[test]
    fn a_reconnect_moves_the_store_answers_to_the_new_feeder() {
        let size = GridSize {
            cols: 10,
            rows: 3,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let terminal = Arc::new(parking_lot::Mutex::new(Terminal::new(size, 10)));
        let owner = terminal.lock().image_owner();
        let (events_tx, _events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (old_replies, mut old_replies_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
        let old = Feeder::new(
            SessionId::next(),
            terminal.clone(),
            events_tx.clone(),
            Arc::default(),
            Arc::default(),
            Arc::new(AtomicBool::new(false)),
            Ended::default(),
            old_replies,
        );
        let (new_replies, mut new_replies_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
        let mut fresh = Feeder::new(
            SessionId::next(),
            terminal.clone(),
            events_tx,
            Arc::default(),
            Arc::default(),
            Arc::new(AtomicBool::new(false)),
            Ended::default(),
            new_replies,
        );

        // A PNG the emulator defers to us: the decode result — and with it the client's answer —
        // must come back through the *newest* feeder's replies queue. The picture is the term
        // crate's own checked-in fixture, read from disk so this crate tests against real bytes.
        let tiny = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../fbterm-term/tests/fixtures/images/tiny.png"
        ))
        .expect("the term crate's tiny PNG fixture");
        let frame = format!("\x1b_Gi=42,a=t,f=100;{}\x1b\\", b64(&tiny));
        fresh.feed(frame.as_bytes());
        assert!(
            store().owner_bytes(owner) > 0,
            "the payload should be queued"
        );
        // Wait for the app-wide worker to finish it (it may or may not be the one that took the
        // job; either way the answer reaches whichever session registered last).
        assert!(store().wait_idle_for(std::time::Duration::from_secs(30)));
        let answered = matches!(new_replies_rx.try_recv().ok(), Some(Command::Input(_)));
        let stale = old_replies_rx.try_recv().ok();
        assert!(answered, "the new feeder should answer the client");
        assert!(
            stale.is_none(),
            "the old feeder got an answer it no longer owns: {stale:?}"
        );
        // The 4x4 fixture decoded to 64 bytes of RGBA.
        assert_eq!(
            store().owner_bytes(owner),
            4 * 4 * 4,
            "the decoded pixels are held"
        );

        // Closing the terminal hands its whole budget back to the store — but only once nothing
        // else holds it: the feeders' references go first.
        drop(old);
        drop(fresh);
        drop(terminal);
        assert_eq!(store().owner_bytes(owner), 0);
    }
}
