//! Session restore as plain functions over plain state: what to reopen from `state.toml`, and
//! what to write back.

use std::collections::{BTreeMap, HashMap};

use fbterm_config::{
    ProfileStore, SavedAxis, SavedLayout, SavedSplitTab, SavedState, SavedTab, SavedWindow,
};
use iced::window;

use crate::layout::{Axis, Layout};
use crate::palette::Recency;
use crate::tabs::{Tab, WindowState};

/// Size of a window nobody resized (and of one restored without a usable size).
pub const DEFAULT_SIZE: (f32, f32) = (1000.0, 640.0);
/// Smallest size a restored window opens at (a stale file must not open an invisible window).
const MIN_SIZE: (f32, f32) = (360.0, 200.0);

/// The windows to reopen: panes whose profile no longer exists are dropped (their sibling takes
/// the space, other ratios stay), tabs left without panes are dropped, `active` follows the tab
/// it pointed at (or is clamped when that tab was dropped), windows left without tabs are
/// dropped. An empty result means: open the default single window.
pub fn plan(state: &SavedState, store: &ProfileStore) -> Vec<SavedWindow> {
    state
        .windows
        .iter()
        .filter_map(|w| {
            let pruned: Vec<Option<SavedTab>> =
                w.tabs.iter().map(|t| prune_tab(t, store)).collect();
            let kept: Vec<usize> = (0..pruned.len()).filter(|&i| pruned[i].is_some()).collect();
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
                tabs: pruned.into_iter().flatten().collect(),
                active,
                size,
            })
        })
        .collect()
}

/// The tab without the panes of missing profiles; `None` when no pane is left. A tab reduced to
/// one unzoomed pane is a plain profile tab again.
fn prune_tab(tab: &SavedTab, store: &ProfileStore) -> Option<SavedTab> {
    match tab {
        SavedTab::Profile(id) => store.get(*id).map(|_| SavedTab::Profile(*id)),
        SavedTab::Split(split) => {
            let mut alive = Vec::new();
            let layout = prune_layout(&split.layout, store, &mut alive)?;
            let kept: Vec<usize> = (0..alive.len()).filter(|&i| alive[i]).collect();
            let focused = kept
                .iter()
                .position(|&i| i >= split.focused)
                .unwrap_or(kept.len() - 1);
            Some(match layout {
                SavedLayout::Pane { profile, zoom: 0 } => SavedTab::Profile(profile),
                layout => SavedTab::Split(SavedSplitTab { layout, focused }),
            })
        }
    }
}

