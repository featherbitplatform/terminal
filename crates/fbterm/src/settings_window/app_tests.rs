//! The Settings tab against a temporary config directory: live apply, in-place writes,
//! own-write suppression and edits on disk.

use fbterm_config::keys::Action;
use fbterm_config::{Paths, ProfileId, SETTINGS_TEMPLATE, Settings, TabBar};

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
    send(&mut app, SettingsMsg::Kitty(true));
    assert!(app.settings.kitty_keyboard);
    let text = read(&paths).unwrap();
    assert_eq!(
        text,
        SETTINGS_TEMPLATE.replace(
            "#kitty_keyboard = false\n",
            "#kitty_keyboard = false\nkitty_keyboard = true\n"
        )
    );
    // The watcher's report of our own write changes nothing.
    // (Startup may already carry environment notices, so compare rather than expect none.)
    let before = app.notices.clone();
    let _ = app.update(Message::ConfigChanged(SETTINGS_CHANGED));
    assert_eq!(app.notices, before);
    assert!(app.settings.kitty_keyboard);
}

#[test]
fn the_tabs_row_and_the_action_flip_the_side_and_write_it() {
    let (mut app, paths) = app();
    send(&mut app, SettingsMsg::TabBar(TabBar::Left));
    assert!(app.settings.tabs_left());
    assert!(
        read(&paths)
            .unwrap()
            .contains("#tab_bar = \"top\"\ntab_bar = \"left\"\n")
    );
    let _ = app.action(None, Action::VerticalTabs);
    assert_eq!(app.settings.tab_bar, Some(TabBar::Top));
    assert!(read(&paths).unwrap().contains("\ntab_bar = \"top\"\n"));
    // The open Settings tab shows the new value.
    let (_, state) = app.settings_win.as_ref().unwrap();
    assert_eq!(state.values.tab_bar, Some(TabBar::Top));
    let _ = app.action(None, Action::VerticalTabs);
    assert_eq!(app.settings.tab_bar, Some(TabBar::Left));
    assert!(read(&paths).unwrap().contains("\ntab_bar = \"left\"\n"));
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
    send(&mut app, SettingsMsg::Ligatures(false));
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
fn switching_away_from_the_settings_tab_stops_recording() {
    use crate::app::TabBarMsg;
    use crate::tabs::{Pane, Tab};
    use fbterm_config::keys::Action;
    let (mut app, _) = app();
    let wid = app.settings_window().unwrap();
    let terminal = app.windows.get_mut(&wid).unwrap();
    terminal.push(Tab::single(Pane::for_profile(
        &crate::restore_tests::profile("a"),
    )));
    let settings = terminal.tabs.iter().position(|t| t.page.is_some()).unwrap();
    let shell = terminal.tabs.iter().position(|t| t.page.is_none()).unwrap();
    let _ = app.update(Message::TabBar(wid, TabBarMsg::Select(settings)));
    send(&mut app, SettingsMsg::Record(Action::NewTab));
    assert_eq!(app.recording_window(), Some(wid));
    let _ = app.update(Message::TabBar(wid, TabBarMsg::Select(shell)));
    assert_eq!(app.recording_window(), None);
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

/// An app with the Settings tab open and one tab whose history holds `lines` lines (the
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
            drop_cache: true,
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
    // Settings is a tab of its own: find works in the terminal tab beside it.
    app.windows.get_mut(&wid).unwrap().select(0);
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

/// An app with two tabs, each with a terminal holding `lines` lines of history.
fn app_with_two_tabs(lines: usize) -> (App, Vec<fbterm_term::SharedTerminal>) {
    use crate::restore_tests::{fill_history, paths_with, profile, saved};
    let (a, b) = (profile("a"), profile("b"));
    let paths = paths_with(&[a.clone(), b.clone()]);
    fbterm_config::SavedState {
        windows: vec![saved(&[&a, &b], 0)],
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let (mut app, _) = App::build(paths, true);
    // A background tab starts on first show.
    let _ = app.update(Message::SelectTab(1));
    let terms: Vec<_> = app
        .windows
        .values()
        .flat_map(|w| &w.tabs)
        .flat_map(|t| &t.panes)
        .filter_map(|p| p.terminal().cloned())
        .collect();
    assert_eq!(terms.len(), 2);
    for t in &terms {
        fill_history(t, lines);
    }
    (app, terms)
}

fn apply(app: &mut App, edit: impl FnOnce(&mut Settings)) {
    let mut new = app.settings.clone();
    edit(&mut new);
    let _ = app.apply_settings(new, crate::app::SettingsSource::Window);
}

#[test]
fn compression_setting_reaches_open_terminals() {
    let (mut app, terms) = app_with_two_tabs(50);
    assert!(
        terms
            .iter()
            .all(|t| t.lock().history_compression().is_none())
    );
    apply(&mut app, |s| s.history_compression = true);
    let keep = app.settings.history_compression_keep();
    assert!(keep.is_some());
    for t in &terms {
        assert_eq!(t.lock().history_compression(), keep);
    }
}

#[test]
fn compression_off_applies_to_open_terminals() {
    let (mut app, terms) = app_with_two_tabs(50);
    apply(&mut app, |s| s.history_compression = true);
    apply(&mut app, |s| s.history_compression = false);
    for t in &terms {
        assert_eq!(t.lock().history_compression(), None);
    }
}

/// Switching pictures off frees every pane's store share, not only when that pane next prints
/// something (finding I2). The emulator queues one `ImageReleased` per picture; what releases them to
/// the store is delivering the terminal's queue, which this apply now does for each pane. Nothing
/// else feeds these panes: their tabs are quiet, and their bytes are gone anyway.
#[test]
fn switching_pictures_off_frees_an_idle_panes_store() {
    use crate::tabs::install_session;
    use fbterm_session::SessionHandle;
    use fbterm_term::images::store;

    let (mut app, _terms) = app_with_two_tabs(0);
    assert!(app.settings.inline_images, "pictures are on by default");

    // Give each pane a session that routes, over the terminal it already shows: a picture arrives,
    // delivery puts it in the store, and nothing else feeds this pane afterwards.
    let mut installed: Vec<(fbterm_term::SharedTerminal, u64)> = Vec::new();
    for window in app.windows.values_mut() {
        for tab in &mut window.tabs {
            for pane in &mut tab.panes {
                let term = pane
                    .placeholder
                    .clone()
                    .expect("a pane whose shell has not connected shows a placeholder");
                let owner = term.lock().image_owner();
                // A 1x1 raw RGBA transmission: the terminal queues it for whoever routes.
                term.lock()
                    .feed(b"\x1b_Gi=1,a=t,f=32,s=1,v=1;AAAAAA==\x1b\\");
                let (handle, _log) = SessionHandle::recording(term.clone());
                handle.deliver_events(); // the way output normally reaches the store
                assert!(
                    store().owner_bytes(owner) > 0,
                    "the picture should be held before the switch"
                );
                let _notice = install_session(pane, handle);
                installed.push((term, owner));
            }
        }
    }
    assert_eq!(installed.len(), 2, "two panes");

    apply(&mut app, |s| s.inline_images = false);

    for (term, owner) in &installed {
        assert_eq!(
            store().owner_bytes(*owner),
            0,
            "an idle pane kept its pictures after the switch went off"
        );
        assert!(
            term.lock().term().visible_placements().is_empty(),
            "and it should not be drawing them either"
        );
    }
}

#[test]
fn switching_on_requests_release_once_idle() {
    let (mut app, _terms) = app_with_two_tabs(50);
    let before = crate::memory::requests();
    apply(&mut app, |s| s.history_compression = true);
    // The release waits for the worker: nothing yet, then exactly one request once idle.
    assert_eq!(crate::memory::requests(), before);
    // The returned task runs this function on the blocking pool; a test cannot observe a
    // request made on another thread (the counter is per thread), so it runs here.
    super::glue::release_when_idle();
    assert_eq!(crate::memory::requests(), before + 1);
}

#[test]
fn two_freeing_changes_back_to_back_request_one_release() {
    let (mut app, _terms) = app_with_two_tabs(50);
    let before = crate::memory::requests();
    let units = |app: &mut App, keep: usize| {
        let mut new = app.settings.clone();
        new.history_compression = true;
        new.uncompressed_history_lines = Some(keep);
        app.apply_settings(new, crate::app::SettingsSource::Window)
            .units()
    };
    assert_eq!(units(&mut app, 5_000), 1);
    // A second lowering while the first release waits spawns no second waiter.
    assert_eq!(units(&mut app, 2_000), 0);
    super::glue::release_when_idle();
    assert_eq!(crate::memory::requests(), before + 1);
    // Once it ran, the next freeing change starts a new one.
    assert_eq!(units(&mut app, 1_000), 1);
}

#[test]
fn only_a_freeing_change_asks_for_the_release_task() {
    let (mut app, _terms) = app_with_two_tabs(50);
    let units = |app: &mut App, edit: &dyn Fn(&mut Settings)| {
        let mut new = app.settings.clone();
        edit(&mut new);
        app.apply_settings(new, crate::app::SettingsSource::Window)
            .units()
    };
    assert_eq!(units(&mut app, &|s| s.history_compression = true), 1);
    // The release ran (a pending one would absorb the next freeing change).
    super::glue::release_when_idle();
    // Lowering the kept lines frees memory; raising them again does not.
    assert_eq!(
        units(&mut app, &|s| s.uncompressed_history_lines = Some(2_000)),
        1
    );
    super::glue::release_when_idle();
    assert_eq!(
        units(&mut app, &|s| s.uncompressed_history_lines = Some(5_000)),
        0
    );
    // Turning off does not.
    assert_eq!(units(&mut app, &|s| s.history_compression = false), 0);
}

#[test]
fn output_driven_compression_never_requests_release() {
    let (mut app, terms) = app_with_two_tabs(0);
    apply(&mut app, |s| s.history_compression = true);
    let before = crate::memory::requests();
    let worker = fbterm_term::history_worker::Worker::new();
    for t in &terms {
        {
            let mut g = t.lock();
            for i in 0..12_000 {
                g.feed(format!("output line {i} with some padding to fill blocks\r\n").as_bytes());
            }
        }
        worker.enqueue(t);
    }
    worker.run_until_idle();
    assert_eq!(crate::memory::requests(), before);
}

#[test]
fn lowering_scrollback_below_uncompressed_applies_both() {
    let (mut app, terms) = app_with_two_tabs(50);
    apply(&mut app, |s| {
        s.history_compression = true;
        s.uncompressed_history_lines = Some(8_000);
    });
    assert_eq!(app.settings.history_compression_keep(), Some(8_000));
    apply(&mut app, |s| s.scrollback_lines = 3_000);
    let keep = app.settings.history_compression_keep();
    assert_eq!(keep, Some(2_999));
    for t in &terms {
        assert_eq!(t.lock().history_compression(), keep);
    }
}

/// A resize that changes a compressed terminal's columns rebuilds its history uncompressed
/// (a reflow); the worker compresses it again, and the memory that frees goes back to the OS
/// once it drains: one release per settled resize, however many steps a drag had, and one
/// waiting at most. Lines-only resizes and terminals without compression ask for none.
#[test]
fn a_reflow_with_compression_on_asks_for_one_release() {
    use fbterm_term::{GridSize, TerminalOutput};
    let (mut app, terms) = app_with_two_tabs(50);
    apply(&mut app, |s| s.history_compression = true);
    // The switch-on release ran.
    super::glue::release_when_idle();
    let (wid, pane) = app
        .windows
        .iter()
        .find_map(|(wid, w)| {
            w.tabs.iter().flat_map(|t| &t.panes).find_map(|p| {
                p.terminal()
                    .is_some_and(|t| std::sync::Arc::ptr_eq(t, &terms[0]))
                    .then_some((*wid, p.id))
            })
        })
        .unwrap();
    let grid = |cols: u16, rows: u16| GridSize {
        cols,
        rows,
        ..crate::app::DEFAULT_GRID
    };
    let resized = |app: &mut App, g: GridSize| {
        let _ = app.update(Message::Terminal(wid, pane, TerminalOutput::Resized(g)));
        app.pending_resizes[&pane].0
    };
    let settle = |app: &mut App, generation: u64| {
        app.update(Message::ResizeSettled(pane, generation)).units()
    };

    // The first layout is not a reflow of anything the worker compressed.
    let g = resized(&mut app, grid(100, 30));
    assert_eq!(settle(&mut app, g), 0);

    // A drag: several column changes, one settle, one release task.
    let generations: Vec<u64> = [90, 80, 70]
        .into_iter()
        .map(|cols| resized(&mut app, grid(cols, 30)))
        .collect();
    for &stale in &generations[..2] {
        assert_eq!(settle(&mut app, stale), 0);
    }
    assert_eq!(settle(&mut app, generations[2]), 1);
    // Another reflow while that release waits for the worker spawns no second waiter.
    let g = resized(&mut app, grid(60, 30));
    assert_eq!(settle(&mut app, g), 0);
    let before = crate::memory::requests();
    super::glue::release_when_idle();
    assert_eq!(crate::memory::requests(), before + 1);

    // Lines only: no reflow, no release.
    let g = resized(&mut app, grid(60, 20));
    assert_eq!(settle(&mut app, g), 0);
    // A divider drag ending flushes the resize at once: the release comes with it.
    resized(&mut app, grid(50, 20));
    assert_eq!(
        app.update(Message::Tiles(wid, crate::tiles::TilesMsg::EndDrag))
            .units(),
        1
    );
    super::glue::release_when_idle();

    // With compression off a reflow frees nothing to release.
    apply(&mut app, |s| s.history_compression = false);
    let g = resized(&mut app, grid(40, 20));
    assert_eq!(settle(&mut app, g), 0);
}

/// A reflow rebuilds a compressed terminal's history uncompressed, and nothing else may come to
/// queue it (the program can stay silent): the settled resize hands it to the worker, which
/// compresses it again before the release.
#[test]
fn a_reflow_queues_the_terminal_for_recompression() {
    use fbterm_term::{GridSize, TerminalOutput};
    let (mut app, terms) = app_with_two_tabs(0);
    apply(&mut app, |s| {
        s.scrollback_lines = 100_000;
        s.history_compression = true;
        s.uncompressed_history_lines = Some(1_000);
    });
    let term = &terms[0];
    {
        let mut t = term.lock();
        for i in 0..20_000 {
            t.feed(format!("service line {i} with some padding to fill blocks\r\n").as_bytes());
        }
        t.run_history_jobs();
        assert!(!t.has_history_jobs());
    }
    super::glue::release_when_idle();
    let (wid, pane) = app
        .windows
        .iter()
        .find_map(|(wid, w)| {
            w.tabs.iter().flat_map(|t| &t.panes).find_map(|p| {
                p.terminal()
                    .is_some_and(|t| std::sync::Arc::ptr_eq(t, term))
                    .then_some((*wid, p.id))
            })
        })
        .unwrap();
    let grid = |cols: u16| GridSize {
        cols,
        rows: 30,
        ..crate::app::DEFAULT_GRID
    };
    // What the widget does: resize the terminal, then report it; the settle flushes it.
    let resize = |app: &mut App, g: GridSize| {
        term.lock().resize(g);
        let _ = app.update(Message::Terminal(wid, pane, TerminalOutput::Resized(g)));
        let generation = app.pending_resizes[&pane].0;
        let _ = app.update(Message::ResizeSettled(pane, generation));
    };
    resize(&mut app, grid(100));
    resize(&mut app, grid(70));
    assert!(
        fbterm_term::history_worker::wait_idle_for(std::time::Duration::from_secs(10)),
        "worker did not drain"
    );
    assert!(
        !term.lock().has_history_jobs(),
        "the reflowed history was never queued for compression"
    );
    super::glue::release_when_idle();
}

/// A stored `uncompressed_history_lines` out of range while compression is off: switching it on
/// in the Settings tab writes the clamped value with the switch, so the file loads with no
/// notice and the field shows no error.
#[test]
fn switching_on_in_the_tab_writes_a_clamped_uncompressed_value() {
    let (mut app, paths) = app();
    std::fs::write(
        paths.settings(),
        "scrollback_lines = 5000\nuncompressed_history_lines = 8000\n",
    )
    .unwrap();
    let _ = app.update(Message::ConfigChanged(SETTINGS_CHANGED));
    assert_eq!(app.settings.uncompressed_history_lines, Some(8_000));
    send(&mut app, SettingsMsg::HistoryCompression(true));
    let (loaded, notice) = Settings::load_checked(&paths.settings()).unwrap();
    assert!(notice.is_none(), "{notice:?}");
    assert!(loaded.history_compression);
    assert_eq!(loaded.uncompressed_history_lines, Some(4_999));
    assert_eq!(app.settings.uncompressed_history_lines, Some(4_999));
    let state = &app.settings_win.as_ref().unwrap().1;
    assert_eq!(state.uncompressed_error(), None);
    assert_eq!(state.uncompressed_text, "4999");
}

/// A value typed into the uncompressed field and still waiting on its quiet period is applied
/// and written when the Settings tab closes.
#[test]
fn closing_the_tab_applies_a_typed_uncompressed_value() {
    let (mut app, paths) = app();
    send(&mut app, SettingsMsg::HistoryCompression(true));
    send(&mut app, SettingsMsg::UncompressedText("5000".into()));
    send(&mut app, SettingsMsg::Close);
    assert!(app.settings_window().is_none());
    assert_eq!(app.settings.uncompressed_history_lines, Some(5_000));
    assert_eq!(
        Settings::load(&paths.settings())
            .unwrap()
            .uncompressed_history_lines,
        Some(5_000)
    );
}

/// Quitting writes a value still typed into the uncompressed field.
#[test]
fn quit_writes_a_typed_uncompressed_value() {
    let (mut app, paths) = app();
    send(&mut app, SettingsMsg::HistoryCompression(true));
    send(&mut app, SettingsMsg::UncompressedText("5000".into()));
    let _ = app.update(Message::Shortcut(
        app.settings_window().unwrap(),
        crate::keys::Action::Quit,
    ));
    assert_eq!(
        Settings::load(&paths.settings())
            .unwrap()
            .uncompressed_history_lines,
        Some(5_000)
    );
}

/// The text of every screen row of `t`, and its history length (with the alternate screen up,
/// the alternate screen's).
fn shown(t: &fbterm_term::SharedTerminal) -> (Vec<String>, usize) {
    use feathercritty::grid::Dimensions;
    use feathercritty::index::Line;
    let t = t.lock();
    let grid = t.term().grid();
    let rows = (0..grid.screen_lines() as i32)
        .map(|l| grid.screen()[Line(l)].into_iter().map(|c| c.c).collect())
        .collect();
    (rows, grid.history_size())
}

/// A tab with the alternate screen up when compression is switched on and off: the
/// process-wide worker compresses (and then decompresses) the normal screen's history behind
/// it, and the alternate screen keeps its content and stays without history.
#[test]
fn compression_leaves_an_alternate_screen_alone() {
    let (mut app, terms) = app_with_two_tabs(0);
    let t = &terms[0];
    {
        let mut g = t.lock();
        for i in 0..10_000 {
            g.feed(format!("normal line {i:05} {}\r\n", "x".repeat(90)).as_bytes());
        }
        g.feed(b"\x1b[?1049h");
        for i in 0..60 {
            g.feed(format!("alt line {i}\r\n").as_bytes());
        }
        assert!(g.mode().contains(fbterm_term::TermMode::ALT_SCREEN));
    }
    let before = shown(t);
    assert_eq!(before.1, 0);
    assert!(before.0.iter().any(|r| r.starts_with("alt line 59")));

    for on in [true, false] {
        // The switch queues the terminal on the process-wide worker; everything is read once
        // that worker has drained, so nothing races its thread.
        apply(&mut app, |s| {
            s.history_compression = on;
            s.uncompressed_history_lines = Some(1_000);
        });
        assert!(fbterm_term::history_worker::wait_idle_for(
            std::time::Duration::from_secs(10)
        ));
        let mut g = t.lock();
        assert_eq!(g.history_compression(), on.then_some(1_000));
        assert!(
            g.next_history_jobs(usize::MAX).is_empty(),
            "the worker drained it"
        );
        assert!(!g.has_history_jobs());
        drop(g);
        assert_eq!(shown(t), before, "compression {on}");
    }
}
