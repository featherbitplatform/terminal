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
    /// This session's event router, for [`SessionHandle::deliver_events`]. `None` on a handle with
    /// no session behind it ([`SessionHandle::inert`]), where nothing runs to route anything.
    pub(crate) router: Option<Arc<Router>>,
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
            router: None,
        }
    }

    /// [`SessionHandle::inert`] that keeps what is sent to it: what the user typed can be
    /// read back from the [`InputLog`].
    ///
    /// It routes as well, so a test can follow an event from the emulator to the program's input:
    /// [`SessionHandle::deliver_events`] on this handle answers out of the same queue keystrokes go
    /// into. Its half of the session-event channel is dropped - titles and redraw notices reach
    /// nobody here, only what is written to the program is kept.
    #[doc(hidden)]
    pub fn recording(terminal: SharedTerminal) -> (Self, InputLog) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (events, _unread) = tokio::sync::mpsc::unbounded_channel();
        let owner = terminal.lock().image_owner();
        let mut handle = Self::inert(terminal.clone());
        handle.commands = tx.clone();
        handle.router = Some(Arc::new(Router {
            id: handle.id,
            events,
            replies: tx,
            dirty_pending: handle.dirty_pending.clone(),
            bell_pending: handle.bell_pending.clone(),
            ended: handle.ended.clone(),
            terminal: Arc::downgrade(&terminal),
            owner,
            last_title: parking_lot::Mutex::new(None),
            routing: parking_lot::Mutex::new(()),
        }));
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

    /// Deliver what this session's terminal queued with nobody to route it, as a feed would: its
    /// replies to the program, its title, its pictures, and one `Dirty`.
    ///
    /// A pane started before its window laid out holds its output until the view reports the grid it
    /// really has (see [`fbterm_term::Terminal::hold_until_sized`]); that resize happens on the app's
    /// thread, where no feed follows it. Call this after such a resize, and never while holding the
    /// terminal's lock: delivery hands pictures to the image store, which locks the terminal itself.
    pub fn deliver_events(&self) {
        if let Some(router) = &self.router {
            router.deliver_queued();
        }
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
    /// One terminal's events are taken and routed together, by one thread at a time.
    ///
    /// Replies must reach a program in the order the emulator produced them, and a picture must
    /// reach the store before the event that releases its key. Three threads move events for one
    /// terminal: the feeder ([`Feeder::feed`]), whoever delivers what a pane's first real grid or a
    /// hidden pane's release parsed ([`Router::deliver_queued`], which is also what the image
    /// store's `deliver` hook calls), and - only through that hook - the image worker, whose
    /// finished decode queues its answer into the terminal rather than carrying it off. Each takes
    /// this lock, then the terminal lock, empties the queue and routes everything it took; nothing
    /// else in the app drains that queue, so no reply is overtaken on the way to the program's input.
    ///
    /// What that guarantees, precisely: whatever was queued before a thread takes the queue goes out
    /// in production order from that thread, and anything produced while it routes goes out after,
    /// from the next taker. It does not order events two threads produce at the same moment - the
    /// emulator's own lock decides which of those was produced first, and this section keeps that
    /// order. The order of locks is routing, then terminal (released before routing), then store:
    /// nothing takes this lock while holding the terminal or the store, the store runs its hooks with
    /// nothing held, and `Dirty` - the redraw notice - is raised after the guard is dropped, never
    /// under a lock.
    routing: parking_lot::Mutex<()>,
}

impl Router {
    /// Pass the events one feed (or one delivery) produced on to their destinations.
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

    /// Hand one image to the store, then reclaim if it pushed this owner over its cap — the
    /// terminal's scrollback first (outside the store's lock, as the store never locks a
    /// terminal), the store's own buffers only when that ran dry. The image worker runs the same
    /// helper when a decode or refusal finishes, so the order holds however the bytes grew.
    fn submit_image(
        &self,
        key: fbterm_term::images::ImageKey,
        data: fbterm_term::images::ImageData,
    ) {
        let store = fbterm_term::images::store();
        store.submit(self.owner, key, data);
        fbterm_term::images::reclaim(store, &self.terminal, self.owner);
    }

