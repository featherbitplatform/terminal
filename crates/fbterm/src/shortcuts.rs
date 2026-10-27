//! Keys before content: a wrapper that turns application shortcuts into messages for each
//! kind of window and overlay, before a terminal or a field sees the key.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Size, Vector, keyboard, mouse, window};

use crate::app::Message;
use crate::editor::EditorMsg;
use crate::find::FindMsg;
use crate::find::all::FindAllMsg;
use crate::keys::{self, Action, Bindings};
use crate::palette::PaletteMsg;
use crate::prompts::PromptMsg;
use crate::schemes_window::{ImportChoice, SchemesMsg};
use crate::settings_window::SettingsMsg;
use crate::tunnels_window::TunnelsMsg;

/// How a [`Shortcuts`] wrapper treats keys.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Application shortcuts act; other keys pass on.
    Normal,
    /// Beneath an overlay: every key is dropped, so nothing reaches the terminal and no
    /// shortcut acts behind the overlay, except the palette and find-in-all-tabs toggles when
    /// `toggles`.
    Blocked { toggles: bool },
    /// The palette's own: Up/Down, Ctrl+P/N, Enter and Escape drive it; the rest goes on to the
    /// search box.
    Palette,
    /// The find-in-all-tabs box: like the palette.
    FindAll,
    /// A window whose active tab has its find bar open: Enter / Shift+Enter and the find keys
    /// move between matches, Escape closes the bar, application shortcuts act, and typing goes
    /// to the bar's field, never to the terminal (the terminal is drawn unfocused).
    Find,
    /// A connection dialog: Escape cancels, Enter submits; other keys go to its field.
    Prompt,
    /// The profile editor window: Cmd+W closes it, Cmd+S saves, Settings and Quit work; other
    /// shortcuts do nothing.
    Editor,
    /// The Settings window: Cmd+W closes it, the profile editor and Quit work; other shortcuts
    /// do nothing.
    Settings,
    /// The Color schemes window: Cmd+W closes it, Cmd+S saves, Settings, the profile editor and
    /// Quit work; other shortcuts do nothing.
    Schemes,
    /// The Color schemes window asking whether to delete a scheme: Enter deletes, Escape
    /// cancels, and no other key reaches a field behind the question or acts as a shortcut.
    SchemesConfirm,
    /// The Color schemes window asking what to do with an imported scheme whose id is taken:
    /// Enter keeps both, Escape cancels, and no other key reaches a field behind the question
    /// or acts as a shortcut.
    SchemesImport,
    /// The Tunnels window: Cmd+W closes it, Settings, the profile editor, Color schemes and
    /// Quit work; other shortcuts do nothing (typing goes to the search bar).
    Tunnels,
    /// The Settings window recording a new chord: every key press goes to the recorder (plain
    /// Escape cancels), none reaches a field or acts as a shortcut.
    Recording,
}

