//! The terminal's right-click menu: `Copy` and `Paste` at the pointer.
//!
//! On macOS every right-click asks for this menu ([`TerminalOutput::ContextMenu`]) and keeps
//! the selection; `Copy` is enabled when there is one and copies it as the copy chord does,
//! leaving it selected. On Windows and Linux a right-click on a selection copies it without a
//! menu (the terminal widget does that, see [`fbterm_term::RightClick`]); with nothing
//! selected the widget asks for the menu, so `Copy` is shown disabled. `Paste` is the paste
//! chord's: the same clipboard read and the same bracketed-paste handling
//! ([`fbterm_term::paste_input`]). The menu closes on a choice,
//! Escape, a click outside it, the window losing focus or being resized. Only the user's own
//! right-click opens it; nothing a program in the terminal sends can.

use fbterm_config::keys::fixed_shortcuts;
use fbterm_term::TerminalOutput;
use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::widget::{Space, button, column, container, opaque, row, text};
use iced::{
    Alignment, Border, Color, Element, Event, Length, Padding, Point, Rectangle, Shadow, Size,
    Task, Vector,
};
use iced::{keyboard, mouse, window};

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

    /// Whether it can be chosen: `Copy` only with a selection to copy (`can_copy`).
    fn enabled(self, can_copy: bool) -> bool {
        match self {
            MenuItem::Copy => can_copy,
            MenuItem::Paste => true,
        }
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

/// The open menu: over which window and pane, where the right-click was, whether the pane
/// had a selection to copy, which item the keyboard is on (`Copy` when it is enabled, else
/// `Paste`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ContextMenu {
    pub window: window::Id,
    pub pane: PaneId,
    /// The pointer when the menu opened, in the window's logical pixels.
    pub at: Point,
    /// The pane had a selection when the menu opened: `Copy` is enabled.
    pub can_copy: bool,
    pub selected: MenuItem,
}

impl ContextMenu {
    pub fn new(window: window::Id, pane: PaneId, at: Point, can_copy: bool) -> Self {
        Self {
            window,
            pane,
            at,
            can_copy,
            selected: if can_copy {
                MenuItem::Copy
            } else {
                MenuItem::Paste
            },
        }
    }

