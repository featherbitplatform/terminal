//! Wrapper around the `alacritty_terminal` emulator grid.

use std::sync::Arc;

use alacritty_terminal::event::WindowSize;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, Rgb as VteRgb};
use fbterm_config::Rgb;
use parking_lot::Mutex;

use crate::Selection;
use alacritty_terminal::index::Side;
use alacritty_terminal::selection::{Selection as EmuSelection, SelectionType};

pub use alacritty_terminal::term::TermMode;

/// Terminal size in cells plus the pixel size of one cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: f32,
    pub cell_height: f32,
}

/// Events the terminal raises towards the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermEvent {
    Title(String),
    Bell,
    /// Replies (device attributes, status reports, ...) that must be written back to the session.
    PtyWrite(Vec<u8>),
    Exit,
}

struct Shared {
    events: Vec<TermEvent>,
    size: GridSize,
    foreground: Rgb,
    background: Rgb,
    cursor: Rgb,
    ansi: [Rgb; 16],
}

impl Shared {
    fn color(&self, index: usize) -> Rgb {
        match index {
            0..=15 => self.ansi[index],
            16..=231 => {
                let i = (index - 16) as u8;
                let lv = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Rgb {
                    r: lv(i / 36),
                    g: lv(i / 6 % 6),
                    b: lv(i % 6),
                }
            }
            232..=255 => {
                let v = 8 + (index - 232) as u8 * 10;
                Rgb { r: v, g: v, b: v }
            }
            257 => self.background,
            258 => self.cursor,
            _ => self.foreground,
        }
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

const XTERM_ANSI: [Rgb; 16] = [
    rgb(0, 0, 0),
    rgb(205, 0, 0),
    rgb(0, 205, 0),
    rgb(205, 205, 0),
    rgb(0, 0, 238),
    rgb(205, 0, 205),
    rgb(0, 205, 205),
    rgb(229, 229, 229),
    rgb(127, 127, 127),
    rgb(255, 0, 0),
    rgb(0, 255, 0),
    rgb(255, 255, 0),
    rgb(92, 92, 255),
    rgb(255, 0, 255),
    rgb(0, 255, 255),
    rgb(255, 255, 255),
];

/// Collects emulator events into a shared queue and answers emulator queries.
#[derive(Clone)]
pub struct EventProxy {
    shared: Arc<Mutex<Shared>>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        let mut sh = self.shared.lock();
        let mapped = match event {
            Event::Title(t) => TermEvent::Title(t),
            Event::Bell => TermEvent::Bell,
            Event::PtyWrite(s) => TermEvent::PtyWrite(s.into_bytes()),
            // Programs never write the clipboard: OSC 52 stores are dropped.
            Event::ClipboardStore(..) => return,
            Event::Exit | Event::ChildExit(_) => TermEvent::Exit,
            Event::TextAreaSizeRequest(fmt) => {
                let s = sh.size;
                TermEvent::PtyWrite(
                    fmt(WindowSize {
                        num_lines: s.rows,
                        num_cols: s.cols,
                        cell_width: s.cell_width as u16,
                        cell_height: s.cell_height as u16,
                    })
                    .into_bytes(),
                )
            }
            Event::ColorRequest(index, fmt) => {
                let c = sh.color(index);
                TermEvent::PtyWrite(
                    fmt(VteRgb {
                        r: c.r,
                        g: c.g,
                        b: c.b,
                    })
                    .into_bytes(),
                )
            }
            // Never read the clipboard on behalf of the program: reply with an empty payload.
            Event::ClipboardLoad(_, fmt) => TermEvent::PtyWrite(fmt("").into_bytes()),
            _ => return,
        };
        sh.events.push(mapped);
    }
}

struct Dims {
    cols: usize,
    rows: usize,
}

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

fn dims(size: GridSize) -> Dims {
    Dims {
        cols: usize::from(size.cols.max(1)),
        rows: usize::from(size.rows.max(1)),
    }
}

/// An emulated terminal: grid, parser and pending events.
pub struct Terminal {
    term: Term<EventProxy>,
    config: Config,
    parser: Processor,
    shared: Arc<Mutex<Shared>>,
    generation: u64,
}

pub type SharedTerminal = Arc<Mutex<Terminal>>;

impl Terminal {
    pub fn new(size: GridSize, scrollback: usize) -> Self {
        let shared = Arc::new(Mutex::new(Shared {
            events: Vec::new(),
            size,
            foreground: rgb(229, 229, 229),
            background: rgb(0, 0, 0),
            cursor: rgb(229, 229, 229),
            ansi: XTERM_ANSI,
        }));
        let proxy = EventProxy {
            shared: shared.clone(),
        };
        let config = Config {
            scrolling_history: scrollback,
            osc52: alacritty_terminal::term::Osc52::OnlyPaste,
            ..Config::default()
        };
        let term = Term::new(config.clone(), &dims(size), proxy);
        Self {
            term,
            config,
            parser: Processor::new(),
            shared,
            generation: 0,
        }
    }

