//! Key encoding for the kitty keyboard protocol (progressive enhancement).
//!
//! Used only when the program has pushed enhancement flags, which the emulator accepts only when
//! the `kitty_keyboard` setting is on. Pure: everything it needs arrives as arguments.

use iced::keyboard::key::{Code, Named, Physical};
use iced::keyboard::{Key, Location, Modifiers};

use crate::TermMode;
use crate::input::{KeyAction, KeyInput};

/// What kind of key this is, and how it is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A text-producing key: `CSI code u`, plain text when unambiguous.
    Text(u32),
    /// Escape, Enter, Tab, Backspace: `CSI code u`.
    Control(u32),
    /// Cursor keys, Home, End, F1, F2, F4: `CSI 1 ; mods X`.
    Letter(u8),
    /// Insert, Delete, PageUp, PageDown, F3, F5-F12: `CSI n ; mods ~`.
    Tilde(u32),
    /// Keys without a legacy encoding (F13+, keypad, media): `CSI code u`.
    Functional(u32),
    /// Modifier and lock keys: reported only with "report all keys as escape codes".
    Modifier(u32),
}

fn letters() -> [Code; 26] {
    use Code::*;
    [
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO,
        KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
    ]
}

/// The key at this position on a standard US PC-101 layout (the protocol's "base layout key").
fn base_layout_key(physical: Physical) -> Option<char> {
    let Physical::Code(code) = physical else {
        return None;
    };
    if let Some(i) = letters().iter().position(|c| *c == code) {
        return Some((b'a' + i as u8) as char);
    }
    Some(match code {
        Code::Digit0 => '0',
        Code::Digit1 => '1',
        Code::Digit2 => '2',
        Code::Digit3 => '3',
        Code::Digit4 => '4',
        Code::Digit5 => '5',
        Code::Digit6 => '6',
        Code::Digit7 => '7',
        Code::Digit8 => '8',
        Code::Digit9 => '9',
        Code::Minus => '-',
        Code::Equal => '=',
        Code::BracketLeft => '[',
        Code::BracketRight => ']',
        Code::Backslash => '\\',
        Code::Semicolon => ';',
        Code::Quote => '\'',
        Code::Backquote => '`',
        Code::Comma => ',',
        Code::Period => '.',
        Code::Slash => '/',
        Code::Space => ' ',
        _ => return None,
    })
}

fn keypad(key: &Key, physical: Physical) -> Option<u32> {
    if let Key::Named(named) = key {
        let code = match named {
            Named::Enter => 57414,
            Named::ArrowLeft => 57417,
            Named::ArrowRight => 57418,
            Named::ArrowUp => 57419,
            Named::ArrowDown => 57420,
            Named::PageUp => 57421,
            Named::PageDown => 57422,
            Named::Home => 57423,
            Named::End => 57424,
            Named::Insert => 57425,
            Named::Delete => 57426,
            Named::Clear => 57427,
            _ => 0,
        };
        if code != 0 {
            return Some(code);
        }
    }
    let Physical::Code(code) = physical else {
        return None;
    };
    Some(match code {
        Code::Numpad0 => 57399,
        Code::Numpad1 => 57400,
        Code::Numpad2 => 57401,
        Code::Numpad3 => 57402,
        Code::Numpad4 => 57403,
        Code::Numpad5 => 57404,
        Code::Numpad6 => 57405,
        Code::Numpad7 => 57406,
        Code::Numpad8 => 57407,
        Code::Numpad9 => 57408,
        Code::NumpadDecimal => 57409,
        Code::NumpadDivide => 57410,
        Code::NumpadMultiply => 57411,
        Code::NumpadSubtract => 57412,
        Code::NumpadAdd => 57413,
        Code::NumpadEnter => 57414,
        Code::NumpadEqual => 57415,
        Code::NumpadComma => 57416,
        _ => return None,
    })
}

