//! Returning freed memory to the OS.
//!
//! Dropping a scrollback frees its blocks to the allocator, which keeps much of them resident.
//! [`release_freed`] asks the allocator to hand them back. It is called only after a *user*
//! action that frees history (closing a tab or window, lowering the scrollback setting). A
//! program clearing its own history (`CSI 3 J`, `RIS`) never reaches it: remote programs must
//! not cause host work, and the freed blocks are reused by the next output anyway.
//!
//! Requests run on tokio's blocking pool and fold: a request runs at once unless one ran less
//! than [`WINDOW`] ago, in which case a single run is scheduled for the end of that window and
//! every request until then joins it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fbterm_term::Terminal;
use parking_lot::Mutex;

use crate::tabs::Pane;

/// A closed pane's terminal, watched until its last holder lets go.
pub(crate) type WeakTerminal = std::sync::Weak<Mutex<Terminal>>;

/// The terminals (session and placeholder) of `panes`, to watch while they close.
pub(crate) fn weak_terminals<'a>(panes: impl Iterator<Item = &'a Pane>) -> Vec<WeakTerminal> {
    panes
        .flat_map(|p| {
            p.session
                .iter()
                .map(|s| &s.terminal)
                .chain(p.placeholder.iter())
        })
        .map(Arc::downgrade)
        .collect()
}

/// Minimum time between two releases.
const WINDOW: Duration = Duration::from_secs(5);

type Job = Arc<dyn Fn() + Send + Sync>;

/// Run a job after a delay, off the caller's thread.
type Exec = Box<dyn Fn(Duration, Job) + Send + Sync>;

#[derive(Default)]
struct State {
    /// Earliest time the next release may run.
    next_slot: Option<Instant>,
    /// A trailing release is scheduled for this time; requests before it fold into it.
    pending_until: Option<Instant>,
}

/// Coalesces release requests into at most one run per [`WINDOW`].
struct Releaser {
    state: Mutex<State>,
    clock: Box<dyn Fn() -> Instant + Send + Sync>,
    backend: Job,
    exec: Exec,
}

impl Releaser {
    fn request(&self) {
        let now = (self.clock)();
        let mut st = self.state.lock();
        if st.pending_until.is_some_and(|p| now < p) {
            return;
        }
        st.pending_until = None;
        match st.next_slot {
            Some(slot) if now < slot => {
                st.pending_until = Some(slot);
                st.next_slot = Some(slot + WINDOW);
                (self.exec)(slot - now, self.backend.clone());
            }
            _ => {
                st.next_slot = Some(now + WINDOW);
                (self.exec)(Duration::ZERO, self.backend.clone());
            }
        }
    }
}

/// Production executor: the blocking pool when a runtime is running, else a short-lived thread.
#[cfg_attr(test, allow(dead_code))]
fn spawn_after(delay: Duration, job: Job) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let _ = tokio::task::spawn_blocking(move || job()).await;
        });
    } else {
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            job();
        });
    }
}

/// Ask the allocator to return freed memory to the OS (see the module docs for when).
#[cfg(not(test))]
pub(crate) fn release_freed() {
    use std::sync::OnceLock;
    static RELEASER: OnceLock<Releaser> = OnceLock::new();
    RELEASER
        .get_or_init(|| Releaser {
            state: Mutex::default(),
            clock: Box::new(Instant::now),
            backend: Arc::new(platform_release),
            exec: Box::new(spawn_after),
        })
        .request();
}

/// Tests count requests per test thread instead of touching the allocator.
#[cfg(test)]
pub(crate) fn release_freed() {
    REQUESTS.with(|r| r.set(r.get() + 1));
}

#[cfg(test)]
thread_local! {
    static REQUESTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Requests made on this thread so far (tests only).
#[cfg(test)]
pub(crate) fn requests() -> usize {
    REQUESTS.with(std::cell::Cell::get)
}

#[cfg(target_os = "macos")]
#[cfg_attr(test, allow(dead_code))]
fn platform_release() {
    unsafe extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
    }
    // SAFETY: a null zone means "every zone" and a goal of 0 means "as much as possible"; the
    // function takes no pointers we own and only frees unused pages inside the allocator.
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[cfg_attr(test, allow(dead_code))]
fn platform_release() {
    // SAFETY: `malloc_trim` takes no pointers and is safe to call from any thread; it only
    // returns unused heap pages to the OS.
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
#[cfg_attr(test, allow(dead_code))]
fn platform_release() {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Rig {
        releaser: Releaser,
        origin: Instant,
        now: Arc<Mutex<Instant>>,
        runs: Arc<AtomicUsize>,
        delays: Arc<Mutex<Vec<Duration>>>,
    }

    fn rig() -> Rig {
        let origin = Instant::now();
        let now = Arc::new(Mutex::new(origin));
        let runs = Arc::new(AtomicUsize::new(0));
        let delays = Arc::new(Mutex::new(Vec::new()));
        let counter = runs.clone();
        let log = delays.clone();
        let clock = now.clone();
        Rig {
            releaser: Releaser {
                state: Mutex::default(),
                clock: Box::new(move || *clock.lock()),
                backend: Arc::new(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                }),
                // Runs the job at once, remembering the delay it was asked to wait.
                exec: Box::new(move |d, job| {
                    log.lock().push(d);
                    job();
                }),
            },
            origin,
            now,
            runs,
            delays,
        }
    }

    impl Rig {
        /// Make a request `secs` seconds after the start.
        fn at(&self, secs: u64) {
            *self.now.lock() = self.origin + Duration::from_secs(secs);
            self.releaser.request();
        }
        fn runs(&self) -> usize {
            self.runs.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn release_requests_coalesce_within_five_seconds() {
        let r = rig();
        r.at(0);
        assert_eq!(r.runs(), 1, "the first request runs at once");
        // 1 s, 2 s and 3 s later: one run is scheduled for the end of the window, the rest join.
        r.at(1);
        r.at(2);
        r.at(3);
        assert_eq!(r.runs(), 2);
        assert_eq!(
            *r.delays.lock(),
            vec![Duration::ZERO, Duration::from_secs(4)]
        );
        // Still before the scheduled run (at 5 s): folds.
        r.at(4);
        assert_eq!(r.runs(), 2);
        // After it, a new request waits for the next slot (10 s), once.
        r.at(6);
        r.at(7);
        assert_eq!(r.runs(), 3);
        assert_eq!(r.delays.lock()[2], Duration::from_secs(4));
        // Long after: runs at once again.
        r.at(60);
        assert_eq!(r.runs(), 4);
        assert_eq!(r.delays.lock()[3], Duration::ZERO);
    }
}
