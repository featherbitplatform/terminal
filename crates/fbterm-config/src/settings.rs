use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::store::{ConfigError, atomic_write};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub font_family: String,
    pub font_size: f32,
    pub scrollback_lines: usize,
    pub theme: ThemeMode,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_family: "Geist Mono".into(),
            font_size: 13.0,
            scrollback_lines: 10_000,
            theme: ThemeMode::Dark,
        }
    }
}

impl Settings {
    /// Load settings, falling back to defaults when the file is missing or
    /// unreadable. Missing fields in a partial file take their defaults.
    pub fn load_or_default(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| ConfigError::new(path, None, e.to_string()))?;
        atomic_write(path, text.as_bytes())
    }
}
