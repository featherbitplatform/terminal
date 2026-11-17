//! Cold history read timings (history compression, spec 6.4, fbterm rows): one tab of 126 x 50
//! with a 1,000,000-line history of colored service-log lines, once with compression off and
//! once with it on (10,000 rows kept uncompressed, every job run), compared on
//!
//! - the draw-list build with the view entirely in compressed history: the first frame at a
//!   position no read has touched (decompression cache cold), then the following frames there
//!   (cache warm) against the same frames with compression off;
//! - a search across the whole history that matches nothing;
//! - an export of the whole history (plain and with colors, to a sink).
//!
//! Every measure is repeated (`RUNS`, default 9; off first on even runs, on first on odd ones)
//! and reported as the median with the range. The on tab has no history job left while timed. Run with
//! `cargo run --release -p fbterm-term --example cold_bench`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use fbterm_term::{
    CellGeometry, ColorScheme, ExportFormat, GridSize, MATCH_CAP, SearchPattern, SearchQuery,
    SearchRequest, SharedTerminal, Terminal, build_draw_list, export, search,
};
use feathercritty::grid::RowReader;
use parking_lot::Mutex;

const LINES: usize = 1_000_000;
const KEEP: usize = 10_000;
const COLS: u16 = 126;
const ROWS: u16 = 50;
/// Warm frames per run of the draw-list measure.
const FRAMES: usize = 200;

/// Small deterministic generator, so the content never depends on a platform or a crate.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

const MODULES: &[&str] = &[
    "api::handlers::items",
    "api::auth",
    "db::pool",
    "worker::queue",
    "cache::redis",
    "http::server",
    "billing::invoices",
    "search::index",
];

/// `rows` service-log lines: dim timestamp, colored level, dim module, then one of a few
/// message templates with varying ids and numbers (about 100 characters).
fn service_log(seed: u64, rows: usize) -> Vec<u8> {
    let mut rng = Lcg(seed);
    let mut ms: u64 = rng.next(80_000_000);
    let mut out = String::with_capacity(rows * 120);
    for _ in 0..rows {
        ms += rng.next(900);
        let (h, m, s, f) = (
            ms / 3_600_000 % 24,
            ms / 60_000 % 60,
            ms / 1000 % 60,
            ms % 1000,
        );
        let which = rng.next(20);
        let (level, color) = match which {
            15 | 16 | 19 => ("ERROR", "\x1b[31m"),
            6 | 9 | 11 => (" WARN", "\x1b[33m"),
            _ => (" INFO", "\x1b[32m"),
        };
        let module = MODULES[rng.next(MODULES.len() as u64) as usize];
        let text = match which % 5 {
            0 => format!(
                "request id={:08x} path=/api/v1/items/{} status={} took={}ms",
                rng.next(1 << 32),
                rng.next(10_000),
                [200, 201, 404, 500][rng.next(4) as usize],
                rng.next(900)
            ),
            1 => format!(
                "connection {} returned to pool (idle={}, busy={})",
                rng.next(64),
                rng.next(32),
                rng.next(32)
            ),
            2 => format!(
                "job {} finished in {}.{:03}s, {} retries",
                rng.next(1_000_000),
                rng.next(30),
                rng.next(1000),
                rng.next(4)
            ),
            3 => format!(
                "cache miss for key user:{}:profile, fetched in {}ms",
                rng.next(100_000),
                rng.next(50)
            ),
            _ => format!(
                "user {} signed in from 10.{}.{}.{}",
                rng.next(100_000),
                rng.next(256),
                rng.next(256),
                rng.next(256)
            ),
        };
        out.push_str(&format!(
            "\x1b[2m2026-11-08T{h:02}:{m:02}:{s:02}.{f:03}Z\x1b[0m {color}{level}\x1b[0m \
             \x1b[2m{module}\x1b[0m: {text}\r\n"
        ));
    }
    out.into_bytes()
}

