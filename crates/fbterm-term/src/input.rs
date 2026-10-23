//! Keyboard, paste and mouse input encoding into the bytes a shell expects.

use iced::keyboard::key::{Named, Physical};
use iced::keyboard::{Key, Location, Modifiers};

use crate::TermMode;
use crate::kitty;

/// Whether a key event is a first press, an auto-repeat or a release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Press,
    Repeat,
    Release,
}

/// One keyboard event, as the windowing layer reports it.
#[derive(Debug, Clone, Copy)]
pub struct KeyInput<'a> {
    /// The key without modifiers applied.
    pub key: &'a Key,
    /// The key with modifiers (except Ctrl) applied.
    pub modified_key: &'a Key,
    pub physical: Physical,
    pub location: Location,
    /// Text the press produced, if any (always `None` for releases).
    pub text: Option<&'a str>,
    pub mods: Modifiers,
    pub action: KeyAction,
}

fn mod_param(mods: Modifiers) -> u8 {
    1 + u8::from(mods.shift()) + 2 * u8::from(mods.alt()) + 4 * u8::from(mods.control())
}

fn has_mods(mods: Modifiers) -> bool {
    mods.shift() || mods.alt() || mods.control()
}

/// `CSI 1 ; m final` / `SS3 final` / `CSI final` cursor-style key.
fn csi_or_ss3(final_byte: u8, mods: Modifiers, ss3: bool) -> Vec<u8> {
    if has_mods(mods) {
        format!("\x1b[1;{}{}", mod_param(mods), final_byte as char).into_bytes()
    } else if ss3 {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

/// `CSI n ; m ~` key.
fn tilde(n: u8, mods: Modifiers) -> Vec<u8> {
    if has_mods(mods) {
        format!("\x1b[{n};{}~", mod_param(mods)).into_bytes()
    } else {
        format!("\x1b[{n}~").into_bytes()
    }
}

fn with_alt(mut bytes: Vec<u8>, mods: Modifiers) -> Vec<u8> {
    if mods.alt() {
        bytes.insert(0, 0x1b);
    }
    bytes
}

fn named_key(named: Named, mods: Modifiers, mode: TermMode) -> Option<Vec<u8>> {
    let app = mode.contains(TermMode::APP_CURSOR);
    Some(match named {
        Named::Enter => with_alt(vec![b'\r'], mods),
        Named::Backspace => {
            if mods.control() {
                with_alt(vec![0x08], mods)
            } else {
                with_alt(vec![0x7f], mods)
            }
        }
        Named::Tab => {
            if mods.shift() {
                b"\x1b[Z".to_vec()
            } else {
                with_alt(vec![b'\t'], mods)
            }
        }
        Named::Escape => with_alt(vec![0x1b], mods),
        Named::Space => {
            if mods.control() {
                with_alt(vec![0], mods)
            } else {
                with_alt(vec![b' '], mods)
            }
        }
        Named::ArrowUp => csi_or_ss3(b'A', mods, app),
        Named::ArrowDown => csi_or_ss3(b'B', mods, app),
        Named::ArrowRight => csi_or_ss3(b'C', mods, app),
        Named::ArrowLeft => csi_or_ss3(b'D', mods, app),
        Named::Home => csi_or_ss3(b'H', mods, app),
        Named::End => csi_or_ss3(b'F', mods, app),
        Named::Insert => tilde(2, mods),
        Named::Delete => tilde(3, mods),
        Named::PageUp => tilde(5, mods),
        Named::PageDown => tilde(6, mods),
        Named::F1 => csi_or_ss3(b'P', mods, true),
        Named::F2 => csi_or_ss3(b'Q', mods, true),
        Named::F3 => csi_or_ss3(b'R', mods, true),
        Named::F4 => csi_or_ss3(b'S', mods, true),
        Named::F5 => tilde(15, mods),
        Named::F6 => tilde(17, mods),
        Named::F7 => tilde(18, mods),
        Named::F8 => tilde(19, mods),
        Named::F9 => tilde(20, mods),
        Named::F10 => tilde(21, mods),
        Named::F11 => tilde(23, mods),
        Named::F12 => tilde(24, mods),
        _ => return None,
    })
}

/// Control byte for `Ctrl+<c>`, if one exists.
fn control_byte(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        '@' | ' ' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '7' | '/' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

/// AltGr is reported as Ctrl+Alt with printable text on non-macOS platforms; the text must be
/// sent as-is rather than turned into a control byte.
fn is_altgr(mods: Modifiers, printable_text: Option<&str>, is_macos: bool) -> bool {
    !is_macos && mods.alt() && mods.control() && printable_text.is_some()
}

/// Encode a key press with the legacy encoding, or the kitty keyboard protocol when the
/// program enabled it. Returns `None` when the key produces no input.
///
/// Shorthand for [`encode_key_input`] with a press of an unshifted key in the standard location.
pub fn encode_key(
    key: &Key,
    physical: Physical,
    text: Option<&str>,
    mods: Modifiers,
    mode: TermMode,
) -> Option<Vec<u8>> {
    encode_key_input(
        &KeyInput {
            key,
            modified_key: key,
            physical,
            location: Location::Standard,
            text,
            mods,
            action: KeyAction::Press,
        },
        mode,
    )
}

/// Encode a key event. Returns `None` when the event produces no input.
///
/// On macOS, chords with Cmd are never encoded (they belong to the application). Option
/// sends the composed text when it produced a different character, otherwise it acts as Alt
/// (ESC prefix). Elsewhere Ctrl+Alt with printable text is AltGr and sends the text.
///
/// Without kitty keyboard protocol flags in `mode` the legacy encoding is used and releases
/// produce nothing. With flags (which the emulator accepts only when the `kitty_keyboard`
/// setting is on), keys are encoded per the protocol's progressive enhancements.
pub fn encode_key_input(ev: &KeyInput<'_>, mode: TermMode) -> Option<Vec<u8>> {
    if cfg!(target_os = "macos") && ev.mods.logo() {
        return None;
    }
    let flags = mode & TermMode::KITTY_KEYBOARD_PROTOCOL;
    let legacy = || {
        if ev.action == KeyAction::Release {
            None
        } else {
            legacy_key(ev.key, ev.text, ev.mods, mode)
        }
    };
    if flags.is_empty() {
        legacy()
    } else {
        kitty::encode(ev, flags, cfg!(target_os = "macos"), legacy)
    }
}

fn legacy_key(key: &Key, text: Option<&str>, mods: Modifiers, mode: TermMode) -> Option<Vec<u8>> {
    match key {
        Key::Named(named) => named_key(*named, mods, mode),
        Key::Character(s) => {
            let mut chars = s.chars();
            let first = chars.next()?;
            let single = chars.next().is_none();
            let composed = text.filter(|t| !t.is_empty() && !t.chars().any(char::is_control));
            if is_altgr(mods, composed, cfg!(target_os = "macos")) {
                return composed.map(|t| t.as_bytes().to_vec());
            }
            if mods.control() && single {
                let b = control_byte(first)?;
                return Some(with_alt(vec![b], mods));
            }
            // macOS: Option composed a different character (e.g. @); elsewhere Ctrl+Alt is AltGr.
            let composes = if cfg!(target_os = "macos") {
                mods.alt() && composed.is_some_and(|t| t.to_lowercase() != s.to_lowercase())
            } else {
                false
            };
            if composes {
                return composed.map(|t| t.as_bytes().to_vec());
            }
            if mods.alt() {
                let body = composed.unwrap_or(s.as_str());
                let mut out = vec![0x1b];
                out.extend_from_slice(body.as_bytes());
                return Some(out);
            }
            let t = text.filter(|t| !t.is_empty()).unwrap_or(s.as_str());
            Some(t.as_bytes().to_vec())
        }
        Key::Unidentified => text
            .filter(|t| !t.is_empty() && !mods.control())
            .map(|t| with_alt(t.as_bytes().to_vec(), mods)),
    }
}

/// Encode pasted text, wrapping it in bracketed-paste markers when the mode is set.
pub fn encode_paste(text: &str, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        let clean: String = text
            .chars()
            .filter(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
            .collect();
        let mut out = b"\x1b[200~".to_vec();
        out.extend_from_slice(clean.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Press,
    Release,
    Drag,
}

/// A mouse event in 0-based grid cell coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MouseReport {
    pub button: MouseButton,
    pub kind: MouseKind,
    pub col: u16,
    pub row: u16,
    pub mods: Modifiers,
}

/// Encode a mouse event, or `None` when the active mode does not report it.
pub fn encode_mouse(event: MouseReport, mode: TermMode) -> Option<Vec<u8>> {
    if !mode.intersects(TermMode::MOUSE_MODE) {
        return None;
    }
    let wheel = matches!(event.button, MouseButton::WheelUp | MouseButton::WheelDown);
    if event.kind == MouseKind::Drag
        && !mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
    {
        return None;
    }
    if wheel && event.kind != MouseKind::Press {
        return None;
    }
    let sgr = mode.contains(TermMode::SGR_MOUSE);
    let mut code: u16 = match event.button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    };
    if event.kind == MouseKind::Release && !sgr {
        code = 3;
    }
    if event.mods.shift() {
        code += 4;
    }
    if event.mods.alt() {
        code += 8;
    }
    if event.mods.control() {
        code += 16;
    }
    if event.kind == MouseKind::Drag {
        code += 32;
    }
    if sgr {
        let end = if event.kind == MouseKind::Release {
            'm'
        } else {
            'M'
        };
        Some(format!("\x1b[<{code};{};{}{end}", event.col + 1, event.row + 1).into_bytes())
    } else {
        let enc = |v: u16| (v.min(222) + 1 + 32) as u8;
        Some(vec![
            0x1b,
            b'[',
            b'M',
            (code + 32) as u8,
            enc(event.col),
            enc(event.row),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::key::{Code, Named, Physical};
    use iced::keyboard::{Key, Modifiers};

    fn key(k: Key, mods: Modifiers, text: Option<&str>, mode: TermMode) -> Option<Vec<u8>> {
        encode_key(&k, Physical::Code(Code::KeyA), text, mods, mode)
    }
    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }

    #[test]
    fn enter_is_cr() {
        let r = key(
            Key::Named(Named::Enter),
            Modifiers::empty(),
            None,
            TermMode::empty(),
        );
        assert_eq!(r.unwrap(), b"\r");
    }
    #[test]
    fn arrow_respects_app_cursor_mode() {
        let up = Key::Named(Named::ArrowUp);
        assert_eq!(
            key(up.clone(), Modifiers::empty(), None, TermMode::empty()).unwrap(),
            b"\x1b[A"
        );
        assert_eq!(
            key(up, Modifiers::empty(), None, TermMode::APP_CURSOR).unwrap(),
            b"\x1bOA"
        );
    }
    #[test]
    fn ctrl_c_is_etx() {
        let r = key(ch("c"), Modifiers::CTRL, Some("c"), TermMode::empty());
        assert_eq!(r.unwrap(), vec![0x03]);
    }
    #[test]
    fn alt_letter_is_esc_prefixed() {
        let r = key(ch("x"), Modifiers::ALT, Some("x"), TermMode::empty());
        assert_eq!(r.unwrap(), b"\x1bx");
    }
    #[test]
    fn plain_text_and_named_keys() {
        assert_eq!(
            key(ch("a"), Modifiers::empty(), Some("a"), TermMode::empty()).unwrap(),
            b"a"
        );
        let e = TermMode::empty();
        let n = |n| key(Key::Named(n), Modifiers::empty(), None, e).unwrap();
        assert_eq!(n(Named::Backspace), vec![0x7f]);
        assert_eq!(n(Named::Tab), b"\t");
        assert_eq!(n(Named::Escape), vec![0x1b]);
        assert_eq!(n(Named::Home), b"\x1b[H");
        assert_eq!(n(Named::Delete), b"\x1b[3~");
        assert_eq!(n(Named::PageUp), b"\x1b[5~");
        assert_eq!(n(Named::F1), b"\x1bOP");
        assert_eq!(n(Named::F5), b"\x1b[15~");
        assert_eq!(n(Named::F12), b"\x1b[24~");
        assert_eq!(
            key(Key::Named(Named::Tab), Modifiers::SHIFT, None, e).unwrap(),
            b"\x1b[Z"
        );
    }
    #[test]
    fn modified_arrow() {
        let r = key(
            Key::Named(Named::ArrowRight),
            Modifiers::CTRL,
            None,
            TermMode::empty(),
        );
        assert_eq!(r.unwrap(), b"\x1b[1;5C");
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn cmd_chords_are_not_encoded() {
        assert!(key(ch("c"), Modifiers::LOGO, Some("c"), TermMode::empty()).is_none());
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn option_composed_char_sent_verbatim() {
        let r = key(ch("l"), Modifiers::ALT, Some("@"), TermMode::empty());
        assert_eq!(r.unwrap(), b"@");
    }
    #[test]
    fn option_letter_without_composition_is_esc_prefixed() {
        let r = key(ch("b"), Modifiers::ALT, Some("b"), TermMode::empty());
        assert_eq!(r.unwrap(), b"\x1bb");
    }
    #[test]
    fn alt_shift_letter_keeps_shift() {
        let m = Modifiers::ALT | Modifiers::SHIFT;
        let r = key(ch("b"), m, Some("B"), TermMode::empty());
        assert_eq!(r.unwrap(), b"\x1bB");
    }
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn altgr_sends_text_on_non_macos() {
        let m = Modifiers::CTRL | Modifiers::ALT;
        let r = key(ch("q"), m, Some("@"), TermMode::empty());
        assert_eq!(r.unwrap(), b"@");
    }
    #[test]
    fn altgr_detection_is_platform_independent() {
        let m = Modifiers::CTRL | Modifiers::ALT;
        assert!(is_altgr(m, Some("@"), false));
        assert!(!is_altgr(m, None, false));
        assert!(!is_altgr(Modifiers::CTRL, Some("@"), false));
        assert!(!is_altgr(m, Some("@"), true));
    }
    #[test]
    fn paste_cannot_terminate_bracket_early() {
        let out = encode_paste("x\x1b[2\x1b[201~01~y", TermMode::BRACKETED_PASTE);
        let s = String::from_utf8(out).unwrap();
        assert_eq!(s.matches("\x1b[201~").count(), 1);
        assert!(s.ends_with("\x1b[201~"));
    }
    #[test]
    fn paste_is_bracketed_when_mode_set() {
        assert_eq!(
            encode_paste("a", TermMode::BRACKETED_PASTE),
            b"\x1b[200~a\x1b[201~"
        );
        assert_eq!(encode_paste("a\nb", TermMode::empty()), b"a\rb");
    }
    #[test]
    fn sgr_mouse_press_encoding() {
        let r = MouseReport {
            button: MouseButton::Left,
            kind: MouseKind::Press,
            col: 4,
            row: 2,
            mods: Modifiers::empty(),
        };
        let mode = TermMode::SGR_MOUSE | TermMode::MOUSE_REPORT_CLICK;
        assert_eq!(encode_mouse(r, mode).unwrap(), b"\x1b[<0;5;3M");
        assert!(encode_mouse(r, TermMode::SGR_MOUSE).is_none());
        let rel = MouseReport {
            kind: MouseKind::Release,
            ..r
        };
        assert_eq!(encode_mouse(rel, mode).unwrap(), b"\x1b[<0;5;3m");
        assert_eq!(
            encode_mouse(r, TermMode::MOUSE_REPORT_CLICK).unwrap(),
            vec![0x1b, b'[', b'M', 32, 37, 35]
        );
    }
}
