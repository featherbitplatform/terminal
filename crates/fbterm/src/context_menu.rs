//! The terminal's right-click menu (Windows and Linux): `Copy` and `Paste` at the pointer.
//!
//! A right-click on a selection copies it without a menu (the terminal widget does that); with
//! nothing selected the widget asks for this menu ([`TerminalOutput::ContextMenu`]), so `Copy`
//! is shown disabled. `Paste` is the paste chord's: the same clipboard read and the same
//! bracketed-paste handling ([`fbterm_term::paste_input`]). The menu closes on a choice,
//! Escape, a click outside it, the window losing focus or being resized. Only the user's own
//! right-click opens it; nothing a program in the terminal sends can.

use fbterm_config::keys::fixed_shortcuts;
use fbterm_term::TerminalOutput;
use iced::widget::{Space, button, column, container, mouse_area, opaque, row, text};
use iced::{Alignment, Border, Color, Element, Length, Padding, Point, Shadow, Size, Task, Vector};
use iced::{keyboard, window};

use crate::app::{App, Message};
use crate::layout::PaneId;
use crate::theme::{Tokens, UI_FONT};
use crate::ui::MONO;

/// One entry of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuItem {
    Copy,
    Paste,
}

impl MenuItem {
    const ALL: [MenuItem; 2] = [MenuItem::Copy, MenuItem::Paste];

    fn title(self) -> &'static str {
        match self {
            MenuItem::Copy => "Copy",
            MenuItem::Paste => "Paste",
        }
    }

    /// The menu opens only with nothing selected, so there is nothing to copy.
    fn enabled(self) -> bool {
        self == MenuItem::Paste
    }

    /// Its chord, from the fixed shortcuts (never a hardcoded label).
    fn chord_label(self) -> Option<String> {
        let mac = cfg!(target_os = "macos");
        fixed_shortcuts(mac)
            .into_iter()
            .find(|f| f.title == self.title())
            .map(|f| f.chord.label(mac))
    }
}

/// The open menu: over which window and pane, where the right-click was, which item the
/// keyboard is on.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ContextMenu {
    pub window: window::Id,
    pub pane: PaneId,
    /// The pointer when the menu opened, in the window's logical pixels.
    pub at: Point,
    pub selected: MenuItem,
}

impl ContextMenu {
    pub fn new(window: window::Id, pane: PaneId, at: Point) -> Self {
        Self {
            window,
            pane,
            at,
            selected: MenuItem::Paste,
        }
    }

    /// Move the keyboard's item by `step` over the enabled items, wrapping around.
    fn step(&mut self, step: isize) {
        let enabled: Vec<MenuItem> = MenuItem::ALL.into_iter().filter(|i| i.enabled()).collect();
        let Some(here) = enabled.iter().position(|i| *i == self.selected) else {
            return;
        };
        let n = enabled.len() as isize;
        self.selected = enabled[(here as isize + step).rem_euclid(n) as usize];
    }
}

#[derive(Debug, Clone)]
pub(crate) enum MenuMsg {
    Up,
    Down,
    /// Enter: the keyboard's item.
    Submit,
    Choose(MenuItem),
    Close,
    /// The clipboard was read for the menu's `Paste` into this pane.
    Pasted(PaneId, Option<String>),
}

/// The menu message a key means while the menu is up (other keys do nothing).
pub(crate) fn menu_key(key: &keyboard::Key) -> Option<MenuMsg> {
    use keyboard::key::Named;
    match key {
        keyboard::Key::Named(Named::ArrowUp) => Some(MenuMsg::Up),
        keyboard::Key::Named(Named::ArrowDown) => Some(MenuMsg::Down),
        keyboard::Key::Named(Named::Enter) => Some(MenuMsg::Submit),
        keyboard::Key::Named(Named::Escape) => Some(MenuMsg::Close),
        _ => None,
    }
}

