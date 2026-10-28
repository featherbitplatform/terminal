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
}

/// One tab to bring back. A bare profile id (what 0.1.0 wrote, and what a single unzoomed pane
/// still writes) is a one-pane tab.
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
