//! Rendering spike (Task 2): can iced draw a busy 200x60 terminal grid at display refresh rate?
//!
//! Throwaway code. Approach A ("native"): a custom `iced::advanced::Widget` that draws the
//! `alacritty_terminal` grid with iced's own primitives: one `fill_quad` per background run and
//! one `fill_text` per foreground run (consecutive cells sharing color and style).
//!
//! `--mode widget` renders through the production `fbterm_term::terminal_view` widget instead,
//! fed by `fbterm_term::Terminal` (Task 4 check).
//!
//! `--mode null` is a control: identical app, PTY and frame loop, but no cells are drawn, to
//! separate grid rendering cost from the environment (compositor, display, other GPU load).
//!
//! Usage:
//!   cargo run --release -p fbterm-term --example spike -- --mode native --workload yes --seconds 30
//!
//! Workloads (the child process runs in a PTY via portable-pty):
//!   yes     `yes` printing 190 full-block characters per line (scrolling output)
//!   tui     a python3 curses loop redrawing the full 200x60 screen with 256 colors
//!   colors  python3 writing every cell with its own truecolor fg/bg (worst case, extra)
//!   idle    a child that fills the screen once and then prints nothing, for idle CPU
//!
//! Every second it prints frames rendered, worst frame interval and widget draw time; after
//! `--warmup` + `--seconds` it prints a SUMMARY line and exits.

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Processor, Rgb};
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad, Renderer as _};
use iced::advanced::text::{self, Paragraph as _, Renderer as _};
use iced::advanced::widget::{self, Widget};
use iced::futures::StreamExt;
use iced::futures::channel::mpsc;
use iced::futures::stream::BoxStream;
use iced::{
    Color, Element, Font, Length, Pixels, Point, Rectangle, Size, Subscription, Task, mouse,
};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const COLS: usize = 200;
const ROWS: usize = 60;
const FONT_SIZE: f32 = 12.0;
const DEFAULT_FG: [u8; 3] = [0xd0, 0xd0, 0xd0];
const DEFAULT_BG: [u8; 3] = [0x10, 0x10, 0x14];

type SharedTerm = Arc<FairMutex<Term<VoidListener>>>;

static TERM: OnceLock<SharedTerm> = OnceLock::new();
static WIDGET_TERM: OnceLock<fbterm_term::SharedTerminal> = OnceLock::new();
static WIDGET_STYLE: OnceLock<fbterm_term::TerminalStyle> = OnceLock::new();
static GENERATION: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static WAKE_PENDING: AtomicBool = AtomicBool::new(false);
static WAKE_RX: OnceLock<Mutex<Option<mpsc::UnboundedReceiver<()>>>> = OnceLock::new();
static CHILD: OnceLock<Mutex<Box<dyn portable_pty::Child + Send + Sync>>> = OnceLock::new();
// Widget draw() CPU time, accumulated by draw and drained by the stats tick.
static DRAW_NS_SUM: AtomicU64 = AtomicU64::new(0);
static DRAW_NS_MAX: AtomicU64 = AtomicU64::new(0);
static DRAW_COUNT: AtomicU64 = AtomicU64::new(0);
static PRIMS_TEXT: AtomicU64 = AtomicU64::new(0);
static PRIMS_QUAD: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct Args {
    mode: String,
    workload: String,
    seconds: u64,
    warmup: u64,
}

fn parse_args() -> Args {
    let mut args = Args {
        mode: "native".into(),
        workload: "yes".into(),
        seconds: 30,
        warmup: 2,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == "--idle" {
            args.workload = "idle".into();
            continue;
        }
        let v = it.next().unwrap_or_default();
        match a.as_str() {
            "--mode" => args.mode = v,
            "--workload" => args.workload = v,
            "--seconds" => args.seconds = v.parse().expect("--seconds N"),
            "--warmup" => args.warmup = v.parse().expect("--warmup N"),
            _ => panic!("unknown argument {a}"),
        }
    }
    // "null" is a control: same app, PTY and frame loop, but the widget draws no cells.
    if !matches!(args.mode.as_str(), "native" | "null" | "widget") {
        panic!("--mode native (approach A), widget (production widget) or null (control)");
    }
    args
}

