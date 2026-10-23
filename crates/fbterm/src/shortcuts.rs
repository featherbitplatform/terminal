//! Keys before content: a wrapper that turns application shortcuts into messages for each
//! kind of window and overlay, before a terminal or a field sees the key.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Size, Vector, keyboard, mouse, window};

use crate::app::Message;
use crate::editor::EditorMsg;
use crate::keys::{self, Action};
use crate::palette::PaletteMsg;
use crate::prompts::PromptMsg;
use crate::settings_window::SettingsMsg;

/// How a [`Shortcuts`] wrapper treats keys.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Application shortcuts act; other keys pass on.
    Normal,
    /// Beneath an overlay: every key is dropped, so nothing reaches the terminal and no
    /// shortcut acts behind the overlay, except the palette toggle when `palette_toggle`.
    Blocked { palette_toggle: bool },
    /// The palette's own: Up/Down, Ctrl+P/N, Enter and Escape drive it; the rest goes on to the
    /// search box.
    Palette,
    /// A connection dialog: Escape cancels, Enter submits; other keys go to its field.
    Prompt,
    /// The profile editor window: Cmd+W closes it, Cmd+S saves, Settings and Quit work; other
    /// shortcuts do nothing.
    Editor,
    /// The Settings window: Cmd+W closes it, the profile editor and Quit work; other shortcuts
    /// do nothing.
    Settings,
}