fn named_kind(named: Named, location: Location) -> Option<Kind> {
    let right = location == Location::Right;
    let side = |left: u32| Kind::Modifier(if right { left + 6 } else { left });
    Some(match named {
        Named::Escape => Kind::Control(27),
        Named::Enter => Kind::Control(13),
        Named::Tab => Kind::Control(9),
        Named::Backspace => Kind::Control(127),
        Named::Space => Kind::Text(32),
        Named::ArrowUp => Kind::Letter(b'A'),
        Named::ArrowDown => Kind::Letter(b'B'),
        Named::ArrowRight => Kind::Letter(b'C'),
        Named::ArrowLeft => Kind::Letter(b'D'),
        Named::End => Kind::Letter(b'F'),
        Named::Home => Kind::Letter(b'H'),
        Named::F1 => Kind::Letter(b'P'),
        Named::F2 => Kind::Letter(b'Q'),
        Named::F4 => Kind::Letter(b'S'),
        Named::Insert => Kind::Tilde(2),
        Named::Delete => Kind::Tilde(3),
        Named::PageUp => Kind::Tilde(5),
        Named::PageDown => Kind::Tilde(6),
        Named::F3 => Kind::Tilde(13),
        Named::F5 => Kind::Tilde(15),
        Named::F6 => Kind::Tilde(17),
        Named::F7 => Kind::Tilde(18),
        Named::F8 => Kind::Tilde(19),
        Named::F9 => Kind::Tilde(20),
        Named::F10 => Kind::Tilde(21),
        Named::F11 => Kind::Tilde(23),
        Named::F12 => Kind::Tilde(24),
        Named::F13 => Kind::Functional(57376),
        Named::F14 => Kind::Functional(57377),
        Named::F15 => Kind::Functional(57378),
        Named::F16 => Kind::Functional(57379),
        Named::F17 => Kind::Functional(57380),
        Named::F18 => Kind::Functional(57381),
        Named::F19 => Kind::Functional(57382),
        Named::F20 => Kind::Functional(57383),
        Named::F21 => Kind::Functional(57384),
        Named::F22 => Kind::Functional(57385),
        Named::F23 => Kind::Functional(57386),
        Named::F24 => Kind::Functional(57387),
        Named::F25 => Kind::Functional(57388),
        Named::F26 => Kind::Functional(57389),
        Named::F27 => Kind::Functional(57390),
        Named::F28 => Kind::Functional(57391),
        Named::F29 => Kind::Functional(57392),
        Named::F30 => Kind::Functional(57393),
        Named::F31 => Kind::Functional(57394),
        Named::F32 => Kind::Functional(57395),
        Named::F33 => Kind::Functional(57396),
        Named::F34 => Kind::Functional(57397),
        Named::F35 => Kind::Functional(57398),
        Named::PrintScreen => Kind::Functional(57361),
        Named::Pause => Kind::Functional(57362),
        Named::ContextMenu => Kind::Functional(57363),
        Named::MediaPlay => Kind::Functional(57428),
        Named::MediaPause => Kind::Functional(57429),
        Named::MediaPlayPause => Kind::Functional(57430),
        Named::MediaStop => Kind::Functional(57432),
        Named::MediaFastForward => Kind::Functional(57433),
        Named::MediaRewind => Kind::Functional(57434),
        Named::MediaTrackNext => Kind::Functional(57435),
        Named::MediaTrackPrevious => Kind::Functional(57436),
        Named::MediaRecord => Kind::Functional(57437),
        Named::AudioVolumeDown => Kind::Functional(57438),
        Named::AudioVolumeUp => Kind::Functional(57439),
        Named::AudioVolumeMute => Kind::Functional(57440),
        Named::CapsLock => Kind::Modifier(57358),
        Named::ScrollLock => Kind::Modifier(57359),
        Named::NumLock => Kind::Modifier(57360),
        Named::Shift => side(57441),
        Named::Control => side(57442),
        Named::Alt => side(57443),
        Named::Super => side(57444),
        Named::Hyper => side(57445),
        Named::Meta => side(57446),
        Named::AltGraph => Kind::Modifier(57453),
        _ => return None,
    })
}

/// The protocol's key code for a character: its lowercase form.
fn char_code(c: char) -> u32 {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l as u32,
        _ => c as u32,
    }
}

fn single_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

/// Modifier parameter: 1 + shift 1, alt 2, ctrl 4, super 8. Hyper, meta, caps lock and num lock
/// are not reported by the windowing layer and are never set.
fn mod_value(mods: Modifiers) -> u32 {
    1 + u32::from(mods.shift())
        + 2 * u32::from(mods.alt())
        + 4 * u32::from(mods.control())
        + 8 * u32::from(mods.logo())
}