/// Width of the menu.
const MENU_WIDTH: f32 = 196.0;
/// Height of one item.
const ITEM_HEIGHT: f32 = 28.0;
/// Space between the menu's edge and its items.
const MENU_PADDING: f32 = 4.0;

fn menu_size() -> Size {
    Size::new(
        MENU_WIDTH,
        ITEM_HEIGHT * MenuItem::ALL.len() as f32 + 2.0 * MENU_PADDING,
    )
}

/// Where a menu of `size` opened at `at` goes in a window of `window` (if known): its top-left
/// corner at the pointer, flipped to the pointer's other side where it would leave the window,
/// and never above or left of the window's edge.
pub(crate) fn menu_origin(at: Point, size: Size, window: Option<Size>) -> Point {
    let fit = |at: f32, len: f32, room: Option<f32>| -> f32 {
        let pos = match room {
            Some(room) if at + len > room => at - len,
            _ => at,
        };
        pos.max(0.0)
    };
    Point::new(
        fit(at.x, size.width, window.map(|w| w.width)),
        fit(at.y, size.height, window.map(|w| w.height)),
    )
}

impl App {
    /// The widget asked for the menu: open it at the pointer.
    pub(crate) fn open_context_menu(&mut self, wid: window::Id, pane: PaneId, at: Point) {
        if self.windows.contains_key(&wid) {
            self.context_menu = Some(ContextMenu::new(wid, pane, at));
        }
    }

    /// Close the menu if it is open over `wid`.
    pub(crate) fn close_context_menu(&mut self, wid: window::Id) {
        if self.context_menu.as_ref().is_some_and(|m| m.window == wid) {
            self.context_menu = None;
        }
    }

    pub(crate) fn context_menu_msg(&mut self, wid: window::Id, msg: MenuMsg) -> Task<Message> {
        if let MenuMsg::Pasted(pane, text) = msg {
            return self.menu_paste(wid, pane, text);
        }
        let Some(menu) = self.context_menu.as_mut().filter(|m| m.window == wid) else {
            return Task::none();
        };
        let item = match msg {
            MenuMsg::Up => {
                menu.step(-1);
                return Task::none();
            }
            MenuMsg::Down => {
                menu.step(1);
                return Task::none();
            }
            MenuMsg::Submit => menu.selected,
            MenuMsg::Choose(item) => item,
            MenuMsg::Close | MenuMsg::Pasted(..) => {
                self.context_menu = None;
                return Task::none();
            }
        };
        let pane = menu.pane;
        self.context_menu = None;
        match item {
            MenuItem::Paste => iced::clipboard::read()
                .map(move |text| Message::ContextMenu(wid, MenuMsg::Pasted(pane, text))),
            // Never enabled: the menu opens only with nothing selected.
            MenuItem::Copy => Task::none(),
        }
    }

    /// The menu's `Paste`: what the paste chord sends, sent the way typing is.
    fn menu_paste(&mut self, wid: window::Id, pane: PaneId, text: Option<String>) -> Task<Message> {
        let Some(text) = text.filter(|t| !t.is_empty()) else {
            return Task::none();
        };
        let terminal = self.windows.get(&wid).and_then(|w| {
            let (t, p) = w.locate(pane)?;
            w.tabs[t].panes.get(p)?.terminal().cloned()
        });
        let Some(terminal) = terminal else {
            return Task::none();
        };
        let bytes = fbterm_term::paste_input(&mut terminal.lock(), &text);
        self.terminal_output(wid, pane, TerminalOutput::Input(bytes))
    }
}

