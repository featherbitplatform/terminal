use std::collections::BTreeMap;
use std::path::PathBuf;

use fbterm_config::*;

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fbterm-state-test-{}", ProfileId::new().0));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn round_trips() {
    let (a, b) = (ProfileId::new(), ProfileId::new());
    let state = SavedState {
        windows: vec![
            SavedWindow {
                tabs: vec![
                    SavedTab::Profile(a),
                    SavedTab::Profile(b),
                    SavedTab::Profile(a),
                ],
                active: 1,
                size: (1000.0, 640.5),
            },
            SavedWindow {
                tabs: vec![SavedTab::Profile(b)],
                active: 0,
                size: (800.0, 600.0),
            },
        ],
        recency: BTreeMap::from([(a, 1_700_000_000), (b, 42)]),
        file_panel_width: Some(412.5),
    };
    let path = tmp("state.toml");
    state.save(&path).unwrap();
    assert_eq!(SavedState::load_or_default(&path), state);
}

#[test]
fn missing_or_corrupt_file_is_the_default_never_an_error() {
    let path = tmp("state.toml");
    assert_eq!(SavedState::load_or_default(&path), SavedState::default());
    std::fs::write(&path, "windows = [[[ not toml").unwrap();
    assert_eq!(SavedState::load_or_default(&path), SavedState::default());
    std::fs::write(&path, "windows = 3").unwrap();
    assert_eq!(SavedState::load_or_default(&path), SavedState::default());
}

#[test]
fn save_leaves_no_temp_file_and_creates_the_directory() {
    let path = tmp("sub/state.toml");
    SavedState::default().save(&path).unwrap();
    let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("state.toml")]);
}

#[test]
fn old_state_loads_as_single_panes() {
    let text = r#"
[[windows]]
tabs = ["6f1c1f4e-8a3e-4f0e-9d53-0b8b6a1c2d01", "0e3b7c52-1d7a-4c3b-8f55-3a9e2b4c7d02"]
active = 1
size = [1000.0, 640.0]

[recency]
"6f1c1f4e-8a3e-4f0e-9d53-0b8b6a1c2d01" = 42
"#;
    let path = tmp("state.toml");
    std::fs::write(&path, text).unwrap();
    let state = SavedState::load_or_default(&path);
    assert_eq!(state.windows.len(), 1);
    assert_eq!(state.windows[0].active, 1);
    assert!(
        state.windows[0]
            .tabs
            .iter()
            .all(|t| matches!(t, SavedTab::Profile(_)))
    );
    assert_eq!(state.windows[0].tabs.len(), 2);
}

fn pane(profile: ProfileId, zoom: i8) -> SavedLayout {
    SavedLayout::Pane { profile, zoom }
}

#[test]
fn split_tab_round_trips() {
    let (a, b, c) = (ProfileId::new(), ProfileId::new(), ProfileId::new());
    let layout = SavedLayout::Split {
        axis: SavedAxis::Row,
        ratio: 0.25,
        first: Box::new(pane(a, 0)),
        second: Box::new(SavedLayout::Split {
            axis: SavedAxis::Column,
            ratio: 0.75,
            first: Box::new(pane(b, -2)),
            second: Box::new(pane(c, 3)),
        }),
    };
    let state = SavedState {
        windows: vec![SavedWindow {
            tabs: vec![
                SavedTab::Profile(a),
                SavedTab::Split(SavedSplitTab { layout, focused: 2 }),
            ],
            active: 1,
            size: (900.0, 500.0),
        }],
        recency: BTreeMap::new(),
        file_panel_width: None,
    };
    let path = tmp("state.toml");
    state.save(&path).unwrap();
    assert_eq!(SavedState::load_or_default(&path), state);
}
