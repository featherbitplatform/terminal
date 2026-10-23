//! Wrapper around the `alacritty_terminal` emulator grid.

use std::sync::Arc;

use alacritty_terminal::event::WindowSize;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, Rgb as VteRgb};
use fbterm_config::Rgb;
use parking_lot::Mutex;

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
    ClipboardStore(String),
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
            Event::ClipboardStore(_, s) => TermEvent::ClipboardStore(s),
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
    parser: Processor,
    shared: Arc<Mutex<Shared>>,
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
            osc52: alacritty_terminal::term::Osc52::CopyPaste,
            ..Config::default()
        };
        let term = Term::new(config, &dims(size), proxy);
        Self {
            term,
            parser: Processor::new(),
            shared,
        }
    }

    /// Feed bytes received from the session into the emulator.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    pub fn resize(&mut self, size: GridSize) {
        self.shared.lock().size = size;
        self.term.resize(dims(size));
    }

    /// Set the palette used to answer colour queries.
    pub fn set_colors(&mut self, foreground: Rgb, background: Rgb, cursor: Rgb, ansi: [Rgb; 16]) {
        let mut sh = self.shared.lock();
        sh.foreground = foreground;
        sh.background = background;
        sh.cursor = cursor;
        sh.ansi = ansi;
    }

    pub fn size(&self) -> GridSize {
        self.shared.lock().size
    }

    /// Scroll the viewport; positive moves towards older history.
    pub fn scroll(&mut self, delta_lines: i32) {
        self.term.scroll_display(Scroll::Delta(delta_lines));
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
    fn title_and_bell_events() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"\x1b]0;hello\x07\x07");
        let ev = t.take_events();
        assert!(ev.contains(&TermEvent::Title("hello".into())));
        assert!(ev.contains(&TermEvent::Bell));
    }
}
