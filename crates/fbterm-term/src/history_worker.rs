//! The background worker that compresses terminals' cold history.
//!
//! With history compression on, a terminal's old history blocks are compressed by history jobs
//! (feathercritty's `Term::next_history_jobs`). A job holds a copy of its block, so the work runs
//! with no lock held; only taking jobs and installing results lock the terminal. One worker
//! thread, `fbterm-history-worker` at a lowered priority, serves every terminal in the process:
//! [`enqueue`] after a feed that leaves jobs, and it sleeps on a condition variable while no
//! terminal has any (it never ticks).
//!
//! Jobs go in batches of [`BATCH`]: one lock hold installs the previous batch's results and takes
//! the next, so the worker keeps up with a parser that holds the lock almost all the time (with
//! one job per hold, a sustained flood grew the backlog without bound). Terminals take turns,
//! one batch each per turn; a terminal's next batch, taken in the hold that installed the last,
//! waits in its queue entry for its next turn.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use feathercritty::term::HistoryJob;
use parking_lot::{Condvar, Mutex};

use crate::{SharedTerminal, Terminal};

/// History jobs taken (and installed) per lock hold.
pub const BATCH: usize = 8;

/// A terminal waiting for its turn.
struct Entry {
    term: Weak<Mutex<Terminal>>,
    /// Jobs taken at the end of its last turn, run at the start of its next.
    jobs: Vec<HistoryJob>,
}

/// The queued terminals and whether a turn is in progress. `busy` lives under the same lock as
/// the entries, so "empty and not busy" is read without a gap between a turn's end and the
/// check of a waiter.
#[derive(Default)]
struct Queue {
    entries: VecDeque<Entry>,
    busy: bool,
    /// Terminals one of whose jobs panicked: never queued again in this session. The weak
    /// reference keeps the allocation, so no other terminal can reuse the pointer while it is
    /// listed; dead ones are pruned when another is added.
    poisoned: Vec<Weak<Mutex<Terminal>>>,
}

/// Ends a turn when dropped, also when it panicked: clears `busy` and wakes waiters, so they
/// never wait on a turn that is gone. A turn that unwinds (a panic outside a job's own guard,
/// such as in taking or installing jobs, which ends the thread) poisons its terminal first, so
/// the restarted thread never runs it again.
struct TurnGuard<'a> {
    worker: &'a Worker,
    /// The terminal of the turn.
    term: Weak<Mutex<Terminal>>,
}

impl Drop for TurnGuard<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.worker.poison(&self.term);
        }
        self.worker.finish_turn();
    }
}

/// Runs terminals' history jobs, round robin. The app uses the process-wide one through
/// [`enqueue`]; tests drive their own with [`Worker::run_until_idle`].
#[derive(Default)]
pub struct Worker {
    queue: Mutex<Queue>,
    wake: Condvar,
    /// A test makes the next turn's job panic.
    #[cfg(test)]
    panic_next: std::sync::atomic::AtomicBool,
    /// Signalled (with `queue` locked) when the queue is empty and no turn is in progress.
    idle: Condvar,
    /// The terminal (as a pointer) of every install hold, in order.
    #[cfg(test)]
    installs: Mutex<Vec<usize>>,
    /// Results installed, and results refused as stale.
    #[cfg(test)]
    installed: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    refused: std::sync::atomic::AtomicUsize,
    /// A test's code run once, right before the next batch's jobs run.
    #[cfg(test)]
    before_run: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Called when a turn let go of the last reference to a terminal (see [`on_released`]).
    released: Mutex<Option<Released>>,
    /// Whether the worker's thread runs (see [`Worker::start`]).
    running: AtomicBool,
}

/// See [`on_released`].
type Released = Arc<dyn Fn() + Send + Sync>;

