//! SSH connections and sessions.
//!
//! A session owns a shared terminal and reports what happens to it through
//! [`SessionEvent`]s. Local PTY sessions live in [`local`]; the SSH backend
//! reuses the same handle and event types.

mod local;
pub mod ssh;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use fbterm_term::{GridSize, SharedTerminal, TermEvent};

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
    /// dropped, so the app can tell when closing the tab freed its terminal.
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
pub(crate) enum Command {
    Input(Vec<u8>),
    Resize(GridSize),
    Close,
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

/// Feeds program output into a session's terminal and raises the coalesced events, the same
/// way for every kind of session.
pub(crate) struct Feeder {
    pub id: SessionId,
    pub terminal: SharedTerminal,
    pub events: EventSink,
    pub dirty_pending: Arc<AtomicBool>,
    pub bell_pending: Arc<AtomicBool>,
    pub detached: Arc<AtomicBool>,
    pub ended: Ended,
    last_title: Option<String>,
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
    pub fn new(
        id: SessionId,
        terminal: SharedTerminal,
        events: EventSink,
        dirty_pending: Arc<AtomicBool>,
        bell_pending: Arc<AtomicBool>,
        detached: Arc<AtomicBool>,
        ended: Ended,
    ) -> Self {
        Self {
            id,
            terminal,
            _released: ReleasedNotice {
                id,
                events: events.clone(),
                detached: detached.clone(),
            },
            events,
            dirty_pending,
            bell_pending,
            detached,
            ended,
            last_title: None,
        }
    }

    /// Feed output; returns the replies the terminal wants written back to the program.
    ///
    /// `Dirty` and `Bell` are raised at most once until [`SessionHandle::take_dirty`] re-arms
    /// them; a title is reported only when it changes. Once the handle is closed the bytes are
    /// dropped (checked under the terminal lock, so nothing lands after `close` returns).
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let term_events = {
            let mut term = self.terminal.lock();
            if self.is_detached() {
                return Vec::new();
            }
            term.feed(bytes);
            term.take_events()
        };
        let mut replies = Vec::new();
        for ev in term_events {
            match ev {
                TermEvent::PtyWrite(bytes) => replies.push(bytes),
                TermEvent::Title(t) => {
                    if self.last_title.as_deref() != Some(t.as_str()) {
                        self.last_title = Some(t.clone());
                        self.send(SessionEvent::Title(t));
                    }
                }
                TermEvent::Bell => {
                    if !self.bell_pending.swap(true, Ordering::AcqRel) {
                        self.send(SessionEvent::Bell);
                    }
                }
                TermEvent::Exit => {}
            }
        }
        self.mark_dirty();
        replies
    }

    /// Whether the handle was closed and output must be dropped.
    pub fn is_detached(&self) -> bool {
        self.detached.load(Ordering::Acquire)
    }

    /// Raise `Dirty` unless one is already pending.
    pub fn mark_dirty(&self) {
        if !self.dirty_pending.swap(true, Ordering::AcqRel) {
            self.send(SessionEvent::Dirty);
        }
    }

    /// Report an event. The end of the session is recorded first, so a handle installed after
    /// the event was delivered still learns of it through [`SessionHandle::ended`].
    pub fn send(&self, event: SessionEvent) {
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
        let f = Feeder::new(
            SessionId::next(),
            terminal,
            tx,
            Arc::default(),
            Arc::default(),
            Arc::new(AtomicBool::new(detached)),
            Ended::default(),
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
