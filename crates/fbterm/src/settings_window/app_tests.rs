//! The Settings window against a temporary config directory: live apply, in-place writes,
//! own-write suppression and edits on disk.

use fbterm_config::{Paths, ProfileId, SETTINGS_TEMPLATE, Settings, ThemeMode};

use super::SettingsMsg;
use crate::app::{App, Message};
use crate::watch::Changed;

const SETTINGS_CHANGED: Changed = Changed {
    profiles: false,
    settings: true,
};

fn app() -> (App, Paths) {
    let dir = std::env::temp_dir().join(format!("fbterm-settings-test-{}", ProfileId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let paths = Paths { config_dir: dir };
    let (mut app, _) = App::build(paths.clone(), true);
    let _ = app.update(Message::Settings(SettingsMsg::Open));
    assert!(app.settings_window().is_some());
    (app, paths)
}

fn send(app: &mut App, m: SettingsMsg) {
    let _ = app.update(Message::Settings(m));
}

fn read(paths: &Paths) -> Option<String> {
    std::fs::read_to_string(paths.settings()).ok()
}

#[test]
fn a_click_applies_live_and_writes_the_template_with_the_key_set() {
    let (mut app, paths) = app();
    assert_eq!(read(&paths), None);
    send(&mut app, SettingsMsg::Theme(ThemeMode::Light));
    assert_eq!(app.settings.theme, ThemeMode::Light);
    assert_eq!(app.tokens, crate::theme::tokens(ThemeMode::Light));
    let text = read(&paths).unwrap();
    assert_eq!(
        text,
        SETTINGS_TEMPLATE.replace(
            "#theme = \"dark\"\n",
            "#theme = \"dark\"\ntheme = \"light\"\n"
        )
    );
    // The watcher's report of our own write changes nothing.
    let _ = app.update(Message::ConfigChanged(SETTINGS_CHANGED));
    assert!(app.notices.is_empty());
    assert_eq!(app.settings.theme, ThemeMode::Light);
}

#[test]
fn a_drag_applies_at_once_and_writes_when_let_go_or_closed() {
    let (mut app, paths) = app();
    send(&mut app, SettingsMsg::Opacity(0.8));
    assert_eq!(app.settings.background_opacity, 0.8);
    assert_eq!(read(&paths), None, "no write while dragging");
    send(&mut app, SettingsMsg::Release);
    assert_eq!(
        Settings::load(&paths.settings())
            .unwrap()
            .background_opacity,
        0.8
    );
    // A change still waiting is written when the window closes.
    send(&mut app, SettingsMsg::FontSize(16.0));
    assert_eq!(app.style.font_size, 16.0);
    let id = app.settings_window().unwrap();
    let _ = app.update(Message::WindowClosed(id));
    assert!(app.settings_window().is_none());
    assert_eq!(Settings::load(&paths.settings()).unwrap().font_size, 16.0);
}

#[test]
fn an_edit_on_disk_moves_the_controls() {
    let (mut app, paths) = app();
    std::fs::write(
        paths.settings(),
        "font_size = 20.0\nkitty_keyboard = true\n",
    )
    .unwrap();
    let _ = app.update(Message::ConfigChanged(SETTINGS_CHANGED));
    let (_, state) = app.settings_win.as_ref().unwrap();
    assert_eq!(state.values.font_size, 20.0);
    assert!(state.values.kitty_keyboard);
    assert_eq!(state.font_size_text, "20");
}

#[test]
fn a_broken_file_makes_the_window_read_only_and_is_never_written() {
    let (mut app, paths) = app();
    std::fs::write(paths.settings(), "font_size = = 3\n").unwrap();
    let _ = app.update(Message::ConfigChanged(SETTINGS_CHANGED));
    assert!(app.settings_win.as_ref().unwrap().1.read_only.is_some());
    send(&mut app, SettingsMsg::Kitty(true));
    assert!(!app.settings.kitty_keyboard);
    assert_eq!(read(&paths).as_deref(), Some("font_size = = 3\n"));
    // Fixed on disk: editable again.
    std::fs::write(paths.settings(), "font_size = 15.0\n").unwrap();
    let _ = app.update(Message::ConfigChanged(SETTINGS_CHANGED));
    assert!(app.settings_win.as_ref().unwrap().1.read_only.is_none());
    send(&mut app, SettingsMsg::Kitty(true));
    assert_eq!(
        read(&paths).as_deref(),
        Some("font_size = 15.0\nkitty_keyboard = true\n")
    );
}