const TUI_PY: &str = r#"
import curses, time, random
def main(s):
    curses.start_color(); curses.use_default_colors(); curses.curs_set(0)
    n = min(curses.COLORS, 256) - 1
    for i in range(1, min(curses.COLOR_PAIRS - 1, 255)):
        curses.init_pair(i, (i * 7) % n + 1, (i * 13) % n + 1)
    h, w = s.getmaxyx(); t = 0
    while True:
        t += 1
        for y in range(h - 1):
            load = (y * 37 + t * 3) % (w - 40)
            pid = 1000 + y * 17 + t % 97
            line = f"{pid:6d} user{y % 7}  {random.random() * 100:6.2f}% {t % 10000:6d} "
            bar = "|" * load
            s.addstr(y, 0, line, curses.color_pair((y + t) % 254 + 1))
            s.addstr(y, len(line), bar[: w - len(line) - 1], curses.color_pair((y * 3 + t) % 254 + 1) | curses.A_BOLD)
            s.clrtoeol()
        s.addstr(h - 1, 0, f"frame {t} " + time.strftime("%H:%M:%S"), curses.A_REVERSE)
        s.refresh()
        time.sleep(0.004)
curses.wrapper(main)
"#;

const COLORS_PY: &str = r#"
import os, random, sys
out = sys.stdout.buffer
glyphs = "abcdefghijklmnopqrstuvwxyz0123456789#@%&*+=<>"
t = 0
while True:
    t += 1
    parts = ["\x1b[H"]
    for y in range(60):
        for x in range(200):
            r = (x * 5 + t * 3) & 255; g = (y * 9 + t) & 255; b = (x + y + t * 7) & 255
            parts.append(f"\x1b[38;2;{255-r};{255-g};{255-b};48;2;{r};{g};{b}m{glyphs[(x + y + t) % len(glyphs)]}")
        if y < 59:
            parts.append("\r\n")
    parts.append("\x1b[0m")
    out.write("".join(parts).encode()); out.flush()
"#;

fn spawn_child(workload: &str) -> Box<dyn Read + Send> {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: ROWS as u16,
            cols: COLS as u16,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut cmd = match workload {
        "yes" => {
            let mut c = CommandBuilder::new("/bin/bash");
            c.args(["-c", r#"yes "$(printf '%.0s█' {1..190})""#]);
            c
        }
        "tui" | "colors" => {
            let path = std::env::temp_dir().join(format!("fbterm-spike-{workload}.py"));
            std::fs::write(&path, if workload == "tui" { TUI_PY } else { COLORS_PY }).unwrap();
            let mut c = CommandBuilder::new("/usr/bin/python3");
            c.arg(path);
            c
        }
        "idle" => {
            // Fill the screen once (so there is a real grid on screen), then print nothing.
            let mut c = CommandBuilder::new("/bin/bash");
            c.args(["-c", r#"for i in $(seq 1 59); do printf '\e[38;2;%d;180;120m%04d idle line, no further output follows\e[0m\n' $((i*4)) $i; done; exec sleep 100000"#]);
            c
        }
        other => panic!("unknown workload {other}"),
    };
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("LANG", "en_US.UTF-8");
    cmd.env("LC_ALL", "en_US.UTF-8");
    let child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);
    let _ = CHILD.set(Mutex::new(child));
    let reader = pair.master.try_clone_reader().expect("reader");
    // Keep the master alive for the life of the process.
    std::mem::forget(pair.master);
    reader
}

fn start_widget_io(workload: &str) {
    let scheme = fbterm_term::ColorScheme::graphite();
    let mut t = fbterm_term::Terminal::new(
        fbterm_term::GridSize {
            cols: COLS as u16,
            rows: ROWS as u16,
            cell_width: 7.0,
            cell_height: 15.0,
        },
        10_000,
    );
    scheme.apply_to(&mut t);
    let term = Arc::new(parking_lot::Mutex::new(t));
    WIDGET_TERM.set(term.clone()).ok();
    WIDGET_STYLE
        .set(fbterm_term::TerminalStyle {
            scheme,
            font: Font::MONOSPACE,
            font_size: FONT_SIZE,
            accent: None,
            focused: true,
        })
        .ok();
    let (tx, rx) = mpsc::unbounded();
    WAKE_RX.set(Mutex::new(Some(rx))).ok();
    let mut reader = spawn_child(workload);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 65536];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            {
                let mut t = term.lock();
                t.feed(&buf[..n]);
                // Replies to queries are not written back in the spike; drop them.
                t.take_events();
            }
            BYTES.fetch_add(n as u64, Ordering::Relaxed);
            GENERATION.fetch_add(1, Ordering::Release);
            if !WAKE_PENDING.swap(true, Ordering::AcqRel) {
                let _ = tx.unbounded_send(());
            }
        }
    });
}