/// The palette message a key means while the palette is up.
fn palette_key(key: &keyboard::Key, mods: keyboard::Modifiers) -> Option<PaletteMsg> {
    use keyboard::key::Named;
    match key {
        keyboard::Key::Named(Named::ArrowUp) => Some(PaletteMsg::Up),
        keyboard::Key::Named(Named::ArrowDown) => Some(PaletteMsg::Down),
        keyboard::Key::Named(Named::Enter) => Some(PaletteMsg::Submit {
            split: mods.shift(),
        }),
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

/// The find-in-all-tabs message a key means while the box is up.
fn find_all_key(key: &keyboard::Key, mods: keyboard::Modifiers) -> Option<FindAllMsg> {
    palette_key(key, mods).map(|m| match m {
        PaletteMsg::Up => FindAllMsg::Up,
        PaletteMsg::Down => FindAllMsg::Down,
        PaletteMsg::Submit { .. } => FindAllMsg::Submit,
        _ => FindAllMsg::Close,
    })
}

/// The find-bar message a key means while the bar is open.
fn find_bar_key(
    bindings: &Bindings,
    key: &keyboard::Key,
    mods: keyboard::Modifiers,
    text: Option<&str>,
) -> Option<FindMsg> {
    use keyboard::key::Named;
    match keys::shortcut(bindings, key, mods, text) {
        Some(Action::FindNext) => return Some(FindMsg::Next),
        Some(Action::FindPrevious) => return Some(FindMsg::Previous),
        _ => {}
    }
    match key {
        keyboard::Key::Named(Named::Escape) => Some(FindMsg::Close),
        keyboard::Key::Named(Named::Enter) if mods.shift() => Some(FindMsg::Previous),
        keyboard::Key::Named(Named::Enter) => Some(FindMsg::Next),
        _ => None,
    }
}

/// Wraps a window's content and turns application shortcuts into messages before the terminal
/// sees the key, so Ctrl+Shift chords never reach the shell.
pub(crate) struct Shortcuts<'a> {
    pub content: Element<'a, Message>,
    pub window: window::Id,
    pub mode: Mode,
    pub bindings: &'a Bindings,
}

/// What a key press means to a [`Shortcuts`] wrapper in `mode` over window `w`:
/// `Some(Some(msg))` publish and stop it, `Some(None)` stop it, `None` let it through.
fn key_message(
    bindings: &Bindings,
    mode: Mode,
    w: window::Id,
    key: &keyboard::Key,
    mods: keyboard::Modifiers,
    repeat: bool,
    text: Option<&str>,
) -> Option<Option<Message>> {
    use keyboard::key::Named;
    // The find-bar keys act only in the bar (`Mode::Find`); elsewhere they reach the terminal.
    let shortcut = keys::shortcut(bindings, key, mods, text).filter(|a| !a.is_find_navigation());
    match mode {
        Mode::Normal => shortcut.map(|action| {
            // Holding a key repeats tab switching and zooming only.
            (!repeat
                || matches!(
                    action,
                    Action::NextTab | Action::PrevTab | Action::ZoomIn | Action::ZoomOut
                ))
            .then_some(Message::Shortcut(w, action))
        }),
        Mode::Blocked { toggles } => shortcut.map(|action| {
            (toggles && matches!(action, Action::Palette | Action::FindAll) && !repeat)
                .then_some(Message::Shortcut(w, action))
        }),
        // Other shortcuts fall through to the layer beneath, which handles them.
        Mode::Palette => palette_key(key, mods).map(|m| Some(Message::Palette(w, m))),
        Mode::FindAll => find_all_key(key, mods).map(|m| Some(Message::FindAll(w, m))),
        Mode::Find => match find_bar_key(bindings, key, mods, text) {
            Some(m) => Some(Some(Message::Find(w, m))),
            None => key_message(bindings, Mode::Normal, w, key, mods, repeat, text),
        },
        Mode::Prompt => match key {
            keyboard::Key::Named(Named::Escape) => {
                Some(Some(Message::Prompt(PromptMsg::Cancel(w))))
            }
            keyboard::Key::Named(Named::Enter) => Some(Some(Message::Prompt(PromptMsg::Submit(w)))),
            _ => shortcut.map(|_| None),
        },
        Mode::Editor => {
            // Exactly Cmd+S (Ctrl+S elsewhere), the chord `keys::reserved` keeps for Save: with
            // Shift or Alt added it is a chord like any other, free to be bound.
            if mods == keyboard::Modifiers::COMMAND
                && matches!(key, keyboard::Key::Character(c) if c.eq_ignore_ascii_case("s"))
            {
                return Some((!repeat).then_some(Message::Editor(EditorMsg::Save)));
            }
            shortcut.map(|action| match action {
                Action::CloseTab if !repeat => Some(Message::Editor(EditorMsg::Close)),
                Action::Quit | Action::Settings | Action::EditSchemes | Action::Tunnels
                    if !repeat =>
                {
                    Some(Message::Shortcut(w, action))
                }
                _ => None,
            })
        }
        Mode::Settings => shortcut.map(|action| match action {
            Action::CloseTab if !repeat => Some(Message::Settings(SettingsMsg::Close)),
            Action::Quit | Action::EditProfiles | Action::EditSchemes | Action::Tunnels
                if !repeat =>
            {
                Some(Message::Shortcut(w, action))
            }
            _ => None,
        }),
        Mode::Schemes => {
            if mods == keyboard::Modifiers::COMMAND
                && matches!(key, keyboard::Key::Character(c) if c.eq_ignore_ascii_case("s"))
            {
                return Some((!repeat).then_some(Message::Schemes(SchemesMsg::Save)));
            }
            shortcut.map(|action| match action {
                Action::CloseTab if !repeat => Some(Message::Schemes(SchemesMsg::Close)),
                Action::Quit | Action::Settings | Action::EditProfiles | Action::Tunnels
                    if !repeat =>
                {
                    Some(Message::Shortcut(w, action))
                }
                _ => None,
            })
        }
        Mode::Tunnels => shortcut.map(|action| match action {
            Action::CloseTab if !repeat => Some(Message::Tunnels(TunnelsMsg::Close)),
            Action::Quit | Action::Settings | Action::EditProfiles | Action::EditSchemes
                if !repeat =>
            {
                Some(Message::Shortcut(w, action))
            }
            _ => None,
        }),
        Mode::SchemesConfirm => Some(match key {
            keyboard::Key::Named(Named::Escape) if !repeat => {
                Some(Message::Schemes(SchemesMsg::CancelDelete))
            }
            keyboard::Key::Named(Named::Enter) if !repeat => {
                Some(Message::Schemes(SchemesMsg::ConfirmDelete))
            }
            _ => None,
        }),
        Mode::SchemesImport => Some(match key {
            keyboard::Key::Named(Named::Escape) if !repeat => Some(Message::Schemes(
                SchemesMsg::ImportChoice(ImportChoice::Cancel),
            )),
            keyboard::Key::Named(Named::Enter) if !repeat => Some(Message::Schemes(
                SchemesMsg::ImportChoice(ImportChoice::KeepBoth),
            )),
            _ => None,
        }),
        Mode::Recording => Some(recording_key(key, mods, repeat)),
    }
}

/// What a key press means to the chord recorder: plain Escape cancels, a modifier alone or a
/// held key does nothing yet, anything else is the chord (or a key that cannot be one).
fn recording_key(key: &keyboard::Key, mods: keyboard::Modifiers, repeat: bool) -> Option<Message> {
    use keyboard::key::Named;
    if repeat || keys::is_modifier(key) {
        return None;
    }
    if matches!(key, keyboard::Key::Named(Named::Escape)) && mods.is_empty() {
        return Some(Message::Settings(SettingsMsg::CancelRecording));
    }
    Some(Message::Settings(SettingsMsg::Pressed(keys::press_chord(
        key, mods,
    ))))
}

/// Printable text a key press types (no Cmd, Ctrl or Alt held).
fn typed_text(event: &Event) -> Option<String> {
    let Event::Keyboard(keyboard::Event::KeyPressed {
        text: Some(text),
        modifiers,
        ..
    }) = event
    else {
        return None;
    };
    if modifiers.command() || modifiers.control() || modifiers.alt() || modifiers.logo() {
        return None;
    }
    let text = text.to_string();
    text.chars()
        .all(|c| !c.is_control())
        .then_some(text)
        .filter(|t| !t.is_empty())
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
            text,
            ..
        }) = event
            && let Some(msg) = key_message(
                self.bindings,
                self.mode,
                self.window,
                key,
                *modifiers,
                *repeat,
                text.as_deref(),
            )
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
        // Typing the bar's field did not take (it lost focus to a click): the bar takes it.
        if self.mode == Mode::Find
            && !shell.is_event_captured()
            && let Some(text) = typed_text(event)
        {
            shell.publish(Message::Find(self.window, FindMsg::Type(text)));
            shell.capture_event();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Bindings {
        Bindings::current(&Default::default())
    }

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
        key_message(
            &defaults(),
            mode,
            window::Id::unique(),
            &key,
            mods,
            false,
            None,
        )
    }

    #[test]
    fn under_the_palette_only_the_toggle_acts() {
        let blocked = Mode::Blocked { toggles: true };
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
        let blocked = Mode::Blocked { toggles: false };
        assert!(matches!(press(blocked, "p", true), Some(None)));
        assert!(matches!(press(Mode::Prompt, "w", false), Some(None)));
        let esc = keyboard::Key::Named(keyboard::key::Named::Escape);
        assert!(matches!(
            key_message(
                &defaults(),
                Mode::Prompt,
                window::Id::unique(),
                &esc,
                keyboard::Modifiers::empty(),
                false,
                None
            ),
            Some(Some(Message::Prompt(PromptMsg::Cancel(_))))
        ));
        // Plain typing reaches the dialog's field.
        let a = keyboard::Key::Character("a".into());
        assert!(
            key_message(
                &defaults(),
                Mode::Prompt,
                window::Id::unique(),
                &a,
                keyboard::Modifiers::empty(),
                false,
                None
            )
            .is_none()
        );
    }

    fn key(
        named: keyboard::key::Named,
        mods: keyboard::Modifiers,
        mode: Mode,
    ) -> Option<Option<Message>> {
        key_message(
            &defaults(),
            mode,
            window::Id::unique(),
            &keyboard::Key::Named(named),
            mods,
            false,
            None,
        )
    }

    #[test]
    fn the_find_bar_takes_enter_escape_and_the_find_keys() {
        use keyboard::key::Named;
        let none = keyboard::Modifiers::empty();
        assert!(matches!(
            key(Named::Enter, none, Mode::Find),
            Some(Some(Message::Find(_, FindMsg::Next)))
        ));
        assert!(matches!(
            key(Named::Enter, keyboard::Modifiers::SHIFT, Mode::Find),
            Some(Some(Message::Find(_, FindMsg::Previous)))
        ));
        assert!(matches!(
            key(Named::Escape, none, Mode::Find),
            Some(Some(Message::Find(_, FindMsg::Close)))
        ));
        let (g, mods) = if cfg!(target_os = "macos") {
            ("g", keyboard::Modifiers::LOGO)
        } else {
            ("g", keyboard::Modifiers::CTRL)
        };
        let g = keyboard::Key::Character(g.into());
        assert!(matches!(
            key_message(
                &defaults(),
                Mode::Find,
                window::Id::unique(),
                &g,
                mods,
                true,
                None
            ),
            Some(Some(Message::Find(_, FindMsg::Next)))
        ));
        assert!(matches!(
            key_message(
                &defaults(),
                Mode::Find,
                window::Id::unique(),
                &g,
                mods | keyboard::Modifiers::SHIFT,
                false,
                None
            ),
            Some(Some(Message::Find(_, FindMsg::Previous)))
        ));
        // Outside the bar, Ctrl+G is the shell's.
        if !cfg!(target_os = "macos") {
            assert!(
                key_message(
                    &defaults(),
                    Mode::Normal,
                    window::Id::unique(),
                    &g,
                    mods,
                    false,
                    None
                )
                .is_none()
            );
        }
        // Application shortcuts still act; plain typing goes on to the bar's field.
        assert!(matches!(
            press(Mode::Find, "t", false),
            Some(Some(Message::Shortcut(_, Action::NewTab)))
        ));
        let a = keyboard::Key::Character("a".into());
        assert!(
            key_message(
                &defaults(),
                Mode::Find,
                window::Id::unique(),
                &a,
                none,
                false,
                None
            )
            .is_none()
        );
        // Typed text is recognised for the bar; chords are not.
        let typed = |text: &str, modifiers| {
            typed_text(&Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Character(text.into()),
                modified_key: keyboard::Key::Character(text.into()),
                physical_key: keyboard::key::Physical::Code(keyboard::key::Code::KeyA),
                location: keyboard::Location::Standard,
                modifiers,
                text: Some(text.into()),
                repeat: false,
            }))
        };
        assert_eq!(typed("a", none).as_deref(), Some("a"));
        assert_eq!(typed("a", keyboard::Modifiers::CTRL), None);
        assert_eq!(typed("\u{8}", none), None);
    }

    #[test]
    fn shift_enter_in_the_palette_asks_for_a_split() {
        assert!(matches!(
            key(
                keyboard::key::Named::Enter,
                keyboard::Modifiers::SHIFT,
                Mode::Palette
            ),
            Some(Some(Message::Palette(
                _,
                PaletteMsg::Submit { split: true }
            )))
        ));
        assert!(matches!(
            key(
                keyboard::key::Named::Enter,
                keyboard::Modifiers::empty(),
                Mode::Palette
            ),
            Some(Some(Message::Palette(
                _,
                PaletteMsg::Submit { split: false }
            )))
        ));
    }

    #[test]
    fn the_find_all_box_is_driven_like_the_palette_and_toggles_from_beneath() {
        use keyboard::key::Named;
        let none = keyboard::Modifiers::empty();
        assert!(matches!(
            key(Named::ArrowDown, none, Mode::FindAll),
            Some(Some(Message::FindAll(_, FindAllMsg::Down)))
        ));
        assert!(matches!(
            key(Named::Enter, none, Mode::FindAll),
            Some(Some(Message::FindAll(_, FindAllMsg::Submit)))
        ));
        assert!(matches!(
            key(Named::Escape, none, Mode::FindAll),
            Some(Some(Message::FindAll(_, FindAllMsg::Close)))
        ));
        let (f, mods) = if cfg!(target_os = "macos") {
            ("f", keyboard::Modifiers::LOGO | keyboard::Modifiers::SHIFT)
        } else {
            ("f", keyboard::Modifiers::CTRL | keyboard::Modifiers::ALT)
        };
        let f = keyboard::Key::Character(f.into());
        assert!(matches!(
            key_message(
                &defaults(),
                Mode::Blocked { toggles: true },
                window::Id::unique(),
                &f,
                mods,
                false,
                None
            ),
            Some(Some(Message::Shortcut(_, Action::FindAll)))
        ));
        assert!(matches!(
            key_message(
                &defaults(),
                Mode::Blocked { toggles: false },
                window::Id::unique(),
                &f,
                mods,
                false,
                None
            ),
            Some(None)
        ));
    }

    #[test]
    fn altgr_typing_a_character_reaches_the_terminal() {
        let altgr = keyboard::Modifiers::CTRL | keyboard::Modifiers::ALT;
        let f = keyboard::Key::Character("f".into());
        let w = window::Id::unique();
        // AltGr+F types "[": nothing intercepts it, so the terminal gets the character.
        assert!(key_message(&defaults(), Mode::Normal, w, &f, altgr, false, Some("[")).is_none());
        assert!(key_message(&defaults(), Mode::Find, w, &f, altgr, false, Some("[")).is_none());
        if !cfg!(target_os = "macos") {
            assert!(matches!(
                key_message(&defaults(), Mode::Normal, w, &f, altgr, false, None),
                Some(Some(Message::Shortcut(_, Action::FindAll)))
            ));
        }
    }

    #[test]
    fn the_delete_question_takes_every_key() {
        let named = |n| keyboard::Key::Named(n);
        let at = |key: keyboard::Key| {
            key_message(
                &defaults(),
                Mode::SchemesConfirm,
                window::Id::unique(),
                &key,
                keyboard::Modifiers::empty(),
                false,
                None,
            )
        };
        use keyboard::key::Named;
        assert!(matches!(
            at(named(Named::Escape)),
            Some(Some(Message::Schemes(SchemesMsg::CancelDelete)))
        ));
        assert!(matches!(
            at(named(Named::Enter)),
            Some(Some(Message::Schemes(SchemesMsg::ConfirmDelete)))
        ));
        assert!(matches!(
            at(keyboard::Key::Character("a".into())),
            Some(None)
        ));
        assert!(matches!(
            press(Mode::SchemesConfirm, "w", false),
            Some(None)
        ));
    }

    #[test]
    fn the_import_question_takes_enter_and_escape_only() {
        let at = |key: keyboard::Key| {
            key_message(
                &defaults(),
                Mode::SchemesImport,
                window::Id::unique(),
                &key,
                keyboard::Modifiers::empty(),
                false,
                None,
            )
        };
        use keyboard::key::Named;
        let named = |n| keyboard::Key::Named(n);
        assert!(matches!(
            at(named(Named::Escape)),
            Some(Some(Message::Schemes(SchemesMsg::ImportChoice(
                ImportChoice::Cancel
            ))))
        ));
        assert!(matches!(
            at(named(Named::Enter)),
            Some(Some(Message::Schemes(SchemesMsg::ImportChoice(
                ImportChoice::KeepBoth
            ))))
        ));
        assert!(matches!(
            at(keyboard::Key::Character("a".into())),
            Some(None)
        ));
        assert!(matches!(press(Mode::SchemesImport, "w", false), Some(None)));
    }

    #[test]
    fn the_tunnels_window_closes_and_reaches_the_others() {
        assert!(matches!(
            press(Mode::Tunnels, "w", false),
            Some(Some(Message::Tunnels(TunnelsMsg::Close)))
        ));
        assert!(matches!(
            press(Mode::Tunnels, ",", true),
            Some(Some(Message::Shortcut(_, Action::EditProfiles)))
        ));
        assert!(matches!(press(Mode::Tunnels, "t", false), Some(None)));
    }

    #[test]
    fn the_schemes_window_closes_saves_and_reaches_the_others() {
        assert!(matches!(
            press(Mode::Schemes, "w", false),
            Some(Some(Message::Schemes(SchemesMsg::Close)))
        ));
        assert!(matches!(
            press(Mode::Schemes, ",", true),
            Some(Some(Message::Shortcut(_, Action::EditProfiles)))
        ));
        assert!(matches!(press(Mode::Schemes, "t", false), Some(None)));
        let s = keyboard::Key::Character("s".into());
        assert!(matches!(
            key_message(
                &defaults(),
                Mode::Schemes,
                window::Id::unique(),
                &s,
                keyboard::Modifiers::COMMAND,
                false,
                None
            ),
            Some(Some(Message::Schemes(SchemesMsg::Save)))
        ));
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
            key_message(
                &defaults(),
                Mode::Editor,
                window::Id::unique(),
                &key,
                mods,
                false,
                None
            ),
            Some(Some(Message::Shortcut(_, Action::Settings)))
        ));
        assert!(matches!(
            key_message(
                &defaults(),
                Mode::Normal,
                window::Id::unique(),
                &key,
                mods,
                false,
                None
            ),
            Some(Some(Message::Shortcut(_, Action::Settings)))
        ));
    }

    #[test]
    fn the_editor_saves_on_exactly_cmd_s() {
        use keyboard::Modifiers as M;
        let w = window::Id::unique();
        let s = keyboard::Key::Character("s".into());
        let save = |b: &Bindings, mods: M| {
            matches!(
                key_message(b, Mode::Editor, w, &s, mods, false, None),
                Some(Some(Message::Editor(EditorMsg::Save)))
            )
        };
        let b = defaults();
        assert!(save(&b, M::COMMAND));
        assert!(!save(&b, M::COMMAND | M::SHIFT));
        assert!(!save(&b, M::COMMAND | M::ALT));
        assert!(!save(&b, M::empty()));
        // A chord bound to Cmd+Shift+S (Ctrl+Shift+S elsewhere) is not taken for Save.
        let mut overrides = fbterm_config::keys::Overrides::new();
        let chord = if cfg!(target_os = "macos") {
            "cmd+shift+s"
        } else {
            "ctrl+shift+s"
        };
        overrides.insert(Action::Settings, Some(parse_chord(chord)));
        let b = Bindings::current(&overrides);
        assert!(matches!(
            key_message(&b, Mode::Editor, w, &s, M::COMMAND | M::SHIFT, false, None),
            Some(Some(Message::Shortcut(_, Action::Settings)))
        ));
        assert!(save(&b, M::COMMAND));
    }

    fn parse_chord(s: &str) -> keys::Chord {
        keys::Chord::parse(s).unwrap().unwrap()
    }

    #[test]
    fn rebound_chords_act_and_the_old_ones_reach_the_terminal() {
        let mut overrides = fbterm_config::keys::Overrides::new();
        overrides.insert(Action::NewTab, Some(parse_chord("f5")));
        overrides.insert(Action::CloseTab, None);
        overrides.insert(Action::FindNext, Some(parse_chord("alt+n")));
        let b = Bindings::current(&overrides);
        let w = window::Id::unique();
        let f5 = keyboard::Key::Named(keyboard::key::Named::F5);
        let none = keyboard::Modifiers::empty();
        assert!(matches!(
            key_message(&b, Mode::Normal, w, &f5, none, false, None),
            Some(Some(Message::Shortcut(_, Action::NewTab)))
        ));
        let (key, mods) = chord_keys("t", false);
        assert!(key_message(&b, Mode::Normal, w, &key, mods, false, None).is_none());
        let (key, mods) = chord_keys("w", false);
        assert!(key_message(&b, Mode::Normal, w, &key, mods, false, None).is_none());
        // The Settings window closes on Close tab's chord: unbound, it does nothing there.
        assert!(key_message(&b, Mode::Settings, w, &key, mods, false, None).is_none());
        // Find navigation acts in the bar only.
        let n = keyboard::Key::Character("n".into());
        let alt = keyboard::Modifiers::ALT;
        assert!(key_message(&b, Mode::Normal, w, &n, alt, false, None).is_none());
        assert!(matches!(
            key_message(&b, Mode::Find, w, &n, alt, false, None),
            Some(Some(Message::Find(_, FindMsg::Next)))
        ));
    }

    fn chord_keys(c: &str, shift: bool) -> (keyboard::Key, keyboard::Modifiers) {
        chord(c, shift)
    }

    #[test]
    fn recording_takes_every_key_and_escape_cancels() {
        use keyboard::key::Named;
        let b = defaults();
        let w = window::Id::unique();
        let none = keyboard::Modifiers::empty();
        let esc = keyboard::Key::Named(Named::Escape);
        assert!(matches!(
            key_message(&b, Mode::Recording, w, &esc, none, false, None),
            Some(Some(Message::Settings(SettingsMsg::CancelRecording)))
        ));
        // A shortcut is recorded, not run.
        let (key, mods) = chord_keys("w", false);
        let Some(Some(Message::Settings(SettingsMsg::Pressed(Some(c))))) =
            key_message(&b, Mode::Recording, w, &key, mods, false, None)
        else {
            panic!("the press is recorded");
        };
        assert_eq!(b.action(c), Some(Action::CloseTab));
        // Plain typing is captured too (and rejected by the recorder), never typed in a field.
        let a = keyboard::Key::Character("a".into());
        assert!(matches!(
            key_message(&b, Mode::Recording, w, &a, none, false, None),
            Some(Some(Message::Settings(SettingsMsg::Pressed(Some(_)))))
        ));
        // A modifier alone or a held key waits.
        let shift = keyboard::Key::Named(Named::Shift);
        assert!(matches!(
            key_message(
                &b,
                Mode::Recording,
                w,
                &shift,
                keyboard::Modifiers::SHIFT,
                false,
                None
            ),
            Some(None)
        ));
        assert!(matches!(
            key_message(&b, Mode::Recording, w, &a, none, true, None),
            Some(None)
        ));
    }
}
