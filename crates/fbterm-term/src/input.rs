//! Keyboard, paste and mouse input encoding into the bytes a shell expects.

use iced::keyboard::key::{Named, Physical};
use iced::keyboard::{Key, Modifiers};

use crate::TermMode;

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

/// Encode a key press. Returns `None` when the key produces no input.
///
/// On macOS, chords with Cmd are never encoded (they belong to the application) and Option acts
/// as Alt, using the unmodified key rather than the composed text.
pub fn encode_key(
    key: &Key,
    _physical: Physical,
    text: Option<&str>,
    mods: Modifiers,
    mode: TermMode,
) -> Option<Vec<u8>> {
    if cfg!(target_os = "macos") && mods.logo() {
        return None;
    }
    match key {
        Key::Named(named) => named_key(*named, mods, mode),
        Key::Character(s) => {
            let mut chars = s.chars();
            let first = chars.next()?;
            let single = chars.next().is_none();
            if mods.control() && single {
                let b = control_byte(first)?;
                return Some(with_alt(vec![b], mods));
            }
            if mods.alt() {
                let mut out = vec![0x1b];
                out.extend_from_slice(s.as_bytes());
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
        let clean = text.replace("\x1b[201~", "");
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