fn start_io(workload: &str) {
    let term = Arc::new(FairMutex::new(Term::new(
        Config::default(),
        &TermSize::new(COLS, ROWS),
        VoidListener,
    )));
    TERM.set(term.clone()).ok();
    let (tx, rx) = mpsc::unbounded();
    WAKE_RX.set(Mutex::new(Some(rx))).ok();
    let mut reader = spawn_child(workload);
    std::thread::spawn(move || {
        let mut parser: Processor = Processor::new();
        let mut buf = vec![0u8; 65536];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            {
                let mut t = term.lock();
                parser.advance(&mut *t, &buf[..n]);
            }
            BYTES.fetch_add(n as u64, Ordering::Relaxed);
            GENERATION.fetch_add(1, Ordering::Release);
            if !WAKE_PENDING.swap(true, Ordering::AcqRel) {
                let _ = tx.unbounded_send(());
            }
        }
    });
}

fn wake_stream() -> BoxStream<'static, Message> {
    match WAKE_RX.get().and_then(|m| m.lock().unwrap().take()) {
        Some(rx) => rx.map(|()| Message::Wakeup).boxed(),
        None => iced::futures::stream::empty().boxed(),
    }
}

// ---------- color resolution ----------

fn palette(idx: usize) -> [u8; 3] {
    const BASE: [[u8; 3]; 16] = [
        [0x00, 0x00, 0x00],
        [0xcd, 0x31, 0x31],
        [0x0d, 0xbc, 0x79],
        [0xe5, 0xe5, 0x10],
        [0x24, 0x72, 0xc8],
        [0xbc, 0x3f, 0xbc],
        [0x11, 0xa8, 0xcd],
        [0xe5, 0xe5, 0xe5],
        [0x66, 0x66, 0x66],
        [0xf1, 0x4c, 0x4c],
        [0x23, 0xd1, 0x8b],
        [0xf5, 0xf5, 0x43],
        [0x3b, 0x8e, 0xea],
        [0xd6, 0x70, 0xd6],
        [0x29, 0xb8, 0xdb],
        [0xff, 0xff, 0xff],
    ];
    match idx {
        0..=15 => BASE[idx],
        16..=231 => {
            let i = idx - 16;
            let s = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
            [s(i / 36), s((i / 6) % 6), s(i % 6)]
        }
        232..=255 => {
            let v = (8 + (idx - 232) * 10) as u8;
            [v, v, v]
        }
        _ => DEFAULT_FG,
    }
}

