//! Application keyboard shortcuts: matching key presses against the binding table (the
//! platform defaults from `fbterm_config::keys` with the user's `[keys]` overrides).
//! Defaults: Cmd on macOS, Ctrl+Shift elsewhere (Settings is Ctrl+,, find in all tabs
//! Ctrl+Alt+F).

use iced::keyboard::key::Named;
use iced::keyboard::{Key, Modifiers};

pub use fbterm_config::keys::{Action, Bindings, Chord, ChordKey};

/// The chord a key press makes (`key` as pressed, without modifiers applied), or `None` for a
/// key that cannot be part of a shortcut (a modifier alone, a dead key, media keys ...).
pub fn press_chord(key: &Key, mods: Modifiers) -> Option<Chord> {
    let k = match key {
        Key::Character(s) => {
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            ChordKey::from_char(c)?
        }
        Key::Named(named) => named_key(*named)?,
        Key::Unidentified => return None,
    };
    Some(Chord::pressed(
        k,
        mods.control(),
        mods.alt(),
        mods.shift(),
        mods.logo(),
    ))
}

fn named_key(named: Named) -> Option<ChordKey> {
    Some(match named {
        Named::ArrowUp => ChordKey::Up,
        Named::ArrowDown => ChordKey::Down,
        Named::ArrowLeft => ChordKey::Left,
        Named::ArrowRight => ChordKey::Right,
        Named::PageUp => ChordKey::PageUp,
        Named::PageDown => ChordKey::PageDown,
        Named::Home => ChordKey::Home,
        Named::End => ChordKey::End,
        Named::Insert => ChordKey::Insert,
        Named::Delete => ChordKey::Delete,
        Named::Backspace => ChordKey::Backspace,
        Named::Space => ChordKey::Space,
        Named::Tab => ChordKey::Tab,
        Named::Enter => ChordKey::Enter,
        Named::Escape => ChordKey::Escape,
        Named::F1 => ChordKey::F(1),
        Named::F2 => ChordKey::F(2),
        Named::F3 => ChordKey::F(3),
        Named::F4 => ChordKey::F(4),
        Named::F5 => ChordKey::F(5),
        Named::F6 => ChordKey::F(6),
        Named::F7 => ChordKey::F(7),
        Named::F8 => ChordKey::F(8),
        Named::F9 => ChordKey::F(9),
        Named::F10 => ChordKey::F(10),
        Named::F11 => ChordKey::F(11),
        Named::F12 => ChordKey::F(12),
        Named::F13 => ChordKey::F(13),
        Named::F14 => ChordKey::F(14),
        Named::F15 => ChordKey::F(15),
        Named::F16 => ChordKey::F(16),
        Named::F17 => ChordKey::F(17),
        Named::F18 => ChordKey::F(18),
        Named::F19 => ChordKey::F(19),
        Named::F20 => ChordKey::F(20),
        Named::F21 => ChordKey::F(21),
        Named::F22 => ChordKey::F(22),
        Named::F23 => ChordKey::F(23),
        Named::F24 => ChordKey::F(24),
        _ => return None,
    })
}

/// Whether `key` is a modifier on its own (pressed while building a chord).
pub fn is_modifier(key: &Key) -> bool {
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
                | Named::Fn
                | Named::FnLock
                | Named::NumLock
                | Named::Symbol
                | Named::SymbolLock
        )
    )
}

/// The action a key press triggers under `bindings`, if any. `text` is what the press types,
/// if anything: off macOS a Ctrl+Alt press that types a character is that character, since
/// AltGr is Ctrl+Alt on Windows and AltGr+F types `[` on several European layouts.
///
/// This includes the find-bar keys ([`Action::is_find_navigation`]); callers act on those only
/// while the find bar is open.
pub fn shortcut(
    bindings: &Bindings,
    key: &Key,
    mods: Modifiers,
    text: Option<&str>,
) -> Option<Action> {
    let types = text.is_some_and(|t| t.chars().any(|c| !c.is_control()));
    if !bindings.mac() && mods.control() && mods.alt() && types {
        return None;
    }
    bindings.action(press_chord(key, mods)?)
}

/// The shortcut for `key` under the default bindings of macOS (`mac`) or the other platforms,
/// without the find-bar keys (see [`find_key`]).
#[cfg(test)]
pub fn shortcut_for(mac: bool, key: &Key, mods: Modifiers) -> Option<Action> {
    shortcut(&Bindings::defaults(mac), key, mods, None).filter(|a| !a.is_find_navigation())
}

/// [`shortcut_for`] with the AltGr rule of [`shortcut`].
#[cfg(test)]
pub fn shortcut_typed(mac: bool, key: &Key, mods: Modifiers, text: Option<&str>) -> Option<Action> {
    shortcut(&Bindings::defaults(mac), key, mods, text).filter(|a| !a.is_find_navigation())
}

/// The find-bar keys under the default bindings.
#[cfg(test)]
pub fn find_key(mac: bool, key: &Key, mods: Modifiers) -> Option<Action> {
    shortcut(&Bindings::defaults(mac), key, mods, None).filter(|a| a.is_find_navigation())
}

