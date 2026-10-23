use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::store::{ConfigError, atomic_write, line_of};

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
    /// Load settings. A missing file gives defaults; a malformed file is an
    /// error carrying the 1-based line, and the file is never modified.
    /// Missing fields in a partial file take their defaults.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(ConfigError::new(path, None, e.to_string())),
        };
        toml::from_str(&text).map_err(|e| {
            let line = e.span().map(|s| line_of(&text, s.start));
            ConfigError::new(path, line, e.message().to_string())
        })
    }

    /// Like [`Settings::load`], but falls back to defaults on any error.
    pub fn load_or_default(path: &Path) -> Self {
        Self::load(path).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| ConfigError::new(path, None, e.to_string()))?;
        atomic_write(path, text.as_bytes())
    }
}