fn resolve(c: AnsiColor, colors: &alacritty_terminal::term::color::Colors, fg: bool) -> [u8; 3] {
    let from_rgb = |r: Rgb| [r.r, r.g, r.b];
    match c {
        AnsiColor::Spec(rgb) => from_rgb(rgb),
        AnsiColor::Indexed(i) => colors[i as usize]
            .map(from_rgb)
            .unwrap_or_else(|| palette(i as usize)),
        AnsiColor::Named(n) => {
            if let Some(rgb) = colors[n] {
                return from_rgb(rgb);
            }
            match n {
                NamedColor::Foreground
                | NamedColor::BrightForeground
                | NamedColor::DimForeground => DEFAULT_FG,
                NamedColor::Background => DEFAULT_BG,
                n if (n as usize) < 16 => palette(n as usize),
                _ => {
                    if fg {
                        DEFAULT_FG
                    } else {
                        DEFAULT_BG
                    }
                }
            }
        }
    }
}

// ---------- grid snapshot ----------

#[derive(Clone, Copy, PartialEq)]
struct SCell {
    c: char,
    fg: [u8; 3],
    bg: [u8; 3],
    flags: Flags,
}

#[derive(Default)]
struct Snapshot {
    cells: Vec<SCell>,
}

fn take_snapshot(snap: &mut Snapshot) {
    let term = TERM.get().unwrap().lock();
    let grid = term.grid();
    let colors = term.colors();
    snap.cells.clear();
    for y in 0..ROWS {
        let row = &grid[Line(y as i32)];
        for x in 0..COLS {
            let cell = &row[Column(x)];
            let mut fg = resolve(cell.fg, colors, true);
            let mut bg = resolve(cell.bg, colors, false);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            snap.cells.push(SCell {
                c: cell.c,
                fg,
                bg,
                flags: cell.flags,
            });
        }
    }
}

// ---------- approach A widget ----------

struct GridWidget<'a> {
    snap: &'a Snapshot,
    null: bool,
}

#[derive(Default)]
struct GridState {
    cell: Option<Size>,
}

fn rgb(c: [u8; 3]) -> Color {
    Color::from_rgb8(c[0], c[1], c[2])
}