/// The default chord of `action`.
#[cfg(test)]
pub fn chord_for(mac: bool, action: Action) -> Option<Chord> {
    fbterm_config::keys::default_chord(mac, action)
}

/// The modifiers held for `chord`.
#[cfg(test)]
pub fn modifiers(chord: Chord) -> Modifiers {
    let mut m = Modifiers::empty();
    if chord.ctrl {
        m |= Modifiers::CTRL;
    }
    if chord.alt {
        m |= Modifiers::ALT;
    }
    if chord.shift {
        m |= Modifiers::SHIFT;
    }
    if chord.cmd {
        m |= Modifiers::LOGO;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Action; 42] = [
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
        Action::EditSchemes,
        Action::Tunnels,
        Action::FilePanel,
        Action::OpenSettingsFile,
        Action::RevealConfigFolder,
        Action::ExportTab,
        Action::Find,
        Action::FindAll,
        Action::FindNext,
        Action::FindPrevious,
        Action::SplitRight,
        Action::SplitDown,
        Action::FocusLeft,
        Action::FocusRight,
        Action::FocusUp,
        Action::FocusDown,
        Action::ZoomIn,
        Action::ZoomOut,
        Action::ZoomReset,
        Action::MaximizePane,
        Action::FindInTab,
        Action::MergeTab,
        Action::MoveTile,
        Action::TileToTab,
    ];

    #[test]
    fn every_shown_chord_triggers_its_action() {
        for mac in [true, false] {
            for action in ALL {
                let Some(c) = chord_for(mac, action) else {
                    continue;
                };
                let key = match c.key {
                    ChordKey::Char(ch) => Key::Character(ch.to_string().into()),
                    ChordKey::Enter => Key::Named(Named::Enter),
                    ChordKey::Left => Key::Named(Named::ArrowLeft),
                    ChordKey::Right => Key::Named(Named::ArrowRight),
                    ChordKey::Up => Key::Named(Named::ArrowUp),
                    ChordKey::Down => Key::Named(Named::ArrowDown),
                    other => panic!("unexpected default key {other:?}"),
                };
                let found = match action {
                    Action::FindNext | Action::FindPrevious => find_key(mac, &key, modifiers(c)),
                    _ => shortcut_for(mac, &key, modifiers(c)),
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
        assert_eq!(m("0", CMD), Some(Action::ZoomReset));
        assert_eq!(m("0", cmd_shift()), None);
        assert_eq!(m("d", CMD | Modifiers::ALT), None);
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
        let current = Bindings::current(&Default::default());
        assert_eq!(
            shortcut(&current, &k("t"), mods, None),
            Some(Action::NewTab)
        );
    }

    fn chord(s: &str) -> Chord {
        Chord::parse(s).unwrap().unwrap()
    }

    #[test]
    fn presses_become_chords() {
        use iced::keyboard::key::Named;
        assert_eq!(
            press_chord(&k("T"), cmd_shift()),
            Some(chord("cmd+shift+t"))
        );
        assert_eq!(
            press_chord(&k("}"), cmd_shift()),
            Some(chord("cmd+shift+]"))
        );
        assert_eq!(
            press_chord(&Key::Named(Named::F5), Modifiers::empty()),
            Some(chord("f5"))
        );
        assert_eq!(
            press_chord(&Key::Named(Named::ArrowLeft), Modifiers::ALT),
            Some(chord("alt+left"))
        );
        assert_eq!(
            press_chord(&Key::Named(Named::Space), Modifiers::CTRL),
            Some(chord("ctrl+space"))
        );
        assert_eq!(press_chord(&k("ab"), CMD), None);
        assert_eq!(press_chord(&Key::Named(Named::MediaPlay), CMD), None);
        assert!(is_modifier(&Key::Named(Named::Shift)));
        assert!(!is_modifier(&k("a")));
    }

    #[test]
    fn overrides_replace_defaults_with_the_same_matching_rules() {
        let mut o = fbterm_config::keys::Overrides::new();
        o.insert(Action::NextTab, Some(chord("ctrl+tab")));
        o.insert(Action::PrevTab, Some(chord("ctrl+shift+tab")));
        o.insert(Action::FindAll, Some(chord("ctrl+alt+g")));
        o.insert(Action::NewTab, None);
        o.insert(Action::OpenSettingsFile, Some(chord("ctrl+alt+shift+]")));
        let b = Bindings::new(false, &o);
        let tab = Key::Named(iced::keyboard::key::Named::Tab);
        assert_eq!(
            shortcut(&b, &tab, Modifiers::CTRL, None),
            Some(Action::NextTab)
        );
        assert_eq!(
            shortcut(&b, &tab, ctrl_shift(), None),
            Some(Action::PrevTab)
        );
        // The old chords are free again.
        assert_eq!(shortcut(&b, &k("]"), ctrl_shift(), None), None);
        assert_eq!(shortcut(&b, &k("t"), ctrl_shift(), None), None);
        // `}` with Shift is `]`, whichever the platform reports.
        assert_eq!(
            shortcut(&b, &k("}"), ctrl_shift() | Modifiers::ALT, None),
            Some(Action::OpenSettingsFile)
        );
        // AltGr typing a character is that character, for a rebound chord too.
        let altgr = Modifiers::CTRL | Modifiers::ALT;
        assert_eq!(shortcut(&b, &k("g"), altgr, Some("@")), None);
        assert_eq!(shortcut(&b, &k("g"), altgr, None), Some(Action::FindAll));
        assert_eq!(shortcut(&b, &k("f"), altgr, None), None);
    }

    #[test]
    fn layout_symbols_are_their_own_keys() {
        // On AZERTY the digit row types & é " ' ( - è _ ç à unshifted: Ctrl+Shift+& is not
        // tab 7 or tab 1, as before; and `!`, `$`, `*` (and German `#`) are not digits. (`+` is the
        // exception: with Shift it is `=`, zoom in.)
        for sym in ["&", "(", "*", "!", "$", "#", "_"] {
            assert_eq!(shortcut_for(false, &k(sym), ctrl_shift()), None, "{sym}");
            assert_eq!(shortcut_for(true, &k(sym), CMD), None, "{sym}");
        }
        // The F keys past 12 are keys too (with a modifier).
        let f13 = Key::Named(iced::keyboard::key::Named::F13);
        assert_eq!(press_chord(&f13, Modifiers::CTRL), Some(chord("ctrl+f13")));
    }

    #[test]
    fn pane_chords_match_on_both_platforms() {
        let ctrl_alt = Modifiers::CTRL | Modifiers::ALT;
        let cmd_alt = Modifiers::LOGO | Modifiers::ALT;
        let s = |mac, key: &Key, mods| shortcut_for(mac, key, mods);
        assert_eq!(s(true, &k("d"), CMD), Some(Action::SplitRight));
        assert_eq!(s(true, &k("d"), cmd_shift()), Some(Action::SplitDown));
        assert_eq!(s(false, &k("d"), ctrl_shift()), Some(Action::SplitRight));
        assert_eq!(
            s(false, &k("d"), ctrl_shift() | Modifiers::ALT),
            Some(Action::SplitDown)
        );
        assert_eq!(
            s(true, &Key::Named(Named::ArrowLeft), cmd_alt),
            Some(Action::FocusLeft)
        );
        assert_eq!(
            s(false, &Key::Named(Named::ArrowDown), ctrl_alt),
            Some(Action::FocusDown)
        );
        // Plain and Shift-only arrows belong to the program in the terminal.
        assert_eq!(
            s(false, &Key::Named(Named::ArrowLeft), Modifiers::CTRL),
            None
        );
        assert_eq!(s(true, &Key::Named(Named::ArrowLeft), CMD), None);
        // The plus key zooms in however it is typed.
        for mods in [CMD, cmd_shift()] {
            for key in ["=", "+"] {
                assert_eq!(
                    s(true, &k(key), mods),
                    Some(Action::ZoomIn),
                    "{key} {mods:?}"
                );
            }
        }
        // Off macOS: Shift+= (US), and a layout's own plus key with or without Shift.
        for (key, mods) in [
            ("=", ctrl_shift()),
            ("+", ctrl_shift()),
            ("+", Modifiers::CTRL),
        ] {
            assert_eq!(
                s(false, &k(key), mods),
                Some(Action::ZoomIn),
                "{key} {mods:?}"
            );
        }
        assert_eq!(
            s(false, &k("="), Modifiers::CTRL),
            None,
            "Ctrl+= is the program's"
        );
        assert_eq!(s(true, &k("="), Modifiers::CTRL), None);
        assert_eq!(s(true, &k("="), cmd_shift() | Modifiers::ALT), None);
        // The label is the default chord's own.
        assert_eq!(chord_for(true, Action::ZoomIn).unwrap().label(true), "⌘=");
        assert_eq!(
            chord_for(false, Action::ZoomIn).unwrap().label(false),
            "Ctrl+Shift+="
        );
        // A rebound zoom in is only that chord: the plus key no longer zooms.
        let mut o = fbterm_config::keys::Overrides::new();
        o.insert(Action::ZoomIn, Chord::parse("cmd+alt+z").unwrap());
        let b = Bindings::new(true, &o);
        assert_eq!(shortcut(&b, &k("="), CMD, None), None);
        assert_eq!(shortcut(&b, &k("z"), cmd_alt, None), Some(Action::ZoomIn));
        assert_eq!(s(true, &k("-"), CMD), Some(Action::ZoomOut));
        assert_eq!(s(false, &k("0"), ctrl_shift()), Some(Action::ZoomReset));
        assert_eq!(
            s(true, &Key::Named(Named::Enter), cmd_shift()),
            Some(Action::MaximizePane)
        );
        assert_eq!(
            s(false, &Key::Named(Named::Enter), ctrl_shift()),
            Some(Action::MaximizePane)
        );
    }
}
