//! The Settings window against a temporary config directory: live apply, in-place writes,
//! own-write suppression and edits on disk.

use fbterm_config::{Paths, ProfileId, SETTINGS_TEMPLATE, Settings, ThemeMode};

use super::SettingsMsg;
use crate::app::{App, Message};
use crate::watch::Changed;

const SETTINGS_CHANGED: Changed = Changed {
    profiles: false,
    settings: true,
    schemes: false,
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
    // (Startup may already carry environment notices, so compare rather than expect none.)
    let before = app.notices.clone();
    let _ = app.update(Message::ConfigChanged(SETTINGS_CHANGED));
    assert_eq!(app.notices, before);
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

#[test]
fn an_edit_on_disk_not_yet_reported_is_applied_not_hidden_by_our_write() {
    let (mut app, paths) = app();
    // The user saves the file; the watcher has not reported it yet.
    std::fs::write(paths.settings(), "# mine\nfont_size = 20.0\n").unwrap();
    send(&mut app, SettingsMsg::Kitty(true));
    assert_eq!(
        read(&paths).as_deref(),
        Some("# mine\nfont_size = 20.0\nkitty_keyboard = true\n")
    );
    assert_eq!(app.settings.font_size, 20.0, "the edit on disk applies");
    assert!(app.settings.kitty_keyboard);
    assert_eq!(app.settings_win.as_ref().unwrap().1.values.font_size, 20.0);
    // The late watcher report changes nothing more.
    let _ = app.update(Message::ConfigChanged(SETTINGS_CHANGED));
    assert_eq!(app.settings.font_size, 20.0);
    // A broken save that our write cannot merge turns the window read-only.
    std::fs::write(paths.settings(), "font_size = = 1\n").unwrap();
    send(&mut app, SettingsMsg::Theme(ThemeMode::Light));
    assert_eq!(read(&paths).as_deref(), Some("font_size = = 1\n"));
    assert!(app.settings_win.as_ref().unwrap().1.read_only.is_some());
}

#[test]
fn quit_writes_what_is_still_waiting() {
    let (mut app, paths) = app();
    send(&mut app, SettingsMsg::Opacity(0.7));
    assert_eq!(read(&paths), None);
    let _ = app.update(Message::Shortcut(
        app.settings_window().unwrap(),
        crate::keys::Action::Quit,
    ));
    assert_eq!(
        Settings::load(&paths.settings())
            .unwrap()
            .background_opacity,
        0.7
    );
}

#[test]
fn closing_the_window_with_its_button_writes_what_is_still_waiting() {
    let (mut app, paths) = app();
    send(&mut app, SettingsMsg::FontSize(17.0));
    let id = app.settings_window().unwrap();
    let _ = app.update(Message::CloseRequested(id));
    assert_eq!(Settings::load(&paths.settings()).unwrap().font_size, 17.0);
}

#[test]
fn the_redraw_rate_applies_live_and_follows_the_display() {
    use fbterm_config::MaxFps;
    let (mut app, paths) = app();
    assert_eq!(app.receiver.rate.get(), 60);
    send(&mut app, SettingsMsg::MaxFps(MaxFps::Fixed(120)));
    assert_eq!(app.receiver.rate.get(), 120);
    assert!(read(&paths).unwrap().contains("\nmax_fps = 120\n"));
    // "display" with no rate known: the default.
    send(&mut app, SettingsMsg::MaxFps(MaxFps::Display));
    assert_eq!(app.receiver.rate.get(), 60);
    assert!(read(&paths).unwrap().contains("\nmax_fps = \"display\"\n"));
    // Two terminal windows on different displays: the faster one sets the rate.
    let (a, b) = (iced::window::Id::unique(), iced::window::Id::unique());
    app.windows.insert(a, Default::default());
    app.windows.insert(b, Default::default());
    let _ = app.update(Message::DisplayRate(a, Some(60)));
    let _ = app.update(Message::DisplayRate(b, Some(144)));
    assert_eq!(app.receiver.rate.get(), 144);
    let state = &app.settings_win.as_ref().unwrap().1;
    assert_eq!(state.display_hz, Some(144));
    assert_eq!(state.rate_hint(), super::RATE_CPU_HINT);
    // Closing the fast window falls back to the other display.
    let _ = app.update(Message::WindowClosed(b));
    assert_eq!(app.receiver.rate.get(), 60);
    // An edit on disk applies too.
    std::fs::write(paths.settings(), "max_fps = 90\n").unwrap();
    let _ = app.update(Message::ConfigChanged(SETTINGS_CHANGED));
    assert_eq!(app.receiver.rate.get(), 90);
}

#[test]
fn a_rebound_chord_applies_live_and_is_written_under_the_keys_comment() {
    use fbterm_config::keys::{Action, Chord};
    let (mut app, paths) = app();
    let chord = Chord::parse("ctrl+alt+shift+k").unwrap().unwrap();
    send(&mut app, SettingsMsg::Record(Action::Palette));
    send(&mut app, SettingsMsg::Pressed(Some(chord)));
    assert_eq!(app.bindings.chord(Action::Palette), Some(chord));
    assert_eq!(app.bindings.action(chord), Some(Action::Palette));
    assert_eq!(
        read(&paths).unwrap(),
        format!("{SETTINGS_TEMPLATE}[keys]\npalette = \"ctrl+alt+shift+k\"\n")
    );
    // Restore defaults removes the entry and keeps the template's comments.
    send(&mut app, SettingsMsg::ResetKeys);
    assert!(app.bindings.is_default(Action::Palette));
    assert_eq!(
        read(&paths).unwrap(),
        format!("{SETTINGS_TEMPLATE}[keys]\n")
    );
}

#[test]
fn hand_edited_keys_apply_live_and_bad_entries_show_a_notice() {
    use fbterm_config::keys::{Action, Chord};
    let (mut app, paths) = app();
    std::fs::write(
        paths.settings(),
        "[keys]\nnew_window = \"ctrl+alt+shift+n\"\nquit = \"x\"\n",
    )
    .unwrap();
    let _ = app.update(Message::ConfigChanged(SETTINGS_CHANGED));
    let n = Chord::parse("ctrl+alt+shift+n").unwrap().unwrap();
    assert_eq!(app.bindings.chord(Action::NewWindow), Some(n));
    assert!(app.bindings.is_default(Action::Quit));
    let notice = app
        .settings_notice
        .clone()
        .expect("the bad entry is reported");
    assert!(notice.contains("keys.quit"), "{notice}");
    assert!(app.notices.contains(&notice));
    // The window shows the file's chords and stays editable.
    let (_, state) = app.settings_win.as_ref().unwrap();
    assert!(state.read_only.is_none());
    assert_eq!(state.bindings().chord(Action::NewWindow), Some(n));
    // A change from the window leaves the bad entry for the user to fix.
    send(&mut app, SettingsMsg::ClearKey(Action::NewWindow));
    assert_eq!(
        read(&paths).unwrap(),
        "[keys]\nnew_window = \"none\"\nquit = \"x\"\n"
    );
    send(&mut app, SettingsMsg::ResetKey(Action::NewWindow));
    assert_eq!(read(&paths).unwrap(), "[keys]\nquit = \"x\"\n");
    assert!(app.settings_notice.is_some());
}

#[test]
fn leaving_the_settings_window_stops_recording() {
    use fbterm_config::keys::Action;
    let (mut app, _) = app();
    send(&mut app, SettingsMsg::Record(Action::NewTab));
    assert!(app.settings_win.as_ref().unwrap().1.recording.is_some());
    let id = app.settings_window().unwrap();
    let _ = app.update(Message::WindowUnfocused(id));
    assert!(app.settings_win.as_ref().unwrap().1.recording.is_none());
}

#[test]
fn the_gear_button_opens_the_settings_window() {
    let dir = std::env::temp_dir().join(format!("fbterm-settings-test-{}", ProfileId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let (mut app, _) = App::build(Paths { config_dir: dir }, true);
    let w = iced::window::Id::unique();
    app.windows.insert(w, Default::default());
    let _ = app.update(Message::TabBar(w, crate::app::TabBarMsg::Settings));
    assert!(app.settings_window().is_some());
}

#[test]
fn clearing_a_chord_does_not_bring_back_an_entry_load_dropped() {
    use fbterm_config::keys::{Action, Bindings};
    let (mut app, paths) = app();
    let mac = app.bindings.mac();
    let close = app.bindings.chord(Action::CloseTab).unwrap();
    std::fs::write(
        paths.settings(),
        format!("[keys]\nnew_tab = \"{}\"\n", close.to_config(mac)),
    )
    .unwrap();
    let _ = app.update(Message::ConfigChanged(SETTINGS_CHANGED));
    assert!(app.settings_notice.is_some(), "the conflict is reported");
    assert!(app.bindings.is_default(Action::NewTab));
    send(&mut app, SettingsMsg::ClearKey(Action::CloseTab));
    // What runs now is what the file gives on the next launch.
    let reloaded = Settings::load_checked(&paths.settings()).unwrap();
    assert!(reloaded.1.is_none(), "{:?}", read(&paths));
    assert_eq!(Bindings::current(&reloaded.0.keys.map), app.bindings);
    assert_eq!(app.bindings.action(close), None);
    assert!(app.settings_notice.is_none());
}

/// An app with the Settings window open and one tab whose history holds `lines` lines (the
/// default limit, 10,000, is what is saved).
fn app_with_history(lines: usize) -> (App, Paths, fbterm_term::SharedTerminal) {
    use crate::restore_tests::{fill_history, first_terminal, paths_with, profile, saved};
    let a = profile("a");
    let paths = paths_with(std::slice::from_ref(&a));
    fbterm_config::SavedState {
        windows: vec![saved(&[&a], 0)],
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let (mut app, _) = App::build(paths.clone(), true);
    let term = first_terminal(&app);
    fill_history(&term, lines);
    let _ = app.update(Message::Settings(SettingsMsg::Open));
    assert!(app.settings_window().is_some());
    (app, paths, term)
}

#[test]
fn lowering_below_a_tabs_history_asks_before_trimming() {
    let (mut app, paths, term) = app_with_history(500);
    let held = term.lock().history_len();
    assert!(held > 400);
    for typed in ["1", "10", "100"] {
        send(&mut app, SettingsMsg::ScrollbackText(typed.into()));
        assert_eq!(term.lock().history_len(), held, "{typed} trimmed");
        assert_eq!(app.settings.scrollback_lines, 10_000);
    }
    assert_eq!(
        app.settings_win.as_ref().unwrap().1.pending_lower,
        Some(100)
    );
    send(&mut app, SettingsMsg::ScrollbackSubmit);
    assert_eq!(term.lock().history_len(), held);
    assert_eq!(read(&paths), None, "nothing written");
    // Cancel keeps everything.
    send(&mut app, SettingsMsg::ScrollbackCancel);
    assert_eq!(term.lock().history_len(), held);
    assert_eq!(
        app.settings_win.as_ref().unwrap().1.scrollback_text,
        "10000"
    );
    // Apply trims, saves, and sets no notice.
    send(&mut app, SettingsMsg::ScrollbackText("100".into()));
    send(&mut app, SettingsMsg::ScrollbackApply);
    assert_eq!(term.lock().history_len(), 100);
    assert_eq!(app.settings.scrollback_lines, 100);
    assert!(app.settings_notice.is_none());
    assert_eq!(
        Settings::load(&paths.settings()).unwrap().scrollback_lines,
        100
    );
}

#[test]
fn lowering_above_every_tabs_history_saves_at_once() {
    let (mut app, paths, term) = app_with_history(500);
    let held = term.lock().history_len();
    assert!(held > 400);
    send(
        &mut app,
        SettingsMsg::ScrollbackText((held + 100).to_string()),
    );
    // Typed, not applied per keystroke: it waits for the quiet period.
    assert_eq!(app.settings.scrollback_lines, 10_000);
    let at = app.settings_win.as_ref().unwrap().1.generation;
    send(&mut app, SettingsMsg::Flush(at));
    assert_eq!(app.settings.scrollback_lines, held + 100);
    assert_eq!(term.lock().history_len(), held);
    assert_eq!(app.settings_win.as_ref().unwrap().1.pending_lower, None);
    send(&mut app, SettingsMsg::ScrollbackSubmit);
    assert_eq!(
        Settings::load(&paths.settings()).unwrap().scrollback_lines,
        held + 100
    );
}

#[test]
fn typing_a_long_number_never_trims_a_tab_with_little_history() {
    let (mut app, _paths, term) = app_with_history(300);
    let held = term.lock().history_len();
    assert!(held > 250 && held < 2_000);
    for typed in ["2", "20", "200", "2000", "20000"] {
        send(&mut app, SettingsMsg::ScrollbackText(typed.into()));
        assert_eq!(term.lock().history_len(), held, "{typed} trimmed the tab");
    }
    assert_eq!(app.settings.scrollback_lines, 20_000);
}

#[test]
fn typing_a_long_number_does_not_cap_output_arriving_meanwhile() {
    // A tab with no history yet: no typed prefix exceeds it, but output scrolling between
    // keystrokes must still be kept.
    let (mut app, _paths, term) = app_with_history(0);
    crate::restore_tests::fill_history(&term, 40);
    let mut expected = term.lock().history_len();
    for typed in ["2", "20", "200", "2000", "20000"] {
        send(&mut app, SettingsMsg::ScrollbackText(typed.into()));
        crate::restore_tests::fill_history(&term, 40);
        expected += 40;
        assert_eq!(term.lock().history_len(), expected, "{typed} capped output");
    }
}

#[test]
fn a_lower_value_nothing_exceeds_applies_when_the_quiet_period_passes() {
    let (mut app, _paths, term) = app_with_history(300);
    let held = term.lock().history_len();
    send(&mut app, SettingsMsg::ScrollbackText("2000".into()));
    assert_eq!(app.settings.scrollback_lines, 10_000);
    // A flush for an earlier keystroke is stale.
    let at = app.settings_win.as_ref().unwrap().1.generation;
    send(&mut app, SettingsMsg::Flush(at - 1));
    assert_eq!(app.settings.scrollback_lines, 10_000);
    send(&mut app, SettingsMsg::Flush(at));
    assert_eq!(app.settings.scrollback_lines, 2_000);
    assert_eq!(term.lock().history_len(), held);
    // The same through Enter.
    send(&mut app, SettingsMsg::ScrollbackText("1500".into()));
    assert_eq!(app.settings.scrollback_lines, 2_000);
    send(&mut app, SettingsMsg::ScrollbackSubmit);
    assert_eq!(app.settings.scrollback_lines, 1_500);
}

/// Run the search the open find bar of `wid`'s first tab is waiting for and deliver it.
fn finish_search(app: &mut App, wid: iced::window::Id, term: &fbterm_term::SharedTerminal) {
    let bar = app.windows[&wid].tabs[0]
        .focused_pane()
        .find
        .as_ref()
        .unwrap();
    let seq = bar.pending_seq().expect("a search is running");
    let id = app.windows[&wid].tabs[0].focused;
    let outcome = fbterm_term::search(
        term,
        &fbterm_term::SearchRequest {
            pattern: bar.pattern().cloned().unwrap(),
            below: None,
            limit: fbterm_term::MATCH_CAP,
            snippets: false,
        },
        &std::sync::atomic::AtomicBool::new(false),
    );
    let _ = app.found(id, seq, outcome);
}

fn matches_in(app: &App, wid: iced::window::Id) -> usize {
    let counter = app.windows[&wid].tabs[0]
        .focused_pane()
        .find
        .as_ref()
        .unwrap()
        .counter();
    counter.rsplit(' ').next().unwrap().parse().unwrap()
}

#[test]
fn lowering_scrollback_refreshes_an_open_find_bar() {
    use crate::find::FindMsg;
    let (mut app, _paths, term) = app_with_history(500);
    let wid = *app.windows.keys().next().unwrap();
    let _ = app.find(wid, FindMsg::Open);
    let _ = app.find(wid, FindMsg::Query("line".into()));
    finish_search(&mut app, wid, &term);
    let before = matches_in(&app, wid);
    assert!(before > 400, "{before}");
    send(&mut app, SettingsMsg::ScrollbackText("100".into()));
    send(&mut app, SettingsMsg::ScrollbackApply);
    assert!(term.lock().history_len() <= 100);
    // The bar searches again, now or when its timer fires.
    let id = app.windows[&wid].tabs[0].focused;
    if app.windows[&wid].tabs[0]
        .focused_pane()
        .find
        .as_ref()
        .unwrap()
        .refresh_scheduled
    {
        let _ = app.find_refresh(id);
    }
    finish_search(&mut app, wid, &term);
    let after = matches_in(&app, wid);
    assert!(after < before && after <= 100 + 24, "{before} -> {after}");
}

#[test]
fn settings_scheme_choice_writes_and_applies() {
    let (mut app, paths) = app();
    send(&mut app, SettingsMsg::ColorScheme("ember".into()));
    assert_eq!(app.settings.color_scheme, "ember");
    assert_eq!(
        Settings::load(&paths.settings()).unwrap().color_scheme,
        "ember"
    );
    let (_, state) = app.settings_win.as_ref().unwrap();
    assert_eq!(state.values.color_scheme, "ember");
}

#[test]
fn settings_lists_builtins_first() {
    let (app, _) = app();
    let ids: Vec<_> = app
        .scheme_choices(false)
        .into_iter()
        .map(|c| c.id.unwrap())
        .collect();
    assert_eq!(ids[..4], ["graphite", "graphite-light", "harbor", "ember"]);
}

#[test]
fn lists_refresh_after_scheme_set_changes() {
    let (mut app, paths) = app();
    assert_eq!(app.scheme_choices(false).len(), 4);
    let mut mine = fbterm_config::schemes::builtins().remove(0);
    mine.id = fbterm_config::schemes::new_user_id();
    mine.name = "Mine".into();
    mine.builtin = false;
    fbterm_config::schemes::save_user(&paths.schemes(), &mine).unwrap();
    let _ = app.update(Message::ConfigChanged(Changed {
        profiles: false,
        settings: false,
        schemes: true,
    }));
    let own = app.scheme_choices(false);
    assert_eq!(own.len(), 5);
    assert_eq!(own[4].name, "Mine");
    let with_global = app.scheme_choices(true);
    assert_eq!(with_global.len(), 6);
    assert_eq!(with_global[0].id, None);
    assert_eq!(with_global[0].to_string(), "Use global scheme");
}

#[test]
fn open_windows_remember_whether_they_were_created_opaque() {
    let (mut app, _paths) = app();
    app.windows.clear();
    assert!(!app.opaque_window_open());
    // At full opacity a window is created opaque, except on macOS (always transparent).
    let (first, _) = app.new_window((800.0, 600.0));
    let mac = cfg!(target_os = "macos");
    assert_eq!(app.windows[&first].transparent, mac);
    assert_eq!(app.opaque_window_open(), !mac);
    // Lowering the opacity leaves it as it was made; a window opened now is transparent.
    send(&mut app, SettingsMsg::Opacity(0.8));
    let (second, _) = app.new_window((800.0, 600.0));
    assert!(app.windows[&second].transparent);
    assert_eq!(app.opaque_window_open(), !mac);
    app.windows.remove(&first);
    assert!(!app.opaque_window_open());
}

#[test]
fn the_key_log_never_listens_while_it_is_off() {
    use fbterm_config::keys::Action;
    let (mut app, _paths) = app();
    assert_eq!(app.recording_window(), None);
    assert!(!app.key_log_listening());
    send(&mut app, SettingsMsg::Record(Action::Palette));
    // A recorder is active, but FBTERM_LOG_KEYS is not set in tests: no listener.
    assert!(app.recording_window().is_some());
    assert!(!app.key_log_listening());
}