fn style_font(flags: Flags) -> Font {
    let mut f = Font::MONOSPACE;
    if flags.contains(Flags::BOLD) {
        f.weight = iced::font::Weight::Bold;
    }
    if flags.contains(Flags::ITALIC) {
        f.style = iced::font::Style::Italic;
    }
    f
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for GridWidget<'_> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<GridState>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(GridState::default())
    }

    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<GridState>();
        if state.cell.is_none() {
            let p = <iced::Renderer as text::Renderer>::Paragraph::with_text(text::Text {
                content: "MMMMMMMMMMMMMMMMMMMM",
                bounds: Size::INFINITE,
                size: Pixels(FONT_SIZE),
                line_height: text::LineHeight::Relative(1.2),
                font: Font::MONOSPACE,
                align_x: text::Alignment::Left,
                align_y: iced::alignment::Vertical::Top,
                shaping: text::Shaping::Advanced,
                wrapping: text::Wrapping::None,
            });
            let b = p.min_bounds();
            state.cell = Some(Size::new(b.width / 20.0, (FONT_SIZE * 1.2).ceil()));
        }
        layout::Node::new(limits.max())
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut iced::Renderer,
        _theme: &iced::Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let t0 = Instant::now();
        let bounds = layout.bounds();
        let Some(cell) = tree.state.downcast_ref::<GridState>().cell else {
            return;
        };
        renderer.fill_quad(
            Quad {
                bounds,
                ..Quad::default()
            },
            rgb(DEFAULT_BG),
        );
        if self.null || self.snap.cells.len() != COLS * ROWS {
            return;
        }
        let mut s = String::with_capacity(COLS * 4);
        let (mut n_text, mut n_quad) = (0u64, 0u64);
        for y in 0..ROWS {
            let row = &self.snap.cells[y * COLS..(y + 1) * COLS];
            let top = bounds.y + y as f32 * cell.height;
            // Background runs.
            let mut x = 0;
            while x < COLS {
                let bg = row[x].bg;
                let start = x;
                while x < COLS && row[x].bg == bg {
                    x += 1;
                }
                if bg != DEFAULT_BG {
                    n_quad += 1;
                    renderer.fill_quad(
                        Quad {
                            bounds: Rectangle {
                                x: bounds.x + start as f32 * cell.width,
                                y: top,
                                width: (x - start) as f32 * cell.width,
                                height: cell.height,
                            },
                            snap: true,
                            ..Quad::default()
                        },
                        rgb(bg),
                    );
                }
            }
            // Foreground runs: consecutive cells with the same fg color and font style.
            let style_mask = Flags::BOLD | Flags::ITALIC;
            let mut x = 0;
            while x < COLS {
                let fg = row[x].fg;
                let style = row[x].flags & style_mask;
                let start = x;
                s.clear();
                let mut visible = false;
                while x < COLS && row[x].fg == fg && (row[x].flags & style_mask) == style {
                    let c = &row[x];
                    if !c.flags.contains(Flags::WIDE_CHAR_SPACER) {
                        s.push(c.c);
                        visible |= c.c != ' ';
                    }
                    x += 1;
                }
                if visible {
                    n_text += 1;
                    let width = (x - start + 1) as f32 * cell.width;
                    renderer.fill_text(
                        text::Text {
                            content: s.clone(),
                            bounds: Size::new(width, cell.height),
                            size: Pixels(FONT_SIZE),
                            line_height: text::LineHeight::Absolute(Pixels(cell.height)),
                            font: style_font(style),
                            align_x: text::Alignment::Left,
                            align_y: iced::alignment::Vertical::Top,
                            shaping: text::Shaping::Advanced,
                            wrapping: text::Wrapping::None,
                        },
                        Point::new(bounds.x + start as f32 * cell.width, top),
                        rgb(fg),
                        bounds,
                    );
                }
                // Underline as a thin quad.
                if row[start].flags.contains(Flags::UNDERLINE) {
                    renderer.fill_quad(
                        Quad {
                            bounds: Rectangle {
                                x: bounds.x + start as f32 * cell.width,
                                y: top + cell.height - 1.0,
                                width: (x - start) as f32 * cell.width,
                                height: 1.0,
                            },
                            ..Quad::default()
                        },
                        rgb(fg),
                    );
                }
            }
        }
        let ns = t0.elapsed().as_nanos() as u64;
        DRAW_NS_SUM.fetch_add(ns, Ordering::Relaxed);
        DRAW_NS_MAX.fetch_max(ns, Ordering::Relaxed);
        DRAW_COUNT.fetch_add(1, Ordering::Relaxed);
        PRIMS_TEXT.store(n_text, Ordering::Relaxed);
        PRIMS_QUAD.store(n_quad, Ordering::Relaxed);
    }
}

// ---------- timing wrapper for --mode widget ----------

/// Delegates to the production widget and records its draw() CPU time like GridWidget does.
struct Timed<'a> {
    inner: Element<'a, Message>,
}

impl Widget<Message, iced::Theme, iced::Renderer> for Timed<'_> {
    fn size(&self) -> Size<Length> {
        self.inner.as_widget().size()
    }
    fn tag(&self) -> widget::tree::Tag {
        self.inner.as_widget().tag()
    }
    fn state(&self) -> widget::tree::State {
        self.inner.as_widget().state()
    }
    fn children(&self) -> Vec<widget::Tree> {
        self.inner.as_widget().children()
    }
    fn diff(&self, tree: &mut widget::Tree) {
        self.inner.as_widget().diff(tree)
    }
    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.inner.as_widget_mut().layout(tree, renderer, limits)
    }
    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.inner.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        )
    }
    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let t0 = Instant::now();
        self.inner
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
        let ns = t0.elapsed().as_nanos() as u64;
        DRAW_NS_SUM.fetch_add(ns, Ordering::Relaxed);
        DRAW_NS_MAX.fetch_max(ns, Ordering::Relaxed);
        DRAW_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

