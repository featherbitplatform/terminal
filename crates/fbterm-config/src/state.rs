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
    /// Profiles of its tabs, in tab order.
    pub tabs: Vec<ProfileId>,
    /// Index of the selected tab.
    #[serde(default)]
    pub active: usize,
    /// Inner size in logical pixels (width, height).
    pub size: (f32, f32),
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedState {
    pub windows: Vec<SavedWindow>,
    /// When each profile was last opened (unix seconds).
    pub recency: BTreeMap<ProfileId, i64>,
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