/// The palette message a key means while the palette is up.
fn palette_key(key: &keyboard::Key, mods: keyboard::Modifiers) -> Option<PaletteMsg> {
    use keyboard::key::Named;
    match key {
        keyboard::Key::Named(Named::ArrowUp) => Some(PaletteMsg::Up),
        keyboard::Key::Named(Named::ArrowDown) => Some(PaletteMsg::Down),
        keyboard::Key::Named(Named::Enter) => Some(PaletteMsg::Submit),
        keyboard::Key::Named(Named::Escape) => Some(PaletteMsg::Close),
        keyboard::Key::Character(c) if mods.control() && !mods.shift() && !mods.logo() => {
            match c.as_str() {
                "p" => Some(PaletteMsg::Up),
                "n" => Some(PaletteMsg::Down),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Wraps a window's content and turns application shortcuts into messages before the terminal
/// sees the key, so Ctrl+Shift chords never reach the shell.
pub(crate) struct Shortcuts<'a> {
    pub content: Element<'a, Message>,
    pub window: window::Id,
    pub mode: Mode,
}

/// What a key press means to a [`Shortcuts`] wrapper in `mode` over window `w`:
/// `Some(Some(msg))` publish and stop it, `Some(None)` stop it, `None` let it through.
fn key_message(
    mode: Mode,
    w: window::Id,
    key: &keyboard::Key,
    mods: keyboard::Modifiers,
    repeat: bool,
) -> Option<Option<Message>> {
    use keyboard::key::Named;
    let shortcut = keys::shortcut(key, mods);
    match mode {
        Mode::Normal => shortcut.map(|action| {
            // Holding a key repeats tab switching only.
            (!repeat || matches!(action, Action::NextTab | Action::PrevTab))
                .then_some(Message::Shortcut(w, action))
        }),
        Mode::Blocked { palette_toggle } => shortcut.map(|action| {
            (palette_toggle && action == Action::Palette && !repeat)
                .then_some(Message::Shortcut(w, action))
        }),
        // Other shortcuts fall through to the layer beneath, which handles them.
        Mode::Palette => palette_key(key, mods).map(|m| Some(Message::Palette(w, m))),
        Mode::Prompt => match key {
            keyboard::Key::Named(Named::Escape) => {
                Some(Some(Message::Prompt(PromptMsg::Cancel(w))))
            }
            keyboard::Key::Named(Named::Enter) => Some(Some(Message::Prompt(PromptMsg::Submit(w)))),
            _ => shortcut.map(|_| None),
        },
        Mode::Editor => {
            if mods.command() && matches!(key, keyboard::Key::Character(c) if c.as_str() == "s") {
                return Some((!repeat).then_some(Message::Editor(EditorMsg::Save)));
            }
            shortcut.map(|action| match action {
                Action::CloseTab if !repeat => Some(Message::Editor(EditorMsg::Close)),
                Action::Quit | Action::Settings if !repeat => Some(Message::Shortcut(w, action)),
                _ => None,
            })
        }
        Mode::Settings => shortcut.map(|action| match action {
            Action::CloseTab if !repeat => Some(Message::Settings(SettingsMsg::Close)),
            Action::Quit | Action::EditProfiles if !repeat => Some(Message::Shortcut(w, action)),
            _ => None,
        }),
    }
}

impl Widget<Message, iced::Theme, iced::Renderer> for Shortcuts<'_> {
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
        if let Event::Keyboard(keyboard::Event::KeyPressed {
            key,
            modifiers,
            repeat,
            ..
        }) = event
            && let Some(msg) = key_message(self.mode, self.window, key, *modifiers, *repeat)
        {
            if let Some(msg) = msg {
                shell.publish(msg);
            }
            shell.capture_event();
            return;
        }
        if matches!(self.mode, Mode::Blocked { .. }) && matches!(event, Event::Keyboard(_)) {
            shell.capture_event();
            return;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(c: &str, shift: bool) -> (keyboard::Key, keyboard::Modifiers) {
        let mut mods = if cfg!(target_os = "macos") {
            keyboard::Modifiers::LOGO
        } else {
            keyboard::Modifiers::CTRL | keyboard::Modifiers::SHIFT
        };
        if shift {
            mods |= keyboard::Modifiers::SHIFT;
        }
        (keyboard::Key::Character(c.into()), mods)
    }

    fn press(mode: Mode, c: &str, shift: bool) -> Option<Option<Message>> {
        let (key, mods) = chord(c, shift);
        key_message(mode, window::Id::unique(), &key, mods, false)
    }

    #[test]
    fn under_the_palette_only_the_toggle_acts() {
        let blocked = Mode::Blocked {
            palette_toggle: true,
        };
        for c in ["w", "n", "t", "1", "9"] {
            assert!(
                matches!(press(blocked, c, false), Some(None)),
                "{c} must be swallowed behind the palette"
            );
        }
        assert!(matches!(
            press(blocked, "p", true),
            Some(Some(Message::Shortcut(_, Action::Palette)))
        ));
        assert!(matches!(
            press(Mode::Normal, "w", false),
            Some(Some(Message::Shortcut(_, Action::CloseTab)))
        ));
    }

    #[test]
    fn under_a_dialog_nothing_acts() {
        let blocked = Mode::Blocked {
            palette_toggle: false,
        };
        assert!(matches!(press(blocked, "p", true), Some(None)));
        assert!(matches!(press(Mode::Prompt, "w", false), Some(None)));
        let esc = keyboard::Key::Named(keyboard::key::Named::Escape);
        assert!(matches!(
            key_message(
                Mode::Prompt,
                window::Id::unique(),
                &esc,
                keyboard::Modifiers::empty(),
                false
            ),
            Some(Some(Message::Prompt(PromptMsg::Cancel(_))))
        ));
        // Plain typing reaches the dialog's field.
        let a = keyboard::Key::Character("a".into());
        assert!(
            key_message(
                Mode::Prompt,
                window::Id::unique(),
                &a,
                keyboard::Modifiers::empty(),
                false
            )
            .is_none()
        );
    }

    #[test]
    fn the_settings_and_editor_windows_close_on_close_tab_and_reach_each_other() {
        assert!(matches!(
            press(Mode::Settings, "w", false),
            Some(Some(Message::Settings(SettingsMsg::Close)))
        ));
        assert!(matches!(
            press(Mode::Settings, ",", true),
            Some(Some(Message::Shortcut(_, Action::EditProfiles)))
        ));
        assert!(matches!(press(Mode::Settings, "t", false), Some(None)));
        let (key, mods) = if cfg!(target_os = "macos") {
            (
                keyboard::Key::Character(",".into()),
                keyboard::Modifiers::LOGO,
            )
        } else {
            (
                keyboard::Key::Character(",".into()),
                keyboard::Modifiers::CTRL,
            )
        };
        assert!(matches!(
            key_message(Mode::Editor, window::Id::unique(), &key, mods, false),
            Some(Some(Message::Shortcut(_, Action::Settings)))
        ));
        assert!(matches!(
            key_message(Mode::Normal, window::Id::unique(), &key, mods, false),
            Some(Some(Message::Shortcut(_, Action::Settings)))
        ));
    }
}