    /// Feed bytes received from the session into the emulator.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
        self.generation += 1;
    }

    /// Accept the kitty keyboard protocol from programs (off by default). While off, programs'
    /// requests to push, pop, set or query enhancement flags are ignored, so keys keep their
    /// legacy encoding. Switching it either way drops any flags a program has set.
    pub fn set_kitty_keyboard(&mut self, enabled: bool) {
        if self.config.kitty_keyboard == enabled {
            return;
        }
        self.config.kitty_keyboard = enabled;
        let before = self.shared.lock().events.len();
        self.term.set_options(self.config.clone());
        // Setting options re-announces the current title; nothing changed, so drop that one
        // event and keep any title a program set before.
        {
            let mut sh = self.shared.lock();
            if let Some(i) =
                (before..sh.events.len()).find(|&i| matches!(sh.events[i], TermEvent::Title(_)))
            {
                sh.events.remove(i);
            }
        }
        self.generation += 1;
    }

    pub fn kitty_keyboard(&self) -> bool {
        self.config.kitty_keyboard
    }

    /// Counter bumped by every change that can alter what is drawn.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The current text selection, in grid coordinates.
    ///
    /// Stored as the emulator's own selection, which moves with its text when output scrolls and
    /// is dropped when that text is erased or scrolls out of history.
    pub fn selection(&self) -> Option<Selection> {
        let range = self.term.selection.as_ref()?.to_range(&self.term)?;
        let mut sel = Selection::start(range.start);
        sel.update(range.end);
        Some(sel)
    }

    pub fn set_selection(&mut self, selection: Option<Selection>) {
        self.term.selection = selection.map(|s| {
            let (start, end) = s.bounds();
            let mut inner = EmuSelection::new(SelectionType::Simple, start, Side::Left);
            inner.update(end, Side::Right);
            inner
        });
        self.generation += 1;
    }

    /// Show the oldest line of the scrollback.
    pub fn scroll_to_top(&mut self) {
        self.term.scroll_display(Scroll::Top);
        self.generation += 1;
    }

