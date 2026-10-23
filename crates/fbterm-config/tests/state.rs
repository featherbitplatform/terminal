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
                tabs: vec![a, b, a],
                active: 1,
                size: (1000.0, 640.5),
            },
            SavedWindow {
                tabs: vec![b],
                active: 0,
                size: (800.0, 600.0),
            },
        ],
        recency: BTreeMap::from([(a, 1_700_000_000), (b, 42)]),
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
