//! Application keyboard shortcuts: Cmd on macOS, Ctrl+Shift elsewhere (Settings is Ctrl+,, find
//! in all tabs Ctrl+Alt+F).

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
    /// Save the layout and exit: Cmd+Q on macOS, Ctrl+Shift+Q elsewhere.
    Quit,
    /// The profile editor: Cmd+Shift+, on macOS, Ctrl+Shift+, elsewhere.
    EditProfiles,
    /// The Settings window: Cmd+, on macOS, Ctrl+, elsewhere.
    Settings,
    /// Create `settings.toml` if missing and open it with the system's default app (palette only).
    OpenSettingsFile,
    /// Show the config folder in the file manager (palette only).
    RevealConfigFolder,
    /// The active tab's find bar: Cmd+F on macOS, Ctrl+Shift+F elsewhere.
    Find,
    /// The search over every tab: Cmd+Shift+F on macOS, Ctrl+Alt+F elsewhere.
    FindAll,
    /// Next match up the scrollback (older), while the find bar is open: Cmd+G on macOS,
    /// Ctrl+G elsewhere (see [`find_key`]).
    FindNext,
    /// Next match down the scrollback (newer), while the find bar is open: Cmd+Shift+G on
    /// macOS, Ctrl+Shift+G elsewhere.
    FindPrevious,
}

/// The shortcut for `key` (as pressed, without modifiers applied) on this platform. `text` is
/// what the key press types, if anything.
pub fn shortcut(key: &Key, mods: Modifiers, text: Option<&str>) -> Option<Action> {
    shortcut_typed(cfg!(target_os = "macos"), key, mods, text)
}

/// [`shortcut_for`], except that a Ctrl+Alt press that types a character is that character:
/// on Windows AltGr is Ctrl+Alt, and AltGr+F types `[` on several European layouts.
pub fn shortcut_typed(mac: bool, key: &Key, mods: Modifiers, text: Option<&str>) -> Option<Action> {
    let types = text.is_some_and(|t| t.chars().any(|c| !c.is_control()));
    if !mac && mods.control() && mods.alt() && types {
        return None;
    }
    shortcut_for(mac, key, mods)
}

/// The shortcut table, for macOS (`mac`) or the other platforms.
///
/// macOS chords use Cmd (with Shift where noted) and never Ctrl or Option. Elsewhere every chord
/// is Ctrl+Shift, so plain Ctrl keys keep reaching the terminal; the one exception is Ctrl+,
/// for Settings (the comma has no control code of its own).
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
    if !mac && c == ',' && mods.control() && !shift && !mods.alt() && !mods.logo() {
        return Some(Action::Settings);
    }
    if !mac && c == 'f' && mods.control() && mods.alt() && !shift && !mods.logo() {
        return Some(Action::FindAll);
    }
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
        'q' if plain => Some(Action::Quit),
        ',' if mac && !shift => Some(Action::Settings),
        ',' | '<' if shifted => Some(Action::EditProfiles),
        'p' if shifted => Some(Action::Palette),
        'f' if plain => Some(Action::Find),
        'f' if mac && shifted => Some(Action::FindAll),
        ']' | '}' if shifted => Some(Action::NextTab),
        '[' | '{' if shifted => Some(Action::PrevTab),
        '1'..='9' if plain => Some(Action::SelectTab(c as u8 - b'0')),
        _ => None,
    }
}

/// Find-bar navigation keys (only while the bar is open, so plain Ctrl+G still reaches the
/// shell otherwise): Cmd+G / Cmd+Shift+G on macOS, Ctrl+G / Ctrl+Shift+G elsewhere.
pub fn find_key(mac: bool, key: &Key, mods: Modifiers) -> Option<Action> {
    let Key::Character(s) = key else {
        return None;
    };
    if !s.eq_ignore_ascii_case("g") || mods.alt() {
        return None;
    }
    let held = if mac {
        mods.logo() && !mods.control()
    } else {
        mods.control() && !mods.logo()
    };
    match (held, mods.shift()) {
        (true, false) => Some(Action::FindNext),
        (true, true) => Some(Action::FindPrevious),
        _ => None,
    }
}

/// A chord as the user presses it: the key plus its modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    /// The key, lowercase for letters.
    pub key: char,
    pub ctrl: bool,
    /// Option on macOS.
    pub alt: bool,
    pub shift: bool,
    /// Cmd on macOS.
    pub cmd: bool,
}

