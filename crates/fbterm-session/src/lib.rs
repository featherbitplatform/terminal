//! SSH connections and sessions.
//!
//! A session owns a shared terminal and reports what happens to it through
//! [`SessionEvent`]s. Local PTY sessions live in [`local`]; the SSH backend
//! reuses the same handle and event types.

mod local;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use fbterm_term::{GridSize, SharedTerminal};

pub use local::spawn_local;

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

/// Commands consumed by a session's writer side.
pub(crate) enum Command {
    Input(Vec<u8>),
    Resize(GridSize),
    Close,
}

/// Handle to a running session. Dropping it closes the session.
pub struct SessionHandle {
    pub id: SessionId,
    pub terminal: SharedTerminal,
    pub(crate) commands: std::sync::mpsc::Sender<Command>,
    pub(crate) dirty_pending: Arc<AtomicBool>,
    pub(crate) bell_pending: Arc<AtomicBool>,
    pub(crate) killer: Arc<parking_lot::Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>>,
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

    /// Kill the session and stop its threads. Safe to call repeatedly.
    ///
    /// The child is killed directly, so this works even if the writer is blocked.
    pub fn close(&self) {
        let _ = self.killer.lock().kill();
        let _ = self.commands.send(Command::Close);
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
