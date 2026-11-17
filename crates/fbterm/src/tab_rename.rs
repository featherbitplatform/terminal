//! Renaming a tab: a double-click on it (bar or sidebar), the tab menu's `Rename tab` or the
//! `rename_tab` shortcut puts a field in place of its label. Enter or a click anywhere else
//! keeps the name, Escape keeps the old one; an empty name gives the tab its automatic label
//! back. Page tabs (Profiles, Settings, Color schemes, Welcome) are never renamed. The name is
//! the tab's own (`Tab::label`): it stays when focus moves between its panes and is saved with
//! the layout. Only the user's own input starts a rename; nothing a program writes does.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::widget::{Id, text_input};
use iced::{Element, Event, Length, Rectangle, Size, Task, Vector, mouse, window};

use crate::app::{App, Message};
use crate::tabs::{Tab, TabId};
use crate::theme::{Tokens, UI_FONT};

/// The most characters a tab name keeps.
pub(crate) const MAX_CHARS: usize = 120;

/// The rename field's widget id (one field in the app at a time).
fn field_id() -> Id {
    Id::new("fbterm-tab-rename")
}

/// A rename in progress: the tab, in which window, and the text typed so far.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Renaming {
    pub window: window::Id,
    pub tab: TabId,
    pub text: String,
}

#[derive(Debug, Clone)]
pub(crate) enum RenameMsg {
    /// The field's text changed.
    Edit(String),
    /// Keep the typed name: Enter, a click elsewhere, the window losing focus.
    Commit,
    /// Keep the old name: Escape.
    Cancel,
}

/// The label a typed name gives a tab: trimmed, control characters dropped, at most
/// [`MAX_CHARS`]; `None` (nothing left) is the automatic label.
pub(crate) fn clean(typed: &str) -> Option<String> {
    let kept: String = typed
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_CHARS)
        .collect();
    let kept = kept.trim();
    (!kept.is_empty()).then(|| kept.to_owned())
}

/// The label `tab` takes when `typed` is committed: unchanged text keeps an automatic label
/// automatic (it still follows the focused pane), so only a real edit pins a name.
pub(crate) fn committed(tab: &Tab, typed: &str) -> Option<String> {
    let name = clean(typed);
    if tab.label.is_none() && name.as_deref() == Some(tab.shown_name()) {
        return None;
    }
    name
}

impl App {
    pub(crate) fn rename_msg(&mut self, msg: RenameMsg) -> Task<Message> {
        match msg {
            RenameMsg::Edit(text) => {
                if let Some(r) = &mut self.renaming {
                    r.text = text.chars().take(MAX_CHARS).collect();
                }
                Task::none()
            }
            RenameMsg::Commit => {
                self.commit_rename();
                Task::none()
            }
            RenameMsg::Cancel => {
                self.renaming = None;
                Task::none()
            }
        }
    }

    /// Put the rename field on `tab` of window `wid`, its text the label it shows, all of it
    /// selected. Not for a page tab, nor while the palette, a dialog or the vault dialog
    /// covers the window. A rename open on another tab keeps its text first.
    pub(crate) fn start_rename(&mut self, wid: window::Id, tab: TabId) -> Task<Message> {
        if self.menu_covered(wid) {
            return Task::none();
        }
        let Some(t) = self
            .windows
            .get(&wid)
            .and_then(|w| w.tabs.iter().find(|t| t.id == tab))
        else {
            return Task::none();
        };
        if t.page.is_some() {
            return Task::none();
        }
        if self.renaming.as_ref().is_some_and(|r| r.tab == tab) {
            return Task::none();
        }
        let text = t.shown_name().to_owned();
        self.commit_rename();
        self.context_menu = None;
        self.renaming = Some(Renaming {
            window: wid,
            tab,
            text,
        });
        Task::batch([
            iced::widget::operation::focus(field_id()),
            iced::widget::operation::select_all(field_id()),
        ])
    }

    /// The `Rename tab` shortcut: the active tab of `wid`.
    pub(crate) fn rename_active_tab(&mut self, wid: window::Id) -> Task<Message> {
        self.reveal_active_tab(wid);
        match self.windows.get(&wid).and_then(|w| w.active_tab()) {
            Some(tab) => self.start_rename(wid, tab.id),
            None => Task::none(),
        }
    }

    /// Give the tab being renamed the typed name (see [`committed`]) and close the field.
    /// The tab is looked up by id in every window, so one dragged to another window meanwhile
    /// still gets it.
    pub(crate) fn commit_rename(&mut self) {
        let Some(r) = self.renaming.take() else {
            return;
        };
        let Some(tab) = self
            .windows
            .values_mut()
            .flat_map(|w| w.tabs.iter_mut())
            .find(|t| t.id == r.tab && t.page.is_none())
        else {
            return;
        };
        let label = committed(tab, &r.text);
        if tab.label != label {
            tab.label = label;
            self.save_state();
        }
    }