impl Worker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Call `f` (on the worker's thread, no lock held) whenever a turn lets go of the last
    /// reference to a terminal; replaces an earlier one.
    pub fn on_released(&self, f: impl Fn() + Send + Sync + 'static) {
        *self.released.lock() = Some(Arc::new(f));
    }

    /// Queue `term` for history jobs and wake the worker. Cheap when it is already queued
    /// (a scan of the few queued terminals, no allocation): it is called after every feed that
    /// leaves jobs.
    pub fn enqueue(&self, term: &SharedTerminal) {
        let mut queue = self.queue.lock();
        let ptr = Arc::as_ptr(term);
        if queue.entries.iter().any(|e| e.term.as_ptr() == ptr)
            || queue.poisoned.iter().any(|p| p.as_ptr() == ptr)
        {
            return;
        }
        queue.entries.push_back(Entry {
            term: Arc::downgrade(term),
            jobs: Vec::new(),
        });
        drop(queue);
        self.wake.notify_one();
    }

    /// Run turns until no terminal is queued (tests; the thread blocks instead).
    pub fn run_until_idle(&self) {
        loop {
            let Some(entry) = self.take(&mut self.queue.lock()) else {
                return;
            };
            let _guard = TurnGuard {
                worker: self,
                term: entry.term.clone(),
            };
            self.turn(entry);
        }
    }

    /// Block until the queue is empty and no turn is in progress. A carried batch keeps its
    /// entry queued, so it counts as work. Sleeps on a condition variable.
    pub fn wait_idle(&self) {
        let mut queue = self.queue.lock();
        while !queue.entries.is_empty() || queue.busy {
            self.idle.wait(&mut queue);
        }
    }

    /// Like [`Worker::wait_idle`], but gives up at one deadline `timeout` from now. Returns
    /// whether the worker was idle.
    pub fn wait_idle_for(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut queue = self.queue.lock();
        while !queue.entries.is_empty() || queue.busy {
            if self.idle.wait_until(&mut queue, deadline).timed_out() {
                return queue.entries.is_empty() && !queue.busy;
            }
        }
        true
    }

    /// Pop the next entry that is not poisoned, marking a turn in progress in the same hold.
    fn take(&self, queue: &mut Queue) -> Option<Entry> {
        loop {
            let entry = queue.entries.pop_front()?;
            let ptr = entry.term.as_ptr();
            if !queue.poisoned.iter().any(|p| p.as_ptr() == ptr) {
                queue.busy = true;
                return Some(entry);
            }
        }
    }

    /// Never run `term` again in this session: list it (once) and drop its queued entries,
    /// such as one a feed queued while its panicking turn ran. A closed terminal needs nothing.
    fn poison(&self, term: &Weak<Mutex<Terminal>>) {
        if term.strong_count() == 0 {
            return;
        }
        let ptr = term.as_ptr();
        let mut queue = self.queue.lock();
        queue.entries.retain(|e| e.term.as_ptr() != ptr);
        queue.poisoned.retain(|p| p.strong_count() > 0);
        if !queue.poisoned.iter().any(|p| p.as_ptr() == ptr) {
            queue.poisoned.push(term.clone());
        }
    }

    /// A turn ended (any requeue already happened): wake waiters if nothing is left.
    fn finish_turn(&self) {
        let mut queue = self.queue.lock();
        queue.busy = false;
        if queue.entries.is_empty() {
            self.idle.notify_all();
        }
    }

    /// The worker thread's loop: returns only by unwinding (see [`Worker::start`]).
    fn run(&self) -> ! {
        loop {
            let entry = {
                let mut queue = self.queue.lock();
                loop {
                    if let Some(entry) = self.take(&mut queue) {
                        break entry;
                    }
                    self.wake.wait(&mut queue);
                }
            };
            let _guard = TurnGuard {
                worker: self,
                term: entry.term.clone(),
            };
            self.turn(entry);
        }
    }

    /// One turn of one terminal: run its batch with no lock held, then in one hold install the
    /// results and take the next batch. Requeued while it has work left.
    ///
    /// The terminal is only held (upgraded from the entry's weak reference) around the lock
    /// holds, never while jobs run: a tab closed meanwhile goes with its app and session
    /// references, which report the memory release. If a close lands during a hold, the turn
    /// lets go last and reports it through [`Worker::on_released`].
    fn turn(&self, entry: Entry) {
        let mut jobs = entry.jobs;
        if jobs.is_empty() {
            // A closed terminal: its jobs are dropped with it.
            let Some(term) = entry.term.upgrade() else {
                return;
            };
            jobs = term.lock().next_history_jobs(BATCH);
            self.let_go(term);
            if jobs.is_empty() {
                return;
            }
        }
        #[cfg(test)]
        if let Some(f) = self.before_run.lock().take() {
            f();
        }
        // A job that panics must not take the thread (and every terminal's compression) down:
        // drop its batch, turn compression off for this terminal and never queue it again in
        // this session (with compression off its cold blocks become decompression jobs, and if
        // decompressing is what panicked, each later feed would panic again).
        let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            jobs.into_iter()
                .map(|job| {
                    #[cfg(test)]
                    if self
                        .panic_next
                        .swap(false, std::sync::atomic::Ordering::SeqCst)
                    {
                        panic!("test: a history job panicked");
                    }
                    HistoryJob::run(job)
                })
                .collect::<Vec<_>>()
        }));
        // Closed while the jobs ran: the results are dropped.
        let Some(term) = entry.term.upgrade() else {
            return;
        };
        let Ok(done) = done else {
            tracing::error!("a history compression job panicked; compression is off for its tab");
            self.poison(&entry.term);
            let off = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                term.lock().set_history_compression(None);
            }));
            if off.is_err() {
                tracing::error!("turning history compression off for a tab panicked");
            }
            self.let_go(term);
            return;
        };
        let (next, more) = {
            let mut t = term.lock();
            for d in done {
                // `false`: stale (the history changed meanwhile); the result is dropped.
                let _installed = t.install_history_job(d);
                #[cfg(test)]
                if _installed {
                    &self.installed
                } else {
                    &self.refused
                }
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            let next = t.next_history_jobs(BATCH);
            let more = !next.is_empty() || t.has_history_jobs();
            (next, more)
        };
        #[cfg(test)]
        self.installs.lock().push(Arc::as_ptr(&term) as usize);
        if more {
            self.requeue(&term, next);
        }
        self.let_go(term);
    }

    /// Drop a turn's reference to `term`; if it was the last one (the tab closed during the
    /// hold), the terminal is dropped here and the release hook is called.
    fn let_go(&self, term: SharedTerminal) {
        if let Some(last) = Arc::into_inner(term) {
            drop(last);
            let hook = self.released.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
    }

    /// Put `term` back at the end of the queue with its next batch, unless a feed queued it
    /// meanwhile (then that entry takes the batch).
    fn requeue(&self, term: &SharedTerminal, jobs: Vec<HistoryJob>) {
        let mut queue = self.queue.lock();
        let ptr = Arc::as_ptr(term);
        if let Some(entry) = queue.entries.iter_mut().find(|e| e.term.as_ptr() == ptr) {
            // Only a turn gives an entry a batch, and the turn's own entry was taken, so with one
            // worker thread a queued entry never holds one here.
            debug_assert!(
                entry.jobs.is_empty(),
                "a queued entry already holds a batch"
            );
            if entry.jobs.is_empty() {
                entry.jobs = jobs;
            }
            return;
        }
        queue.entries.push_back(Entry {
            term: Arc::downgrade(term),
            jobs,
        });
    }
}

