//! Golden outputs for export, search and selection over fixed input streams.
//!
//! The files under `tests/golden/` were recorded before scrollback storage changed inside the
//! emulation library and must stay byte-identical afterwards. `UPDATE_GOLDEN=1` rewrites them.
//!
//! Per stream and per state (`initial` at 126x37, `narrow` after a resize to 80 columns,
//! `restored` after resizing back to 126) there is an export as `.txt` and `.ansi` (the `.gz`
//! variants are written to a temporary file and compared after decompressing, since gzip
//! headers carry timestamps), and one `<stream>.json` with find-bar matches, find-in-all-tabs
//! results and selection texts for all three states.
//!
//! Find-in-all-tabs lives in the app crate; it runs `fbterm_term::search` once per tab with
//! `limit = 51` and snippets on, so the golden runs exactly that primitive across the first
//! three streams.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use fbterm_term::{
    ExportFormat, GridSize, Hit, MATCH_CAP, SearchOutcome, SearchPattern, SearchQuery,
    SearchRequest, Selection, SharedTerminal, Terminal, export, search, write_file,
};
use feathercritty::index::{Column, Line, Point};
use serde_json::{Value, json};

const COLS_WIDE: u16 = 126;
const COLS_NARROW: u16 = 80;
const ROWS: u16 = 37;
const HISTORY: usize = 150;
/// The find-in-all-tabs list size (`PER_TAB + 1` in the app crate).
const ALL_TABS_LIMIT: usize = 51;

fn size(cols: u16) -> GridSize {
    GridSize {
        cols,
        rows: ROWS,
        cell_width: 8.0,
        cell_height: 16.0,
    }
}

/// Small deterministic generator, so streams never depend on a platform or a crate version.
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

/// A colored log: levels in different SGR styles, long lines that wrap at both widths, more
/// lines than the scrollback holds.
fn colored_log() -> Vec<u8> {
    let mut rng = Lcg(7);
    let levels = [
        ("\x1b[32mINFO \x1b[0m", "request served"),
        ("\x1b[33mWARN \x1b[0m", "slow upstream"),
        ("\x1b[1;31mERROR\x1b[0m", "error: connection reset by peer"),
        ("\x1b[2;37mDEBUG\x1b[0m", "cache miss"),
        ("\x1b[38;5;208mTRACE\x1b[0m", "span closed"),
        ("\x1b[38;2;10;200;180mNOTE \x1b[0m", "rgb styled text"),
    ];
    let mut out = String::new();
    for i in 0..260u64 {
        let (lvl, msg) = levels[rng.next(levels.len() as u64) as usize];
        let id = rng.next(90000) + 10000;
        out.push_str(&format!(
            "\x1b[90m2026-10-25T12:{:02}:{:02}Z\x1b[0m {lvl} {msg} id={id} seq={i}",
            (i / 60) % 60,
            i % 60
        ));
        if i % 9 == 0 {
            out.push_str(
                " \x1b[4mpath\x1b[24m=/srv/app/releases/current/very/long/path/to/a/resource/that/wraps/at/narrow/widths.json",
            );
        }
        if i % 13 == 0 {
            out.push_str(" \x1b[48;5;52m background \x1b[49m");
        }
        out.push_str("\r\n");
    }
    out.into_bytes()
}

/// A shell session: prompts, typed input with edits, cursor movement, screen clears, tabs.
fn shell_session() -> Vec<u8> {
    let mut out = String::new();
    for i in 0..45 {
        out.push_str(&format!(
            "\x1b[1;32muser@host\x1b[0m:\x1b[1;34m~/proj{}\x1b[0m$ ",
            i % 4
        ));
        out.push_str(&format!("ls -la dirx\x08\x08\x08\x08rs{i}\r\n"));
        out.push_str("total 8\r\n");
        out.push_str(&format!(
            "drwxr-xr-x  3 user user 4096 Oct 25 12:00 sub{i}\r\n"
        ));
        out.push_str("-rw-r--r--  1 user user  220 Oct 25 12:00 notes.txt\r\n");
        if i % 7 == 3 {
            out.push_str(&format!(
                "bash: foo{i}: command not found, error code 127\r\n"
            ));
        }
        if i % 5 == 0 {
            out.push_str("col1\tcol2\tcol3\tid=");
            out.push_str(&format!("{}\r\n", 40000 + i * 17));
        }
    }
    // Overwrite a line in place with a carriage return, then clear part of the screen.
    out.push_str("progress 10%\rprogress 100%\x1b[K\r\n");
    out.push_str("\x1b[2K\x1b[1Aoverwritten line, error gone\r\n");
    out.push_str("\x1b[1;32muser@host\x1b[0m:\x1b[1;34m~\x1b[0m$ ");
    out.into_bytes()
}