    /// Whether window `wid` shows the rename field: its terminals then do not take keys.
    pub(crate) fn renaming_in(&self, wid: window::Id) -> bool {
        self.renaming.as_ref().is_some_and(|r| r.window == wid)
    }

    /// After every update: a rename whose tab left its window, or whose window something now
    /// covers, ends (keeping the typed name when the tab is still there).
    pub(crate) fn sync_renaming(&mut self) {
        let Some(r) = &self.renaming else {
            return;
        };
        let here = self
            .windows
            .get(&r.window)
            .is_some_and(|w| w.tabs.iter().any(|t| t.id == r.tab));
        if !here || self.menu_covered(r.window) {
            self.commit_rename();
        }
    }
}

/// The field shown in place of a tab's label while it is renamed, `size` its text size.
pub(crate) fn field<'a>(t: &'static Tokens, r: &Renaming, size: f32) -> Element<'a, Message> {
    let input = text_input("Tab name", &r.text)
        .id(field_id())
        .on_input(|s| Message::Rename(RenameMsg::Edit(s)))
        .on_submit(Message::Rename(RenameMsg::Commit))
        .font(UI_FONT)
        .size(size)
        .padding([1.0, 4.0])
        .width(Length::Fill)
        .style(move |_, status| {
            let mut style = crate::ui::input_style(t, status);
            style.border.radius = 4.0.into();
            style
        });
    Element::new(CommitOutside {
        content: input.into(),
    })
}

/// Commits the rename when a mouse button is pressed outside the field or the window loses
/// focus: iced's field only lets go of the keyboard then and says nothing.
struct CommitOutside<'a> {
    content: Element<'a, Message>,
}

impl Widget<Message, iced::Theme, iced::Renderer> for CommitOutside<'_> {
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
        if leaves(event, cursor.is_over(layout.bounds())) {
            shell.publish(Message::Rename(RenameMsg::Commit));
        }
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