    /// Deliver what the terminal queued with nobody to route it: output held for a pane's first real
    /// grid (see [`fbterm_term::Terminal::hold_until_sized`]) is parsed by its view's resize, on the
    /// app's thread, and so is a hidden pane's release; what either produces - replies, titles,
    /// pictures - needs the same delivery a feed gives it. The image store's `deliver` hook calls
    /// this too: that is how a finished decode's answer joins the queue of replies instead of being
    /// carried past them. Takes the terminal lock only to empty the queue: routing hands pictures to
    /// the store, which may lock the terminal itself. Raises `Dirty` for the same reason a feed does,
    /// once this section's lock is dropped.
    fn deliver_queued(&self) {
        let Some(terminal) = self.terminal.upgrade() else {
            return; // nobody owns this terminal any more: nothing waits for its events
        };
        // One critical section with the feeder and the image worker (see `Router::routing`).
        let routing = self.routing.lock();
        let events = {
            let mut term = terminal.lock();
            term.take_events()
        };
        drop(terminal);
        if events.is_empty() {
            drop(routing);
            return; // nothing queued: this call is not a reason to redraw
        }
        self.route(events);
        drop(routing);
        // The notice that the pane must redraw goes out with no lock held.
        self.mark_dirty();
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
            routing: parking_lot::Mutex::new(()),
        });
        fbterm_term::images::store().register_owner(
            owner,
            fbterm_term::images::OwnerHooks {
                term: Arc::downgrade(&terminal),
                // The worker leaves its answer in the terminal's queue; this delivers it through the
                // same critical section a feed uses, behind whatever output queued ahead of it.
                deliver: {
                    let router = router.clone();
                    Arc::new(move || router.deliver_queued())
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

    /// This session's router: what delivers terminal events when no feed produced them (see
    /// [`SessionHandle::deliver_events`]). A handle keeps a copy so the app can ask for that
    /// delivery without reaching into the session's own thread.
    pub(crate) fn router(&self) -> Arc<Router> {
        self.router.clone()
    }

    /// Feed output, answering programs through the session's replies queue as the emulator asks.
    ///
    /// `Dirty` and `Bell` are raised at most once until [`SessionHandle::take_dirty`] re-arms
    /// them; a title is reported only when it changes. Once the handle is closed the bytes are
    /// dropped (checked under the terminal lock, so nothing lands after `close` returns). The
    /// graphics protocols' stall watchdog runs on the same lock: this app schedules no timers,
    /// so feeding output is what keeps chunked transmissions and parked replies moving.
    pub fn feed(&mut self, bytes: &[u8]) {
        // Held across taking the events and routing them: this thread, whoever delivers what a pane's
        // release parsed, and the image worker (through `Router::deliver_queued`) all move the same
        // terminal's events out of its queue, and only one of them at a time may do it (see
        // `Router::routing`). Taken before the terminal lock, which is the order everywhere.
        let routing = self.router.routing.lock();
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
        drop(routing);
        // The notice that the pane must redraw goes out with no lock held.
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
            scale: 1.0,
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
mod routing_tests {
    use super::*;
    use fbterm_term::{GridSize, Terminal};

    fn grid(cols: u16) -> GridSize {
        GridSize {
            cols,
            rows: 3,
            cell_width: 8.0,
            cell_height: 16.0,
            scale: 1.0,
        }
    }

    /// A feeder over its own terminal, with the queue a session's writer would read and the router
    /// the handle would carry. `holding` starts it waiting for its first real grid, as `App::start`
    /// does for a pane whose window has not laid out.
    fn feeder(holding: bool) -> (Feeder, tokio::sync::mpsc::UnboundedReceiver<Command>) {
        let terminal = Arc::new(parking_lot::Mutex::new(Terminal::new(grid(10), 10)));
        if holding {
            terminal.lock().hold_until_sized();
        }
        let (events, _unread) = tokio::sync::mpsc::unbounded_channel();
        let (replies_tx, replies_rx) = tokio::sync::mpsc::unbounded_channel();
        let f = Feeder::new(
            SessionId::next(),
            terminal,
            events,
            Arc::default(),
            Arc::default(),
            Arc::new(AtomicBool::new(false)),
            Ended::default(),
            replies_tx,
        );
        (f, replies_rx)
    }

    /// What one query answers with, as bytes: `feed` sends them to the program.
    fn answer_of(query: &[u8]) -> Vec<u8> {
        let mut t = Terminal::new(grid(10), 10);
        t.feed(query);
        t.take_events()
            .into_iter()
            .filter_map(|e| match e {
                fbterm_term::TermEvent::PtyWrite(b) => Some(b),
                _ => None,
            })
            .collect::<Vec<_>>()
            .concat()
    }

    fn written(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Command>) -> Vec<u8> {
        let mut out = Vec::new();
        while let Ok(Command::Input(bytes)) = rx.try_recv() {
            out.extend_from_slice(&bytes);
        }
        out
    }

    /// A query asked before the pane had cells is not answered by the feed that carried it, nor by
    /// the resize that parses it: only delivering what that release queued - as `App::start`'s
    /// caller does when its view reports the grid - writes the answer to the program.
    #[test]
    fn a_reply_waits_for_the_delivery_that_follows_its_release() {
        let (mut feeder, mut replies) = feeder(true);
        feeder.feed(b"\x1b[c");
        assert!(
            written(&mut replies).is_empty(),
            "held output answered nobody"
        );

        // The view reports the grid its window really has: the resize parses what waited, and the
        // answer is queued. On this thread nothing feeds after a resize.
        feeder.terminal.lock().resize(grid(20));
        assert!(
            written(&mut replies).is_empty(),
            "a resize routes nothing itself"
        );

        feeder.router().deliver_queued();
        assert_eq!(written(&mut replies), answer_of(b"\x1b[c"));
    }

    /// The app delivering a release and the session feeding new output are two threads over one
    /// event queue. Taking the events and routing them is one section, so whichever gets there
    /// first takes what the other had not yet produced too - and every reply reaches the program in
    /// the order the emulator made it, no matter how the two threads interleave.
    #[test]
    fn events_reach_the_program_in_the_order_they_were_produced() {
        let (mut feeder, mut replies) = feeder(true);
        // Asked while held: its answer is what the release below queues first.
        feeder.feed(b"\x1b[c");
        feeder.terminal.lock().resize(grid(20));
        let da = answer_of(b"\x1b[c");

        let deliver = feeder.router();
        let later = std::thread::spawn(move || {
            // Output arriving now: asked after the release, so its answer must not overtake the
            // first one's.
            feeder.feed(b"\x1b[16t");
        });
        let released = std::thread::spawn(move || deliver.deliver_queued());
        later.join().expect("the feeder thread");
        released.join().expect("the delivering thread");

        let got = written(&mut replies);
        let cell = answer_of(b"\x1b[16t");
        assert!(got.len() >= da.len() + cell.len(), "both answers were sent");
        assert!(
            got.starts_with(&da),
            "the queued reply went last: {:?}",
            String::from_utf8_lossy(&got)
        );
        assert_eq!(got, [da.as_slice(), cell.as_slice()].concat());
    }

    /// The image worker's hand-off is not a second route. Its answer stays in the terminal's queue,
    /// and whoever delivers empties the whole queue under one lock - so a reply that output produced
    /// afterwards cannot overtake it. A channel says when the worker reached its hook (no sleeping):
    /// by then its answer is queued, this feeds a query behind it, and one delivery takes both.
    ///
    /// Under the hand-off this replaces - take the events, drop the lock, route them - the decode's
    /// answer would leave last: it had already left the queue when the second reply was produced.
    #[test]
    fn a_decode_answer_and_the_reply_that_follows_it_leave_in_order() {
        use fbterm_term::images::{ImageStore, OwnerHooks};

        let (feeder, mut replies) = feeder(false);
        let terminal = feeder.terminal.clone();
        let owner = terminal.lock().image_owner();
        // A store of our own, so nothing here depends on the app-wide one or on another test's jobs.
        let store = ImageStore::with_caps(1024 * 1024, 8 * 1024 * 1024);

        // The gate: the hook reports that it was reached (the answer is queued by then) and waits
        // here until this test opens it, so nothing the worker does can race with what follows. A
        // `Receiver` is not `Sync`, and a hook must be, so it lives behind a mutex - the only one
        // that ever takes it is the thread running this hook.
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let gate = Arc::new(parking_lot::Mutex::new(go_rx));
        let hook_router = feeder.router();
        store.register_owner(
            owner,
            OwnerHooks {
                term: Arc::downgrade(&terminal),
                deliver: Arc::new(move || {
                    entered_tx
                        .send(())
                        .expect("nobody was waiting for the hand-off");
                    gate.lock()
                        .recv()
                        .expect("the gate closed before it opened");
                    hook_router.deliver_queued();
                }),
                wake: Arc::new(|| {}),
            },
        );

        // A PNG the emulator defers to us, so the client's answer comes from this hand-off: the
        // frame is fed (the terminal parks its pending transmission and takes no answer yet), and
        // the payload goes to this store the way routing would hand it over. The bytes are the term
        // crate's own checked-in 4x4 fixture, read rather than retyped.
        let tiny = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../fbterm-term/tests/fixtures/images/tiny.png"
        ))
        .expect("the term crate's tiny PNG fixture");
        let frame = format!(
            "\x1b_Gi=1,a=t,f=100;{}\x1b\\",
            super::feeder_image_tests::b64(&tiny)
        );
        terminal.lock().feed(frame.as_bytes());
        let events = {
            let mut term = terminal.lock();
            term.take_events()
        };
        let (key, data) = events
            .into_iter()
            .find_map(|e| match e {
                fbterm_term::TermEvent::ImageReceived { key, data } => Some((key, data)),
                _ => None,
            })
            .expect("the terminal should have deferred this PNG to us");
        store.submit(owner, key, data);
        // Jobs run on another thread: it parks in the hook while this one works.
        let driver = std::thread::spawn(move || store.run_jobs_until_idle());

        entered_rx
            .recv()
            .expect("the worker never reached its deliver hook");
        // Output arriving now, put into the terminal as a feed puts it: asked after the decode was
        // answered, so its reply has to leave after the decode's.
        terminal.lock().feed(b"\x1b[c");
        feeder.router().deliver_queued();

        let got = written(&mut replies);
        let text = String::from_utf8_lossy(&got).into_owned();
        let ok_at = text
            .find("i=1;OK")
            .unwrap_or_else(|| panic!("the decode's answer never reached the program: {text:?}"));
        let da_at = text
            .find("\x1b[?")
            .unwrap_or_else(|| panic!("no device-attributes answer: {text:?}"));
        assert!(
            ok_at < da_at,
            "the decode's answer left after the reply produced after it: {text:?}"
        );

        // Open the gate and let that thread finish: by the time it returns, the hook has delivered
        // too, so what is checked below is everything the hand-off ever sent.
        let _ = go_tx.send(());
        driver.join().expect("the thread driving the jobs");
        assert!(
            written(&mut replies).is_empty(),
            "the hand-off routed a second copy of an answer"
        );
    }
}

#[cfg(test)]
mod feeder_image_tests {
    use super::*;
    use fbterm_term::images::store;

    /// Minimal standard-alphabet base64 for building kitty frames.
    pub(super) fn b64(bytes: &[u8]) -> String {
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
            scale: 1.0,
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
            scale: 1.0,
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
