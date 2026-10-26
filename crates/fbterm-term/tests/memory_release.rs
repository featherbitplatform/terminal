//! Closing full tabs gives their memory back: fill 20 terminals with more than their default
//! scrollback (short, long, colored and wrapping lines), drop them all, and repeat. Live heap
//! bytes, counted by a test-only global allocator, must return to the baseline after every drop,
//! so nothing a terminal allocates outlives it and repeated fill-and-drop cycles cannot grow.
//!
//! The default test runs 3 cycles. The long run (30 cycles, or `FBTERM_MEMORY_CYCLES`) also
//! checks that the allocator stops asking the OS for more memory once it holds a cycle's peak:
//!
//! ```text
//! cargo test --release -p fbterm-term --test memory_release -- --ignored --nocapture
//! ```
//!
//! `--nocapture` prints the per-cycle numbers (on macOS with the malloc zones' bytes in use and
//! held, and the process RSS).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};

use fbterm_term::{GridSize, Terminal};

/// Counts the bytes and blocks currently allocated through the global allocator.
struct Counting;

static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);
static LIVE_BLOCKS: AtomicIsize = AtomicIsize::new(0);

// SAFETY: every call forwards to `System` with the caller's arguments unchanged; the counters
// are only bookkeeping.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded as is.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            LIVE_BYTES.fetch_add(layout.size() as isize, Ordering::Relaxed);
            LIVE_BLOCKS.fetch_add(1, Ordering::Relaxed);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded as is.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            LIVE_BYTES.fetch_add(layout.size() as isize, Ordering::Relaxed);
            LIVE_BLOCKS.fetch_add(1, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded as is.
        unsafe { System.dealloc(ptr, layout) };
        LIVE_BYTES.fetch_sub(layout.size() as isize, Ordering::Relaxed);
        LIVE_BLOCKS.fetch_sub(1, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded as is.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            LIVE_BYTES.fetch_add(
                new_size as isize - layout.size() as isize,
                Ordering::Relaxed,
            );
        }
        p
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const TABS: usize = 20;
/// More lines than the scrollback holds, so every history is full and has trimmed.
const LINES: usize = 12_000;
/// Live bytes allowed above the baseline after a drop (lazily created statics, test harness).
const SLACK: isize = 16 * 1024;

fn live() -> (isize, isize) {
    (
        LIVE_BYTES.load(Ordering::Relaxed),
        LIVE_BLOCKS.load(Ordering::Relaxed),
    )
}

/// Fill a terminal like the perf scenarios do, with a mix of line shapes: long plain lines,
/// short lines, colored log lines and an occasional line wider than the terminal (wraps).
fn fill(t: &mut Terminal, seed: usize, buf: &mut String) {
    use std::fmt::Write;
    let levels = [
        "\x1b[32mINFO\x1b[0m",
        "\x1b[36mDEBUG\x1b[0m",
        "\x1b[33mWARN\x1b[0m",
        "\x1b[1;31mERROR\x1b[0m",
    ];
    for i in 0..LINES {
        buf.clear();
        match (i + seed) % 4 {
            0 => {
                let _ = write!(buf, "{i:05} {}\r\n", "x".repeat(90));
            }
            1 => {
                let _ = write!(buf, "{i:05} xxxxxxxxxx\r\n");
            }
            2 => {
                let _ = write!(
                    buf,
                    "2026-10-26T12:00:{:02}Z {} [worker-{}] \x1b[38;5;{}mid={:08x}\x1b[0m status={}\r\n",
                    i % 60,
                    levels[i % 4],
                    (i + seed) % 16,
                    (i + seed) % 256,
                    i.wrapping_mul(2_654_435_761),
                    [200, 404, 500][i % 3],
                );
            }
            _ => {
                let _ = write!(
                    buf,
                    "{i:05} \x1b[48;2;40;40;{}m{}\x1b[0m\r\n",
                    i % 256,
                    "y".repeat(200)
                );
            }
        }
        t.feed(buf.as_bytes());
    }
}

#[cfg(target_os = "macos")]
mod os {
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Default)]
    struct MallocStatistics {
        blocks_in_use: u32,
        size_in_use: usize,
        max_size_in_use: usize,
        size_allocated: usize,
    }

    unsafe extern "C" {
        fn malloc_zone_statistics(zone: *mut c_void, stats: *mut MallocStatistics);
    }

    /// (bytes in use, bytes the zones hold) over all malloc zones, in bytes.
    pub fn zones() -> Option<(usize, usize)> {
        let mut s = MallocStatistics::default();
        // SAFETY: a null zone asks for the sum over all zones; `s` is a valid, writable
        // `malloc_statistics_t`.
        unsafe { malloc_zone_statistics(std::ptr::null_mut(), &mut s) };
        Some((s.size_in_use, s.size_allocated))
    }
}