// ---------- app ----------

#[derive(Debug, Clone)]
enum Message {
    Wakeup,
    Term,
    Frame(Instant),
    Second,
}

struct Stats {
    // current second
    frames: u64,
    worst: Duration,
    // whole measurement window
    total_frames: u64,
    total_worst: Duration,
    intervals: Vec<f64>,
    per_second_fps: Vec<u64>,
    draw_max_ns: u64,
    draw_sum_ns: u64,
    draw_count: u64,
    bytes: u64,
}

struct App {
    args: Args,
    start: Instant,
    snap: Snapshot,
    seen_generation: u64,
    active: bool,
    idle_frames: u32,
    last_frame: Option<Instant>,
    elapsed_s: u64,
    stats: Stats,
}

impl App {
    fn measuring(&self) -> bool {
        self.elapsed_s >= self.args.warmup
    }
}

fn boot(args: Args) -> (App, Task<Message>) {
    if args.mode == "widget" {
        start_widget_io(&args.workload);
    } else {
        start_io(&args.workload);
    }
    println!(
        "spike pid={} mode={} workload={} grid={}x{} warmup={}s seconds={}s",
        std::process::id(),
        args.mode,
        args.workload,
        COLS,
        ROWS,
        args.warmup,
        args.seconds
    );
    (
        App {
            args,
            start: Instant::now(),
            snap: Snapshot::default(),
            seen_generation: 0,
            active: true,
            idle_frames: 0,
            last_frame: None,
            elapsed_s: 0,
            stats: Stats {
                frames: 0,
                worst: Duration::ZERO,
                total_frames: 0,
                total_worst: Duration::ZERO,
                intervals: Vec::new(),
                per_second_fps: Vec::new(),
                draw_max_ns: 0,
                draw_sum_ns: 0,
                draw_count: 0,
                bytes: 0,
            },
        },
        Task::none(),
    )
}

