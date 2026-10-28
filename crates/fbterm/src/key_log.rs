//! Debug aid `FBTERM_LOG_KEYS=1` (read once at startup; works in release builds): while the
//! Settings window records a chord, every keyboard event the app receives, in any window, is
//! logged at INFO, once from the window-level event listener (`App::subscription`: does the
//! system deliver the event at all, and did any widget capture it?) and once from each
//! [`Shortcuts`](crate::shortcuts) wrapper it reaches (was it captured before the wrapper saw
//! it?). Nothing is logged while no chord is being recorded, so terminal typing never is; the
//! text a press types and character keys are logged only for the recording window.

use std::sync::OnceLock;

use iced::{event, keyboard, window};
use parking_lot::Mutex;

/// Whether `FBTERM_LOG_KEYS` was set at startup.
static ENABLED: OnceLock<bool> = OnceLock::new();

/// The Settings window while it records a chord.
static RECORDING: Mutex<Option<window::Id>> = Mutex::new(None);

/// Read `FBTERM_LOG_KEYS` (any value but empty or `0` turns the log on).
pub fn init_from_env() {
    let on = std::env::var_os("FBTERM_LOG_KEYS").is_some_and(|v| !v.is_empty() && v != "0");
    if ENABLED.set(on).is_ok() && on {
        tracing::info!("key log on: keyboard events are logged while a shortcut is recorded");
    }
}

/// Whether the key log is on for this run.
pub fn enabled() -> bool {
    ENABLED.get().copied().unwrap_or(false)
}

/// Note which window records a chord now, if any (the app calls this after every update).
pub fn set_recording(window: Option<window::Id>) {
    if enabled() {
        *RECORDING.lock() = window;
    }
}

/// The window recording a chord, while the log is on.
fn recorder() -> Option<window::Id> {
    active(enabled(), *RECORDING.lock())
}

/// The window whose recording is logged: `recording`, only when the log is `enabled`.
pub fn active(enabled: bool, recording: Option<window::Id>) -> Option<window::Id> {
    recording.filter(|_| enabled)
}

/// `key` for the log: character keys only in the recording window (`here`); named keys
/// everywhere.
fn shown(key: &keyboard::Key, here: bool) -> String {
    match key {
        keyboard::Key::Character(_) if !here => "<character>".to_owned(),
        key => format!("{key:?}"),
    }
}

/// Where an event was seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The window-level event listener, after the window's widgets saw the event.
    Listener,
    /// A [`Shortcuts`](crate::shortcuts) wrapper, before its content sees the event.
    Shortcuts,
}

/// For the window-level listener (`iced::event::listen_with`, subscribed only while the log is
/// on and a chord is recorded): log keyboard events. Never produces a message.
pub fn listen<M>(event: iced::Event, status: event::Status, window: window::Id) -> Option<M> {
    if let iced::Event::Keyboard(event) = &event
        && let Some(recorder) = recorder()
    {
        let captured = status == event::Status::Captured;
        tracing::info!(
            "{}",
            describe(Source::Listener, window, recorder, event, captured)
        );
    }
    None
}

/// For a [`Shortcuts`](crate::shortcuts) wrapper over `window`: log a keyboard `event` it
/// receives while a chord is recorded. `captured`: captured before the wrapper saw it.
pub fn shortcuts(window: window::Id, event: &iced::Event, captured: bool) {
    if let iced::Event::Keyboard(event) = event
        && let Some(recorder) = recorder()
    {
        tracing::info!(
            "{}",
            describe(Source::Shortcuts, window, recorder, event, captured)
        );
    }
}