/// The menu over window `window`, which is `size` (logical pixels) when known: a clear layer
/// over the whole window (a click on it closes the menu) holding the menu at the pointer.
pub(crate) fn view<'a>(
    t: &'static Tokens,
    window: window::Id,
    menu: &ContextMenu,
    size: Option<Size>,
) -> Element<'a, Message> {
    let msg = move |m| Message::ContextMenu(window, m);
    let mut items = column![];
    for item in MenuItem::ALL {
        items = items.push(item_view(t, window, item, item == menu.selected));
    }
    let card = container(items)
        .width(MENU_WIDTH)
        .padding(MENU_PADDING)
        .style(move |_| container::Style {
            background: Some(t.raised.into()),
            border: Border {
                color: t.border,
                width: 1.0,
                radius: 8.0.into(),
            },
            shadow: Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
                offset: Vector::new(0.0, 6.0),
                blur_radius: 16.0,
            },
            ..container::Style::default()
        });
    let origin = menu_origin(menu.at, menu_size(), size);
    let layer = container(opaque(card))
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(Padding {
            top: origin.y,
            left: origin.x,
            ..Padding::ZERO
        });
    mouse_area(layer)
        .on_press(msg(MenuMsg::Close))
        .on_right_press(msg(MenuMsg::Close))
        .on_middle_press(msg(MenuMsg::Close))
        .into()
}

