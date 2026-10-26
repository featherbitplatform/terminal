//! Session restore, state saving and live config reload, against a temporary config directory.

use std::path::PathBuf;

use fbterm_config::{
    LocalProfile, Paths, Profile, ProfileId, ProfileKind, ProfileStore, SavedState, SavedWindow,
};

use crate::app::{App, Message};
use crate::tabs::TabState;
use crate::watch::Changed;

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("fbterm-restore-test-{}", ProfileId::new()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A profile whose program does not exist: starting its tab fails at once (state
/// `Disconnected`), so "was started" is observable without spawning anything.
pub(crate) fn profile(name: &str) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        group: None,
        accent: None,
        accent_style: Default::default(),
        kind: ProfileKind::Local(LocalProfile {
            program: "/nonexistent/fbterm-test-shell".into(),
            args: vec![],
            cwd: None,
            env: Default::default(),
        }),
    }
}

pub(crate) fn paths_with(profiles: &[Profile]) -> Paths {
    let paths = Paths { config_dir: dir() };
    ProfileStore {
        profiles: profiles.to_vec(),
    }
    .save(&paths.profiles())
    .unwrap();
    paths
}

pub(crate) fn saved(tabs: &[&Profile], active: usize) -> SavedWindow {
    SavedWindow {
        tabs: tabs.iter().map(|p| p.id).collect(),
        active,
        size: (900.0, 500.0),
    }
}

fn states(app: &App) -> Vec<Vec<bool>> {
    // Per window: whether each tab was started.
    app.windows
        .values()
        .map(|w| {
            w.tabs
                .iter()
                .map(|t| t.state != TabState::NotConnected)
                .collect()
        })
        .collect()
}

