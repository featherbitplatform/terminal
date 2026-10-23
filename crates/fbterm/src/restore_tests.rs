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
fn profile(name: &str) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        group: None,
        accent: None,
        kind: ProfileKind::Local(LocalProfile {
            program: "/nonexistent/fbterm-test-shell".into(),
            args: vec![],
            cwd: None,
            env: Default::default(),
        }),
    }
}

fn paths_with(profiles: &[Profile]) -> Paths {
    let paths = Paths { config_dir: dir() };
    ProfileStore {
        profiles: profiles.to_vec(),
    }
    .save(&paths.profiles())
    .unwrap();
    paths
}

fn saved(tabs: &[&Profile], active: usize) -> SavedWindow {
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