/// Encode a key event for the active enhancement `flags` (never empty here).
///
/// `legacy` produces the key's legacy bytes; it is used only where the protocol keeps them
/// (unmodified Enter/Tab/Backspace without flag 8). Cursor and function keys always use their
/// CSI forms here, never the SS3 forms of application cursor mode.
pub(crate) fn encode(
    ev: &KeyInput<'_>,
    flags: TermMode,
    is_macos: bool,
    legacy: impl FnOnce() -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    let all = flags.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
    let events = flags.contains(TermMode::REPORT_EVENT_TYPES);
    let alternates = flags.contains(TermMode::REPORT_ALTERNATE_KEYS);
    let with_text = all && flags.contains(TermMode::REPORT_ASSOCIATED_TEXT);
    let action = match ev.action {
        KeyAction::Release if !events => return None,
        KeyAction::Repeat if !events => KeyAction::Press,
        a => a,
    };
    let release = action == KeyAction::Release;

    let printable = ev
        .text
        .filter(|t| !t.is_empty() && !t.chars().any(char::is_control));
    let produced = printable.or(match ev.modified_key {
        Key::Character(s) if !s.chars().any(char::is_control) => Some(s.as_str()),
        _ => None,
    });

    // Option (macOS) or AltGr (Ctrl+Alt elsewhere) composed a different character: the composed
    // character is the key, and the composing modifiers are consumed.
    let mut mods = ev.mods;
    let mut composed = None;
    if let (Key::Character(base), Some(p)) = (ev.key, produced) {
        let composing = if is_macos {
            mods.alt() && !mods.control()
        } else {
            mods.alt() && mods.control()
        };
        if composing && p.to_lowercase() != base.to_lowercase() {
            mods.remove(Modifiers::ALT);
            if !is_macos {
                mods.remove(Modifiers::CTRL);
            }
            composed = Some(p);
        }
    }

    let kind = if ev.location == Location::Numpad
        && let Some(code) = keypad(ev.key, ev.physical)
    {
        // Keypad keys that type text (digits with Num Lock on) stay text keys.
        match printable {
            Some(_) if !matches!(ev.key, Key::Named(Named::Enter)) => Kind::Text(code),
            _ => Kind::Functional(code),
        }
    } else {
        match ev.key {
            Key::Named(named) => named_kind(*named, ev.location)?,
            Key::Character(s) => match single_char(composed.unwrap_or(s)) {
                Some(c) => Kind::Text(char_code(c)),
                None => return (!release).then(|| printable.unwrap_or(s).as_bytes().to_vec()),
            },
            Key::Unidentified => match printable.and_then(single_char) {
                Some(c) => Kind::Text(char_code(c)),
                None => {
                    return printable
                        .filter(|_| !release)
                        .map(|t| t.as_bytes().to_vec());
                }
            },
        }
    };

    let plain = !mods.shift() && !mods.alt() && !mods.control() && !mods.logo();
    if !all {
        match kind {
            Kind::Text(_) if !release && !mods.alt() && !mods.control() && !mods.logo() => {
                let text = composed.or(printable).or(match ev.key {
                    Key::Character(s) => Some(s.as_str()),
                    Key::Named(Named::Space) => Some(" "),
                    _ => None,
                });
                if let Some(t) = text {
                    return Some(t.as_bytes().to_vec());
                }
            }
            Kind::Control(13 | 9 | 127) if plain => return (!release).then(legacy).flatten(),
            Kind::Modifier(_) => return None,
            _ => {}
        }
    }

    let event = match action {
        KeyAction::Press => None,
        KeyAction::Repeat => Some(2),
        KeyAction::Release => Some(3),
    };
    let mut text_field = None;
    let mut number = String::new();
    let terminator = match kind {
        Kind::Text(code) => {
            number = code.to_string();
            if alternates {
                let shifted = match ev.modified_key {
                    Key::Character(s) if mods.shift() && composed.is_none() => {
                        single_char(s).map(|c| c as u32).filter(|&c| c != code)
                    }
                    _ => None,
                };
                let base = base_layout_key(ev.physical)
                    .map(|c| c as u32)
                    .filter(|&c| c != code);
                match (shifted, base) {
                    (None, None) => {}
                    (Some(s), None) => number.push_str(&format!(":{s}")),
                    (s, Some(b)) => {
                        let s = s.map(|s| s.to_string()).unwrap_or_default();
                        number.push_str(&format!(":{s}:{b}"));
                    }
                }
            }
            if with_text && !release {
                text_field = composed.or(printable).map(|t| {
                    t.chars()
                        .map(|c| (c as u32).to_string())
                        .collect::<Vec<_>>()
                        .join(":")
                });
            }
            'u'
        }
        Kind::Control(code) | Kind::Functional(code) | Kind::Modifier(code) => {
            number = code.to_string();
            'u'
        }
        Kind::Tilde(n) => {
            number = n.to_string();
            '~'
        }
        Kind::Letter(b) => b as char,
    };

    let m = mod_value(mods);
    let mut out = String::from("\x1b[");
    if m > 1 || event.is_some() || text_field.is_some() {
        if number.is_empty() {
            number.push('1');
        }
        out.push_str(&number);
        out.push_str(&format!(";{m}"));
        if let Some(e) = event {
            out.push_str(&format!(":{e}"));
        }
        if let Some(t) = text_field {
            out.push(';');
            out.push_str(&t);
        }
    } else {
        out.push_str(&number);
    }
    out.push(terminator);
    Some(out.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: TermMode = TermMode::DISAMBIGUATE_ESC_CODES;
    const A: TermMode = TermMode::REPORT_ALL_KEYS_AS_ESC;
    const E: TermMode = TermMode::REPORT_EVENT_TYPES;
    const ALT_KEYS: TermMode = TermMode::REPORT_ALTERNATE_KEYS;
    const TXT: TermMode = TermMode::REPORT_ASSOCIATED_TEXT;

    struct K {
        key: Key,
        modified: Key,
        physical: Physical,
        location: Location,
        text: Option<&'static str>,
        mods: Modifiers,
        action: KeyAction,
    }

    impl K {
        fn ch(base: &str, text: &str) -> Self {
            Self {
                key: Key::Character(base.into()),
                modified: Key::Character(text.into()),
                physical: Physical::Code(Code::KeyA),
                location: Location::Standard,
                text: Some(Box::leak(text.to_string().into_boxed_str())),
                mods: Modifiers::empty(),
                action: KeyAction::Press,
            }
        }
        fn named(n: Named) -> Self {
            Self {
                key: Key::Named(n),
                modified: Key::Named(n),
                physical: Physical::Code(Code::Fn),
                location: Location::Standard,
                text: None,
                mods: Modifiers::empty(),
                action: KeyAction::Press,
            }
        }
        fn mods(mut self, m: Modifiers) -> Self {
            self.mods = m;
            self
        }
        fn text(mut self, t: Option<&'static str>) -> Self {
            self.text = t;
            self
        }
        fn act(mut self, a: KeyAction) -> Self {
            self.action = a;
            self
        }
        fn at(mut self, p: Physical, l: Location) -> Self {
            self.physical = p;
            self.location = l;
            self
        }
        fn enc_on(&self, flags: TermMode, is_macos: bool) -> Option<String> {
            let ev = KeyInput {
                key: &self.key,
                modified_key: &self.modified,
                physical: self.physical,
                location: self.location,
                text: self.text,
                mods: self.mods,
                action: self.action,
            };
            encode(&ev, flags, is_macos, || Some(b"<legacy>".to_vec()))
                .map(|b| String::from_utf8(b).unwrap())
        }
        fn enc(&self, flags: TermMode) -> Option<String> {
            self.enc_on(flags, false)
        }
    }

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn table() {
        use Modifiers as M;
        let cases: Vec<(&str, K, TermMode, Option<String>)> = vec![
            ("esc flag 1", K::named(Named::Escape), D, s("\x1b[27u")),
            (
                "ctrl+a",
                K::ch("a", "\x01").mods(M::CTRL),
                D,
                s("\x1b[97;5u"),
            ),
            ("alt+a", K::ch("a", "a").mods(M::ALT), D, s("\x1b[97;3u")),
            (
                "ctrl+shift+a",
                K::ch("a", "A").mods(M::CTRL | M::SHIFT),
                D,
                s("\x1b[97;6u"),
            ),
            (
                "shift+tab",
                K::named(Named::Tab).mods(M::SHIFT),
                D,
                s("\x1b[9;2u"),
            ),
            (
                "ctrl+enter",
                K::named(Named::Enter).mods(M::CTRL),
                D,
                s("\x1b[13;5u"),
            ),
            (
                "plain enter flag 1",
                K::named(Named::Enter),
                D,
                s("<legacy>"),
            ),
            ("plain tab flag 1", K::named(Named::Tab), D, s("<legacy>")),
            (
                "plain bs flag 1",
                K::named(Named::Backspace),
                D,
                s("<legacy>"),
            ),
            ("enter flag 8", K::named(Named::Enter), A, s("\x1b[13u")),
            ("tab flag 8", K::named(Named::Tab), A, s("\x1b[9u")),
            ("bs flag 8", K::named(Named::Backspace), A, s("\x1b[127u")),
            ("esc flag 8", K::named(Named::Escape), A, s("\x1b[27u")),
            ("text flag 1", K::ch("a", "a"), D, s("a")),
            (
                "shifted text flag 1",
                K::ch("a", "A").mods(M::SHIFT),
                D,
                s("A"),
            ),
            (
                "space flag 1",
                K::named(Named::Space).text(Some(" ")),
                D,
                s(" "),
            ),
            ("text flag 8", K::ch("a", "a"), A, s("\x1b[97u")),
            (
                "shifted text flag 8",
                K::ch("a", "A").mods(M::SHIFT),
                A,
                s("\x1b[97;2u"),
            ),
            (
                "space flag 8",
                K::named(Named::Space).text(Some(" ")),
                A,
                s("\x1b[32u"),
            ),
            ("f5 flag 1", K::named(Named::F5), D, s("\x1b[15~")),
            ("f3 flag 1", K::named(Named::F3), D, s("\x1b[13~")),
            ("f1 flag 1", K::named(Named::F1), D, s("\x1b[P")),
            ("f5 flag 8", K::named(Named::F5), A, s("\x1b[15~")),
            (
                "ctrl+f5",
                K::named(Named::F5).mods(M::CTRL),
                D,
                s("\x1b[15;5~"),
            ),
            ("f3 flag 8", K::named(Named::F3), A, s("\x1b[13~")),
            ("f1 flag 8", K::named(Named::F1), A, s("\x1b[P")),
            (
                "shift+f1",
                K::named(Named::F1).mods(M::SHIFT),
                D,
                s("\x1b[1;2P"),
            ),
            ("f13", K::named(Named::F13), D, s("\x1b[57376u")),
            ("arrow flag 1", K::named(Named::ArrowUp), D, s("\x1b[A")),
            ("arrow flag 8", K::named(Named::ArrowUp), A, s("\x1b[A")),
            (
                "ctrl+shift+right",
                K::named(Named::ArrowRight).mods(M::CTRL | M::SHIFT),
                D,
                s("\x1b[1;6C"),
            ),
            (
                "alt+left",
                K::named(Named::ArrowLeft).mods(M::ALT),
                A,
                s("\x1b[1;3D"),
            ),
            (
                "pgup with super",
                K::named(Named::PageUp).mods(M::LOGO),
                D,
                s("\x1b[5;9~"),
            ),
            ("shift alone flag 1", K::named(Named::Shift), D, None),
            (
                "left shift flag 8",
                K::named(Named::Shift).mods(M::SHIFT),
                A,
                s("\x1b[57441;2u"),
            ),
            (
                "right ctrl flag 8",
                K::named(Named::Control).at(Physical::Code(Code::ControlRight), Location::Right),
                A,
                s("\x1b[57448u"),
            ),
            (
                "caps lock flag 8",
                K::named(Named::CapsLock),
                A,
                s("\x1b[57358u"),
            ),
            (
                "keypad enter",
                K::named(Named::Enter)
                    .text(Some("\r"))
                    .at(Physical::Code(Code::NumpadEnter), Location::Numpad),
                D,
                s("\x1b[57414u"),
            ),
            (
                "keypad digit flag 1",
                K::ch("1", "1").at(Physical::Code(Code::Numpad1), Location::Numpad),
                D,
                s("1"),
            ),
            (
                "keypad digit flag 8",
                K::ch("1", "1").at(Physical::Code(Code::Numpad1), Location::Numpad),
                A,
                s("\x1b[57400u"),
            ),
        ];
        for (name, k, flags, want) in cases {
            assert_eq!(k.enc(flags), want, "{name}");
        }
    }

    #[test]
    fn event_types() {
        let a = K::ch("a", "a");
        assert_eq!(a.enc(D | E), s("a"), "press of a text key stays text");
        let rel = K::ch("a", "a").text(None).act(KeyAction::Release);
        assert_eq!(rel.enc(D | E), s("\x1b[97;1:3u"));
        assert_eq!(rel.enc(D), None, "no releases without flag 2");
        let rep = K::named(Named::Escape).act(KeyAction::Repeat);
        assert_eq!(rep.enc(D | E), s("\x1b[27;1:2u"));
        assert_eq!(
            rep.enc(D),
            s("\x1b[27u"),
            "repeat is a press without flag 2"
        );
        let up = K::named(Named::ArrowUp).act(KeyAction::Release);
        assert_eq!(up.enc(D | E), s("\x1b[1;1:3A"));
        let f5 = K::named(Named::F5).act(KeyAction::Repeat);
        assert_eq!(f5.enc(D | E), s("\x1b[15;1:2~"));
        // Enter/Tab/Backspace releases only with flag 8.
        let enter = K::named(Named::Enter).act(KeyAction::Release);
        assert_eq!(enter.enc(D | E), None);
        assert_eq!(enter.enc(D | E | A), s("\x1b[13;1:3u"));
        let ctrl_rel = K::ch("a", "a")
            .text(None)
            .mods(Modifiers::CTRL)
            .act(KeyAction::Release);
        assert_eq!(ctrl_rel.enc(D | E), s("\x1b[97;5:3u"));
    }

    #[test]
    fn alternate_keys() {
        let shifted = K::ch("a", "A").mods(Modifiers::SHIFT);
        assert_eq!(shifted.enc(A | ALT_KEYS), s("\x1b[97:65;2u"));
        // Shifted key only when shift is held.
        assert_eq!(K::ch("a", "a").enc(A | ALT_KEYS), s("\x1b[97u"));
        // A non-US layout: Cyrillic key at the position of `c` reports `c` as base layout key.
        let cyr = K::ch("с", "с").at(Physical::Code(Code::KeyC), Location::Standard);
        assert_eq!(cyr.enc(A | ALT_KEYS), s("\x1b[1089::99u"));
        let cyr_shift = K::ch("с", "С")
            .mods(Modifiers::SHIFT)
            .at(Physical::Code(Code::KeyC), Location::Standard);
        assert_eq!(cyr_shift.enc(A | ALT_KEYS), s("\x1b[1089:1057:99;2u"));
        let one = K::ch("1", "!")
            .mods(Modifiers::SHIFT)
            .at(Physical::Code(Code::Digit1), Location::Standard);
        assert_eq!(one.enc(D | ALT_KEYS), s("!"), "flag 1 keeps text");
        assert_eq!(one.enc(A | ALT_KEYS), s("\x1b[49:33;2u"));
    }

    #[test]
    fn associated_text() {
        let shifted = K::ch("a", "A").mods(Modifiers::SHIFT);
        assert_eq!(shifted.enc(A | TXT), s("\x1b[97;2;65u"));
        assert_eq!(K::ch("a", "a").enc(A | TXT), s("\x1b[97;1;97u"));
        // Only meaningful with flag 8; no text for control characters or releases.
        assert_eq!(K::ch("a", "a").enc(D | TXT), s("a"));
        let ctrl = K::ch("a", "\x01").mods(Modifiers::CTRL);
        assert_eq!(ctrl.enc(A | TXT), s("\x1b[97;5u"));
        let rel = K::ch("a", "a").text(None).act(KeyAction::Release);
        assert_eq!(rel.enc(A | E | TXT), s("\x1b[97;1:3u"));
    }

    #[test]
    fn composition_rules() {
        // macOS: Option composed a different character, sent as text with Alt consumed.
        let opt = K::ch("l", "@").mods(Modifiers::ALT);
        assert_eq!(opt.enc_on(D, true), s("@"));
        assert_eq!(opt.enc_on(A, true), s("\x1b[64u"));
        // Option that did not compose acts as Alt.
        let alt_b = K::ch("b", "b").mods(Modifiers::ALT);
        assert_eq!(alt_b.enc_on(D, true), s("\x1b[98;3u"));
        // Elsewhere Alt alone never composes, Ctrl+Alt with different text is AltGr.
        assert_eq!(opt.enc_on(D, false), s("\x1b[108;3u"));
        let altgr = K::ch("q", "@").mods(Modifiers::CTRL | Modifiers::ALT);
        assert_eq!(altgr.enc_on(D, false), s("@"));
        assert_eq!(altgr.enc_on(D, true), s("\x1b[113;7u"));
    }

    #[test]
    fn helpers() {
        assert_eq!(base_layout_key(Physical::Code(Code::KeyZ)), Some('z'));
        assert_eq!(base_layout_key(Physical::Code(Code::Slash)), Some('/'));
        assert_eq!(base_layout_key(Physical::Code(Code::F1)), None);
        assert_eq!(char_code('A'), 97);
        assert_eq!(
            mod_value(Modifiers::all()),
            1 + 1 + 2 + 4 + 8,
            "only shift, alt, ctrl and super exist"
        );
    }
}