/// Wide characters (CJK), emoji, combining marks, mixed with ASCII and wrapping.
fn cjk_and_emoji() -> Vec<u8> {
    let mut out = String::new();
    let words = [
        "日本語のテキスト",
        "中文字符测试",
        "한국어 텍스트",
        "emoji 😀🚀🎉 mixed",
        "combining e\u{301}a\u{300}n\u{303}",
        "ASCII error line id=",
    ];
    for i in 0..70usize {
        let w = words[i % words.len()];
        out.push_str(&format!("{i:03} {w} {}\r\n", 20000 + i * 31));
        if i % 6 == 0 {
            // Long enough to wrap, with wide characters near the wrap edge at both widths.
            out.push_str(&"日本".repeat(45));
            out.push_str(" error\r\n");
        }
        if i % 11 == 0 {
            out.push_str("\x1b[1;35m太字\x1b[0m \x1b[3m斜体\x1b[0m \x1b[7m反転\x1b[0m\r\n");
        }
    }
    out.into_bytes()
}

/// OSC 8 hyperlinks around text, with surrounding plain and styled text.
fn hyperlinks() -> Vec<u8> {
    let mut out = String::new();
    for i in 0..60 {
        out.push_str(&format!(
            "see \x1b]8;;https://example.com/docs/{i}\x1b\\the docs page {i}\x1b]8;;\x1b\\ for error details id={}\r\n",
            30000 + i
        ));
        if i % 8 == 0 {
            out.push_str(
                "\x1b]8;id=a;https://example.com/long\x1b\\\x1b[34;4m日本 link text that is long enough to wrap around at the narrow width of eighty columns\x1b[0m\x1b]8;;\x1b\\\r\n",
            );
        }
    }
    out.into_bytes()
}

/// A full-screen program left on the alternate screen, with normal-screen history behind it.
fn alt_screen() -> Vec<u8> {
    let mut out = String::new();
    for i in 0..50 {
        out.push_str(&format!(
            "before the program {i}: error? id={}\r\n",
            50000 + i
        ));
    }
    out.push_str("\x1b[?1049h\x1b[2J\x1b[H");
    for row in 1..=ROWS {
        out.push_str(&format!(
            "\x1b[{row};1H\x1b[7m{:<60}\x1b[0m",
            format!(" pane row {row} 日本"),
        ));
    }
    out.push_str("\x1b[10;5Hbox error id=99999");
    out.push_str("\x1b[20;70H\x1b[1;33mstatus line\x1b[0m");
    out.into_bytes()
}

/// The fixed inputs: name and the bytes fed to a fresh terminal.
fn streams() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("colored_log", colored_log()),
        ("shell_session", shell_session()),
        ("cjk_emoji", cjk_and_emoji()),
        ("hyperlinks", hyperlinks()),
        ("alt_screen", alt_screen()),
    ]
}

fn new_terminal(bytes: &[u8]) -> SharedTerminal {
    let mut t = Terminal::new(size(COLS_WIDE), HISTORY);
    // Feed in uneven pieces so the parser state across calls is covered too.
    for piece in bytes.chunks(997) {
        t.feed(piece);
    }
    Arc::new(parking_lot::Mutex::new(t))
}

/// The states one terminal passes through: name, then the terminal in that state.
fn walk(shared: &SharedTerminal, mut each: impl FnMut(&str, &SharedTerminal)) {
    each("initial", shared);
    shared.lock().resize(size(COLS_NARROW));
    each("narrow", shared);
    shared.lock().resize(size(COLS_WIDE));
    each("restored", shared);
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn check(name: &str, content: &[u8]) {
    let path = golden_dir().join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(golden_dir()).unwrap();
        fs::write(&path, content).unwrap();
        return;
    }
    let want = fs::read(&path)
        .unwrap_or_else(|e| panic!("missing golden {name} ({e}); run with UPDATE_GOLDEN=1"));
    if want != content {
        let got = String::from_utf8_lossy(content);
        let exp = String::from_utf8_lossy(&want);
        let line = got
            .lines()
            .zip(exp.lines())
            .position(|(a, b)| a != b)
            .map_or_else(
                || "length differs".to_string(),
                |n| format!("line {}", n + 1),
            );
        panic!("golden {name} differs ({line})");
    }
}

fn export_text(shared: &SharedTerminal, format: ExportFormat) -> Vec<u8> {
    let mut out = Vec::new();
    export(shared, format, &mut out).unwrap();
    out
}

fn gunzip(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut out)
        .unwrap();
    out
}

/// A scratch directory under the target dir's temp area, removed afterwards.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fbterm-golden-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn pattern(text: &str, regex: bool) -> SearchPattern {
    SearchPattern::new(&SearchQuery::new(text, regex))
        .unwrap()
        .unwrap()
}

/// The three fixed find queries: smart-case plain text, a regex, and a wide-character string.
fn queries() -> Vec<(&'static str, SearchPattern)> {
    vec![
        ("error", pattern("error", false)),
        (r"id=\d{5}", pattern(r"id=\d{5}", true)),
        ("日本", pattern("日本", false)),
    ]
}

fn run_search(
    shared: &SharedTerminal,
    p: &SearchPattern,
    limit: usize,
    snippets: bool,
) -> SearchOutcome {
    search(
        shared,
        &SearchRequest {
            pattern: p.clone(),
            below: None,
            limit,
            snippets,
        },
        &AtomicBool::new(false),
    )
}