    /// Show the newest output (leave scrollback).
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
        self.generation += 1;
    }

    pub fn resize(&mut self, size: GridSize) {
        self.shared.lock().size = size;
        self.term.resize(dims(size));
        self.generation += 1;
    }

    /// Set the palette used to answer colour queries.
    pub fn set_colors(&mut self, foreground: Rgb, background: Rgb, cursor: Rgb, ansi: [Rgb; 16]) {
        let mut sh = self.shared.lock();
        sh.foreground = foreground;
        sh.background = background;
        sh.cursor = cursor;
        sh.ansi = ansi;
        drop(sh);
        self.generation += 1;
    }

    pub fn size(&self) -> GridSize {
        self.shared.lock().size
    }

    /// Scroll the viewport; positive moves towards older history.
    pub fn scroll(&mut self, delta_lines: i32) {
        self.term.scroll_display(Scroll::Delta(delta_lines));
        self.generation += 1;
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    pub fn take_events(&mut self) -> Vec<TermEvent> {
        std::mem::take(&mut self.shared.lock().events)
    }

    pub fn term(&self) -> &Term<EventProxy> {
        &self.term
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::term::cell::Flags;
    use alacritty_terminal::vte::ansi::{Color, Rgb};

    fn size(cols: u16, rows: u16) -> GridSize {
        GridSize {
            cols,
            rows,
            cell_width: 8.0,
            cell_height: 16.0,
        }
    }

    #[test]
    fn feed_plain_text_lands_in_grid() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"hi");
        let g = t.term().grid();
        assert_eq!(g[Line(0)][Column(0)].c, 'h');
        assert_eq!(g[Line(0)][Column(1)].c, 'i');
    }

    #[test]
    fn sgr_truecolor_sets_cell_fg() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"\x1b[38;2;10;20;30mX");
        let c = &t.term().grid()[Line(0)][Column(0)];
        assert_eq!(
            c.fg,
            Color::Spec(Rgb {
                r: 10,
                g: 20,
                b: 30
            })
        );
    }

    #[test]
    fn wide_char_occupies_two_cells() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed("界".as_bytes());
        let g = t.term().grid();
        assert!(g[Line(0)][Column(0)].flags.contains(Flags::WIDE_CHAR));
        assert!(
            g[Line(0)][Column(1)]
                .flags
                .contains(Flags::WIDE_CHAR_SPACER)
        );
    }

    #[test]
    fn resize_reflows_and_keeps_scrollback() {
        let mut t = Terminal::new(size(80, 24), 1000);
        for i in 0..100 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        t.resize(size(40, 24));
        assert!(t.term().grid().history_size() > 0);
        assert_eq!(t.term().grid().columns(), 40);
    }

    #[test]
    fn scrollback_is_capped() {
        let mut t = Terminal::new(size(80, 24), 100);
        for i in 0..1000 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        assert!(t.term().grid().history_size() <= 100);
        assert!(t.term().grid().history_size() > 0);
    }

    #[test]
    fn device_attributes_query_produces_ptywrite() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"\x1b[c");
        let ev = t.take_events();
        assert!(
            ev.iter()
                .any(|e| matches!(e, TermEvent::PtyWrite(b) if !b.is_empty()))
        );
        assert!(t.take_events().is_empty());
    }

    fn reply(t: &mut Terminal) -> String {
        let b: Vec<u8> = t
            .take_events()
            .into_iter()
            .filter_map(|e| {
                if let TermEvent::PtyWrite(b) = e {
                    Some(b)
                } else {
                    None
                }
            })
            .flatten()
            .collect();
        String::from_utf8(b).unwrap()
    }

    #[test]
    fn osc11_background_query_gets_reply() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"\x1b]11;?\x07");
        assert!(reply(&mut t).contains("rgb:"));
    }

    #[test]
    fn csi_14t_gets_pixel_size_reply() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"\x1b[14t");
        assert_eq!(reply(&mut t), "\x1b[4;384;640t");
    }

    #[test]
    fn osc52_read_gets_empty_reply() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"\x1b]52;c;?\x07");
        assert_eq!(reply(&mut t), "\x1b]52;c;\x07");
    }

    #[test]
    fn osc52_write_is_ignored() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"\x1b]52;c;aGVsbG8=\x07");
        assert!(t.take_events().is_empty());
    }

    fn select_word_and_scroll(prefill: usize) -> (String, String) {
        use alacritty_terminal::index::Point;
        let mut t = Terminal::new(size(20, 5), 100);
        for i in 0..prefill {
            t.feed(format!("old {i}\r\n").as_bytes());
        }
        t.feed(b"\x1b[2J\x1b[Hhello world\r\n");
        let mut sel = Selection::start(Point::new(Line(0), Column(6)));
        sel.update(Point::new(Line(0), Column(10)));
        t.set_selection(Some(sel));
        let before = t.selection().unwrap().text(&t);
        for i in 0..8 {
            t.feed(format!("new {i}\r\n").as_bytes());
        }
        let after = t.selection().map(|s| s.text(&t)).unwrap_or_default();
        (before, after)
    }

    #[test]
    fn selection_follows_text_when_output_scrolls() {
        assert_eq!(select_word_and_scroll(0), ("world".into(), "world".into()));
        // Full history: lines drop off the top while history size stays constant.
        assert_eq!(
            select_word_and_scroll(500),
            ("world".into(), "world".into())
        );
    }

    #[test]
    fn generation_tracks_changes() {
        let mut t = Terminal::new(size(80, 24), 100);
        let g0 = t.generation();
        t.feed(b"x");
        let g1 = t.generation();
        assert!(g1 > g0);
        t.set_selection(None);
        assert!(t.generation() > g1);
    }

    #[test]
    fn scroll_to_bottom_leaves_history() {
        let mut t = Terminal::new(size(80, 24), 1000);
        for i in 0..100 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        t.scroll(10);
        assert_eq!(t.term().grid().display_offset(), 10);
        t.scroll_to_bottom();
        assert_eq!(t.term().grid().display_offset(), 0);
    }

    fn esc(t: &Terminal) -> Vec<u8> {
        use iced::keyboard::key::{Code, Named, Physical};
        use iced::keyboard::{Key, Modifiers};
        crate::encode_key(
            &Key::Named(Named::Escape),
            Physical::Code(Code::Escape),
            None,
            Modifiers::empty(),
            t.mode(),
        )
        .unwrap()
    }

    #[test]
    fn kitty_requests_ignored_when_setting_off() {
        let mut t = Terminal::new(size(80, 24), 100);
        assert!(!t.kitty_keyboard());
        t.feed(b"\x1b[>1u\x1b[=8;1u\x1b[?u");
        assert!(!t.mode().intersects(TermMode::KITTY_KEYBOARD_PROTOCOL));
        assert!(reply(&mut t).is_empty(), "no answer to the flags query");
        assert_eq!(esc(&t), b"\x1b");
    }

    #[test]
    fn kitty_push_query_set_pop_when_setting_on() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.set_kitty_keyboard(true);
        t.feed(b"\x1b[>1u");
        assert!(t.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
        assert_eq!(esc(&t), b"\x1b[27u");
        t.feed(b"\x1b[?u");
        assert_eq!(reply(&mut t), "\x1b[?1u");
        t.feed(b"\x1b[=10;2u");
        assert!(t.mode().contains(
            TermMode::DISAMBIGUATE_ESC_CODES
                | TermMode::REPORT_EVENT_TYPES
                | TermMode::REPORT_ALL_KEYS_AS_ESC
        ));
        t.feed(b"\x1b[<u");
        assert!(!t.mode().intersects(TermMode::KITTY_KEYBOARD_PROTOCOL));
        assert_eq!(esc(&t), b"\x1b");
    }

    #[test]
    fn turning_kitty_off_drops_active_flags() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"\x1b]0;keep\x07");
        t.take_events();
        t.feed(b"\x1b]0;pending\x07");
        t.set_kitty_keyboard(true);
        assert_eq!(
            t.take_events(),
            vec![TermEvent::Title("pending".into())],
            "a title set before the toggle is kept, the re-announcement dropped"
        );
        t.feed(b"\x1b[>31u");
        assert!(t.mode().contains(TermMode::KITTY_KEYBOARD_PROTOCOL));
        t.set_kitty_keyboard(false);
        assert!(!t.mode().intersects(TermMode::KITTY_KEYBOARD_PROTOCOL));
        assert!(t.take_events().is_empty(), "no spurious title event");
        assert_eq!(esc(&t), b"\x1b");
    }

    /// Depth of the flag stack, measured by popping: pops `n` and reports whether any
    /// flags remain. Leaves the stack shorter.
    fn flags_after_popping(t: &mut Terminal, n: u16) -> bool {
        t.feed(format!("\x1b[<{n}u").as_bytes());
        t.mode().intersects(TermMode::KITTY_KEYBOARD_PROTOCOL)
    }

    fn pushes(n: usize) -> Vec<u8> {
        b"\x1b[>1u".repeat(n)
    }

    /// Regression test for the vendored alacritty_terminal fix (FBTERM_PATCHES.md): a full
    /// flag stack evicts its oldest entry instead of panicking.
    #[test]
    fn flag_stack_floods_do_not_crash_and_stay_capped() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.set_kitty_keyboard(true);
        let flood = pushes(5000);
        t.feed(&flood);
        assert!(t.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
        // Capped at 4096 entries: popping 4095 leaves one, the next pop empties it.
        assert!(flags_after_popping(&mut t, 4095));
        assert!(!flags_after_popping(&mut t, 1));
        // One byte at a time.
        for chunk in flood.chunks(1) {
            t.feed(chunk);
        }
        assert!(flags_after_popping(&mut t, 4095));
        assert!(!flags_after_popping(&mut t, 1));
    }

    #[test]
    fn screen_swaps_cannot_grow_the_flag_stack_past_its_cap() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.set_kitty_keyboard(true);
        // `CSI ? 1049 ; 0 ; 0 ... h` with 31 extra parameters still swaps screens.
        let extra = ";0".repeat(31);
        let enter = format!("\x1b[?1049{extra}h");
        let leave = format!("\x1b[?1049{extra}l");
        let burst = pushes(128);
        for _ in 0..40 {
            t.feed(&burst);
            t.feed(enter.as_bytes());
            t.feed(b"\x1b[<128u");
            t.feed(leave.as_bytes());
        }
        assert!(!t.mode().contains(TermMode::ALT_SCREEN));
        // Main screen holds 40 * 128 = 5120 pushes, capped at 4096.
        assert!(flags_after_popping(&mut t, 4095));
        assert!(!flags_after_popping(&mut t, 1));
    }

    #[test]
    fn title_and_bell_events() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"\x1b]0;hello\x07\x07");
        let ev = t.take_events();
        assert!(ev.contains(&TermEvent::Title("hello".into())));
        assert!(ev.contains(&TermEvent::Bell));
    }
}
