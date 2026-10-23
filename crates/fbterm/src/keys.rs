//! Application keyboard shortcuts: Cmd on macOS, Ctrl+Shift elsewhere.

use iced::keyboard::{Key, Modifiers};

/// What an application shortcut asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    /// Select tab `n` (1-based, 1..=9).
    SelectTab(u8),
    Palette,
    NewWindow,
    Settings,
    /// The profile editor: Cmd+, or Cmd+Shift+, on macOS, Ctrl+Shift+, elsewhere (the Settings
    /// chord until settings have a window).
    EditProfiles,
}

/// The shortcut for `key` (as pressed, without modifiers applied) on this platform.
pub fn shortcut(key: &Key, mods: Modifiers) -> Option<Action> {
    shortcut_for(cfg!(target_os = "macos"), key, mods)
}

/// The shortcut table, for macOS (`mac`) or the other platforms.
///
/// macOS chords use Cmd (with Shift where noted) and never Ctrl or Option. Elsewhere every chord
/// is Ctrl+Shift, so plain Ctrl keys keep reaching the terminal.
pub fn shortcut_for(mac: bool, key: &Key, mods: Modifiers) -> Option<Action> {
    let Key::Character(s) = key else {
        return None;
    };
    let mut chars = s.chars();
    let c = chars.next()?.to_ascii_lowercase();
    if chars.next().is_some() {
        return None;
    }
    let shift = mods.shift();
    if mac {
        if !mods.logo() || mods.control() || mods.alt() {
            return None;
        }
    } else if !(mods.control() && shift) || mods.alt() || mods.logo() {
        return None;
    }
    // On macOS Shift is part of the chord only where noted; elsewhere it is always held.
    let plain = !mac || !shift;
    let shifted = shift;
    match c {
        't' if plain => Some(Action::NewTab),
        'w' if plain => Some(Action::CloseTab),
        'n' if plain => Some(Action::NewWindow),
        // Settings has no window yet; its chord opens the profile editor for now.
        ',' | '<' => Some(Action::EditProfiles),
        'p' if shifted => Some(Action::Palette),
        ']' | '}' if shifted => Some(Action::NextTab),
        '[' | '{' if shifted => Some(Action::PrevTab),
        '1'..='9' if plain => Some(Action::SelectTab(c as u8 - b'0')),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> Key {
        Key::Character(s.into())
    }

    const CMD: Modifiers = Modifiers::LOGO;
    fn cmd_shift() -> Modifiers {
        Modifiers::LOGO | Modifiers::SHIFT
    }
    fn ctrl_shift() -> Modifiers {
        Modifiers::CTRL | Modifiers::SHIFT
    }

    #[test]
    fn macos_table() {
        let m = |key: &str, mods| shortcut_for(true, &k(key), mods);
        assert_eq!(m("t", CMD), Some(Action::NewTab));
        assert_eq!(m("w", CMD), Some(Action::CloseTab));
        assert_eq!(m("]", cmd_shift()), Some(Action::NextTab));
        assert_eq!(m("[", cmd_shift()), Some(Action::PrevTab));
        assert_eq!(m("}", cmd_shift()), Some(Action::NextTab));
        assert_eq!(m("{", cmd_shift()), Some(Action::PrevTab));
        for n in 1..=9u8 {
            assert_eq!(m(&n.to_string(), CMD), Some(Action::SelectTab(n)));
        }
        assert_eq!(m("p", cmd_shift()), Some(Action::Palette));
        assert_eq!(m("P", cmd_shift()), Some(Action::Palette));
        assert_eq!(m("n", CMD), Some(Action::NewWindow));
        // Settings has no window yet: both chords open the profile editor.
        assert_eq!(m(",", CMD), Some(Action::EditProfiles));
        assert_eq!(m(",", cmd_shift()), Some(Action::EditProfiles));
        assert_eq!(m("<", cmd_shift()), Some(Action::EditProfiles));
    }

    #[test]
    fn macos_rejects_other_chords() {
        let m = |key: &str, mods| shortcut_for(true, &k(key), mods);
        assert_eq!(m("t", Modifiers::empty()), None);
        assert_eq!(m("t", Modifiers::CTRL), None);
        assert_eq!(m("t", ctrl_shift()), None);
        assert_eq!(m("p", CMD), None, "palette needs Shift");
        assert_eq!(m("]", CMD), None, "next tab needs Shift");
        assert_eq!(m("t", CMD | Modifiers::ALT), None);
        assert_eq!(m("c", CMD), None, "copy belongs to the terminal");
        assert_eq!(m("0", CMD), None);
        assert_eq!(
            shortcut_for(true, &Key::Named(iced::keyboard::key::Named::Enter), CMD),
            None
        );
    }

    #[test]
    fn other_platforms_use_ctrl_shift() {
        let m = |key: &str, mods| shortcut_for(false, &k(key), mods);
        assert_eq!(m("t", ctrl_shift()), Some(Action::NewTab));
        assert_eq!(m("T", ctrl_shift()), Some(Action::NewTab));
        assert_eq!(m("w", ctrl_shift()), Some(Action::CloseTab));
        assert_eq!(m("]", ctrl_shift()), Some(Action::NextTab));
        assert_eq!(m("[", ctrl_shift()), Some(Action::PrevTab));
        for n in 1..=9u8 {
            assert_eq!(m(&n.to_string(), ctrl_shift()), Some(Action::SelectTab(n)));
        }
        assert_eq!(m("p", ctrl_shift()), Some(Action::Palette));
        assert_eq!(m("n", ctrl_shift()), Some(Action::NewWindow));
        assert_eq!(m(",", ctrl_shift()), Some(Action::EditProfiles));
        // Plain Ctrl chords belong to the shell (Ctrl+T, Ctrl+W, Ctrl+P ...).
        assert_eq!(m("t", Modifiers::CTRL), None);
        assert_eq!(m("w", Modifiers::CTRL), None);
        assert_eq!(m("t", CMD), None);
        assert_eq!(m("t", ctrl_shift() | Modifiers::ALT), None);
    }

    #[test]
    fn current_platform_matches_its_table() {
        let mods = if cfg!(target_os = "macos") {
            CMD
        } else {
            ctrl_shift()
        };
        assert_eq!(shortcut(&k("t"), mods), Some(Action::NewTab));
    }
}