/// Queue a terminal that has history jobs on the process-wide worker, starting its thread on
/// the first call (and again after the thread ended, see [`Worker::start`]).
pub fn enqueue(term: &SharedTerminal) {
    global().enqueue(term);
}

/// Call `f` whenever the process-wide worker lets go of the last reference to a terminal (a tab
/// closed while one of its lock holds ran), so the app can ask for the memory release as it does
/// when a session task lets go last. Does not start the worker's thread.
pub fn on_released(f: impl Fn() + Send + Sync + 'static) {
    WORKER.get_or_init(Worker::new).on_released(f);
}

/// Block until the process-wide worker has nothing queued and no turn running. Returns at once
/// when its thread was never started.
pub fn wait_idle() {
    if let Some(worker) = WORKER.get() {
        worker.wait_idle();
    }
}

/// [`wait_idle`] for at most `timeout`; returns whether the worker was idle.
pub fn wait_idle_for(timeout: Duration) -> bool {
    WORKER.get().is_none_or(|w| w.wait_idle_for(timeout))
}

static WORKER: OnceLock<Worker> = OnceLock::new();

fn global() -> &'static Worker {
    let worker = WORKER.get_or_init(Worker::new);
    worker.start();
    worker
}

impl Worker {
    /// Start the worker's thread unless it runs. The thread's loop never returns, but a panic
    /// outside a job's guard (taking or installing jobs) ends it: its turn's terminal is
    /// poisoned ([`TurnGuard`]), and a guard clears the flag and starts a new thread at once if
    /// other terminals are queued (else the next enqueue does).
    pub fn start(&'static self) {
        // A plain load first: this runs after every feed that leaves jobs.
        if self.running.load(Ordering::Relaxed) || self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("fbterm-history-worker".into())
            .spawn(move || {
                let _exit = ThreadExit(self);
                lower_priority();
                self.run()
            });
        if let Err(e) = spawned {
            // History stays uncompressed until a later enqueue starts the thread; said once.
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!("cannot start the history compression worker: {e}");
            }
            self.running.store(false, Ordering::SeqCst);
        }
    }
}

