//! Session restore as plain functions over plain state: what to reopen from `state.toml`, and
//! what to write back.

use std::collections::{BTreeMap, HashMap};

use fbterm_config::{ProfileStore, SavedState, SavedWindow};
use iced::window;

use crate::palette::Recency;
use crate::tabs::WindowState;

/// Size of a window nobody resized (and of one restored without a usable size).
pub const DEFAULT_SIZE: (f32, f32) = (1000.0, 640.0);
/// Smallest size a restored window opens at (a stale file must not open an invisible window).
const MIN_SIZE: (f32, f32) = (360.0, 200.0);

/// The windows to reopen: tabs whose profile no longer exists are dropped, `active` follows the
/// tab it pointed at (or is clamped when that tab was dropped), windows left without tabs are
/// dropped. An empty result means: open the default single window.
pub fn plan(state: &SavedState, store: &ProfileStore) -> Vec<SavedWindow> {
    state
        .windows
        .iter()
        .filter_map(|w| {
            let kept: Vec<usize> = (0..w.tabs.len())
                .filter(|&i| store.get(w.tabs[i]).is_some())
                .collect();
            if kept.is_empty() {
                return None;
            }
            let active = kept
                .iter()
                .position(|&i| i >= w.active)
                .unwrap_or(kept.len() - 1);
            let (width, height) = w.size;
            let size = if width.is_finite() && height.is_finite() {
                (width.max(MIN_SIZE.0), height.max(MIN_SIZE.1))
            } else {
                DEFAULT_SIZE
            };
            Some(SavedWindow {
                tabs: kept.iter().map(|&i| w.tabs[i]).collect(),
                active,
                size,
            })
        })
        .collect()
}

/// The state to write: every window that has tabs (in window order), and the recency.
pub fn snapshot(
    windows: &BTreeMap<window::Id, WindowState>,
    sizes: &HashMap<window::Id, (f32, f32)>,
    recency: &Recency,
) -> SavedState {
    SavedState {
        windows: windows
            .iter()
            .filter(|(_, w)| !w.tabs.is_empty())
            .map(|(id, w)| SavedWindow {
                tabs: w.tabs.iter().map(|t| t.focused_pane().profile).collect(),
                active: w.active.min(w.tabs.len() - 1),
                size: sizes.get(id).copied().unwrap_or(DEFAULT_SIZE),
            })
            .collect(),
        recency: recency.clone().into(),
    }
}

#[cfg(test)]
mod tests {
    use fbterm_config::{LocalProfile, Profile, ProfileId, ProfileKind};

    use super::*;
    use crate::tabs::{Pane, Tab};

    fn store(ids: &[ProfileId]) -> ProfileStore {
        ProfileStore {
            profiles: ids
                .iter()
                .map(|&id| Profile {
                    id,
                    name: id.to_string(),
                    group: None,
                    accent: None,
                    accent_style: Default::default(),
                    kind: ProfileKind::Local(LocalProfile {
                        program: "/bin/sh".into(),
                        args: vec![],
                        cwd: None,
                        env: Default::default(),
                    }),
                })
                .collect(),
        }
    }

    fn saved(tabs: Vec<ProfileId>, active: usize) -> SavedWindow {
        SavedWindow {
            tabs,
            active,
            size: (900.0, 500.0),
        }
    }

    #[test]
    fn tabs_of_missing_profiles_are_dropped() {
        let (a, b, gone) = (ProfileId::new(), ProfileId::new(), ProfileId::new());
        let state = SavedState {
            windows: vec![saved(vec![a, gone, b], 2)],
            ..Default::default()
        };
        let plan = plan(&state, &store(&[a, b]));
        assert_eq!(plan[0].tabs, vec![a, b]);
        assert_eq!(plan[0].active, 1, "still the tab it was");
        assert_eq!(plan[0].size, (900.0, 500.0));
    }

    #[test]
    fn active_is_clamped_when_its_tab_was_dropped() {
        let (a, b, gone) = (ProfileId::new(), ProfileId::new(), ProfileId::new());
        let state = SavedState {
            windows: vec![saved(vec![a, b, gone], 2), saved(vec![a, b], 9)],
            ..Default::default()
        };
        let plan = plan(&state, &store(&[a, b]));
        assert_eq!(plan[0].active, 1);
        assert_eq!(plan[1].active, 1, "out of range clamps to the last tab");
    }

    #[test]
    fn a_window_with_no_remaining_tabs_is_dropped() {
        let (a, gone) = (ProfileId::new(), ProfileId::new());
        let state = SavedState {
            windows: vec![saved(vec![gone], 0), saved(vec![], 0), saved(vec![a], 0)],
            ..Default::default()
        };
        let plan = plan(&state, &store(&[a]));
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].tabs, vec![a]);
        assert!(super::plan(&SavedState::default(), &store(&[a])).is_empty());
    }

    #[test]
    fn nonsense_sizes_become_usable() {
        let a = ProfileId::new();
        let mut w = saved(vec![a], 0);
        w.size = (f32::NAN, 5.0);
        let state = SavedState {
            windows: vec![
                w,
                SavedWindow {
                    size: (1.0, 1.0),
                    ..saved(vec![a], 0)
                },
            ],
            ..Default::default()
        };
        let plan = plan(&state, &store(&[a]));
        assert_eq!(plan[0].size, DEFAULT_SIZE);
        assert_eq!(plan[1].size, MIN_SIZE);
    }

    #[test]
    fn snapshot_records_windows_tabs_active_sizes_and_recency() {
        let (a, b) = (ProfileId::new(), ProfileId::new());
        let (w1, w2, w3) = (
            window::Id::unique(),
            window::Id::unique(),
            window::Id::unique(),
        );
        let mut one = WindowState::new();
        one.push(Tab::single(Pane::new(a, "a", None)));
        one.push(Tab::single(Pane::new(b, "b", None)));
        one.active = 0;
        let mut two = WindowState::new();
        two.push(Tab::single(Pane::new(b, "b", None)));
        let windows = BTreeMap::from([(w1, one), (w2, two), (w3, WindowState::new())]);
        let sizes = HashMap::from([(w1, (1200.0, 700.0))]);
        let mut recency = Recency::new();
        recency.bump(a, 7);
        let state = snapshot(&windows, &sizes, &recency);
        assert_eq!(state.windows.len(), 2, "a window without tabs is not saved");
        assert_eq!(state.windows[0].tabs, vec![a, b]);
        assert_eq!(state.windows[0].active, 0);
        assert_eq!(state.windows[0].size, (1200.0, 700.0));
        assert_eq!(state.windows[1].size, DEFAULT_SIZE);
        assert_eq!(state.recency, BTreeMap::from([(a, 7)]));
    }
}
