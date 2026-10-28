//! Mouse and keyboard handling of the terminal widget.

use std::time::{Duration, Instant};

use feathercritty::index::{Column, Point};
use feathercritty::term::cell::Hyperlink;
use iced::advanced::Clipboard;
use iced::advanced::clipboard::Kind;
use iced::keyboard::{self, Key, Modifiers};
use iced::{Event, Rectangle, mouse};

use super::geometry::{pixel_to_cell, pixel_to_point};
use super::{RightClick, TerminalOutput};
use crate::{
    GridSize, KeyAction, KeyInput, MouseButton, MouseKind, MouseReport, Selection, SharedTerminal,
    TermMode, Terminal, encode_key_input, encode_mouse, encode_paste,
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
    /// What a right-click the program does not take does (see [`super::RIGHT_CLICK`]).
    pub right_click: RightClick,
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
    /// Keys whose press reached the program: only their releases are reported (kitty keyboard
    /// protocol event types), never those of chords the app or widget handled itself.
    held: Vec<HeldKey>,
}

/// A key whose press was sent to the program, kept to encode its release.
struct HeldKey {
    physical: keyboard::key::Physical,
    key: Key,
    modified_key: Key,
    location: keyboard::Location,
}

/// Modifier and lock keys: pressing one alone must not disturb the selection or the view.
fn is_modifier_key(key: &Key) -> bool {
    use keyboard::key::Named;
    matches!(
        key,
        Key::Named(
            Named::Shift
                | Named::Control
                | Named::Alt
                | Named::AltGraph
                | Named::Super
                | Named::Meta
                | Named::Hyper
                | Named::CapsLock
                | Named::NumLock
                | Named::ScrollLock
        )
    )
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

/// A keyboard chord that moves through the scrollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollChord {
    PageUp,
    PageDown,
    Top,
    Bottom,
}

/// Shift+PageUp/PageDown/Home/End on every system; on macOS also Cmd+Home/End for top and
/// bottom. Any other modifier held makes it an ordinary key.
fn scroll_chord(key: &Key, m: Modifiers, is_macos: bool) -> Option<ScrollChord> {
    use keyboard::key::Named;
    let Key::Named(named) = key else { return None };
    let shift_only = m.shift() && !m.control() && !m.alt() && !m.logo();
    let cmd_only = is_macos && m.logo() && !m.shift() && !m.control() && !m.alt();
    match named {
        Named::PageUp if shift_only => Some(ScrollChord::PageUp),
        Named::PageDown if shift_only => Some(ScrollChord::PageDown),
        Named::Home if shift_only || cmd_only => Some(ScrollChord::Top),
        Named::End if shift_only || cmd_only => Some(ScrollChord::Bottom),
        _ => None,
    }
}

/// Scroll the view for a scrollback chord. On the alternate screen (editors, pagers) there is
/// no scrollback, so the chord is left for the program: returns whether it was consumed.
fn keyboard_scroll(term: &mut Terminal, key: &Key, m: Modifiers, is_macos: bool) -> bool {
    if term.mode().contains(TermMode::ALT_SCREEN) {
        return false;
    }
    let Some(chord) = scroll_chord(key, m, is_macos) else {
        return false;
    };
    // A page keeps one line of context.
    let page = i32::from(term.size().rows.saturating_sub(1).max(1));
    match chord {
        ScrollChord::PageUp => term.scroll(page),
        ScrollChord::PageDown => term.scroll(-page),
        ScrollChord::Top => term.scroll_to_top(),
        ScrollChord::Bottom => term.scroll_to_bottom(),
    }
    true
}