fn tab(compression: Option<usize>, bytes: &[u8]) -> SharedTerminal {
    let mut t = Terminal::new(
        GridSize {
            cols: COLS,
            rows: ROWS,
            cell_width: 8.0,
            cell_height: 16.0,
            scale: 1.0,
        },
        LINES,
    );
    t.set_history_compression(compression);
    for piece in bytes.chunks(64 * 1024) {
        t.feed(piece);
    }
    Arc::new(Mutex::new(t))
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Median and range of `samples` (milliseconds or ratios), as "median [min - max]".
fn stats(samples: &mut [f64]) -> (f64, String) {
    samples.sort_by(f64::total_cmp);
    let median = samples[samples.len() / 2];
    let fmt = |v: f64| {
        if v < 1.0 {
            format!("{v:.4}")
        } else if v < 10.0 {
            format!("{v:.3}")
        } else {
            format!("{v:.1}")
        }
    };
    (
        median,
        format!(
            "{} [{} - {}]",
            fmt(median),
            fmt(samples[0]),
            fmt(samples[samples.len() - 1])
        ),
    )
}

/// Median per-frame time of `FRAMES` builds with one reader, as the widget keeps it.
fn frames(t: &Terminal, scheme: &ColorScheme) -> f64 {
    let mut reader = RowReader::new();
    let mut times: Vec<f64> = (0..FRAMES)
        .map(|_| {
            let start = Instant::now();
            let list = build_draw_list(
                &mut reader,
                t,
                scheme,
                true,
                None,
                None,
                fbterm_term::images::store(),
                CellGeometry::new(iced::Size::new(8.0, 16.0), 1.0),
            );
            std::hint::black_box(list.texts.len());
            ms(start.elapsed())
        })
        .collect();
    stats(&mut times).0
}

fn main() {
    let runs: usize = std::env::var("RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9)
        .max(5);
    let bytes = service_log(7, LINES + 2 * usize::from(ROWS));
    println!(
        "content: {} service-log lines, {:.1} MB, {COLS}x{ROWS}, {LINES}-line scrollback",
        LINES + 2 * usize::from(ROWS),
        bytes.len() as f64 / 1e6
    );
    let start = Instant::now();
    let off = tab(None, &bytes);
    println!("fill, off: {:.0} ms", ms(start.elapsed()));
    let start = Instant::now();
    let on = tab(Some(KEEP), &bytes);
    let fill_on = start.elapsed();
    let start = Instant::now();
    on.lock().run_history_jobs();
    let fill_jobs = start.elapsed();
    {
        // Nothing may be left to compress while timing (the flag stays up after the last
        // install until a look for more finds none).
        let mut t = on.lock();
        assert!(t.next_history_jobs(usize::MAX).is_empty(), "jobs left");
        assert!(!t.has_history_jobs(), "jobs left");
    }
    println!(
        "fill, on: {:.0} ms, then every job run inline: {:.0} ms (history {} lines)",
        ms(fill_on),
        ms(fill_jobs),
        on.lock().history_len()
    );
    println!("runs per measure: {runs}, median [min - max] in ms");

    // Draw list: each run views a fresh position deep in compressed history, 60,000 lines
    // apart, so nothing there is in the decompression cache (4 blocks of about 500 rows).
    let scheme = ColorScheme::graphite();
    let (mut first_on, mut first_off, mut warm_on, mut warm_off, mut ratio) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for r in 0..runs {
        let offset = (100_000 + r * 60_000 % 840_000) as i32;
        let first = |tab: &SharedTerminal| {
            let mut t = tab.lock();
            t.scroll_to_bottom();
            t.scroll(offset);
            let start = Instant::now();
            let list = build_draw_list(
                &mut RowReader::new(),
                &t,
                &scheme,
                true,
                None,
                None,
                fbterm_term::images::store(),
                CellGeometry::new(iced::Size::new(8.0, 16.0), 1.0),
            );
            let d = ms(start.elapsed());
            std::hint::black_box(list.texts.len());
            d
        };
        let (x, y) = in_turn(r, || first(&off), || first(&on));
        first_off.push(x);
        first_on.push(y);
        let (a, b) = in_turn(
            r,
            || frames(&off.lock(), &scheme),
            || frames(&on.lock(), &scheme),
        );
        warm_off.push(a);
        warm_on.push(b);
        ratio.push(b / a);
    }
    let (f, fs) = stats(&mut first_on);
    let (_, fos) = stats(&mut first_off);
    let (_, wos) = stats(&mut warm_off);
    let (_, wns) = stats(&mut warm_on);
    let (q, qs) = stats(&mut ratio);
    println!("draw-list build, view entirely in compressed history:");
    println!(
        "  first frame (cache cold): on {fs}, off {fos}  gate <= 2 ms: {}",
        verdict(f <= 2.0)
    );
    println!(
        "  following frames: on {wns}, off {wos}, ratio {qs}  gate <= 1.15x: {}",
        verdict(q <= 1.15)
    );

    // Search across the whole history, no match.
    let no = AtomicBool::new(false);
    for (label, text, regex) in [
        ("literal, no match", "zebra", false),
        ("regex, no match", r"took=9\d{3}ms", true),
    ] {
        let r = SearchRequest {
            pattern: SearchPattern::new(&SearchQuery::new(text, regex))
                .unwrap()
                .unwrap(),
            below: None,
            limit: MATCH_CAP,
            snippets: false,
            drop_cache: true,
        };
        let (mut a, mut b, mut q) = (Vec::new(), Vec::new(), Vec::new());
        for run in 0..runs {
            let time = |t: &SharedTerminal| {
                let start = Instant::now();
                let out = search(t, &r, &no);
                assert!(out.hits.is_empty() && out.resume.is_none());
                ms(start.elapsed())
            };
            let (x, y) = in_turn(run, || time(&off), || time(&on));
            a.push(x);
            b.push(y);
            q.push(y / x);
        }
        let (_, as_) = stats(&mut a);
        let (_, bs) = stats(&mut b);
        let (m, qs) = stats(&mut q);
        println!(
            "search, {label}: off {as_}, on {bs}, ratio {qs}  gate <= 2x: {}",
            verdict(m <= 2.0)
        );
    }

    // Export of the whole history.
    for (label, format) in [
        ("plain", ExportFormat::Plain),
        ("with colors", ExportFormat::Ansi),
    ] {
        let (mut a, mut b, mut q) = (Vec::new(), Vec::new(), Vec::new());
        for run in 0..runs {
            let time = |t: &SharedTerminal| {
                let start = Instant::now();
                export(t, format, &mut std::io::sink()).unwrap();
                ms(start.elapsed())
            };
            let (x, y) = in_turn(run, || time(&off), || time(&on));
            a.push(x);
            b.push(y);
            q.push(y / x);
        }
        let (_, as_) = stats(&mut a);
        let (_, bs) = stats(&mut b);
        let (m, qs) = stats(&mut q);
        println!(
            "export, {label}: off {as_}, on {bs}, ratio {qs}  gate <= 2x: {}",
            verdict(m <= 2.0)
        );
    }
}

/// Run `off` and `on` (returned in that order), off first on even runs and on first on odd
/// ones, so neither always runs right after the other's work.
fn in_turn<T>(run: usize, off: impl FnOnce() -> T, on: impl FnOnce() -> T) -> (T, T) {
    if run.is_multiple_of(2) {
        let a = off();
        (a, on())
    } else {
        let b = on();
        (off(), b)
    }
}

fn verdict(pass: bool) -> &'static str {
    if pass { "PASS" } else { "MISS" }
}