#[test]
fn relaunch_restores_windows_and_connects_only_the_active_tabs() {
    let (a, b, c) = (profile("a"), profile("b"), profile("c"));
    let paths = paths_with(&[a.clone(), b.clone(), c.clone()]);
    SavedState {
        windows: vec![saved(&[&a, &b], 1), saved(&[&c, &a], 0)],
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let (app, _) = App::build(paths, true);
    assert_eq!(app.windows.len(), 2);
    assert_eq!(
        states(&app),
        vec![vec![false, true], vec![true, false]],
        "only each window's active tab started"
    );
    let first = app.windows.values().next().unwrap();
    assert_eq!(first.active, 1);
    assert_eq!(first.tabs[0].profile, a.id);
    assert_eq!(app.sizes.values().next(), Some(&(900.0, 500.0)));
}

#[test]
fn restore_skips_missing_profiles_and_falls_back_to_the_default_window() {
    let a = profile("a");
    let gone = profile("gone");
    let paths = paths_with(std::slice::from_ref(&a));
    SavedState {
        windows: vec![saved(&[&gone], 0), saved(&[&gone, &a], 0)],
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let (app, _) = App::build(paths, true);
    assert_eq!(app.windows.len(), 1);
    assert_eq!(app.windows.values().next().unwrap().tabs.len(), 1);

    // Nothing restorable: the default single window, with one tab.
    let paths = paths_with(std::slice::from_ref(&a));
    SavedState {
        windows: vec![saved(&[&gone], 0)],
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let (app, _) = App::build(paths, true);
    assert_eq!(app.windows.len(), 1);
}

#[test]
fn recency_comes_back() {
    let a = profile("a");
    let paths = paths_with(std::slice::from_ref(&a));
    SavedState {
        recency: [(a.id, 1234)].into(),
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let (app, _) = App::build(paths, true);
    assert!(app.recency.get(a.id).is_some_and(|t| t >= 1234));
}

#[test]
fn layout_changes_are_saved_and_debug_runs_do_not_save() {
    let (a, b) = (profile("a"), profile("b"));
    let paths = paths_with(&[a.clone(), b.clone()]);
    SavedState {
        windows: vec![saved(&[&a], 0)],
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let (mut app, _) = App::build(paths.clone(), true);
    let wid = *app.windows.keys().next().unwrap();
    let _ = app.update(Message::OpenProfile {
        profile: b.id,
        window: wid,
    });
    let on_disk = SavedState::load_or_default(&paths.state());
    assert_eq!(on_disk.windows[0].tabs, vec![a.id, b.id]);
    assert_eq!(on_disk.windows[0].active, 1);
    assert!(on_disk.recency.contains_key(&b.id));

    let _ = app.update(Message::TabBar(wid, crate::app::TabBarMsg::Select(0)));
    assert_eq!(
        SavedState::load_or_default(&paths.state()).windows[0].active,
        0
    );
    let _ = app.update(Message::TabBar(wid, crate::app::TabBarMsg::Close(1)));
    assert_eq!(
        SavedState::load_or_default(&paths.state()).windows[0].tabs,
        vec![a.id]
    );

    // A debug aid that opens tabs must not overwrite the real layout.
    let before = std::fs::read_to_string(paths.state()).unwrap();
    app.state_saving_enabled = false;
    let _ = app.update(Message::OpenProfile {
        profile: b.id,
        window: wid,
    });
    assert_eq!(std::fs::read_to_string(paths.state()).unwrap(), before);
}

#[test]
fn unreadable_profiles_neither_restore_nor_clobber_the_state_file() {
    let a = profile("a");
    let paths = paths_with(std::slice::from_ref(&a));
    SavedState {
        windows: vec![saved(&[&a], 0)],
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let before = std::fs::read_to_string(paths.state()).unwrap();
    std::fs::write(paths.profiles(), "[[profiles]\nbroken").unwrap();
    let (mut app, _) = App::build(paths.clone(), true);
    assert!(app.profiles_error.is_some());
    let wid = *app.windows.keys().next().unwrap();
    let _ = app.update(Message::OpenProfile {
        profile: a.id,
        window: wid,
    });
    app.save_state();
    assert_eq!(std::fs::read_to_string(paths.state()).unwrap(), before);
}

#[test]
fn a_hand_edit_of_profiles_replaces_them_and_keeps_open_tabs() {
    let (a, b) = (profile("a"), profile("b"));
    let paths = paths_with(std::slice::from_ref(&a));
    let (mut app, _) = App::build(paths.clone(), true);
    assert!(app.profiles.get(b.id).is_none());
    let renamed = Profile {
        name: "a2".into(),
        ..a.clone()
    };
    ProfileStore {
        profiles: vec![renamed, b.clone()],
    }
    .save(&paths.profiles())
    .unwrap();
    let _ = app.update(Message::ConfigChanged(Changed {
        profiles: true,
        settings: false,
    }));
    assert!(app.profiles.get(b.id).is_some());
    assert!(app.profiles_error.is_none());
    let tab = &app.windows.values().next().unwrap().tabs[0];
    assert_eq!((tab.profile, tab.name.as_str()), (a.id, "a2"));
}

#[test]
fn a_broken_edit_keeps_the_previous_profiles_and_goes_read_only_until_fixed() {
    let a = profile("a");
    let paths = paths_with(std::slice::from_ref(&a));
    let (mut app, _) = App::build(paths.clone(), true);
    let changed = Changed {
        profiles: true,
        settings: false,
    };

    let broken = "[[profiles]\nname = 1\n";
    std::fs::write(paths.profiles(), broken).unwrap();
    let _ = app.update(Message::ConfigChanged(changed));
    assert!(app.profiles.get(a.id).is_some(), "previous profiles kept");
    let error = app.profiles_error.clone().expect("flagged");
    assert!(error.line.is_some());
    assert!(
        app.visible_notices()
            .iter()
            .any(|n| n.starts_with(&format!("{}:", paths.profiles().display()))),
        "banner names path:line"
    );
    assert_eq!(
        std::fs::read_to_string(paths.profiles()).unwrap(),
        broken,
        "the file is never overwritten"
    );

    // The same broken text again changes nothing (no duplicate banner lines).
    let count = app.notices.len();
    let _ = app.update(Message::ConfigChanged(changed));
    assert_eq!(app.notices.len(), count);

    // Fixed: loads, flag and banner clear.
    ProfileStore {
        profiles: vec![a.clone()],
    }
    .save(&paths.profiles())
    .unwrap();
    let _ = app.update(Message::ConfigChanged(changed));
    assert!(app.profiles_error.is_none());
    assert!(app.notices.is_empty(), "{:?}", app.notices);
}

#[test]
fn a_change_event_for_text_we_already_have_is_ignored() {
    let a = profile("a");
    let paths = paths_with(std::slice::from_ref(&a));
    let (mut app, _) = App::build(paths, true);
    // Stands in for the in-memory result of our own save: a reload would drop it.
    app.profiles.profiles.push(profile("marker"));
    let n = app.profiles.profiles.len();
    let _ = app.update(Message::ConfigChanged(Changed {
        profiles: true,
        settings: false,
    }));
    assert_eq!(
        app.profiles.profiles.len(),
        n,
        "no reload for unchanged text"
    );
}

#[test]
fn settings_reload_applies_or_keeps_the_previous_and_says_why() {
    let paths = paths_with(&[profile("a")]);
    let (mut app, _) = App::build(paths.clone(), true);
    let changed = Changed {
        profiles: false,
        settings: true,
    };
    std::fs::write(
        paths.settings(),
        "font_size = 18.0\ntheme = \"light\"\nscrollback_lines = 50\n",
    )
    .unwrap();
    let _ = app.update(Message::ConfigChanged(changed));
    assert_eq!(app.settings.font_size, 18.0);
    assert_eq!(app.style.font_size, 18.0);
    assert_eq!(app.style_unfocused.font_size, 18.0);
    assert_eq!(
        app.tokens,
        crate::theme::tokens(fbterm_config::ThemeMode::Light)
    );
    assert_eq!(app.settings.scrollback_lines, 50);

    std::fs::write(paths.settings(), "font_size = \"big\"\n").unwrap();
    let _ = app.update(Message::ConfigChanged(changed));
    assert_eq!(app.settings.font_size, 18.0, "previous settings kept");
    assert_eq!(app.notices.len(), 1);
    assert!(app.notices[0].contains("settings.toml:1"));

    std::fs::write(paths.settings(), "font_size = 14.0\n").unwrap();
    let _ = app.update(Message::ConfigChanged(changed));
    assert_eq!(app.settings.font_size, 14.0);
    assert!(app.notices.is_empty());
}

#[test]
fn kitty_keyboard_setting_reaches_open_and_new_terminals() {
    let a = profile("a");
    let paths = paths_with(std::slice::from_ref(&a));
    SavedState {
        windows: vec![saved(&[&a], 0)],
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let (mut app, _) = App::build(paths.clone(), true);
    let open = |app: &App| {
        app.windows.values().next().unwrap().tabs[0]
            .terminal()
            .cloned()
            .expect("started tab has a terminal")
    };
    assert!(!app.settings.kitty_keyboard, "off by default");
    assert!(!open(&app).lock().kitty_keyboard());
    let changed = Changed {
        profiles: false,
        settings: true,
    };
    std::fs::write(paths.settings(), "kitty_keyboard = true\n").unwrap();
    let _ = app.update(Message::ConfigChanged(changed));
    assert!(app.settings.kitty_keyboard);
    assert!(open(&app).lock().kitty_keyboard(), "open terminal updated");
    let grid = open(&app).lock().size();
    assert!(app.new_terminal(grid).lock().kitty_keyboard());
    std::fs::write(paths.settings(), "kitty_keyboard = false\n").unwrap();
    let _ = app.update(Message::ConfigChanged(changed));
    assert!(!open(&app).lock().kitty_keyboard());
}

#[test]
fn out_of_range_settings_apply_clamped_and_say_so() {
    let paths = paths_with(&[profile("a")]);
    std::fs::write(
        paths.settings(),
        "font_size = 300.0
",
    )
    .unwrap();
    let (mut app, _) = App::build(paths.clone(), true);
    assert_eq!(app.settings.font_size, 72.0, "clamped at startup");
    assert_eq!(app.notices.len(), 1);
    assert!(
        app.notices[0].contains("settings.toml:1"),
        "{}",
        app.notices[0]
    );
    assert!(app.notices[0].contains("font_size"), "{}", app.notices[0]);

    let changed = Changed {
        profiles: false,
        settings: true,
    };
    std::fs::write(
        paths.settings(),
        "scrollback_lines = 2000000
",
    )
    .unwrap();
    let _ = app.update(Message::ConfigChanged(changed));
    assert_eq!(
        app.settings.scrollback_lines, 1_000_000,
        "clamped on reload"
    );
    assert_eq!(app.settings.font_size, 13.0);
    assert_eq!(app.notices.len(), 1, "the previous warning is replaced");
    assert!(app.notices[0].contains("scrollback_lines"));

    std::fs::write(paths.settings(), "scrollback_lines = 20\n").unwrap();
    let _ = app.update(Message::ConfigChanged(changed));
    assert_eq!(app.settings.scrollback_lines, 20);
    assert!(app.notices.is_empty());
}

fn three_windows() -> (App, Paths, Vec<iced::window::Id>) {
    let (a, b) = (profile("a"), profile("b"));
    let paths = paths_with(&[a.clone(), b.clone()]);
    SavedState {
        windows: vec![saved(&[&a], 0), saved(&[&b, &a], 1), saved(&[&b], 0)],
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let (app, _) = App::build(paths.clone(), true);
    let ids = app.windows.keys().copied().collect();
    (app, paths, ids)
}

#[test]
fn quit_keeps_all_windows_in_state() {
    let (mut app, paths, ids) = three_windows();
    // Something changes first, so the saved state is not just the seeded file.
    let _ = app.update(Message::Shortcut(ids[0], crate::keys::Action::Quit));
    for id in &ids {
        let _ = app.update(Message::WindowClosed(*id));
    }
    let state = SavedState::load_or_default(&paths.state());
    assert_eq!(state.windows.len(), 3);
    assert_eq!(state.windows[1].active, 1);
}

#[test]
fn a_close_the_os_did_on_its_own_is_a_quit() {
    // macOS menu Cmd+Q: windows close without a request.
    let (mut app, paths, ids) = three_windows();
    for id in &ids {
        let _ = app.update(Message::WindowClosed(*id));
    }
    assert_eq!(SavedState::load_or_default(&paths.state()).windows.len(), 3);
}

#[test]
fn closing_one_of_several_windows_drops_it() {
    let (mut app, paths, ids) = three_windows();
    let _ = app.update(Message::CloseRequested(ids[1]));
    assert!(!app.quitting);
    let _ = app.update(Message::WindowClosed(ids[1]));
    let state = SavedState::load_or_default(&paths.state());
    assert_eq!(state.windows.len(), 2);
    assert_eq!(state.windows[0].tabs.len(), 1);
    assert_eq!(state.windows[1].tabs.len(), 1);
}

#[test]
fn closing_last_window_keeps_its_tabs() {
    let (mut app, paths, ids) = three_windows();
    for id in &ids[..2] {
        let _ = app.update(Message::CloseRequested(*id));
        let _ = app.update(Message::WindowClosed(*id));
    }
    let _ = app.update(Message::CloseRequested(ids[2]));
    assert!(app.quitting, "the last window's close button is Quit");
    let _ = app.update(Message::WindowClosed(ids[2]));
    let state = SavedState::load_or_default(&paths.state());
    assert_eq!(state.windows.len(), 1);
    assert_eq!(state.windows[0].tabs.len(), 1);
}

#[test]
fn closing_last_tab_of_last_window_restores_default() {
    let a = profile("a");
    let paths = paths_with(std::slice::from_ref(&a));
    SavedState {
        windows: vec![saved(&[&a], 0)],
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let (mut app, _) = App::build(paths.clone(), true);
    let wid = *app.windows.keys().next().unwrap();
    let _ = app.update(Message::TabBar(wid, crate::app::TabBarMsg::Close(0)));
    let _ = app.update(Message::WindowClosed(wid));
    assert!(
        SavedState::load_or_default(&paths.state())
            .windows
            .is_empty()
    );
    let (relaunched, _) = App::build(paths, true);
    assert_eq!(relaunched.windows.len(), 1, "the default window");
}

pub(crate) fn first_terminal(app: &App) -> fbterm_term::SharedTerminal {
    app.windows.values().next().unwrap().tabs[0]
        .terminal()
        .cloned()
        .expect("started tab has a terminal")
}

pub(crate) fn fill_history(term: &fbterm_term::SharedTerminal, lines: usize) {
    let mut t = term.lock();
    for i in 0..lines {
        t.feed(format!("line {i}\r\n").as_bytes());
    }
}

#[test]
fn toml_lowering_trims_and_sets_notice() {
    let a = profile("a");
    let paths = paths_with(std::slice::from_ref(&a));
    SavedState {
        windows: vec![saved(&[&a], 0)],
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let (mut app, _) = App::build(paths.clone(), true);
    let term = first_terminal(&app);
    fill_history(&term, 5_000);
    let changed = Changed {
        profiles: false,
        settings: true,
    };

    // Raising applies at once and says nothing.
    std::fs::write(paths.settings(), "scrollback_lines = 20000\n").unwrap();
    let _ = app.update(Message::ConfigChanged(changed));
    assert!(app.settings_notice.is_none());
    let before = crate::memory::requests();

    // Lowering below what the tab holds trims it, says so, and releases memory.
    std::fs::write(paths.settings(), "scrollback_lines = 2000\n").unwrap();
    let _ = app.update(Message::ConfigChanged(changed));
    assert_eq!(term.lock().history_len(), 2_000);
    assert_eq!(
        app.settings_notice.as_deref(),
        Some("Scrollback lowered to 2,000 lines: 1 open tab lost its oldest lines.")
    );
    assert_eq!(app.notices.len(), 1);
    assert_eq!(crate::memory::requests(), before + 1);

    // Each further lowering trims again and updates the notice.
    std::fs::write(paths.settings(), "scrollback_lines = 1500\n").unwrap();
    let _ = app.update(Message::ConfigChanged(changed));
    assert_eq!(term.lock().history_len(), 1_500);
    assert_eq!(
        app.settings_notice.as_deref(),
        Some("Scrollback lowered to 1,500 lines: 1 open tab lost its oldest lines.")
    );
    // Lowering below a limit nothing reaches trims nothing: no notice, no release.
    std::fs::write(paths.settings(), "scrollback_lines = 1600\n").unwrap();
    let _ = app.update(Message::ConfigChanged(changed));
    let before = crate::memory::requests();
    std::fs::write(paths.settings(), "scrollback_lines = 1550\n").unwrap();
    let _ = app.update(Message::ConfigChanged(changed));
    assert_eq!(term.lock().history_len(), 1_500);
    assert!(app.settings_notice.is_none(), "nothing was trimmed");
    assert_eq!(crate::memory::requests(), before);

    // New terminals use the new limit.
    let grid = term.lock().size();
    let fresh = app.new_terminal(grid);
    fill_history(&fresh, 3_000);
    assert_eq!(fresh.lock().history_len(), 1_550);
}

#[test]
fn lowering_in_two_tabs_says_plural_with_separators() {
    let (a, b) = (profile("a"), profile("b"));
    let paths = paths_with(&[a.clone(), b.clone()]);
    std::fs::write(paths.settings(), "scrollback_lines = 1000000\n").unwrap();
    SavedState {
        windows: vec![saved(&[&a, &b], 0)],
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let (mut app, _) = App::build(paths.clone(), true);
    let wid = *app.windows.keys().next().unwrap();
    let _ = app.update(Message::SelectTab(1));
    for tab in &app.windows[&wid].tabs {
        fill_history(tab.terminal().expect("both tabs have a terminal"), 300);
    }
    std::fs::write(paths.settings(), "scrollback_lines = 100\n").unwrap();
    let _ = app.update(Message::ConfigChanged(Changed {
        profiles: false,
        settings: true,
    }));
    assert_eq!(
        app.settings_notice.as_deref(),
        Some("Scrollback lowered to 100 lines: 2 open tabs lost their oldest lines.")
    );
}

#[test]
fn confirmed_lowering_from_the_settings_window_sets_no_notice() {
    let a = profile("a");
    let paths = paths_with(std::slice::from_ref(&a));
    SavedState {
        windows: vec![saved(&[&a], 0)],
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let (mut app, _) = App::build(paths, true);
    let term = first_terminal(&app);
    fill_history(&term, 500);
    let new = fbterm_config::Settings {
        scrollback_lines: 100,
        ..app.settings.clone()
    };
    let before = crate::memory::requests();
    app.apply_settings(new, crate::app::SettingsSource::Window);
    assert_eq!(term.lock().history_len(), 100);
    assert!(app.settings_notice.is_none());
    assert_eq!(crate::memory::requests(), before + 1);
}

#[test]
fn closing_tab_requests_release_after_last_reference() {
    let (a, b) = (profile("a"), profile("b"));
    let paths = paths_with(&[a.clone(), b.clone()]);
    SavedState {
        windows: vec![saved(&[&a, &b], 1)],
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let (mut app, _) = App::build(paths, true);
    let wid = *app.windows.keys().next().unwrap();
    let held = app.windows[&wid].tabs[1].terminal().cloned().unwrap();
    let before = crate::memory::requests();
    let _ = app.close_tab(wid, 1);
    assert_eq!(
        crate::memory::requests(),
        before,
        "something still holds the terminal"
    );
    assert_eq!(app.pending_release.len(), 1, "waits for the session task");
    // Another session ending changes nothing while the terminal is still held.
    let _ = app.update(Message::Session(
        fbterm_session::SessionId::next(),
        fbterm_session::SessionEvent::Released,
    ));
    assert_eq!(
        crate::memory::requests(),
        before,
        "still held: keeps waiting"
    );
    drop(held);
    // The session task dropped its reference: it reports, and the release is requested once.
    let _ = app.update(Message::Session(
        fbterm_session::SessionId::next(),
        fbterm_session::SessionEvent::Released,
    ));
    assert_eq!(crate::memory::requests(), before + 1);
    assert!(app.pending_release.is_empty());
    let _ = app.update(Message::Session(
        fbterm_session::SessionId::next(),
        fbterm_session::SessionEvent::Released,
    ));
    assert_eq!(crate::memory::requests(), before + 1, "no second request");

    // With nothing else holding it, closing requests the release at once.
    let (c, d) = (profile("c"), profile("d"));
    let paths = paths_with(&[c.clone(), d.clone()]);
    SavedState {
        windows: vec![saved(&[&c, &d], 1)],
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let (mut app, _) = App::build(paths, true);
    let wid = *app.windows.keys().next().unwrap();
    let before = crate::memory::requests();
    let _ = app.close_tab(wid, 1);
    assert_eq!(crate::memory::requests(), before + 1);
}

#[test]
fn remote_history_clear_never_requests_release() {
    let a = profile("a");
    let paths = paths_with(std::slice::from_ref(&a));
    SavedState {
        windows: vec![saved(&[&a], 0)],
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let (mut app, _) = App::build(paths, true);
    let term = first_terminal(&app);
    let before = crate::memory::requests();
    fill_history(&term, 500);
    assert!(term.lock().history_len() > 0);
    term.lock().feed(b"\x1b[3J");
    assert_eq!(term.lock().history_len(), 0, "the program cleared it");
    fill_history(&term, 500);
    term.lock().feed(b"\x1bc");
    assert_eq!(term.lock().history_len(), 0);
    // The app handles output for the window of the tab whose history was cleared.
    let wid = *app.windows.keys().next().unwrap();
    let size = term.lock().size();
    let _ = app.update(Message::Terminal(
        wid,
        fbterm_term::TerminalOutput::Resized(size),
    ));
    assert_eq!(
        crate::memory::requests(),
        before,
        "no release was requested"
    );
}

#[test]
fn a_session_ending_without_a_user_close_requests_nothing() {
    let a = profile("a");
    let paths = paths_with(std::slice::from_ref(&a));
    SavedState {
        windows: vec![saved(&[&a], 0)],
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let (mut app, _) = App::build(paths, true);
    let before = crate::memory::requests();
    for event in [
        fbterm_session::SessionEvent::Exited(Some(0)),
        fbterm_session::SessionEvent::Disconnected("connection reset".into()),
        fbterm_session::SessionEvent::Released,
    ] {
        let _ = app.update(Message::Session(fbterm_session::SessionId::next(), event));
    }
    assert_eq!(crate::memory::requests(), before);
    assert!(app.pending_release.is_empty());
}