/// Clears the worker's `running` flag when its thread ends, which is only ever by a panic, and
/// starts a new thread when terminals are still queued, so none waits for its next feed.
struct ThreadExit(&'static Worker);

impl Drop for ThreadExit {
    fn drop(&mut self) {
        tracing::error!("the history compression worker stopped; it restarts for queued work");
        self.0.running.store(false, Ordering::SeqCst);
        if !self.0.queue.lock().entries.is_empty() {
            self.0.start();
        }
    }
}

/// Lower the calling thread's priority, so compression yields to parsing and drawing. A
/// failure is ignored: the worker then runs at normal priority.
fn lower_priority() {
    #[cfg(target_os = "macos")]
    // SAFETY: sets the quality of service of the calling thread only; it takes two plain values
    // and touches no memory of ours.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
    #[cfg(target_os = "linux")]
    // SAFETY: on Linux the nice value belongs to the calling thread, not the process; `nice`
    // takes a plain value and touches no memory of ours.
    unsafe {
        libc::nice(10);
    }
    #[cfg(windows)]
    // SAFETY: `GetCurrentThread` returns a pseudo handle for the calling thread that needs no
    // closing; `SetThreadPriority` takes it and a plain value and touches no memory of ours.
    unsafe {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
        };
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use feathercritty::grid::{Dimensions, RowReader};
    use feathercritty::index::Line;
    use parking_lot::Mutex;

    use super::*;
    use crate::{GridSize, Terminal};

    fn size() -> GridSize {
        GridSize {
            scale: 1.0,
            cols: 120,
            rows: 24,
            cell_width: 8.0,
            cell_height: 16.0,
        }
    }

    /// `lines` numbered lines of about `width` characters.
    fn output(lines: usize, width: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for i in 0..lines {
            let text = format!("line {i:06} ");
            out.extend_from_slice(text.as_bytes());
            for k in 0..width.saturating_sub(text.len()) {
                out.push(b'a' + ((i + k) % 26) as u8);
            }
            out.extend_from_slice(b"\r\n");
        }
        out
    }

    fn terminal(scrollback: usize, keep: Option<usize>, bytes: &[u8]) -> Terminal {
        let mut t = Terminal::new(size(), scrollback);
        t.set_history_compression(keep);
        t.feed(bytes);
        t
    }

    fn shared(t: Terminal) -> SharedTerminal {
        Arc::new(Mutex::new(t))
    }

    #[test]
    fn worker_runs_jobs_until_none() {
        let bytes = output(20_000, 40);
        let term = shared(terminal(5_000, Some(1_000), &bytes));
        assert!(term.lock().has_history_jobs());

        let worker = Worker::new();
        worker.enqueue(&term);
        worker.run_until_idle();

        // Every result was installed: none was refused as stale.
        assert!(worker.installed.load(Ordering::SeqCst) > 0);
        assert_eq!(worker.refused.load(Ordering::SeqCst), 0);
        let t = term.lock();
        assert!(!t.has_history_jobs());
        let plain = terminal(5_000, None, &bytes);
        let (a, b) = (t.term().grid(), plain.term().grid());
        assert_eq!(a.history_size(), b.history_size());
        assert_eq!(a.history_size(), 5_000);
        let (mut ra, mut rb) = (RowReader::new(), RowReader::new());
        for i in 1..=a.history_size() as i32 {
            assert!(ra.row(a, Line(-i)) == rb.row(b, Line(-i)), "row {}", -i);
        }
    }

    #[test]
    fn wait_idle_returns_when_nothing_is_queued() {
        Worker::new().wait_idle();
    }

    #[test]
    fn wait_idle_blocks_until_every_job_is_done() {
        let term = shared(terminal(5_000, Some(1_000), &output(20_000, 40)));
        let worker = Arc::new(Worker::new());
        worker.enqueue(&term);
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = {
            let worker = worker.clone();
            std::thread::spawn(move || {
                worker.wait_idle();
                tx.send(()).unwrap();
            })
        };
        // Work is queued: the waiter must still be waiting.
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        worker.run_until_idle();
        rx.recv_timeout(Duration::from_secs(30)).unwrap();
        waiter.join().unwrap();
        assert!(!term.lock().has_history_jobs());
    }

    #[test]
    fn wait_idle_for_gives_up_at_its_deadline_while_a_turn_runs() {
        let worker = Worker::new();
        assert!(worker.wait_idle_for(Duration::from_millis(50)));
        worker.queue.lock().busy = true;
        let start = Instant::now();
        assert!(!worker.wait_idle_for(Duration::from_millis(50)));
        assert!(start.elapsed() >= Duration::from_millis(50));
        worker.finish_turn();
        assert!(worker.wait_idle_for(Duration::from_millis(50)));
    }

    #[test]
    fn a_panicking_job_does_not_stop_the_worker() {
        let bad = shared(terminal(5_000, Some(1_000), &output(20_000, 40)));
        let good = shared(terminal(5_000, Some(1_000), &output(20_000, 40)));
        let worker = Arc::new(Worker::new());
        worker
            .panic_next
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // The bad terminal is first in line, so it takes the panic.
        worker.enqueue(&bad);
        worker.enqueue(&good);
        // The worker thread's own loop, not `run_until_idle`.
        let thread = worker.clone();
        std::thread::spawn(move || thread.run());
        worker.wait_idle();
        assert_eq!(bad.lock().history_compression(), None);
        assert!(!good.lock().has_history_jobs());
        assert_eq!(good.lock().history_compression(), Some(1_000));
        // The thread still serves later enqueues.
        let later = shared(terminal(5_000, Some(1_000), &output(20_000, 40)));
        worker.enqueue(&later);
        worker.wait_idle();
        assert!(!later.lock().has_history_jobs());
        assert!(!worker.queue.lock().busy);
    }

    #[test]
    fn a_panic_outside_the_job_still_frees_waiters() {
        // The backstop: `TurnGuard` clears `busy` when a turn unwinds for any reason.
        let worker = Worker::new();
        worker.queue.lock().busy = true;
        drop(TurnGuard {
            worker: &worker,
            term: Weak::new(),
        });
        worker.wait_idle();
    }

    #[test]
    fn worker_skips_dropped_terminal() {
        let term = shared(terminal(5_000, Some(1_000), &output(20_000, 40)));
        let weak = Arc::downgrade(&term);
        let worker = Worker::new();
        worker.enqueue(&term);
        drop(term);
        worker.run_until_idle();
        assert!(worker.queue.lock().entries.is_empty());
        assert_eq!(weak.strong_count(), 0);
        assert!(worker.installs.lock().is_empty());
    }

    /// A panic outside a job's `catch_unwind` (here before the batch runs) ends the thread. The
    /// terminal of that turn is skipped from then on, so a deterministic panic cannot restart
    /// the thread and panic again on each feed; a terminal queued meanwhile is still served,
    /// the thread starting again by itself because work is left.
    #[test]
    fn a_dead_worker_thread_starts_again_without_the_terminal_it_died_on() {
        let worker: &'static Worker = Box::leak(Box::new(Worker::new()));
        let first = shared(terminal(5_000, Some(1_000), &output(20_000, 40)));
        let other = shared(terminal(5_000, Some(1_000), &output(20_000, 40)));
        let queued = other.clone();
        *worker.before_run.lock() = Some(Box::new(move || {
            // A feed of another tab lands during the turn that dies.
            worker.enqueue(&queued);
            panic!("test: the worker thread dies");
        }));
        worker.start();
        worker.enqueue(&first);
        assert!(
            worker.wait_idle_for(Duration::from_secs(30)),
            "the entry queued during the dying turn was stranded"
        );
        assert!(!other.lock().has_history_jobs());
        // The terminal the thread died on is never queued again.
        assert!(first.lock().has_history_jobs());
        worker.start();
        worker.enqueue(&first);
        assert!(worker.queue.lock().entries.is_empty());
        assert_eq!(worker.queue.lock().poisoned.len(), 1);
    }

    /// A feed during the turn whose job panics queues the terminal again before the panic is
    /// known: that entry is dropped with the poisoning, and the terminal is listed once.
    #[test]
    fn an_entry_queued_during_a_panicking_turn_does_not_run() {
        let worker: &'static Worker = Box::leak(Box::new(Worker::new()));
        let bad = shared(terminal(5_000, Some(1_000), &output(20_000, 40)));
        {
            // One cold block, so turning compression off leaves a decompression job, and more
            // blocks still to compress, so the worker's turn has jobs to panic on.
            let mut t = bad.lock();
            for job in t.next_history_jobs(1) {
                assert!(t.install_history_job(HistoryJob::run(job)));
            }
        }
        let fed = bad.clone();
        *worker.before_run.lock() = Some(Box::new(move || worker.enqueue(&fed)));
        worker.panic_next.store(true, Ordering::SeqCst);
        worker.enqueue(&bad);
        worker.run_until_idle();
        assert_eq!(bad.lock().history_compression(), None);
        assert!(bad.lock().has_history_jobs(), "decompression jobs left");
        assert!(worker.installs.lock().is_empty(), "the second entry ran");
        assert!(worker.queue.lock().entries.is_empty());
        assert_eq!(worker.queue.lock().poisoned.len(), 1);
    }

    /// A terminal whose job panicked is skipped for the rest of the session: with compression
    /// on again (its blocks would be offered once more), enqueuing it runs nothing.
    #[test]
    fn a_terminal_that_panicked_is_not_queued_again() {
        let bad = shared(terminal(5_000, Some(1_000), &output(20_000, 40)));
        let worker = Worker::new();
        worker.panic_next.store(true, Ordering::SeqCst);
        worker.enqueue(&bad);
        worker.run_until_idle();
        assert_eq!(bad.lock().history_compression(), None);

        bad.lock().set_history_compression(Some(1_000));
        assert!(bad.lock().has_history_jobs());
        worker.enqueue(&bad);
        assert!(worker.queue.lock().entries.is_empty());
        worker.run_until_idle();
        assert!(worker.installs.lock().is_empty());
        assert!(bad.lock().has_history_jobs());
        // Others are still served.
        let good = shared(terminal(5_000, Some(1_000), &output(20_000, 40)));
        worker.enqueue(&good);
        worker.run_until_idle();
        assert!(!good.lock().has_history_jobs());
    }

    /// The tab of a terminal is closed while a turn runs its jobs: the worker holds no
    /// reference across the run, so the terminal goes with the app's and the session's, and
    /// when the worker is the last to let go it says so once (the app then asks for the memory
    /// release, as for a session that let go last).
    #[test]
    fn closing_a_terminal_mid_turn_reports_its_release() {
        let term = shared(terminal(5_000, Some(1_000), &output(20_000, 40)));
        let weak = Arc::downgrade(&term);
        let worker = Worker::new();
        let released = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = released.clone();
        worker.on_released(move || {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        // Mid-run, the app and the session let go: nothing but the worker may still hold it.
        let holder = Arc::new(Mutex::new(Some(term.clone())));
        let held = holder.clone();
        let seen = Arc::new(Mutex::new(None));
        let strong = seen.clone();
        let watched = weak.clone();
        *worker.before_run.lock() = Some(Box::new(move || {
            held.lock().take();
            *strong.lock() = Some(watched.strong_count());
        }));
        worker.enqueue(&term);
        drop(term);
        worker.run_until_idle();
        assert_eq!(
            *seen.lock(),
            Some(0),
            "the worker held the terminal across the run"
        );
        assert_eq!(weak.strong_count(), 0);
        // Nobody else let go last here, so the worker reported nothing it did not do.
        assert!(holder.lock().is_none());
        assert_eq!(released.load(std::sync::atomic::Ordering::SeqCst), 0);

        // The worker lets go last (the others dropped theirs during one of its lock holds):
        // reported exactly once, and only then.
        let term = shared(Terminal::new(size(), 100));
        let other = term.clone();
        worker.let_go(term);
        assert_eq!(released.load(std::sync::atomic::Ordering::SeqCst), 0);
        worker.let_go(other);
        assert_eq!(released.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn worker_round_robins_terminals() {
        // About 600 rows per 64 KiB block: dozens of blocks, so several batches each.
        let bytes = output(30_000, 110);
        let a = shared(terminal(30_000, Some(1_000), &bytes));
        let b = shared(terminal(30_000, Some(1_000), &bytes));
        let worker = Worker::new();
        worker.enqueue(&a);
        worker.enqueue(&b);
        // Enqueuing again changes nothing.
        worker.enqueue(&a);
        worker.run_until_idle();

        assert!(!a.lock().has_history_jobs());
        assert!(!b.lock().has_history_jobs());

        let (pa, pb) = (Arc::as_ptr(&a) as usize, Arc::as_ptr(&b) as usize);
        let order = worker.installs.lock().clone();
        let batches = |p: usize| order.iter().filter(|&&q| q == p).count();
        assert!(batches(pa) >= 3, "{order:?}");
        assert!(batches(pb) >= 3, "{order:?}");
        // One batch per terminal per turn while both have work: strictly alternating until the
        // one with fewer batches is done.
        let alternating = 2 * batches(pa).min(batches(pb));
        for pair in order[..alternating].windows(2) {
            assert_ne!(pair[0], pair[1], "{order:?}");
        }
    }
}
