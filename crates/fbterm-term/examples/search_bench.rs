//! Scrollback search timings: one full 10,000-line tab (plain text and a regex) and the
//! all-tabs search over 20 full tabs. Run with
//! `cargo run --release -p fbterm-term --example search_bench`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use fbterm_term::{
    GridSize, MATCH_CAP, SearchPattern, SearchQuery, SearchRequest, SharedTerminal, Terminal,
    search,
};
use parking_lot::Mutex;

const LINES: usize = 10_000;
const TABS: usize = 20;

fn full_tab(seed: usize) -> SharedTerminal {
    let mut t = Terminal::new(
        GridSize {
            cols: 160,
            rows: 50,
            cell_width: 8.0,
            cell_height: 16.0,
        },
        LINES,
    );
    let levels = ["INFO", "DEBUG", "WARN", "ERROR"];
    for i in 0..LINES + 100 {
        let line = format!(
            "2026-10-24T12:{:02}:{:02}.{:03}Z {:5} [worker-{}] request id={:08x} path=/api/v1/items/{} status={} took={}ms\r\n",
            (i / 60) % 60,
            i % 60,
            (i * 7) % 1000,
            levels[(i + seed) % 4],
            (i + seed) % 16,
            i.wrapping_mul(2_654_435_761) ^ seed,
            i % 977,
            [200, 201, 404, 500][(i / 3 + seed) % 4],
            (i * 13 + seed) % 900,
        );
        t.feed(line.as_bytes());
    }
    Arc::new(Mutex::new(t))
}

fn req(text: &str, regex: bool, limit: usize, snippets: bool) -> SearchRequest {
    SearchRequest {
        pattern: SearchPattern::new(&SearchQuery::new(text, regex))
            .unwrap()
            .unwrap(),
        below: None,
        limit,
        snippets,
    }
}

fn best_of<T>(n: usize, mut f: impl FnMut() -> T) -> (Duration, T) {
    let mut best = Duration::MAX;
    let mut last = None;
    for _ in 0..n {
        let start = Instant::now();
        let r = f();
        best = best.min(start.elapsed());
        last = Some(r);
    }
    (best, last.unwrap())
}

fn main() {
    let tabs: Vec<SharedTerminal> = (0..TABS).map(full_tab).collect();
    let one = &tabs[0];
    let lines = {
        let t = one.lock();
        let g = t.term().grid();
        alacritty_terminal::grid::Dimensions::total_lines(g)
    };
    println!("one tab: {lines} lines of 160 columns");
    let no = AtomicBool::new(false);
    for (label, text, regex) in [
        ("literal, rare (no match)", "zebra", false),
        ("literal, 1 in 4 lines", "status=500", false),
        ("literal, every line (capped)", "request", false),
        ("regex, rare", r"took=89\dms", true),
        ("regex, 1 in 4 lines", r"ERROR .*status=5\d\d", true),
    ] {
        let r = req(text, regex, MATCH_CAP, false);
        let (d, out) = best_of(5, || search(one, &r, &no));
        println!(
            "  {label:30} {:7.2} ms  {} hits{}",
            d.as_secs_f64() * 1000.0,
            out.hits.len(),
            if out.resume.is_some() {
                " (capped)"
            } else {
                ""
            }
        );
    }

    // Longest single lock: one chunk.
    let r = req("zebra", false, MATCH_CAP, false);
    let (d, _) = best_of(5, || {
        let start = Instant::now();
        let _ = search(one, &r, &no);
        start.elapsed()
    });
    let chunks = lines.div_ceil(fbterm_term::CHUNK_ROWS);
    println!(
        "  per lock (one {}-row chunk) about {:.2} ms",
        fbterm_term::CHUNK_ROWS,
        d.as_secs_f64() * 1000.0 / chunks as f64
    );

    println!("all tabs: {TABS} tabs, 50 hits per tab with snippets");
    for (label, text, regex) in [
        ("literal, rare", "zebra", false),
        ("literal, common", "status=500", false),
        ("regex", r"ERROR .*status=5\d\d", true),
    ] {
        let r = req(text, regex, 51, true);
        let (seq, _) = best_of(3, || {
            tabs.iter()
                .map(|t| search(t, &r, &no).hits.len())
                .sum::<usize>()
        });
        let (par, hits) = best_of(3, || {
            std::thread::scope(|s| {
                let handles: Vec<_> = tabs
                    .iter()
                    .map(|t| s.spawn(|| search(t, &r, &no).hits.len()))
                    .collect();
                handles
                    .into_iter()
                    .map(|h| h.join().unwrap())
                    .sum::<usize>()
            })
        });
        println!(
            "  {label:16} sequential {:7.2} ms, one thread per tab {:7.2} ms, {hits} hits",
            seq.as_secs_f64() * 1000.0,
            par.as_secs_f64() * 1000.0
        );
    }
}