/// One hit on one line of text, so the golden files stay small and diff well.
fn hit_json(h: &Hit) -> Value {
    let (s, e) = (h.range.start, h.range.end);
    let mut line = format!("{}:{}-{}:{}", s.line, s.col, e.line, e.col);
    if let Some(sn) = &h.snippet {
        line.push_str(&format!(
            " [{}..{}] {:?}",
            sn.highlight.start, sn.highlight.end, sn.text
        ));
    }
    Value::String(line)
}

fn outcome_json(o: &SearchOutcome) -> Value {
    json!({
        "count": o.hits.len(),
        "resume": o.resume,
        "alt_screen": o.alt_screen,
        "cols": o.cols,
        "hits": o.hits.iter().map(hit_json).collect::<Vec<_>>(),
    })
}

fn p(line: i32, col: usize) -> Point {
    Point::new(Line(line), Column(col))
}

/// Four fixed selections (anchor, head), including one reversed, one reaching into the
/// scrollback and one across the history/screen boundary. Points above the history are
/// clamped by the selection itself, so they stay valid at every width.
fn selections() -> Vec<(&'static str, Point, Point)> {
    vec![
        ("screen", p(0, 0), p(i32::from(ROWS) - 1, 70)),
        ("history", p(-12, 5), p(-3, 60)),
        ("row_reversed", p(10, 60), p(10, 3)),
        ("history_into_screen", p(-30, 0), p(4, 40)),
    ]
}

fn selection_json(shared: &SharedTerminal) -> Value {
    let t = shared.lock();
    let mut map = serde_json::Map::new();
    for (name, a, b) in selections() {
        let mut s = Selection::start(a);
        s.update(b);
        map.insert(name.into(), Value::String(s.text(&t)));
    }
    Value::Object(map)
}

#[test]
fn exports_match_the_golden_files() {
    let scratch = Scratch::new();
    for (name, bytes) in streams() {
        let shared = new_terminal(&bytes);
        walk(&shared, |state, shared| {
            let txt = export_text(shared, ExportFormat::Plain);
            let ansi = export_text(shared, ExportFormat::Ansi);
            assert!(!txt.is_empty(), "{name} {state} exported nothing");
            check(&format!("{name}.{state}.txt"), &txt);
            check(&format!("{name}.{state}.ansi"), &ansi);
            for (ext, want, format) in [
                ("txt.gz", &txt, ExportFormat::PlainGz),
                ("ansi.gz", &ansi, ExportFormat::AnsiGz),
            ] {
                let path = scratch.0.join(format!("{name}.{state}.{ext}"));
                write_file(&path, format, shared).unwrap();
                assert_eq!(
                    &gunzip(&fs::read(&path).unwrap()),
                    want,
                    "{name} {state} .{ext} differs from the plain golden"
                );
            }
        });
    }
}

#[test]
fn search_and_selection_match_the_golden_files() {
    let all = streams();
    for (name, bytes) in &all {
        let shared = new_terminal(bytes);
        let mut states = serde_json::Map::new();
        walk(&shared, |state, shared| {
            let mut bar = Vec::new();
            for (label, pat) in queries() {
                let o = run_search(shared, &pat, MATCH_CAP, false);
                let visible = shared.lock().visible_matches(&pat);
                bar.push(json!({
                    "query": label,
                    "matches": outcome_json(&o),
                    "visible": visible.iter().map(|m| Value::String(format!(
                        "{}:{}-{}:{}",
                        m.start.line, m.start.col, m.end.line, m.end.col
                    ))).collect::<Vec<_>>(),
                }));
            }
            states.insert(
                state.into(),
                json!({ "find_bar": bar, "selections": selection_json(shared) }),
            );
        });
        check(
            &format!("{name}.json"),
            pretty(&Value::Object(states)).as_bytes(),
        );
    }
}

#[test]
fn find_in_all_tabs_matches_the_golden_file() {
    // Three terminals searched at once, as the app does for its tabs.
    let tabs: Vec<(&str, SharedTerminal)> = streams()
        .into_iter()
        .take(3)
        .map(|(name, bytes)| (name, new_terminal(&bytes)))
        .collect();
    let mut states = serde_json::Map::new();
    for state in ["initial", "narrow", "restored"] {
        match state {
            "narrow" => tabs
                .iter()
                .for_each(|(_, t)| t.lock().resize(size(COLS_NARROW))),
            "restored" => tabs
                .iter()
                .for_each(|(_, t)| t.lock().resize(size(COLS_WIDE))),
            _ => {}
        }
        let mut per_query = Vec::new();
        for (label, pat) in queries() {
            let results: Vec<Value> = tabs
                .iter()
                .map(|(name, t)| {
                    let o = run_search(t, &pat, ALL_TABS_LIMIT, true);
                    json!({ "tab": name, "outcome": outcome_json(&o) })
                })
                .collect();
            per_query.push(json!({ "query": label, "tabs": results }));
        }
        states.insert(state.into(), Value::Array(per_query));
    }
    check(
        "find_all_tabs.json",
        pretty(&Value::Object(states)).as_bytes(),
    );
}

fn pretty(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap();
    s.push('\n');
    s
}