/// Prunes `layout`, recording in `alive` (reading order) which of its original panes survived.
fn prune_layout(
    layout: &SavedLayout,
    store: &ProfileStore,
    alive: &mut Vec<bool>,
) -> Option<SavedLayout> {
    match layout {
        SavedLayout::Pane { profile, zoom } => {
            let ok = store.get(*profile).is_some();
            alive.push(ok);
            ok.then_some(SavedLayout::Pane {
                profile: *profile,
                zoom: *zoom,
            })
        }
        SavedLayout::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let first = prune_layout(first, store, alive);
            let second = prune_layout(second, store, alive);
            match (first, second) {
                (Some(first), Some(second)) => Some(SavedLayout::Split {
                    axis: *axis,
                    ratio: if ratio.is_finite() {
                        ratio.clamp(0.05, 0.95)
                    } else {
                        0.5
                    },
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (one, other) => one.or(other),
            }
        }
    }
}

/// A live tab for a saved one: fresh pane ids, all panes not connected, `focused` by reading
/// order. `None` if a profile is missing (the plan already dropped those).
pub fn build_tab(saved: &SavedTab, store: &ProfileStore) -> Option<Tab> {
    use crate::tabs::{Pane, TabId};
    match saved {
        SavedTab::Profile(id) => Some(Tab::single(Pane::for_profile(store.get(*id)?))),
        SavedTab::Split(split) => {
            let mut panes = Vec::new();
            let layout = build_layout(&split.layout, store, &mut panes)?;
            let order = layout.panes();
            let focused = *order.get(split.focused).or(order.first())?;
            let mut recent = vec![focused];
            recent.extend(order.into_iter().filter(|&p| p != focused));
            Some(Tab {
                id: TabId::next(),
                layout,
                panes,
                focused,
                maximized: None,
                recent,
            })
        }
    }
}

fn build_layout(
    saved: &SavedLayout,
    store: &ProfileStore,
    panes: &mut Vec<crate::tabs::Pane>,
) -> Option<Layout> {
    match saved {
        SavedLayout::Pane { profile, zoom } => {
            let mut pane = crate::tabs::Pane::for_profile(store.get(*profile)?);
            pane.zoom = *zoom;
            let id = pane.id;
            panes.push(pane);
            Some(Layout::Pane(id))
        }
        SavedLayout::Split {
            axis,
            ratio,
            first,
            second,
        } => Some(Layout::Split {
            axis: match axis {
                SavedAxis::Row => Axis::Row,
                SavedAxis::Column => Axis::Column,
            },
            ratio: *ratio,
            first: Box::new(build_layout(first, store, panes)?),
            second: Box::new(build_layout(second, store, panes)?),
        }),
    }
}

fn save_layout(layout: &Layout, tab: &Tab) -> SavedLayout {
    match layout {
        Layout::Pane(id) => {
            let pane = tab.pane(*id).expect("every layout pane is in the tab");
            SavedLayout::Pane {
                profile: pane.profile,
                zoom: pane.zoom,
            }
        }
        Layout::Split {
            axis,
            ratio,
            first,
            second,
        } => SavedLayout::Split {
            axis: match axis {
                Axis::Row => SavedAxis::Row,
                Axis::Column => SavedAxis::Column,
            },
            ratio: *ratio,
            first: Box::new(save_layout(first, tab)),
            second: Box::new(save_layout(second, tab)),
        },
    }
}

/// How a tab is written: a single unzoomed pane stays a bare profile id. Maximize is not saved.
fn save_tab(tab: &Tab) -> SavedTab {
    if let Layout::Pane(_) = tab.layout {
        let pane = tab.focused_pane();
        if pane.zoom == 0 {
            return SavedTab::Profile(pane.profile);
        }
    }
    SavedTab::Split(SavedSplitTab {
        layout: save_layout(&tab.layout, tab),
        focused: tab
            .layout
            .panes()
            .iter()
            .position(|&p| p == tab.focused)
            .unwrap_or(0),
    })
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
                tabs: w.tabs.iter().map(save_tab).collect(),
                active: w.active.min(w.tabs.len() - 1),
                size: sizes.get(id).copied().unwrap_or(DEFAULT_SIZE),
            })
            .collect(),
        recency: recency.clone().into(),
        file_panel_width: None,
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
                    color_scheme: None,
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
            tabs: tabs.into_iter().map(SavedTab::Profile).collect(),
            active,
            size: (900.0, 500.0),
        }
    }

    fn profiles(tabs: &[SavedTab]) -> Vec<ProfileId> {
        tabs.iter()
            .map(|t| match t {
                SavedTab::Profile(id) => *id,
                SavedTab::Split(_) => panic!("expected a plain profile tab"),
            })
            .collect()
    }

    fn leaf(profile: ProfileId, zoom: i8) -> SavedLayout {
        SavedLayout::Pane { profile, zoom }
    }

    fn row(ratio: f32, first: SavedLayout, second: SavedLayout) -> SavedLayout {
        SavedLayout::Split {
            axis: SavedAxis::Row,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    fn split_window(layout: SavedLayout, focused: usize) -> SavedWindow {
        SavedWindow {
            tabs: vec![SavedTab::Split(SavedSplitTab { layout, focused })],
            active: 0,
            size: (900.0, 500.0),
        }
    }

    #[test]
    fn deleted_profile_drops_its_pane() {
        let (a, b, c, gone) = (
            ProfileId::new(),
            ProfileId::new(),
            ProfileId::new(),
            ProfileId::new(),
        );
        // a | (gone / c) with b after: the sibling c takes gone's space, other ratios stay.
        let layout = row(
            0.2,
            leaf(a, 1),
            row(0.6, row(0.4, leaf(gone, 0), leaf(c, 0)), leaf(b, 0)),
        );
        let state = SavedState {
            windows: vec![split_window(layout, 1)],
            ..Default::default()
        };
        let planned = plan(&state, &store(&[a, b, c]));
        let SavedTab::Split(tab) = &planned[0].tabs[0] else {
            panic!("still a split tab");
        };
        assert_eq!(
            tab.layout,
            row(0.2, leaf(a, 1), row(0.6, leaf(c, 0), leaf(b, 0)))
        );
        assert_eq!(tab.focused, 1, "the focused pane was dropped: the next one");

        // A tab whose every pane is gone is dropped, and one left with a single unzoomed
        // pane is a plain profile tab.
        let all_gone = split_window(row(0.5, leaf(gone, 0), leaf(gone, 0)), 0);
        let one_left = split_window(row(0.5, leaf(gone, 0), leaf(a, 0)), 0);
        let state = SavedState {
            windows: vec![all_gone, one_left],
            ..Default::default()
        };
        let planned = plan(&state, &store(&[a]));
        assert_eq!(planned.len(), 1);
        assert_eq!(profiles(&planned[0].tabs), vec![a]);
    }

    #[test]
    fn single_unzoomed_tab_is_written_as_profile() {
        let a = ProfileId::new();
        let w = window::Id::unique();
        let mut win = WindowState::new();
        win.push(Tab::single(Pane::new(a, "a", None)));
        let mut zoomed = Pane::new(a, "a", None);
        zoomed.zoom = 2;
        win.push(Tab::single(zoomed));
        let windows = BTreeMap::from([(w, win)]);
        let state = snapshot(&windows, &HashMap::new(), &Recency::new());
        assert_eq!(state.windows[0].tabs[0], SavedTab::Profile(a));
        assert_eq!(
            state.windows[0].tabs[1],
            SavedTab::Split(SavedSplitTab {
                layout: leaf(a, 2),
                focused: 0
            })
        );
    }

    #[test]
    fn tabs_of_missing_profiles_are_dropped() {
        let (a, b, gone) = (ProfileId::new(), ProfileId::new(), ProfileId::new());
        let state = SavedState {
            windows: vec![saved(vec![a, gone, b], 2)],
            ..Default::default()
        };
        let plan = plan(&state, &store(&[a, b]));
        assert_eq!(profiles(&plan[0].tabs), vec![a, b]);
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
        assert_eq!(profiles(&plan[0].tabs), vec![a]);
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
        assert_eq!(profiles(&state.windows[0].tabs), vec![a, b]);
        assert_eq!(state.windows[0].active, 0);
        assert_eq!(state.windows[0].size, (1200.0, 700.0));
        assert_eq!(state.windows[1].size, DEFAULT_SIZE);
        assert_eq!(state.recency, BTreeMap::from([(a, 7)]));
    }
}