#[cfg(not(target_os = "macos"))]
mod os {
    pub fn zones() -> Option<(usize, usize)> {
        None
    }
}

/// Resident set size in bytes, where `ps` can tell.
fn rss() -> Option<usize> {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    let kb: usize = String::from_utf8(out.stdout).ok()?.trim().parse().ok()?;
    Some(kb * 1024)
}

fn mb(b: isize) -> f64 {
    b as f64 / 1_048_576.0
}

type Row = (isize, isize, isize, Option<(usize, usize)>, Option<usize>);

/// Run `cycles` fill-and-drop cycles of `TABS` full terminals, check live bytes after each drop
/// and return the baseline (bytes, blocks) and one row per cycle: (live bytes with the tabs
/// full, live bytes after the drop, live blocks after the drop, zones, rss).
fn cycles(cycles: usize) -> ((isize, isize), Vec<Row>) {
    // Tests of this binary share the counters: run one at a time.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let size = GridSize {
        cols: 120,
        rows: 40,
        cell_width: 8.0,
        cell_height: 16.0,
    };
    let scrollback = fbterm_config::Settings::default().scrollback_lines;
    let mut buf = String::with_capacity(512);
    // Preallocated so recording does not move the counters.
    let mut rows: Vec<Row> = Vec::with_capacity(cycles);
    let mut tabs: Vec<Terminal> = Vec::with_capacity(TABS);
    let base = live();
    let base_zones = os::zones();
    let base_rss = rss();

    for _ in 0..cycles {
        for seed in 0..TABS {
            let mut t = Terminal::new(size, scrollback);
            fill(&mut t, seed, &mut buf);
            assert_eq!(t.history_len(), scrollback, "history full");
            tabs.push(t);
        }
        let (filled, _) = live();
        tabs.clear();
        let (bytes, blocks) = live();
        rows.push((filled, bytes, blocks, os::zones(), rss()));
    }

    println!(
        "baseline: live {:.2} MB in {} blocks, zones in use / held {}, rss {}",
        mb(base.0),
        base.1,
        fmt_zones(base_zones),
        fmt_rss(base_rss),
    );
    println!(
        "cycle | live, {TABS} full tabs (MB) | live after drop - baseline | zones in use / held (MB) | rss (MB)"
    );
    for (i, (filled, bytes, blocks, zones, rss)) in rows.iter().enumerate() {
        println!(
            "{:>5} | {:>8.2} | {:+} B, {:+} blocks | {} | {}",
            i + 1,
            mb(*filled),
            bytes - base.0,
            blocks - base.1,
            fmt_zones(*zones),
            fmt_rss(*rss),
        );
    }

    for (i, (filled, bytes, _, _, _)) in rows.iter().enumerate() {
        assert!(
            filled - base.0 > 10 * 1024 * 1024,
            "cycle {}: the tabs should hold real history (got {} bytes)",
            i + 1,
            filled - base.0
        );
        assert!(
            bytes - base.0 <= SLACK,
            "cycle {}: {} bytes still live after dropping every terminal",
            i + 1,
            bytes - base.0
        );
    }
    // From the second cycle on, nothing at all is left behind per cycle.
    let second = rows.get(1).map_or(isize::MAX, |r| r.1);
    for (i, row) in rows.iter().enumerate().skip(2) {
        assert!(
            row.1 <= second,
            "cycle {}: live bytes after drop grew from {second} to {}",
            i + 1,
            row.1
        );
    }
    (base, rows)
}

fn fmt_zones(z: Option<(usize, usize)>) -> String {
    z.map_or("na".into(), |(u, a)| {
        format!("{:.1} / {:.1}", mb(u as isize), mb(a as isize))
    })
}

fn fmt_rss(r: Option<usize>) -> String {
    r.map_or("na".into(), |r| format!("{:.1}", mb(r as isize)))
}

#[test]
fn closing_full_tabs_returns_live_memory_to_baseline() {
    cycles(3);
}

/// Long run: live bytes as above, plus (macOS) the memory the malloc zones hold must stop
/// growing. A leak grows it in nearly every cycle; the allocator's cache grows it in the first
/// cycles and, rarely, in a single later step (seen once in ten 30- to 150-cycle runs: a step
/// of 84 MB at cycle 13, flat afterwards), so a few growing cycles are allowed.
#[test]
#[ignore = "about 4 s in release, a minute in debug; see the module docs"]
fn repeated_fill_and_drop_plateaus() {
    let n = std::env::var("FBTERM_MEMORY_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30usize)
        .max(10);
    let (_, rows) = cycles(n);
    let held: Vec<usize> = rows.iter().filter_map(|r| r.3.map(|z| z.1)).collect();
    if held.len() == rows.len() {
        let grew = held.windows(2).filter(|w| w[1] > w[0]).count();
        assert!(
            grew <= (n / 5).max(3),
            "the malloc zones grew in {grew} of {n} cycles: {held:?}"
        );
    }
}
