//! The app-owned session state: which windows and tabs were open, and when each profile was
//! last used. Unlike the user's configuration files, the app writes this one.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::profile::ProfileId;
use crate::store::{ConfigError, atomic_write};

/// One window to bring back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedWindow {
    /// Its tabs, in tab order.
    pub tabs: Vec<SavedTab>,
    /// Index of the selected tab.
    #[serde(default)]
    pub active: usize,
    /// Inner size in logical pixels (width, height).
    pub size: (f32, f32),
    /// The tab sidebar's chosen width in logical pixels, when the user changed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_width: Option<f32>,
    /// The tab groups folded in the sidebar, by name (`tab_groups`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collapsed_groups: Vec<String>,
}

/// One tab to bring back. A bare profile id (what 0.1.0 wrote, and what a single unzoomed,
/// unrenamed pane still writes) is a one-pane tab.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SavedTab {
    Profile(ProfileId),
    Split(SavedSplitTab),
}

/// A tab with a pane arrangement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedSplitTab {
    pub layout: SavedLayout,
    /// The focused pane, as an index in reading order.
    #[serde(default)]
    pub focused: usize,
    /// The name the user gave the tab, when renamed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SavedAxis {
    Row,
    Column,
}

/// A pane arrangement: leaves are panes (profile and font zoom), nodes are splits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SavedLayout {
    Pane {
        profile: ProfileId,
        #[serde(default)]
        zoom: i8,
    },
    Split {
        axis: SavedAxis,
        ratio: f32,
        first: Box<SavedLayout>,
        second: Box<SavedLayout>,
    },
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedState {
    pub windows: Vec<SavedWindow>,
    /// When each profile was last opened (unix seconds).
    pub recency: BTreeMap<ProfileId, i64>,
    /// The file panel's width, when the user changed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_panel_width: Option<f32>,
    /// Whether the file panel shows hidden files (names starting with `.`), when the user
    /// turned it off; shown by default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_panel_show_hidden: Option<bool>,
    /// The tunnels panel's width, when the user changed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnels_panel_width: Option<f32>,
}

impl SavedState {
    /// A missing or unreadable `state.toml` is no state at all (the app then opens its default
    /// window); it is never an error and never rewritten here.
    pub fn load_or_default(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Atomically write the state (temp file in the same directory + rename).
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| ConfigError::new(path, None, e.to_string()))?;
        atomic_write(path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(sidebar_width: Option<f32>) -> SavedWindow {
        SavedWindow {
            tabs: vec![SavedTab::Profile(ProfileId::new())],
            active: 0,
            size: (900.0, 500.0),
            sidebar_width,
            collapsed_groups: vec![],
        }
    }

    #[test]
    fn collapsed_groups_round_trip_and_default() {
        let state = SavedState {
            windows: vec![
                SavedWindow {
                    collapsed_groups: vec!["staging".into(), "other".into()],
                    ..window(None)
                },
                window(None),
            ],
            ..Default::default()
        };
        let text = toml::to_string_pretty(&state).unwrap();
        assert_eq!(
            text.matches("collapsed_groups").count(),
            1,
            "empty: not written"
        );
        let back: SavedState = toml::from_str(&text).unwrap();
        assert_eq!(back.windows[0].collapsed_groups, vec!["staging", "other"]);
        assert!(back.windows[1].collapsed_groups.is_empty());
        let old = "[[windows]]\ntabs = [\"00000000-0000-0000-0000-000000000000\"]\nactive = 0\nsize = [900.0, 500.0]\n";
        let state: SavedState = toml::from_str(old).unwrap();
        assert!(state.windows[0].collapsed_groups.is_empty());
    }

    #[test]
    fn a_sidebar_width_round_trips_and_the_default_is_not_written() {
        let state = SavedState {
            windows: vec![window(Some(300.0)), window(None)],
            ..Default::default()
        };
        let text = toml::to_string_pretty(&state).unwrap();
        assert_eq!(text.matches("sidebar_width").count(), 1);
        let back: SavedState = toml::from_str(&text).unwrap();
        assert_eq!(back.windows[0].sidebar_width, Some(300.0));
        assert_eq!(back.windows[1].sidebar_width, None);
    }

    #[test]
    fn a_tab_name_round_trips_and_none_is_not_written() {
        let named = SavedTab::Split(SavedSplitTab {
            layout: SavedLayout::Pane {
                profile: ProfileId::new(),
                zoom: 0,
            },
            focused: 0,
            name: Some("build box".into()),
        });
        let state = SavedState {
            windows: vec![SavedWindow {
                tabs: vec![named.clone(), SavedTab::Profile(ProfileId::new())],
                ..window(None)
            }],
            ..Default::default()
        };
        let text = toml::to_string_pretty(&state).unwrap();
        assert_eq!(text.matches("name").count(), 1, "{text}");
        let back: SavedState = toml::from_str(&text).unwrap();
        assert_eq!(back.windows[0].tabs[0], named);
        assert!(matches!(back.windows[0].tabs[1], SavedTab::Profile(_)));
    }

    #[test]
    fn a_state_file_from_before_the_width_loads() {
        let text = "[[windows]]\ntabs = [\"00000000-0000-0000-0000-000000000000\"]\nactive = 0\nsize = [900.0, 500.0]\n";
        let state: SavedState = toml::from_str(text).unwrap();
        assert_eq!(state.windows.len(), 1);
        assert_eq!(state.windows[0].sidebar_width, None);
    }
}