/// The chord that triggers `action` on macOS (`mac`) or the other platforms, if it has one.
/// Kept in step with [`shortcut_for`] by the tests, so what the app shows is what works.
pub fn chord_for(mac: bool, action: Action) -> Option<Chord> {
    let (key, mac_shift) = match action {
        Action::NewTab => ('t', false),
        Action::CloseTab => ('w', false),
        Action::NewWindow => ('n', false),
        Action::Quit => ('q', false),
        Action::NextTab => (']', true),
        Action::PrevTab => ('[', true),
        Action::Palette => ('p', true),
        Action::SelectTab(n @ 1..=9) => (char::from(b'0' + n), false),
        Action::EditProfiles => (',', true),
        Action::Settings if !mac => {
            return Some(Chord {
                key: ',',
                ctrl: true,
                alt: false,
                shift: false,
                cmd: false,
            });
        }
        Action::Settings => (',', false),
        Action::Find => ('f', false),
        Action::FindAll if !mac => {
            return Some(Chord {
                key: 'f',
                ctrl: true,
                alt: true,
                shift: false,
                cmd: false,
            });
        }
        Action::FindAll => ('f', true),
        Action::FindNext | Action::FindPrevious => {
            let shift = action == Action::FindPrevious;
            return Some(Chord {
                key: 'g',
                ctrl: !mac,
                alt: false,
                shift,
                cmd: mac,
            });
        }
        Action::SelectTab(_) | Action::OpenSettingsFile | Action::RevealConfigFolder => {
            return None;
        }
    };
    Some(if mac {
        Chord {
            key,
            ctrl: false,
            alt: false,
            shift: mac_shift,
            cmd: true,
        }
    } else {
        Chord {
            key,
            ctrl: true,
            alt: false,
            shift: true,
            cmd: false,
        }
    })
}

impl Chord {
    /// How the chord is written: `⇧⌘,` on macOS, `Ctrl+Shift+T` elsewhere.
    pub fn label(self, mac: bool) -> String {
        let key = self.key.to_ascii_uppercase();
        if mac {
            let mut s = String::new();
            if self.ctrl {
                s.push('⌃');
            }
            if self.alt {
                s.push('⌥');
            }
            if self.shift {
                s.push('⇧');
            }
            if self.cmd {
                s.push('⌘');
            }
            s.push(key);
            s
        } else {
            let mut parts: Vec<String> = Vec::new();
            if self.ctrl {
                parts.push("Ctrl".into());
            }
            if self.alt {
                parts.push("Alt".into());
            }
            if self.shift {
                parts.push("Shift".into());
            }
            if self.cmd {
                parts.push("Super".into());
            }
            parts.push(key.to_string());
            parts.join("+")
        }
    }

    /// The modifiers held for this chord.
    #[cfg(test)]
    pub fn modifiers(self) -> Modifiers {
        let mut m = Modifiers::empty();
        if self.ctrl {
            m |= Modifiers::CTRL;
        }
        if self.alt {
            m |= Modifiers::ALT;
        }
        if self.shift {
            m |= Modifiers::SHIFT;
        }
        if self.cmd {
            m |= Modifiers::LOGO;
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Action; 24] = [
        Action::NewTab,
        Action::CloseTab,
        Action::NextTab,
        Action::PrevTab,
        Action::SelectTab(1),
        Action::SelectTab(2),
        Action::SelectTab(3),
        Action::SelectTab(4),
        Action::SelectTab(5),
        Action::SelectTab(6),
        Action::SelectTab(7),
        Action::SelectTab(8),
        Action::SelectTab(9),
        Action::Palette,
        Action::NewWindow,
        Action::Quit,
        Action::EditProfiles,
        Action::Settings,
        Action::OpenSettingsFile,
        Action::RevealConfigFolder,
        Action::Find,
        Action::FindAll,
        Action::FindNext,
        Action::FindPrevious,
    ];

    #[test]
    fn every_shown_chord_triggers_its_action() {
        for mac in [true, false] {
            for action in ALL {
                let Some(c) = chord_for(mac, action) else {
                    continue;
                };
                let key = Key::Character(c.key.to_string().into());
                let found = match action {
                    Action::FindNext | Action::FindPrevious => find_key(mac, &key, c.modifiers()),
                    _ => shortcut_for(mac, &key, c.modifiers()),
                };
                assert_eq!(found, Some(action), "{} (mac: {mac})", c.label(mac));
            }
        }
    }

    #[test]
    fn chord_labels_per_platform() {
        let l = |mac, a| chord_for(mac, a).map(|c| c.label(mac));
        assert_eq!(l(true, Action::NewTab).as_deref(), Some("⌘T"));
        assert_eq!(l(true, Action::CloseTab).as_deref(), Some("⌘W"));
        assert_eq!(l(true, Action::EditProfiles).as_deref(), Some("⇧⌘,"));
        assert_eq!(l(true, Action::Palette).as_deref(), Some("⇧⌘P"));
        assert_eq!(l(false, Action::NewTab).as_deref(), Some("Ctrl+Shift+T"));
        assert_eq!(l(false, Action::CloseTab).as_deref(), Some("Ctrl+Shift+W"));
        assert_eq!(
            l(false, Action::EditProfiles).as_deref(),
            Some("Ctrl+Shift+,")
        );
        assert_eq!(l(true, Action::Settings).as_deref(), Some("⌘,"));
        assert_eq!(l(false, Action::Settings).as_deref(), Some("Ctrl+,"));
        assert_eq!(l(true, Action::Find).as_deref(), Some("⌘F"));
        assert_eq!(l(true, Action::FindAll).as_deref(), Some("⇧⌘F"));
        assert_eq!(l(true, Action::FindNext).as_deref(), Some("⌘G"));
        assert_eq!(l(true, Action::FindPrevious).as_deref(), Some("⇧⌘G"));
        assert_eq!(l(false, Action::Find).as_deref(), Some("Ctrl+Shift+F"));
        assert_eq!(l(false, Action::FindAll).as_deref(), Some("Ctrl+Alt+F"));
        assert_eq!(l(false, Action::FindNext).as_deref(), Some("Ctrl+G"));
        assert_eq!(
            l(false, Action::FindPrevious).as_deref(),
            Some("Ctrl+Shift+G")
        );
        assert_eq!(l(true, Action::OpenSettingsFile), None);
        assert_eq!(l(false, Action::RevealConfigFolder), None);
    }

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
        assert_eq!(m("q", CMD), Some(Action::Quit));
        assert_eq!(m("q", cmd_shift()), None, "Cmd+Shift+Q is log out");
        assert_eq!(m(",", CMD), Some(Action::Settings));
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
        assert_eq!(m("q", ctrl_shift()), Some(Action::Quit));
        assert_eq!(m("q", Modifiers::CTRL), None);
        assert_eq!(m(",", ctrl_shift()), Some(Action::EditProfiles));
        assert_eq!(m("<", ctrl_shift()), Some(Action::EditProfiles));
        assert_eq!(m(",", Modifiers::CTRL), Some(Action::Settings));
        assert_eq!(m(",", Modifiers::CTRL | Modifiers::ALT), None);
        assert_eq!(m(",", CMD), None);
        // Plain Ctrl chords belong to the shell (Ctrl+T, Ctrl+W, Ctrl+P ...).
        assert_eq!(m("t", Modifiers::CTRL), None);
        assert_eq!(m("w", Modifiers::CTRL), None);
        assert_eq!(m("t", CMD), None);
        assert_eq!(m("t", ctrl_shift() | Modifiers::ALT), None);
    }