/// The log line for keyboard `event` seen by `source` in `window`, while `recorder` records.
/// The text a press types and character keys are shown only for `recorder`; elsewhere only
/// named keys, physical keys and locations.
pub fn describe(
    source: Source,
    window: window::Id,
    recorder: window::Id,
    event: &keyboard::Event,
    captured: bool,
) -> String {
    let source = match source {
        Source::Listener => "listener",
        Source::Shortcuts => "shortcuts",
    };
    let here = window == recorder;
    let head = format!("key event ({source}) window={window:?} recording_window={here}");
    match event {
        keyboard::Event::KeyPressed {
            key,
            modified_key,
            physical_key,
            location,
            modifiers,
            text,
            repeat,
        } => {
            let text = if here {
                format!("{text:?}")
            } else {
                "<withheld>".to_owned()
            };
            let (key, modified_key) = (shown(key, here), shown(modified_key, here));
            format!(
                "{head} KeyPressed key={key} modified_key={modified_key} \
                 physical_key={physical_key:?} location={location:?} text={text} \
                 modifiers={modifiers:?} repeat={repeat} captured={captured}"
            )
        }
        keyboard::Event::KeyReleased {
            key,
            modified_key,
            physical_key,
            location,
            modifiers,
        } => format!(
            "{head} KeyReleased key={} modified_key={} \
             physical_key={physical_key:?} location={location:?} modifiers={modifiers:?} \
             captured={captured}",
            shown(key, here),
            shown(modified_key, here),
        ),
        keyboard::Event::ModifiersChanged(modifiers) => {
            format!("{head} ModifiersChanged modifiers={modifiers:?} captured={captured}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::key::{Code, Named, Physical};
    use iced::keyboard::{Key, Location, Modifiers};

    fn press(text: Option<&str>) -> keyboard::Event {
        keyboard::Event::KeyPressed {
            key: Key::Character("1".into()),
            modified_key: Key::Character("!".into()),
            physical_key: Physical::Code(Code::Digit1),
            location: Location::Standard,
            modifiers: Modifiers::CTRL | Modifiers::SHIFT,
            text: text.map(Into::into),
            repeat: false,
        }
    }

    #[test]
    fn a_press_in_the_recording_window_shows_everything() {
        let w = window::Id::unique();
        let line = describe(Source::Shortcuts, w, w, &press(Some("!")), false);
        for part in [
            "shortcuts",
            "KeyPressed",
            "key=Character(\"1\")",
            "modified_key=Character(\"!\")",
            "physical_key=Code(Digit1)",
            "location=Standard",
            "text=Some(\"!\")",
            "repeat=false",
            "captured=false",
            "recording_window=true",
        ] {
            assert!(line.contains(part), "{part} in {line}");
        }
        assert!(line.contains("CTRL") && line.contains("SHIFT"), "{line}");
    }

    #[test]
    fn characters_typed_in_another_window_are_never_logged() {
        let (recorder, other) = (window::Id::unique(), window::Id::unique());
        let line = describe(Source::Listener, other, recorder, &press(Some("x")), true);
        // Neither the text nor the character keys; the physical key and location stay.
        for secret in ["\"x\"", "\"1\"", "\"!\""] {
            assert!(!line.contains(secret), "{secret} in {line}");
        }
        for part in [
            "text=<withheld>",
            "key=<character>",
            "modified_key=<character>",
            "physical_key=Code(Digit1)",
            "location=Standard",
            "listener",
            "captured=true",
            "recording_window=false",
        ] {
            assert!(line.contains(part), "{part} in {line}");
        }
        let release = keyboard::Event::KeyReleased {
            key: Key::Character("a".into()),
            modified_key: Key::Character("a".into()),
            physical_key: Physical::Code(Code::KeyA),
            location: Location::Standard,
            modifiers: Modifiers::empty(),
        };
        let line = describe(Source::Shortcuts, other, recorder, &release, false);
        assert!(!line.contains("\"a\""), "{line}");
        assert!(line.contains("physical_key=Code(KeyA)"), "{line}");
    }

    #[test]
    fn named_keys_in_another_window_are_logged() {
        let (recorder, other) = (window::Id::unique(), window::Id::unique());
        let enter = keyboard::Event::KeyPressed {
            key: Key::Named(Named::Enter),
            modified_key: Key::Named(Named::Enter),
            physical_key: Physical::Code(Code::Enter),
            location: Location::Standard,
            modifiers: Modifiers::empty(),
            text: Some("\r".into()),
            repeat: false,
        };
        let line = describe(Source::Listener, other, recorder, &enter, false);
        assert!(line.contains("key=Named(Enter)"), "{line}");
        assert!(line.contains("text=<withheld>"), "{line}");
    }

    #[test]
    fn nothing_is_logged_or_listened_to_unless_on_and_recording() {
        let w = window::Id::unique();
        assert_eq!(active(false, None), None);
        assert_eq!(active(false, Some(w)), None);
        assert_eq!(active(true, None), None);
        assert_eq!(active(true, Some(w)), Some(w));
        // This test run never set FBTERM_LOG_KEYS: nothing is logged.
        set_recording(Some(w));
        assert_eq!(recorder(), None);
    }

    #[test]
    fn releases_and_modifier_changes_are_logged() {
        let w = window::Id::unique();
        let release = keyboard::Event::KeyReleased {
            key: Key::Named(Named::Shift),
            modified_key: Key::Named(Named::Shift),
            physical_key: Physical::Code(Code::ShiftLeft),
            location: Location::Left,
            modifiers: Modifiers::empty(),
        };
        let line = describe(Source::Listener, w, w, &release, false);
        assert!(
            line.contains("KeyReleased") && line.contains("Named(Shift)"),
            "{line}"
        );
        assert!(line.contains("location=Left"), "{line}");
        let mods = keyboard::Event::ModifiersChanged(Modifiers::CTRL);
        let line = describe(Source::Shortcuts, w, w, &mods, false);
        assert!(
            line.contains("ModifiersChanged") && line.contains("CTRL"),
            "{line}"
        );
    }
}