fn update(app: &mut App, msg: Message) -> Task<Message> {
    match msg {
        Message::Term => {}
        Message::Wakeup => {
            app.active = true;
            app.idle_frames = 0;
        }
        Message::Frame(at) => {
            if let Some(last) = app.last_frame {
                let dt = at.saturating_duration_since(last);
                app.stats.frames += 1;
                app.stats.worst = app.stats.worst.max(dt);
                if app.measuring() {
                    app.stats.total_frames += 1;
                    app.stats.total_worst = app.stats.total_worst.max(dt);
                    app.stats.intervals.push(dt.as_secs_f64() * 1000.0);
                }
            }
            app.last_frame = Some(at);
            WAKE_PENDING.store(false, Ordering::Release);
            let generation = GENERATION.load(Ordering::Acquire);
            if generation != app.seen_generation {
                app.seen_generation = generation;
                app.idle_frames = 0;
                if app.args.mode != "widget" {
                    take_snapshot(&mut app.snap);
                }
            } else {
                app.idle_frames += 1;
                if app.idle_frames > 3 {
                    // Nothing new: stop requesting frames until the next wakeup.
                    app.active = false;
                    app.last_frame = None;
                }
            }
        }
        Message::Second => {
            app.elapsed_s += 1;
            let bytes = BYTES.swap(0, Ordering::Relaxed);
            let dsum = DRAW_NS_SUM.swap(0, Ordering::Relaxed);
            let dmax = DRAW_NS_MAX.swap(0, Ordering::Relaxed);
            let dcount = DRAW_COUNT.swap(0, Ordering::Relaxed);
            let s = &mut app.stats;
            let measuring = app.elapsed_s > app.args.warmup;
            println!(
                "t={:>3}s{} fps={:>4} worst_frame={:>6.2}ms draws={:>4} draw_avg={:>5.2}ms draw_max={:>5.2}ms pty={:>7.2}MB/s text_runs={} bg_quads={}",
                app.elapsed_s,
                if measuring { " " } else { "w" },
                s.frames,
                s.worst.as_secs_f64() * 1000.0,
                dcount,
                if dcount > 0 {
                    dsum as f64 / dcount as f64 / 1e6
                } else {
                    0.0
                },
                dmax as f64 / 1e6,
                bytes as f64 / 1e6,
                PRIMS_TEXT.load(Ordering::Relaxed),
                PRIMS_QUAD.load(Ordering::Relaxed),
            );
            if measuring {
                s.per_second_fps.push(s.frames);
                s.draw_max_ns = s.draw_max_ns.max(dmax);
                s.draw_sum_ns += dsum;
                s.draw_count += dcount;
                s.bytes += bytes;
            }
            s.frames = 0;
            s.worst = Duration::ZERO;
            if app.elapsed_s >= app.args.warmup + app.args.seconds {
                let n = s.per_second_fps.len().max(1) as f64;
                let mut iv = s.intervals.clone();
                iv.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let pct = |p: f64| {
                    if iv.is_empty() {
                        0.0
                    } else {
                        iv[((iv.len() - 1) as f64 * p) as usize]
                    }
                };
                let over_25 = iv.iter().filter(|&&v| v > 25.0).count();
                println!(
                    "SUMMARY mode={} workload={} wall_since_launch={:.1}s seconds={} frames={} avg_fps={:.1} min_sec_fps={} max_sec_fps={} worst_frame={:.2}ms p50={:.2}ms p99={:.2}ms frames_over_25ms={} draw_avg={:.2}ms draw_max={:.2}ms pty_avg={:.2}MB/s",
                    app.args.mode,
                    app.args.workload,
                    app.start.elapsed().as_secs_f64(),
                    s.per_second_fps.len(),
                    s.total_frames,
                    s.total_frames as f64 / n,
                    s.per_second_fps.iter().min().copied().unwrap_or(0),
                    s.per_second_fps.iter().max().copied().unwrap_or(0),
                    s.total_worst.as_secs_f64() * 1000.0,
                    pct(0.5),
                    pct(0.99),
                    over_25,
                    if s.draw_count > 0 {
                        s.draw_sum_ns as f64 / s.draw_count as f64 / 1e6
                    } else {
                        0.0
                    },
                    s.draw_max_ns as f64 / 1e6,
                    s.bytes as f64 / 1e6 / n,
                );
                for y in 0..(app.snap.cells.len() / COLS).min(3) {
                    let row: String = app.snap.cells[y * COLS..(y + 1) * COLS]
                        .iter()
                        .map(|c| c.c)
                        .collect();
                    println!("row{y}: {}", row.trim_end());
                }
                if let Some(c) = CHILD.get() {
                    let _ = c.lock().unwrap().kill();
                }
                return iced::exit();
            }
        }
    }
    Task::none()
}

fn view(app: &App) -> Element<'_, Message> {
    if app.args.mode == "widget" {
        let term = WIDGET_TERM.get().expect("widget terminal");
        let style = WIDGET_STYLE.get().expect("widget style");
        return Element::new(Timed {
            inner: fbterm_term::terminal_view(term, style, |_| Message::Term),
        });
    }
    Element::new(GridWidget {
        snap: &app.snap,
        null: app.args.mode == "null",
    })
}

fn subscription(app: &App) -> Subscription<Message> {
    let mut subs = vec![
        Subscription::run(wake_stream),
        iced::time::every(Duration::from_secs(1)).map(|_| Message::Second),
    ];
    if app.active {
        subs.push(iced::window::frames().map(Message::Frame));
    }
    Subscription::batch(subs)
}

fn main() -> iced::Result {
    let args = parse_args();
    iced::application(move || boot(args.clone()), update, view)
        .title("fbterm rendering spike")
        .subscription(subscription)
        .window_size(Size::new(1460.0, 900.0))
        .run()
}