/// Whether `event` ends a rename: a press of any button with the pointer off the field
/// (`over`), or the window losing focus.
fn leaves(event: &Event, over: bool) -> bool {
    match event {
        Event::Mouse(mouse::Event::ButtonPressed(_)) => !over,
        Event::Window(window::Event::Unfocused) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use iced::{Point, window};

    use super::{MAX_CHARS, RenameMsg, clean, leaves};
    use crate::app::{App, Message, TabBarMsg};
    use crate::context_menu::MenuItem;
    use crate::keys::Action;
    use crate::restore_tests::{paths_with, profile};
    use crate::tabs::{Page, Pane, Tab, WindowState};

    #[test]
    fn a_typed_name_is_trimmed_cleaned_and_capped() {
        assert_eq!(clean("  build box \t"), Some("build box".into()));
        assert_eq!(clean("a\u{1b}[31mb\nc"), Some("a[31mbc".into()));
        assert_eq!(clean("   "), None);
        assert_eq!(clean(""), None);
        let long = "x".repeat(MAX_CHARS + 30);
        assert_eq!(clean(&long).map(|s| s.chars().count()), Some(MAX_CHARS));
    }

    #[test]
    fn a_press_off_the_field_or_losing_focus_ends_the_rename() {
        let press =
            iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left));
        assert!(leaves(&press, false));
        assert!(
            !leaves(&press, true),
            "a click in the field moves the caret"
        );
        assert!(leaves(&iced::Event::Window(window::Event::Unfocused), true));
        let moved = iced::Event::Mouse(iced::mouse::Event::CursorMoved {
            position: Point::ORIGIN,
        });
        assert!(!leaves(&moved, false));
    }

    /// An app with one window holding a terminal tab of profile `a`, a second of `b` and the
    /// Settings page tab.
    fn app() -> (App, window::Id) {
        let (a, b) = (profile("alpha"), profile("beta"));
        let (mut app, _) = App::build(paths_with(&[a.clone(), b.clone()]), true);
        app.state_saving_enabled = false;
        app.windows.clear();
        let wid = window::Id::unique();
        let mut w = WindowState::new();
        w.push(Tab::single(Pane::for_profile(&a)));
        w.push(Tab::single(Pane::for_profile(&b)));
        w.push(Tab::page(Page::Settings));
        w.active = 0;
        app.windows.insert(wid, w);
        app.sizes.insert(wid, (900.0, 600.0));
        app.focused = Some(wid);
        (app, wid)
    }

    fn tab_id(app: &App, wid: window::Id, index: usize) -> crate::tabs::TabId {
        app.windows[&wid].tabs[index].id
    }

    fn rename(app: &mut App, wid: window::Id, index: usize, name: &str) {
        let tab = tab_id(app, wid, index);
        let _ = app.start_rename(wid, tab);
        let _ = app.update(Message::Rename(RenameMsg::Edit(name.into())));
        let _ = app.update(Message::Rename(RenameMsg::Commit));
    }

    #[test]
    fn a_committed_name_replaces_the_label_and_an_empty_one_restores_it() {
        let (mut app, wid) = app();
        rename(&mut app, wid, 1, "  deploy  ");
        let tab = &app.windows[&wid].tabs[1];
        assert_eq!(tab.label.as_deref(), Some("deploy"));
        assert_eq!(tab.shown_name(), "deploy");
        assert_eq!(tab.shown_title(), "deploy");
        assert!(app.renaming.is_none());
        rename(&mut app, wid, 1, "   ");
        let tab = &app.windows[&wid].tabs[1];
        assert_eq!(tab.label, None);
        assert_eq!(tab.shown_name(), "beta");
    }

    #[test]
    fn the_field_starts_with_the_shown_label_and_unchanged_text_pins_nothing() {
        let (mut app, wid) = app();
        let tab = tab_id(&app, wid, 0);
        let _ = app.start_rename(wid, tab);
        assert_eq!(
            app.renaming.as_ref().map(|r| r.text.as_str()),
            Some("alpha")
        );
        let _ = app.update(Message::Rename(RenameMsg::Commit));
        assert_eq!(app.windows[&wid].tabs[0].label, None);
    }

    #[test]
    fn escape_keeps_the_old_name() {
        let (mut app, wid) = app();
        rename(&mut app, wid, 0, "first");
        let tab = tab_id(&app, wid, 0);
        let _ = app.start_rename(wid, tab);
        let _ = app.update(Message::Rename(RenameMsg::Edit("second".into())));
        let _ = app.update(Message::Rename(RenameMsg::Cancel));
        assert_eq!(app.windows[&wid].tabs[0].label.as_deref(), Some("first"));
        assert!(app.renaming.is_none());
    }

    #[test]
    fn a_page_tab_is_never_renamed() {
        let (mut app, wid) = app();
        let page = tab_id(&app, wid, 2);
        let _ = app.start_rename(wid, page);
        assert!(app.renaming.is_none());
        app.windows.get_mut(&wid).unwrap().active = 2;
        let _ = app.update(Message::Shortcut(wid, Action::RenameTab));
        assert!(app.renaming.is_none());
        let _ = app.update(Message::TabBar(wid, TabBarMsg::Rename(2)));
        assert!(app.renaming.is_none());
        let _ = app.update(Message::TabBar(
            wid,
            TabBarMsg::Menu {
                index: 2,
                at: Point::ORIGIN,
            },
        ));
        let items = &app.context_menu.as_ref().expect("the menu opens").items;
        assert!(!items.iter().any(|(i, _)| *i == MenuItem::RenameTab));
    }

    #[test]
    fn the_shortcut_the_double_click_and_the_menu_start_a_rename() {
        let (mut app, wid) = app();
        let _ = app.update(Message::Shortcut(wid, Action::RenameTab));
        assert_eq!(
            app.renaming.as_ref().map(|r| r.tab),
            Some(tab_id(&app, wid, 0)),
            "the active tab"
        );
        let _ = app.update(Message::Rename(RenameMsg::Cancel));
        let _ = app.update(Message::TabBar(wid, TabBarMsg::Rename(1)));
        assert_eq!(
            app.renaming.as_ref().map(|r| r.tab),
            Some(tab_id(&app, wid, 1))
        );
        let _ = app.update(Message::Rename(RenameMsg::Cancel));
        let _ = app.update(Message::TabBar(
            wid,
            TabBarMsg::Menu {
                index: 1,
                at: Point::ORIGIN,
            },
        ));
        let _ = app.update(Message::ContextMenu(
            wid,
            crate::context_menu::MenuMsg::Choose(MenuItem::RenameTab),
        ));
        assert_eq!(
            app.renaming.as_ref().map(|r| r.tab),
            Some(tab_id(&app, wid, 1))
        );
    }

    #[test]
    fn a_rename_whose_tab_closes_ends_and_renaming_another_tab_keeps_the_first() {
        let (mut app, wid) = app();
        let first = tab_id(&app, wid, 0);
        let _ = app.start_rename(wid, first);
        let _ = app.update(Message::Rename(RenameMsg::Edit("kept".into())));
        let second = tab_id(&app, wid, 1);
        let _ = app.start_rename(wid, second);
        assert_eq!(app.windows[&wid].tabs[0].label.as_deref(), Some("kept"));
        assert_eq!(app.renaming.as_ref().map(|r| r.tab), Some(second));
        let _ = app.update(Message::TabBar(wid, TabBarMsg::Close(1)));
        assert!(app.renaming.is_none());
    }

    #[test]
    fn the_name_shows_in_the_window_title_and_survives_a_focus_change() {
        let (mut app, wid) = app();
        rename(&mut app, wid, 0, "logs");
        assert_eq!(app.title(wid), "logs · fbterm");
    }
}
