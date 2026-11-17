//! Output throughput of a local session: a real pseudo-terminal, the real reader and parser,
//! no window. Writes a file of log-like lines, runs `cat` on it in a session and times until
//! a marker printed after the file shows on the screen, so every byte has been parsed.
//!
//! Usage: `cargo run --release -p fbterm-session --example throughput [MB] [runs]`
//! (defaults: 64 MB, 3 runs). Prints MB/s per run and the median, for plain ASCII lines and
//! for lines mixing CJK and emoji.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fbterm_config::LocalProfile;
use fbterm_session::{EventSink, spawn_local};
use fbterm_term::GridSize;
use feathercritty::index::{Column, Line};
use tokio::sync::mpsc::unbounded_channel;

const MARKER: &str = "FBTERM-THROUGHPUT-DONE";
const LIMIT: Duration = Duration::from_secs(300);

/// A file of about `mb` megabytes of log-like lines, `unicode` mixing CJK and emoji in.
fn write_input(dir: &Path, mb: usize, unicode: bool) -> PathBuf {
    let path = dir.join(if unicode { "unicode.log" } else { "ascii.log" });
    let mut file = std::io::BufWriter::new(std::fs::File::create(&path).expect("create input"));
    let mut written = 0;
    let mut n = 0u64;
    while written < mb * 1024 * 1024 {
        let line = if unicode {
            format!(
                "2026-11-08T10:{:02}:{:02}Z worker-{} 処理を完了しました 🚀 request={} 状態=正常 ✅ latency={}ms\n",
                n / 60 % 60,
                n % 60,
                n % 16,
                n,
                n % 997
            )
        } else {
            format!(
                "2026-11-08T10:{:02}:{:02}Z INFO worker-{} handled request id={} path=/api/v1/items status=200 latency={}ms\n",
                n / 60 % 60,
                n % 60,
                n % 16,
                n,
                n % 997
            )
        };
        file.write_all(line.as_bytes()).expect("write input");
        written += line.len();
        n += 1;
    }
    file.flush().expect("flush input");
    path
}

/// Whether the marker shows on the screen.
fn done(handle: &fbterm_session::SessionHandle) -> bool {
    let t = handle.terminal.lock();
    let g = t.term().grid();
    let size = t.size();
    (0..size.rows as i32).any(|row| {
        let text: String = (0..size.cols as usize)
            .map(|col| g.screen()[Line(row)][Column(col)].c)
            .collect();
        text.contains(MARKER)
    })
}

/// Seconds to push `path` through a fresh session until the marker shows.
fn run(path: &Path) -> f64 {
    let profile = LocalProfile {
        program: PathBuf::from("/bin/sh"),
        args: vec![
            "-c".into(),
            format!(
                "cat '{}'; printf '\\n{MARKER}\\n'; sleep 30",
                path.display()
            ),
        ],
        cwd: None,
        env: BTreeMap::new(),
    };
    let size = GridSize {
        scale: 1.0,
        cols: 120,
        rows: 40,
        cell_width: 8.0,
        cell_height: 16.0,
    };
    let (tx, _rx): (EventSink, _) = unbounded_channel();
    let start = Instant::now();
    let handle = spawn_local(&profile, size, 10_000, tx).expect("spawn session");
    while !done(&handle) {
        assert!(start.elapsed() < LIMIT, "no marker after {LIMIT:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
    let secs = start.elapsed().as_secs_f64();
    handle.close();
    secs
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mb: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(64);
    let runs: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(3);
    let dir = tempfile::tempdir().expect("temp dir");
    for unicode in [false, true] {
        let path = write_input(dir.path(), mb, unicode);
        let bytes = std::fs::metadata(&path).expect("input size").len() as f64;
        let mut rates: Vec<f64> = (0..runs)
            .map(|_| bytes / (1024.0 * 1024.0) / run(&path))
            .collect();
        let label = if unicode { "cjk+emoji" } else { "ascii" };
        let each: Vec<String> = rates.iter().map(|r| format!("{r:.1}")).collect();
        rates.sort_by(f64::total_cmp);
        println!(
            "{label}: {mb} MB, MB/s per run [{}], median {:.1}",
            each.join(", "),
            rates[rates.len() / 2]
        );
    }
}
