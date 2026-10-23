//! Wrapper around the `alacritty_terminal` emulator grid.

use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
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

/// Collects emulator events into a shared queue.
#[derive(Clone, Default)]
pub struct EventProxy {
    events: Arc<Mutex<Vec<TermEvent>>>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        let mapped = match event {
            Event::Title(t) => TermEvent::Title(t),
            Event::Bell => TermEvent::Bell,
            Event::PtyWrite(s) => TermEvent::PtyWrite(s.into_bytes()),
            Event::ClipboardStore(_, s) => TermEvent::ClipboardStore(s),
            Event::Exit | Event::ChildExit(_) => TermEvent::Exit,
            _ => return,
        };
        self.events.lock().push(mapped);
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
    events: Arc<Mutex<Vec<TermEvent>>>,
    size: GridSize,
}

pub type SharedTerminal = Arc<Mutex<Terminal>>;

impl Terminal {
    pub fn new(size: GridSize, scrollback: usize) -> Self {
        let proxy = EventProxy::default();
        let events = proxy.events.clone();
        let config = Config {
            scrolling_history: scrollback,
            ..Config::default()
        };
        let term = Term::new(config, &dims(size), proxy);
        Self {
            term,
            parser: Processor::new(),
            events,
            size,
        }
    }

    /// Feed bytes received from the session into the emulator.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    pub fn resize(&mut self, size: GridSize) {
        self.size = size;
        let d = dims(size);
        self.term.resize(TermSize::new(d.cols, d.rows));
    }

    pub fn size(&self) -> GridSize {
        self.size
    }

    /// Scroll the viewport; positive moves towards older history.
    pub fn scroll(&mut self, delta_lines: i32) {
        self.term.scroll_display(Scroll::Delta(delta_lines));
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    pub fn take_events(&mut self) -> Vec<TermEvent> {
        std::mem::take(&mut *self.events.lock())
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

    #[test]
    fn title_and_bell_events() {
        let mut t = Terminal::new(size(80, 24), 100);
        t.feed(b"\x1b]0;hello\x07\x07");
        let ev = t.take_events();
        assert!(ev.contains(&TermEvent::Title("hello".into())));
        assert!(ev.contains(&TermEvent::Bell));
    }
}
