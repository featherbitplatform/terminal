//! Mouse and keyboard handling of the terminal widget.

use std::time::{Duration, Instant};

use alacritty_terminal::index::{Column, Point};
use alacritty_terminal::term::cell::Hyperlink;
use iced::advanced::Clipboard;
use iced::advanced::clipboard::Kind;
use iced::keyboard::{self, Key, Modifiers};
use iced::{Event, Rectangle, mouse};

use super::TerminalOutput;
use super::geometry::{pixel_to_cell, pixel_to_point};
use crate::{
    GridSize, MouseButton, MouseKind, MouseReport, Selection, SharedTerminal, TermMode, Terminal,
    encode_key, encode_mouse, encode_paste,
};

/// Lines scrolled per wheel notch.
const LINES_PER_NOTCH: f32 = 3.0;
/// Maximum interval between clicks of a double or triple click.
const MULTI_CLICK: Duration = Duration::from_millis(400);

pub struct Context<'a> {
    pub term: &'a SharedTerminal,
    pub focused: bool,
    pub bounds: Rectangle,
    pub grid: GridSize,
    pub cursor: mouse::Cursor,
}

/// What the widget should do after handling an event.
#[derive(Debug, Default, Clone, Copy)]
pub struct Effect {
    pub redraw: bool,
    pub capture: bool,
}

impl Effect {
    const NONE: Self = Self {
        redraw: false,
        capture: false,
    };
    const CAPTURE: Self = Self {
        redraw: false,
        capture: true,
    };
    const REDRAW: Self = Self {
        redraw: true,
        capture: true,
    };
}

#[derive(Default)]
pub struct InputState {
    modifiers: Modifiers,
    hover: Option<Hyperlink>,
    hover_cell: Option<Point>,
    /// Local selection drag in progress: its anchor and whether the mouse moved yet.
    drag: Option<(Point, bool)>,
    /// Button held while it is being reported to the program (mouse mode).
    reported: Option<(MouseButton, (u16, u16))>,
    last_click: Option<(Instant, Point, u8)>,
    /// Fractional wheel lines not yet applied (trackpads scroll in pixels).
    scroll_acc: f32,
}