/// The bytes a paste of `text` sends to the program in `term`: bracketed when the program
/// asked for it. Like typing, it drops the selection and shows the newest output. Every paste
/// goes through here (the paste chord and the context menu's `Paste`).
pub fn paste_input(term: &mut Terminal, text: &str) -> Vec<u8> {
    let bytes = encode_paste(text, term.mode());
    reset_view(term);
    bytes
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
        // Losing focus: the releases of keys still down would never arrive, so send them now.
        let unfocused = matches!(event, Event::Window(iced::window::Event::Unfocused));
        if (unfocused || !ctx.focused) && !self.held.is_empty() {
            self.release_all(ctx, out);
        }
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
                modified_key,
                physical_key,
                location,
                modifiers,
                text,
                repeat,
            }) if ctx.focused => {
                self.modifiers = *modifiers;
                if let Some(chord) = clipboard_chord(key, *modifiers) {
                    return self.clipboard(ctx, chord, clipboard, out);
                }
                let mut term = ctx.term.lock();
                if keyboard_scroll(&mut term, key, *modifiers, cfg!(target_os = "macos")) {
                    return Effect::REDRAW;
                }
                let input = KeyInput {
                    key,
                    modified_key,
                    physical: *physical_key,
                    location: *location,
                    text: text.as_deref(),
                    mods: *modifiers,
                    action: if *repeat {
                        KeyAction::Repeat
                    } else {
                        KeyAction::Press
                    },
                };
                let Some(bytes) = encode_key_input(&input, term.mode()) else {
                    return Effect::NONE;
                };
                if !self.held.iter().any(|h| h.physical == *physical_key) {
                    self.held.push(HeldKey {
                        physical: *physical_key,
                        key: key.clone(),
                        modified_key: modified_key.clone(),
                        location: *location,
                    });
                }
                if !is_modifier_key(key) {
                    reset_view(&mut term);
                }
                out.push(TerminalOutput::Input(bytes));
                Effect::REDRAW
            }
            Event::Keyboard(keyboard::Event::KeyReleased {
                key,
                modified_key,
                physical_key,
                location,
                modifiers,
            }) => {
                let Some(i) = self.held.iter().position(|h| h.physical == *physical_key) else {
                    return Effect::NONE;
                };
                self.held.swap_remove(i);
                if !ctx.focused {
                    return Effect::NONE;
                }
                let input = KeyInput {
                    key,
                    modified_key,
                    physical: *physical_key,
                    location: *location,
                    text: None,
                    mods: *modifiers,
                    action: KeyAction::Release,
                };
                let mode = ctx.term.lock().mode();
                match encode_key_input(&input, mode) {
                    Some(bytes) => {
                        out.push(TerminalOutput::Input(bytes));
                        Effect::CAPTURE
                    }
                    None => Effect::NONE,
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(button)) => {
                self.press(ctx, *button, clipboard, out)
            }
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

    /// Report the release of every key whose press reached the program (only produces output
    /// when the program asked for event types).
    fn release_all(&mut self, ctx: &Context<'_>, out: &mut Vec<TerminalOutput>) {
        let mode = ctx.term.lock().mode();
        for h in std::mem::take(&mut self.held) {
            let input = KeyInput {
                key: &h.key,
                modified_key: &h.modified_key,
                physical: h.physical,
                location: h.location,
                text: None,
                mods: self.modifiers,
                action: KeyAction::Release,
            };
            if let Some(bytes) = encode_key_input(&input, mode) {
                out.push(TerminalOutput::Input(bytes));
            }
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
                    let bytes = paste_input(&mut ctx.term.lock(), &text);
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
        clipboard: &mut dyn Clipboard,
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
            if let Some(link) = term.term().grid().cell(point).hyperlink() {
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
        if button == mouse::Button::Right {
            return self.right_click(&mut term, ctx, clipboard, out);
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

    /// A right-click the program does not get. [`RightClick::CopyOrMenu`]: copy the selection
    /// to the clipboard (as the Copy chord does) and clear it; without one, ask the host for
    /// the context menu at the pointer. [`RightClick::Menu`]: ask for the menu, saying whether
    /// there is a selection to copy, and keep it. Only a press of the user's mouse gets here,
    /// never anything a program sends.
    fn right_click(
        &mut self,
        term: &mut Terminal,
        ctx: &Context<'_>,
        clipboard: &mut dyn Clipboard,
        out: &mut Vec<TerminalOutput>,
    ) -> Effect {
        self.drag = None;
        let text = term
            .selection()
            .map(|s| s.text(term))
            .filter(|t| !t.is_empty());
        let can_copy = text.is_some();
        if let (RightClick::CopyOrMenu, Some(text)) = (ctx.right_click, text) {
            clipboard.write(Kind::Standard, text.clone());
            out.push(TerminalOutput::Copy(text));
            term.set_selection(None);
            return Effect::REDRAW;
        }
        if let Some(at) = ctx.cursor.position() {
            out.push(TerminalOutput::ContextMenu { at, can_copy });
        }
        Effect::CAPTURE
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
        let link = grid.cell(point).hyperlink();
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
    use crate::encode_key;

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

    fn filled(rows: u16) -> Terminal {
        let size = GridSize {
            cols: 40,
            rows,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let mut t = Terminal::new(size, 1000);
        for i in 0..200 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        t
    }

    fn offset(t: &Terminal) -> usize {
        t.term().grid().display_offset()
    }

    const PAGE_UP: Key = Key::Named(keyboard::key::Named::PageUp);
    const PAGE_DOWN: Key = Key::Named(keyboard::key::Named::PageDown);
    const HOME: Key = Key::Named(keyboard::key::Named::Home);
    const END: Key = Key::Named(keyboard::key::Named::End);

    #[test]
    fn shift_page_keys_scroll_one_page_less_a_line() {
        let mut t = filled(24);
        assert!(keyboard_scroll(&mut t, &PAGE_UP, Modifiers::SHIFT, false));
        assert_eq!(offset(&t), 23);
        assert!(keyboard_scroll(&mut t, &PAGE_UP, Modifiers::SHIFT, true));
        assert_eq!(offset(&t), 46);
        assert!(keyboard_scroll(&mut t, &PAGE_DOWN, Modifiers::SHIFT, false));
        assert_eq!(offset(&t), 23);
        assert!(keyboard_scroll(&mut t, &PAGE_DOWN, Modifiers::SHIFT, false));
        assert!(keyboard_scroll(&mut t, &PAGE_DOWN, Modifiers::SHIFT, false));
        assert_eq!(offset(&t), 0, "never below the bottom");
    }

    #[test]
    fn top_and_bottom_chords() {
        let mut t = filled(24);
        let history = feathercritty::grid::Dimensions::history_size(t.term().grid());
        assert!(history > 100);
        assert!(keyboard_scroll(&mut t, &HOME, Modifiers::SHIFT, false));
        assert_eq!(offset(&t), history);
        assert!(keyboard_scroll(&mut t, &END, Modifiers::SHIFT, false));
        assert_eq!(offset(&t), 0);
        // macOS aliases.
        assert!(keyboard_scroll(&mut t, &HOME, Modifiers::LOGO, true));
        assert_eq!(offset(&t), history);
        assert!(keyboard_scroll(&mut t, &END, Modifiers::LOGO, true));
        assert_eq!(offset(&t), 0);
        // Cmd is not a scroll modifier elsewhere.
        assert_eq!(scroll_chord(&HOME, Modifiers::LOGO, false), None);
    }

    #[test]
    fn other_modifiers_and_keys_are_not_scroll_chords() {
        assert_eq!(scroll_chord(&PAGE_UP, Modifiers::empty(), false), None);
        assert_eq!(
            scroll_chord(&PAGE_UP, Modifiers::SHIFT | Modifiers::CTRL, false),
            None
        );
        assert_eq!(
            scroll_chord(&HOME, Modifiers::LOGO | Modifiers::SHIFT, true),
            None
        );
        assert_eq!(
            scroll_chord(&Key::Character("a".into()), Modifiers::SHIFT, false),
            None
        );
        let mut t = filled(24);
        assert!(!keyboard_scroll(
            &mut t,
            &PAGE_UP,
            Modifiers::empty(),
            false
        ));
        assert_eq!(offset(&t), 0);
    }

    #[test]
    fn alternate_screen_passes_scroll_chords_to_the_program() {
        let mut t = filled(24);
        t.feed(b"\x1b[?1049h");
        assert!(t.mode().contains(TermMode::ALT_SCREEN));
        for key in [&PAGE_UP, &PAGE_DOWN, &HOME, &END] {
            assert!(!keyboard_scroll(&mut t, key, Modifiers::SHIFT, false));
        }
        assert_eq!(offset(&t), 0);
        // ...which then gets the key's usual encoding.
        let physical =
            keyboard::key::Physical::Unidentified(keyboard::key::NativeCode::Unidentified);
        assert_eq!(
            encode_key(&PAGE_UP, physical, None, Modifiers::SHIFT, t.mode()),
            Some(b"\x1b[5;2~".to_vec())
        );
        assert_eq!(
            encode_key(&HOME, physical, None, Modifiers::SHIFT, t.mode()),
            Some(b"\x1b[1;2H".to_vec())
        );
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

    /// A clipboard that remembers what was written and hands out `text` when read.
    #[derive(Default)]
    struct TestClipboard {
        text: Option<String>,
        written: Vec<String>,
    }

    impl Clipboard for TestClipboard {
        fn read(&self, _kind: Kind) -> Option<String> {
            self.text.clone()
        }
        fn write(&mut self, _kind: Kind, contents: String) {
            self.written.push(contents);
        }
    }

    fn shared(t: Terminal) -> SharedTerminal {
        std::sync::Arc::new(parking_lot::Mutex::new(t))
    }

    /// A context over a 40x24 terminal at (10, 20) in the window, the pointer at `at`.
    fn ctx_at(term: &SharedTerminal, at: iced::Point, right_click: RightClick) -> Context<'_> {
        let grid = term.lock().size();
        Context {
            term,
            focused: true,
            bounds: Rectangle::new(iced::Point::new(10.0, 20.0), iced::Size::new(320.0, 384.0)),
            grid,
            cursor: mouse::Cursor::Available(at),
            right_click,
        }
    }

    fn right_press() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right))
    }

    fn select_first_word(term: &SharedTerminal) {
        let mut t = term.lock();
        let line = feathercritty::index::Line(0);
        let mut sel = Selection::start(Point::new(line, Column(0)));
        sel.update(Point::new(line, Column(3)));
        t.set_selection(Some(sel));
    }

    #[test]
    fn right_click_on_a_selection_copies_it_and_clears_it() {
        let mut t = filled(24);
        t.feed(b"\x1b[H\x1b[2Jword two\r\n");
        let term = shared(t);
        select_first_word(&term);
        let at = iced::Point::new(50.0, 40.0);
        let mut st = InputState::default();
        let mut clip = TestClipboard::default();
        let mut out = Vec::new();
        let effect = st.handle(
            &ctx_at(&term, at, RightClick::CopyOrMenu),
            &right_press(),
            &mut clip,
            &mut out,
        );
        assert_eq!(clip.written, vec!["word".to_owned()]);
        assert_eq!(out, vec![TerminalOutput::Copy("word".into())]);
        assert!(
            term.lock().selection().is_none(),
            "the selection is cleared"
        );
        assert!(effect.redraw && effect.capture);
    }

    #[test]
    fn right_click_without_a_selection_asks_for_the_menu_at_the_pointer() {
        let term = shared(filled(24));
        let at = iced::Point::new(50.0, 40.0);
        let mut st = InputState::default();
        let mut clip = TestClipboard::default();
        let mut out = Vec::new();
        let effect = st.handle(
            &ctx_at(&term, at, RightClick::CopyOrMenu),
            &right_press(),
            &mut clip,
            &mut out,
        );
        assert_eq!(
            out,
            vec![TerminalOutput::ContextMenu {
                at,
                can_copy: false
            }]
        );
        assert!(clip.written.is_empty());
        assert!(effect.capture);
        // Releasing the button does nothing more.
        let mut out = Vec::new();
        let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Right));
        st.handle(
            &ctx_at(&term, at, RightClick::CopyOrMenu),
            &release,
            &mut clip,
            &mut out,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn right_click_on_macos_asks_for_the_menu_and_keeps_the_selection() {
        let mut t = filled(24);
        t.feed(b"\x1b[H\x1b[2Jword two\r\n");
        let term = shared(t);
        select_first_word(&term);
        let at = iced::Point::new(50.0, 40.0);
        let mut st = InputState::default();
        let mut clip = TestClipboard::default();
        let mut out = Vec::new();
        let effect = st.handle(
            &ctx_at(&term, at, RightClick::Menu),
            &right_press(),
            &mut clip,
            &mut out,
        );
        assert_eq!(
            out,
            vec![TerminalOutput::ContextMenu { at, can_copy: true }]
        );
        assert!(clip.written.is_empty(), "nothing is copied");
        assert!(term.lock().selection().is_some(), "the selection stays");
        assert!(effect.capture);
        // Without a selection there is nothing to copy.
        term.lock().set_selection(None);
        let mut out = Vec::new();
        st.handle(
            &ctx_at(&term, at, RightClick::Menu),
            &right_press(),
            &mut clip,
            &mut out,
        );
        assert_eq!(
            out,
            vec![TerminalOutput::ContextMenu {
                at,
                can_copy: false
            }]
        );
        assert!(clip.written.is_empty());
    }

    #[test]
    fn under_mouse_reporting_right_click_on_macos_is_the_programs_and_shift_opens_the_menu() {
        let mut t = filled(24);
        t.feed(b"\x1b[?1000h\x1b[?1006h");
        let term = shared(t);
        let at = iced::Point::new(50.0, 40.0);
        let mut st = InputState::default();
        let mut clip = TestClipboard::default();
        let mut out = Vec::new();
        st.handle(
            &ctx_at(&term, at, RightClick::Menu),
            &right_press(),
            &mut clip,
            &mut out,
        );
        assert_eq!(inputs(out), vec![b"\x1b[<2;6;2M".to_vec()]);
        let shift = Event::Keyboard(keyboard::Event::ModifiersChanged(Modifiers::SHIFT));
        let mut out = Vec::new();
        st.handle(
            &ctx_at(&term, at, RightClick::Menu),
            &shift,
            &mut clip,
            &mut out,
        );
        select_first_word(&term);
        st.handle(
            &ctx_at(&term, at, RightClick::Menu),
            &right_press(),
            &mut clip,
            &mut out,
        );
        assert_eq!(
            out,
            vec![TerminalOutput::ContextMenu { at, can_copy: true }]
        );
        assert!(clip.written.is_empty());
        assert!(term.lock().selection().is_some());
    }

    #[test]
    fn under_mouse_reporting_right_click_is_the_programs_and_shift_right_click_is_ours() {
        let mut t = filled(24);
        t.feed(b"\x1b[?1000h\x1b[?1006h");
        let term = shared(t);
        let at = iced::Point::new(50.0, 40.0);
        let mut st = InputState::default();
        let mut clip = TestClipboard::default();
        let mut out = Vec::new();
        st.handle(
            &ctx_at(&term, at, RightClick::CopyOrMenu),
            &right_press(),
            &mut clip,
            &mut out,
        );
        let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Right));
        st.handle(
            &ctx_at(&term, at, RightClick::CopyOrMenu),
            &release,
            &mut clip,
            &mut out,
        );
        assert_eq!(
            inputs(out),
            vec![b"\x1b[<2;6;2M".to_vec(), b"\x1b[<2;6;2m".to_vec()]
        );
        // With Shift held: the menu, and nothing reaches the program.
        let shift = Event::Keyboard(keyboard::Event::ModifiersChanged(Modifiers::SHIFT));
        let mut out = Vec::new();
        st.handle(
            &ctx_at(&term, at, RightClick::CopyOrMenu),
            &shift,
            &mut clip,
            &mut out,
        );
        st.handle(
            &ctx_at(&term, at, RightClick::CopyOrMenu),
            &right_press(),
            &mut clip,
            &mut out,
        );
        assert_eq!(
            out,
            vec![TerminalOutput::ContextMenu {
                at,
                can_copy: false
            }]
        );
        // ...and with a selection, the copy.
        select_first_word(&term);
        let mut out = Vec::new();
        st.handle(
            &ctx_at(&term, at, RightClick::CopyOrMenu),
            &right_press(),
            &mut clip,
            &mut out,
        );
        assert!(matches!(out.as_slice(), [TerminalOutput::Copy(_)]));
        assert!(term.lock().selection().is_none());
    }

    #[test]
    fn paste_input_brackets_when_asked_and_resets_the_view() {
        let mut t = filled(24);
        t.scroll(5);
        assert_eq!(offset(&t), 5);
        assert_eq!(paste_input(&mut t, "ls\n"), b"ls\r".to_vec());
        assert_eq!(offset(&t), 0, "a paste shows the newest output");
        t.feed(b"\x1b[?2004h");
        let line = feathercritty::index::Line(0);
        t.set_selection(Some(Selection::start(Point::new(line, Column(0)))));
        assert_eq!(paste_input(&mut t, "ls"), b"\x1b[200~ls\x1b[201~".to_vec());
        assert!(t.selection().is_none());
    }

    fn key_event(pressed: bool, named: keyboard::key::Named, code: keyboard::key::Code) -> Event {
        let key = Key::Named(named);
        let physical_key = keyboard::key::Physical::Code(code);
        Event::Keyboard(if pressed {
            keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key,
                physical_key,
                location: keyboard::Location::Standard,
                modifiers: Modifiers::empty(),
                text: None,
                repeat: false,
            }
        } else {
            keyboard::Event::KeyReleased {
                key: key.clone(),
                modified_key: key,
                physical_key,
                location: keyboard::Location::Standard,
                modifiers: Modifiers::empty(),
            }
        })
    }

    fn inputs(out: Vec<TerminalOutput>) -> Vec<Vec<u8>> {
        out.into_iter()
            .filter_map(|o| match o {
                TerminalOutput::Input(b) => Some(b),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn releases_reported_only_for_keys_sent_to_the_program() {
        use keyboard::key::{Code, Named};
        let mut t = filled(24);
        t.set_kitty_keyboard(true);
        // Disambiguate + event types.
        t.feed(b"\x1b[>3u");
        let grid = t.size();
        let term: SharedTerminal = std::sync::Arc::new(parking_lot::Mutex::new(t));
        let ctx = Context {
            term: &term,
            focused: true,
            bounds: Rectangle::new(iced::Point::ORIGIN, iced::Size::new(320.0, 384.0)),
            grid,
            cursor: mouse::Cursor::Unavailable,
            right_click: RightClick::CopyOrMenu,
        };
        let mut st = InputState::default();
        let mut clip = iced::advanced::clipboard::Null;
        let mut out = Vec::new();
        st.handle(
            &ctx,
            &key_event(true, Named::Escape, Code::Escape),
            &mut clip,
            &mut out,
        );
        st.handle(
            &ctx,
            &key_event(false, Named::Escape, Code::Escape),
            &mut clip,
            &mut out,
        );
        // Released without a press reaching the program (a scroll chord, an app shortcut).
        st.handle(
            &ctx,
            &key_event(false, Named::F5, Code::F5),
            &mut clip,
            &mut out,
        );
        assert_eq!(
            inputs(out),
            vec![b"\x1b[27u".to_vec(), b"\x1b[27;1:3u".to_vec()]
        );
        assert!(st.held.is_empty());
    }

    #[test]
    fn losing_focus_releases_held_keys() {
        use keyboard::key::{Code, Named};
        let mut t = filled(24);
        t.set_kitty_keyboard(true);
        t.feed(b"\x1b[>3u");
        let grid = t.size();
        let term: SharedTerminal = std::sync::Arc::new(parking_lot::Mutex::new(t));
        let mut ctx = Context {
            term: &term,
            focused: true,
            bounds: Rectangle::new(iced::Point::ORIGIN, iced::Size::new(320.0, 384.0)),
            grid,
            cursor: mouse::Cursor::Unavailable,
            right_click: RightClick::CopyOrMenu,
        };
        let mut clip = iced::advanced::clipboard::Null;
        let mut out = Vec::new();
        let mut st = InputState::default();
        st.handle(
            &ctx,
            &key_event(true, Named::Escape, Code::Escape),
            &mut clip,
            &mut out,
        );
        st.handle(
            &ctx,
            &key_event(true, Named::F5, Code::F5),
            &mut clip,
            &mut out,
        );
        let unfocus = Event::Window(iced::window::Event::Unfocused);
        st.handle(&ctx, &unfocus, &mut clip, &mut out);
        assert!(st.held.is_empty());
        let mut got = inputs(out);
        got.sort();
        assert_eq!(
            got,
            vec![
                b"\x1b[15;1:3~".to_vec(),
                b"\x1b[15~".to_vec(),
                b"\x1b[27;1:3u".to_vec(),
                b"\x1b[27u".to_vec(),
            ]
        );
        // The pane losing focus (no window event) flushes too; the late release is dropped.
        let mut out = Vec::new();
        st.handle(
            &ctx,
            &key_event(true, Named::Escape, Code::Escape),
            &mut clip,
            &mut out,
        );
        ctx.focused = false;
        st.handle(
            &ctx,
            &key_event(false, Named::Escape, Code::Escape),
            &mut clip,
            &mut out,
        );
        assert_eq!(
            inputs(out),
            vec![b"\x1b[27u".to_vec(), b"\x1b[27;1:3u".to_vec()]
        );
    }
}