fn item_view<'a>(
    t: &'static Tokens,
    window: window::Id,
    item: MenuItem,
    selected: bool,
) -> Element<'a, Message> {
    let enabled = item.enabled();
    let color = if enabled { t.text } else { t.text_muted };
    let mut line = row![
        text(item.title()).font(UI_FONT).size(13).color(color),
        Space::new().width(Length::Fill),
    ]
    .align_y(Alignment::Center);
    if let Some(chord) = item.chord_label() {
        line = line.push(text(chord).font(MONO).size(11.5).color(t.text_muted));
    }
    button(line)
        .on_press_maybe(enabled.then(|| Message::ContextMenu(window, MenuMsg::Choose(item))))
        .width(Length::Fill)
        .height(ITEM_HEIGHT)
        .padding(Padding::from([0.0, 10.0]))
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: ((hovered || selected) && enabled).then(|| t.hover.into()),
                text_color: color,
                border: Border {
                    radius: 6.0.into(),
                    ..Border::default()
                },
                ..button::Style::default()
            }
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbterm_config::{Paths, ProfileId};

    use crate::tabs::{Pane, Tab, WindowState};

    #[test]
    fn the_menu_opens_at_the_pointer_and_stays_inside_the_window() {
        let size = Size::new(196.0, 64.0);
        let window = Some(Size::new(800.0, 600.0));
        let at = |x, y| menu_origin(Point::new(x, y), size, window);
        assert_eq!(at(100.0, 200.0), Point::new(100.0, 200.0));
        // Near the right or bottom edge it opens to the pointer's other side.
        assert_eq!(at(700.0, 200.0), Point::new(504.0, 200.0));
        assert_eq!(at(100.0, 580.0), Point::new(100.0, 516.0));
        // A window smaller than the menu: pinned to its top-left corner.
        let tiny = Some(Size::new(150.0, 50.0));
        assert_eq!(
            menu_origin(Point::new(100.0, 40.0), size, tiny),
            Point::ORIGIN
        );
        // Size not known yet: at the pointer.
        assert_eq!(
            menu_origin(Point::new(700.0, 580.0), size, None),
            Point::new(700.0, 580.0)
        );
    }

    #[test]
    fn the_menu_keys() {
        use keyboard::key::Named;
        let k = |n| menu_key(&keyboard::Key::Named(n));
        assert!(matches!(k(Named::ArrowUp), Some(MenuMsg::Up)));
        assert!(matches!(k(Named::ArrowDown), Some(MenuMsg::Down)));
        assert!(matches!(k(Named::Enter), Some(MenuMsg::Submit)));
        assert!(matches!(k(Named::Escape), Some(MenuMsg::Close)));
        assert!(menu_key(&keyboard::Key::Character("c".into())).is_none());
    }

    #[test]
    fn copy_is_disabled_and_paste_shows_the_paste_chord() {
        assert!(!MenuItem::Copy.enabled() && MenuItem::Paste.enabled());
        let mac = cfg!(target_os = "macos");
        let paste = if mac { "\u{2318}V" } else { "Ctrl+Shift+V" };
        assert_eq!(MenuItem::Paste.chord_label().as_deref(), Some(paste));
        assert!(MenuItem::Copy.chord_label().is_some());
    }

    fn app_with_pane() -> (App, window::Id, PaneId) {
        let dir = std::env::temp_dir().join(format!("fbterm-menu-test-{}", ProfileId::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, _) = App::build(Paths { config_dir: dir }, true);
        app.state_saving_enabled = false;
        let pane = Pane::new(ProfileId::new(), "one", None);
        let id = pane.id;
        let wid = window::Id::unique();
        let mut w = WindowState::new();
        w.push(Tab::single(pane));
        app.windows.clear();
        app.windows.insert(wid, w);
        (app, wid, id)
    }

    fn right_click(app: &mut App, wid: window::Id, pane: PaneId) {
        let at = Point::new(120.0, 80.0);
        let _ = app.update(Message::Terminal(
            wid,
            pane,
            TerminalOutput::ContextMenu(at),
        ));
        assert_eq!(app.context_menu, Some(ContextMenu::new(wid, pane, at)));
    }

    #[test]
    fn a_right_click_opens_the_menu_and_escape_focus_loss_or_a_resize_close_it() {
        let (mut app, wid, pane) = app_with_pane();
        assert_eq!(app.context_menu, None);
        right_click(&mut app, wid, pane);
        // The keyboard stays on Paste: Copy cannot be chosen.
        let _ = app.update(Message::ContextMenu(wid, MenuMsg::Up));
        let _ = app.update(Message::ContextMenu(wid, MenuMsg::Down));
        assert_eq!(app.context_menu.as_ref().unwrap().selected, MenuItem::Paste);
        let _ = app.update(Message::ContextMenu(wid, MenuMsg::Close));
        assert_eq!(app.context_menu, None);

        right_click(&mut app, wid, pane);
        let _ = app.update(Message::WindowUnfocused(wid));
        assert_eq!(app.context_menu, None);

        right_click(&mut app, wid, pane);
        let _ = app.update(Message::WindowResized(wid, Size::new(900.0, 700.0)));
        assert_eq!(app.context_menu, None);

        right_click(&mut app, wid, pane);
        let _ = app.update(Message::WindowClosed(wid));
        assert_eq!(app.context_menu, None);
    }

    #[test]
    fn choosing_paste_closes_the_menu() {
        let (mut app, wid, pane) = app_with_pane();
        right_click(&mut app, wid, pane);
        let _ = app.update(Message::ContextMenu(wid, MenuMsg::Submit));
        assert_eq!(app.context_menu, None);
        right_click(&mut app, wid, pane);
        let _ = app.update(Message::ContextMenu(wid, MenuMsg::Choose(MenuItem::Paste)));
        assert_eq!(app.context_menu, None);
    }

    #[cfg(unix)]
    #[test]
    fn the_menus_paste_sends_what_the_paste_chord_sends() {
        use crate::sessions::pane_tests::unix::{connected, shell, wait_for};
        let (mut app, _, _) = app_with_pane();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let pane = connected("cat", shell("cat -v", tx));
        let term = pane.terminal().unwrap().clone();
        let id = pane.id;
        let wid = window::Id::unique();
        let mut w = WindowState::new();
        w.push(Tab::single(pane));
        app.windows.clear();
        app.windows.insert(wid, w);
        // A program that asked for bracketed paste gets the paste bracketed.
        term.lock().feed(b"\x1b[?2004h");
        let _ = app.update(Message::ContextMenu(
            wid,
            MenuMsg::Pasted(id, Some("pasted\n".into())),
        ));
        wait_for(&term, "^[[200~pasted");
        // Nothing on the clipboard sends nothing.
        let _ = app.update(Message::ContextMenu(wid, MenuMsg::Pasted(id, None)));
    }
}
