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
    /// The program asked to store text on the clipboard (OSC 52).
    ClipboardStore(String),
    Exited(Option<i32>),
    Disconnected(String),
}

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
    /// SSH sessions keep their shared connection alive until closed.
    pub(crate) connection: parking_lot::Mutex<Option<Arc<ssh::pool::Connection>>>,
}

impl SessionHandle {
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
    last_title: Option<String>,
}

impl Feeder {
    pub fn new(
        id: SessionId,
        terminal: SharedTerminal,
        events: EventSink,
        dirty_pending: Arc<AtomicBool>,
        bell_pending: Arc<AtomicBool>,
        detached: Arc<AtomicBool>,
    ) -> Self {
        Self {
            id,
            terminal,
            events,
            dirty_pending,
            bell_pending,
            detached,
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
                TermEvent::ClipboardStore(s) => self.send(SessionEvent::ClipboardStore(s)),
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

    pub fn send(&self, event: SessionEvent) {
        let _ = self.events.send((self.id, event));
    }
}