    #[test]
    fn find_chords_do_not_collide() {
        let m = |key: &str, mods| shortcut_for(true, &k(key), mods);
        assert_eq!(m("f", CMD), Some(Action::Find));
        assert_eq!(m("F", cmd_shift()), Some(Action::FindAll));
        assert_eq!(m("f", CMD | Modifiers::ALT), None);
        let o = |key: &str, mods| shortcut_for(false, &k(key), mods);
        assert_eq!(o("f", ctrl_shift()), Some(Action::Find));
        assert_eq!(o("F", ctrl_shift()), Some(Action::Find));
        assert_eq!(
            o("f", Modifiers::CTRL | Modifiers::ALT),
            Some(Action::FindAll)
        );
        assert_eq!(o("f", ctrl_shift() | Modifiers::ALT), None);
        // AltGr is Ctrl+Alt on Windows: AltGr+F typing "[" is that character, not the chord.
        let altgr = Modifiers::CTRL | Modifiers::ALT;
        assert_eq!(shortcut_typed(false, &k("f"), altgr, Some("[")), None);
        assert_eq!(
            shortcut_typed(false, &k("f"), altgr, None),
            Some(Action::FindAll)
        );
        assert_eq!(
            shortcut_typed(false, &k("f"), altgr, Some("\u{6}")),
            Some(Action::FindAll)
        );
        assert_eq!(
            shortcut_typed(true, &k("f"), CMD, Some("f")),
            Some(Action::Find)
        );
        // Plain Ctrl+F (forward a character) and Ctrl+G stay with the shell.
        assert_eq!(o("f", Modifiers::CTRL), None);
        assert_eq!(o("g", Modifiers::CTRL), None);
        assert_eq!(o("g", ctrl_shift()), None);
        // Every chord in the table is distinct.
        for mac in [true, false] {
            let chords: Vec<Chord> = ALL.iter().filter_map(|a| chord_for(mac, *a)).collect();
            for (i, a) in chords.iter().enumerate() {
                assert!(!chords[i + 1..].contains(a), "{} twice", a.label(mac));
            }
        }
        // Find navigation only answers its own chords.
        assert_eq!(find_key(true, &k("g"), CMD), Some(Action::FindNext));
        assert_eq!(
            find_key(true, &k("G"), cmd_shift()),
            Some(Action::FindPrevious)
        );
        assert_eq!(find_key(true, &k("g"), Modifiers::CTRL), None);
        assert_eq!(
            find_key(false, &k("g"), Modifiers::CTRL),
            Some(Action::FindNext)
        );
        assert_eq!(
            find_key(false, &k("G"), ctrl_shift()),
            Some(Action::FindPrevious)
        );
        assert_eq!(find_key(false, &k("h"), Modifiers::CTRL), None);
    }

    #[test]
    fn current_platform_matches_its_table() {
        let mods = if cfg!(target_os = "macos") {
            CMD
        } else {
            ctrl_shift()
        };
        assert_eq!(shortcut(&k("t"), mods, None), Some(Action::NewTab));
    }
}