    /// Move the keyboard's item by `step` over the enabled items, wrapping around.
    fn step(&mut self, step: isize) {
        let enabled: Vec<MenuItem> = MenuItem::ALL
            .into_iter()
            .filter(|i| i.enabled(self.can_copy))
            .collect();
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
    /// The widget asked for the menu: open it at the pointer (again, at the new point, when
    /// it was already open), `Copy` enabled when `can_copy`, and focus the right-clicked pane, so the menu's Paste and the
    /// paste chord go to the same pane. Not while the palette, the find-in-all-tabs box or a
    /// dialog is up in that window: their backdrops let a right-click through to the terminal.
    pub(crate) fn open_context_menu(
        &mut self,
        wid: window::Id,
        pane: PaneId,
        at: Point,
        can_copy: bool,
    ) -> Task<Message> {
        let covered = self.palettes.contains_key(&wid)
            || self.find_box.as_ref().is_some_and(|f| f.window == wid)
            || self.prompts.front(wid).is_some()
            || self.vault.dialog.as_ref().is_some_and(|d| d.window == wid);
        if covered || !self.windows.contains_key(&wid) {
            return Task::none();
        }
        self.context_menu = Some(ContextMenu::new(wid, pane, at, can_copy));
        self.focus_pane(wid, pane)
    }

    /// Close the menu once its pane is no longer shown (closed, its tab closed or another
    /// tab selected), so its Paste never has nowhere to go.
    pub(crate) fn sync_context_menu(&mut self) {
        let Some(menu) = &self.context_menu else {
            return;
        };
        let shown = self
            .windows
            .get(&menu.window)
            .and_then(|w| w.active_tab())
            .is_some_and(|t| t.visible_panes().contains(&menu.pane));
        if !shown {
            self.context_menu = None;
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
        let (pane, can_copy) = (menu.pane, menu.can_copy);
        self.context_menu = None;
        if !item.enabled(can_copy) {
            return Task::none();
        }
        match item {
            MenuItem::Paste => iced::clipboard::read()
                .map(move |text| Message::ContextMenu(wid, MenuMsg::Pasted(pane, text))),
            MenuItem::Copy => self.menu_copy(wid, pane),
        }
    }

    /// The text selected in `pane`, if any (what the copy chord would copy).
    fn selected_text(&self, wid: window::Id, pane: PaneId) -> Option<String> {
        let w = self.windows.get(&wid)?;
        let (t, p) = w.locate(pane)?;
        let terminal = w.tabs[t].panes.get(p)?.terminal()?;
        let term = terminal.lock();
        term.selection()
            .map(|s| s.text(&term))
            .filter(|t| !t.is_empty())
    }

    /// The menu's `Copy`: what the copy chord does, the selection put on the clipboard and
    /// reported as the widget reports it ([`TerminalOutput::Copy`]). The selection stays.
    fn menu_copy(&mut self, wid: window::Id, pane: PaneId) -> Task<Message> {
        let Some(text) = self.selected_text(wid, pane) else {
            return Task::none();
        };
        let copied = self.terminal_output(wid, pane, TerminalOutput::Copy(text.clone()));
        Task::batch([iced::clipboard::write(text), copied])
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
        let enabled = item.enabled(menu.can_copy);
        items = items.push(item_view(t, window, item, enabled, item == menu.selected));
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
    Element::new(Layer {
        content: layer.into(),
        close: msg(MenuMsg::Close),
    })
}

/// The clear layer over the window while the menu is open. Outside the menu a press closes
/// it, and only a right-click goes on to the terminal beneath, which then copies a selection
/// there (Windows and Linux) or opens the menu again at the new point (the same right-click
/// rules). The wheel,
/// pointer moves and other presses stop here, so the terminal neither scrolls, hovers nor
/// reports them to its program while the menu is up.
struct Layer<'a> {
    content: Element<'a, Message>,
    close: Message,
}

impl Widget<Message, iced::Theme, iced::Renderer> for Layer<'_> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if shell.is_event_captured() {
            return;
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) => {
                shell.publish(self.close.clone());
            }
            Event::Mouse(mouse::Event::ButtonPressed(_)) => {
                shell.publish(self.close.clone());
                shell.capture_event();
            }
            Event::Mouse(_) | Event::Touch(_) => shell.capture_event(),
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, iced::Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

fn item_view<'a>(
    t: &'static Tokens,
    window: window::Id,
    item: MenuItem,
    enabled: bool,
    selected: bool,
) -> Element<'a, Message> {
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
    fn copy_needs_a_selection_and_paste_shows_the_paste_chord() {
        assert!(!MenuItem::Copy.enabled(false) && MenuItem::Paste.enabled(false));
        assert!(MenuItem::Copy.enabled(true) && MenuItem::Paste.enabled(true));
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
            TerminalOutput::ContextMenu {
                at,
                can_copy: false,
            },
        ));
        assert_eq!(
            app.context_menu,
            Some(ContextMenu::new(wid, pane, at, false))
        );
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
        let pane = connected("cat", shell("stty -echo; echo ready; cat -v", tx));
        let term = pane.terminal().unwrap().clone();
        let id = pane.id;
        let wid = window::Id::unique();
        let mut w = WindowState::new();
        w.push(Tab::single(pane));
        app.windows.clear();
        app.windows.insert(wid, w);
        wait_for(&term, "ready");
        // A program that asked for bracketed paste gets the paste bracketed.
        term.lock().feed(b"\x1b[?2004h");
        let _ = app.update(Message::ContextMenu(
            wid,
            MenuMsg::Pasted(id, Some("pasted\n".into())),
        ));
        wait_for(&term, "^[[200~pasted");
        // Nothing (or nothing but an empty text) on the clipboard sends nothing: the next
        // paste follows the first one's closing bracket directly.
        let _ = app.update(Message::ContextMenu(wid, MenuMsg::Pasted(id, None)));
        let _ = app.update(Message::ContextMenu(
            wid,
            MenuMsg::Pasted(id, Some(String::new())),
        ));
        let _ = app.update(Message::ContextMenu(
            wid,
            MenuMsg::Pasted(id, Some("next\n".into())),
        ));
        wait_for(&term, "^[[201~^[[200~next");
    }

    /// Gives `pane` a terminal showing `word two` with `word` selected.
    fn select_word(app: &mut App, wid: window::Id, pane: PaneId) -> fbterm_term::SharedTerminal {
        let mut term = fbterm_term::Terminal::new(crate::app::DEFAULT_GRID, 100);
        term.feed(b"word two");
        term.set_selection(Some(fbterm_term::Selection::on_line(0, 0, 3)));
        let term = std::sync::Arc::new(parking_lot::Mutex::new(term));
        let w = app.windows.get_mut(&wid).unwrap();
        let (t, p) = w.locate(pane).unwrap();
        w.tabs[t].panes[p].placeholder = Some(term.clone());
        term
    }

    /// The widget asks for the menu with `can_copy` (as on macOS, whatever the host).
    fn open_menu(app: &mut App, wid: window::Id, pane: PaneId, can_copy: bool) {
        let at = Point::new(120.0, 80.0);
        let _ = app.update(Message::Terminal(
            wid,
            pane,
            TerminalOutput::ContextMenu { at, can_copy },
        ));
        assert_eq!(
            app.context_menu,
            Some(ContextMenu::new(wid, pane, at, can_copy))
        );
    }

    #[test]
    fn with_a_selection_copy_is_enabled_and_copies_it_keeping_it_selected() {
        let (mut app, wid, pane) = app_with_pane();
        let term = select_word(&mut app, wid, pane);
        open_menu(&mut app, wid, pane, true);
        // The keyboard starts on Copy and moves between both items.
        let selected = |app: &App| app.context_menu.as_ref().unwrap().selected;
        assert_eq!(selected(&app), MenuItem::Copy);
        let _ = app.update(Message::ContextMenu(wid, MenuMsg::Down));
        assert_eq!(selected(&app), MenuItem::Paste);
        let _ = app.update(Message::ContextMenu(wid, MenuMsg::Up));
        assert_eq!(selected(&app), MenuItem::Copy);
        assert_eq!(app.selected_text(wid, pane).as_deref(), Some("word"));
        let task = app.update(Message::ContextMenu(wid, MenuMsg::Submit));
        // The clipboard write (the widget's Copy report needs no work of the app).
        assert_eq!(task.units(), 1);
        assert_eq!(app.context_menu, None);
        assert!(term.lock().selection().is_some(), "the selection stays");
        // Chosen with the pointer, the same.
        open_menu(&mut app, wid, pane, true);
        let task = app.update(Message::ContextMenu(wid, MenuMsg::Choose(MenuItem::Copy)));
        assert_eq!(task.units(), 1);
        assert!(term.lock().selection().is_some());
    }

    #[test]
    fn without_a_selection_copy_is_disabled() {
        let (mut app, wid, pane) = app_with_pane();
        open_menu(&mut app, wid, pane, false);
        assert_eq!(app.context_menu.as_ref().unwrap().selected, MenuItem::Paste);
        let _ = app.update(Message::ContextMenu(wid, MenuMsg::Up));
        assert_eq!(app.context_menu.as_ref().unwrap().selected, MenuItem::Paste);
        let task = app.update(Message::ContextMenu(wid, MenuMsg::Choose(MenuItem::Copy)));
        assert_eq!(task.units(), 0, "a disabled Copy does nothing");
        assert_eq!(app.context_menu, None);
    }

    /// An app with one window whose tab is split into two panes; the first is focused.
    fn app_with_split() -> (App, window::Id, PaneId, PaneId) {
        let (mut app, wid, a) = app_with_pane();
        let second = Pane::new(ProfileId::new(), "two", None);
        let b = second.id;
        let tab = &mut app.windows.get_mut(&wid).unwrap().tabs[0];
        tab.layout.split(a, crate::layout::Side::Right, b);
        tab.panes.push(second);
        assert_eq!(tab.focused, a);
        (app, wid, a, b)
    }

    #[test]
    fn the_menu_focuses_the_right_clicked_pane() {
        let (mut app, wid, _, b) = app_with_split();
        right_click(&mut app, wid, b);
        assert_eq!(app.windows[&wid].tabs[0].focused, b);
    }

    #[test]
    fn the_menu_closes_with_its_pane_or_tab() {
        let (mut app, wid, a, b) = app_with_split();
        right_click(&mut app, wid, b);
        // Closing the right-clicked (now focused) pane.
        let _ = app.update(Message::Shortcut(wid, crate::keys::Action::CloseTab));
        assert!(app.windows[&wid].tabs[0].pane(b).is_none());
        assert_eq!(app.context_menu, None);
        // A tab that goes away with the menu's pane in it (the window keeps another tab).
        app.windows
            .get_mut(&wid)
            .unwrap()
            .push(Tab::single(Pane::new(ProfileId::new(), "three", None)));
        let _ = app.update(Message::TabBar(wid, crate::app::TabBarMsg::Select(0)));
        right_click(&mut app, wid, a);
        let _ = app.update(Message::TabBar(wid, crate::app::TabBarMsg::Close(0)));
        assert_eq!(app.windows[&wid].tabs.len(), 1);
        assert_eq!(app.context_menu, None);
    }

    #[test]
    fn a_right_click_while_the_menu_is_open_moves_it() {
        let (mut app, wid, a, b) = app_with_split();
        right_click(&mut app, wid, a);
        let at = Point::new(400.0, 300.0);
        let menu = TerminalOutput::ContextMenu {
            at,
            can_copy: false,
        };
        let _ = app.update(Message::Terminal(wid, b, menu));
        assert_eq!(app.context_menu, Some(ContextMenu::new(wid, b, at, false)));
    }

    #[test]
    fn the_menu_does_not_open_over_the_palette_or_the_find_box() {
        use crate::keys::Action;
        let (mut app, wid, pane) = app_with_pane();
        let at = Point::new(120.0, 80.0);
        let menu = TerminalOutput::ContextMenu {
            at,
            can_copy: false,
        };
        let _ = app.update(Message::Shortcut(wid, Action::Palette));
        assert!(app.palettes.contains_key(&wid));
        let _ = app.update(Message::Terminal(wid, pane, menu.clone()));
        assert_eq!(app.context_menu, None);
        let _ = app.update(Message::Shortcut(wid, Action::Palette));
        assert!(!app.palettes.contains_key(&wid));
        let _ = app.update(Message::Shortcut(wid, Action::FindAll));
        assert!(app.find_box.is_some());
        let _ = app.update(Message::Terminal(wid, pane, menu));
        assert_eq!(app.context_menu, None);
    }

    /// What the menu's full-window layer does with `event` with the pointer at `at`:
    /// the messages it publishes and whether it keeps the event from the layers beneath.
    fn layer_takes(event: iced::Event, at: Point) -> (Vec<Message>, bool) {
        use iced::advanced::layout::{self, Layout};
        use iced::advanced::widget::Tree;
        use iced::advanced::{Shell, clipboard};
        use iced::{Pixels, Rectangle, mouse};
        let t = crate::theme::tokens(fbterm_config::ThemeMode::Dark);
        let renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::DEFAULT,
            Pixels(16.0),
        ));
        let size = Size::new(800.0, 600.0);
        let wid = window::Id::unique();
        let menu = ContextMenu::new(wid, PaneId::next(), Point::new(100.0, 100.0), false);
        let mut element = view(t, wid, &menu, Some(size));
        let mut tree = Tree::new(&element);
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &event,
            Layout::new(&node),
            mouse::Cursor::Available(at),
            &renderer,
            &mut clipboard::Null,
            &mut shell,
            &Rectangle::with_size(size),
        );
        let captured = shell.is_event_captured();
        (messages, captured)
    }

    #[test]
    fn under_the_menu_only_a_right_click_reaches_the_terminal() {
        use iced::{Event, mouse};
        let outside = Point::new(500.0, 400.0);
        let press = |b| Event::Mouse(mouse::Event::ButtonPressed(b));
        let closes = |m: &[Message]| matches!(m, [Message::ContextMenu(_, MenuMsg::Close)]);
        // A left click outside closes the menu and goes no further.
        let (m, captured) = layer_takes(press(mouse::Button::Left), outside);
        assert!(closes(&m) && captured);
        // A right-click outside closes it and goes on to the terminal, which copies or
        // opens the menu again at the new point.
        let (m, captured) = layer_takes(press(mouse::Button::Right), outside);
        assert!(closes(&m) && !captured);
        // The wheel and pointer moves stop at the menu.
        let wheel = Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
        });
        let (m, captured) = layer_takes(wheel, outside);
        assert!(m.is_empty() && captured);
        let moved = Event::Mouse(mouse::Event::CursorMoved { position: outside });
        let (m, captured) = layer_takes(moved, outside);
        assert!(m.is_empty() && captured);
        // A right-click on the menu itself does nothing.
        let (m, captured) = layer_takes(press(mouse::Button::Right), Point::new(150.0, 115.0));
        assert!(m.is_empty() && captured);
    }
}