/// The chord that opens links: Cmd on macOS, Ctrl elsewhere.
fn link_modifier(m: Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        m.logo()
    } else {
        m.control()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chord {
    Copy,
    Paste,
}

fn clipboard_chord(key: &Key, m: Modifiers) -> Option<Chord> {
    let Key::Character(c) = key else { return None };
    let held = if cfg!(target_os = "macos") {
        m.logo() && !m.control() && !m.alt()
    } else {
        m.control() && m.shift() && !m.alt()
    };
    if !held {
        return None;
    }
    match c.to_lowercase().as_str() {
        "c" => Some(Chord::Copy),
        "v" => Some(Chord::Paste),
        _ => None,
    }
}

/// Typing or pasting: drop the selection and show the newest output.
fn reset_view(term: &mut Terminal) {
    if term.selection().is_some() {
        term.set_selection(None);
    }
    if term.term().grid().display_offset() != 0 {
        term.scroll_to_bottom();
    }
}

impl InputState {
    /// Forget per-terminal state (the widget slot now shows another terminal); keeps modifiers.
    pub fn reset(&mut self) {
        *self = Self {
            modifiers: self.modifiers,
            ..Self::default()
        };
    }

    pub fn hover(&self) -> Option<&Hyperlink> {
        self.hover.as_ref()
    }

    /// A link is under the pointer and the open-link modifier is held.
    pub fn link_armed(&self) -> bool {
        self.hover.is_some() && link_modifier(self.modifiers)
    }

    pub fn handle(
        &mut self,
        ctx: &Context<'_>,
        event: &Event,
        clipboard: &mut dyn Clipboard,
        out: &mut Vec<TerminalOutput>,
    ) -> Effect {
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                let was = self.link_armed();
                self.modifiers = *m;
                Effect {
                    redraw: was != self.link_armed(),
                    capture: false,
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                text,
                ..
            }) if ctx.focused => {
                self.modifiers = *modifiers;
                if let Some(chord) = clipboard_chord(key, *modifiers) {
                    return self.clipboard(ctx, chord, clipboard, out);
                }
                let mut term = ctx.term.lock();
                let Some(bytes) =
                    encode_key(key, *physical_key, text.as_deref(), *modifiers, term.mode())
                else {
                    return Effect::NONE;
                };
                reset_view(&mut term);
                out.push(TerminalOutput::Input(bytes));
                Effect::REDRAW
            }
            Event::Mouse(mouse::Event::ButtonPressed(button)) => self.press(ctx, *button, out),
            Event::Mouse(mouse::Event::ButtonReleased(button)) => self.release(ctx, *button, out),
            Event::Mouse(mouse::Event::CursorMoved { .. }) => self.moved(ctx, out),
            Event::Mouse(mouse::Event::CursorLeft) => {
                self.hover_cell = None;
                Effect {
                    redraw: self.hover.take().is_some(),
                    capture: false,
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => self.wheel(ctx, *delta, out),
            _ => Effect::NONE,
        }
    }

    fn clipboard(
        &mut self,
        ctx: &Context<'_>,
        chord: Chord,
        clipboard: &mut dyn Clipboard,
        out: &mut Vec<TerminalOutput>,
    ) -> Effect {
        match chord {
            Chord::Copy => {
                let text = {
                    let term = ctx.term.lock();
                    term.selection().map(|s| s.text(&term))
                };
                if let Some(text) = text.filter(|t| !t.is_empty()) {
                    clipboard.write(Kind::Standard, text.clone());
                    out.push(TerminalOutput::Copy(text));
                }
                Effect::CAPTURE
            }
            Chord::Paste => {
                if let Some(text) = clipboard.read(Kind::Standard).filter(|t| !t.is_empty()) {
                    let mut term = ctx.term.lock();
                    let bytes = encode_paste(&text, term.mode());
                    reset_view(&mut term);
                    out.push(TerminalOutput::Input(bytes));
                }
                Effect::REDRAW
            }
        }
    }

    /// Whether mouse events go to the program rather than to local selection and scrolling.
    fn reporting(&self, mode: TermMode) -> bool {
        mode.intersects(TermMode::MOUSE_MODE) && !self.modifiers.shift()
    }

    fn report(
        &self,
        button: MouseButton,
        kind: MouseKind,
        cell: (u16, u16),
        mode: TermMode,
        out: &mut Vec<TerminalOutput>,
    ) {
        let report = MouseReport {
            button,
            kind,
            col: cell.0,
            row: cell.1,
            mods: self.modifiers,
        };
        if let Some(bytes) = encode_mouse(report, mode) {
            out.push(TerminalOutput::Input(bytes));
        }
    }

    fn press(
        &mut self,
        ctx: &Context<'_>,
        button: mouse::Button,
        out: &mut Vec<TerminalOutput>,
    ) -> Effect {
        let Some(pos) = ctx.cursor.position_in(ctx.bounds) else {
            return Effect::NONE;
        };
        let mut term = ctx.term.lock();
        let mode = term.mode();
        let cell = pixel_to_cell(pos, ctx.grid);
        let offset = term.term().grid().display_offset();
        let point = pixel_to_point(pos, ctx.grid, offset);
        if button == mouse::Button::Left && link_modifier(self.modifiers) {
            // Read the link from the clicked cell: the content may have changed under a still
            // pointer since the hover was last computed.
            if let Some(link) = term.term().grid()[point].hyperlink() {
                out.push(TerminalOutput::OpenUrl(link.uri().to_owned()));
                return Effect::CAPTURE;
            }
        }
        if self.reporting(mode) {
            let b = match button {
                mouse::Button::Left => MouseButton::Left,
                mouse::Button::Middle => MouseButton::Middle,
                mouse::Button::Right => MouseButton::Right,
                _ => return Effect::NONE,
            };
            self.reported = Some((b, cell));
            self.report(b, MouseKind::Press, cell, mode, out);
            return Effect::CAPTURE;
        }
        if button != mouse::Button::Left {
            return Effect::NONE;
        }
        let now = Instant::now();
        let count = match self.last_click {
            Some((at, p, n)) if p == point && now.duration_since(at) <= MULTI_CLICK => n % 3 + 1,
            _ => 1,
        };
        self.last_click = Some((now, point, count));
        self.drag = None;
        match count {
            1 if self.modifiers.shift() && term.selection().is_some() => {
                // Keep the end farther from the click fixed and move the other one.
                let (start, end) = term.selection().expect("checked").bounds();
                let anchor = if point >= start { start } else { end };
                let mut sel = Selection::start(anchor);
                sel.update(point);
                term.set_selection(Some(sel));
                self.drag = Some((anchor, true));
            }
            1 => {
                if term.selection().is_some() {
                    term.set_selection(None);
                }
                self.drag = Some((point, false));
            }
            2 => {
                let t = term.term();
                let (a, b) = (
                    t.semantic_search_left(point),
                    t.semantic_search_right(point),
                );
                let mut sel = Selection::start(a);
                sel.update(b);
                term.set_selection(Some(sel));
            }
            _ => {
                let t = term.term();
                let (a, b) = (t.line_search_left(point), t.line_search_right(point));
                let mut sel = Selection::start(Point::new(a.line, Column(0)));
                sel.update(b);
                term.set_selection(Some(sel));
            }
        }
        Effect::REDRAW
    }

    fn release(
        &mut self,
        ctx: &Context<'_>,
        button: mouse::Button,
        out: &mut Vec<TerminalOutput>,
    ) -> Effect {
        if let Some((b, cell)) = self.reported.take() {
            let mode = ctx.term.lock().mode();
            let at = ctx
                .cursor
                .position()
                .map(|p| {
                    let rel = p - ctx.bounds.position();
                    pixel_to_cell(iced::Point::new(rel.x, rel.y), ctx.grid)
                })
                .unwrap_or(cell);
            self.report(b, MouseKind::Release, at, mode, out);
            return Effect::CAPTURE;
        }
        if button == mouse::Button::Left && self.drag.take().is_some() {
            return Effect::CAPTURE;
        }
        Effect::NONE
    }

    fn moved(&mut self, ctx: &Context<'_>, out: &mut Vec<TerminalOutput>) -> Effect {
        let Some(abs) = ctx.cursor.position() else {
            return Effect::NONE;
        };
        let pos = abs - ctx.bounds.position();
        let pos = iced::Point::new(pos.x, pos.y);
        if let Some((b, last)) = self.reported {
            let cell = pixel_to_cell(pos, ctx.grid);
            if cell != last {
                self.reported = Some((b, cell));
                let mode = ctx.term.lock().mode();
                self.report(b, MouseKind::Drag, cell, mode, out);
            }
            return Effect::CAPTURE;
        }
        if let Some((anchor, started)) = self.drag {
            let mut term = ctx.term.lock();
            let point = pixel_to_point(pos, ctx.grid, term.term().grid().display_offset());
            if !started && point == anchor {
                return Effect::CAPTURE;
            }
            let mut sel = Selection::start(anchor);
            sel.update(point);
            if term.selection().map(|s| s.bounds()) != Some(sel.bounds()) {
                term.set_selection(Some(sel));
            }
            self.drag = Some((anchor, true));
            return Effect::REDRAW;
        }
        self.update_hover(ctx, pos)
    }

    /// Track the OSC 8 hyperlink under the pointer.
    fn update_hover(&mut self, ctx: &Context<'_>, pos: iced::Point) -> Effect {
        if !ctx.cursor.is_over(ctx.bounds) {
            self.hover_cell = None;
            return Effect {
                redraw: self.hover.take().is_some(),
                capture: false,
            };
        }
        let term = ctx.term.lock();
        let grid = term.term().grid();
        let point = pixel_to_point(pos, ctx.grid, grid.display_offset());
        if self.hover_cell == Some(point) {
            return Effect::NONE;
        }
        self.hover_cell = Some(point);
        let link = grid[point].hyperlink();
        if link != self.hover {
            self.hover = link;
            return Effect {
                redraw: true,
                capture: false,
            };
        }
        Effect::NONE
    }

    fn wheel(
        &mut self,
        ctx: &Context<'_>,
        delta: mouse::ScrollDelta,
        out: &mut Vec<TerminalOutput>,
    ) -> Effect {
        let Some(pos) = ctx.cursor.position_in(ctx.bounds) else {
            return Effect::NONE;
        };
        self.scroll_acc += match delta {
            mouse::ScrollDelta::Lines { y, .. } => y * LINES_PER_NOTCH,
            mouse::ScrollDelta::Pixels { y, .. } => y / ctx.grid.cell_height.max(1.0),
        };
        let lines = self.scroll_acc.trunc() as i32;
        if lines == 0 {
            return Effect::CAPTURE;
        }
        self.scroll_acc -= lines as f32;
        let mut term = ctx.term.lock();
        let mode = term.mode();
        let up = lines > 0;
        let count = lines.unsigned_abs().min(1000) as usize;
        if self.reporting(mode) {
            let cell = pixel_to_cell(pos, ctx.grid);
            let b = if up {
                MouseButton::WheelUp
            } else {
                MouseButton::WheelDown
            };
            for _ in 0..count {
                self.report(b, MouseKind::Press, cell, mode, out);
            }
            return Effect::CAPTURE;
        }
        if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            // Pagers and editors on the alternate screen scroll with the arrow keys.
            let key: &[u8] = match (mode.contains(TermMode::APP_CURSOR), up) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1bOB",
                (false, true) => b"\x1b[A",
                (false, false) => b"\x1b[B",
            };
            out.push(TerminalOutput::Input(key.repeat(count)));
            return Effect::CAPTURE;
        }
        term.scroll(lines);
        Effect::REDRAW
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_forgets_terminal_state_but_keeps_modifiers() {
        let mut st = InputState {
            modifiers: Modifiers::SHIFT,
            hover: Some(Hyperlink::new(None::<String>, "https://example.com".into())),
            drag: Some((Point::default(), true)),
            reported: Some((MouseButton::Left, (1, 1))),
            last_click: Some((Instant::now(), Point::default(), 2)),
            ..InputState::default()
        };
        st.reset();
        assert_eq!(st.modifiers, Modifiers::SHIFT);
        assert!(st.hover.is_none() && st.drag.is_none() && st.reported.is_none());
        assert!(st.last_click.is_none());
    }

    #[test]
    fn copy_and_paste_chords_per_platform() {
        let c = Key::Character("c".into());
        let v = Key::Character("V".into());
        if cfg!(target_os = "macos") {
            assert_eq!(clipboard_chord(&c, Modifiers::LOGO), Some(Chord::Copy));
            assert_eq!(clipboard_chord(&v, Modifiers::LOGO), Some(Chord::Paste));
            assert_eq!(clipboard_chord(&c, Modifiers::CTRL), None);
        } else {
            let cs = Modifiers::CTRL | Modifiers::SHIFT;
            assert_eq!(clipboard_chord(&c, cs), Some(Chord::Copy));
            assert_eq!(clipboard_chord(&v, cs), Some(Chord::Paste));
            assert_eq!(clipboard_chord(&c, Modifiers::CTRL), None);
        }
        assert_eq!(
            clipboard_chord(&Key::Character("x".into()), Modifiers::all()),
            None
        );
    }
}
