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
    /// Let programs turn on the kitty keyboard protocol (off: requests are ignored and keys keep
    /// their legacy encoding).
    pub kitty_keyboard: bool,
    /// Opacity of the terminal's default background and the window chrome around it, from
    /// `0.05` (nearly clear) to `1.0` (opaque). Text, the cursor, the selection and cells with their own
    /// background stay opaque.
    pub background_opacity: f32,
    /// Blur what shows through a translucent background, where the platform supports it.
    pub background_blur: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_family: "Geist Mono".into(),
            font_size: 13.0,
            scrollback_lines: 10_000,
            theme: ThemeMode::Dark,
            kitty_keyboard: false,
            background_opacity: 1.0,
            background_blur: false,
        }
    }
}

/// Allowed range of `font_size`, in points.
pub const FONT_SIZE_RANGE: (f32, f32) = (6.0, 72.0);
/// Allowed range of `background_opacity`. Not quite zero: fully clear pixels can let clicks
/// through to what is behind the window and leave nothing of it visible.
pub const OPACITY_RANGE: (f32, f32) = (0.05, 1.0);
/// Allowed range of `scrollback_lines`.
pub const SCROLLBACK_RANGE: (usize, usize) = (0, 1_000_000);

impl Settings {
    /// Load settings. A missing file gives defaults; a malformed file is an
    /// error carrying the 1-based line, and the file is never modified.
    /// Missing fields in a partial file take their defaults. Out-of-range values are
    /// clamped silently; use [`Settings::load_checked`] to learn about them.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Self::load_checked(path).map(|(s, _)| s)
    }

    /// Like [`Settings::load`], and also reports values that were out of range. Such values
    /// do not reject the file: they are clamped to the nearest allowed value (a `font_size` or
    /// `background_opacity` that is not a finite number takes the default), and the returned
    /// warning names each field, its value and what is used instead, with the line of the first
    /// one.
    pub fn load_checked(path: &Path) -> Result<(Self, Option<ConfigError>), ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Self::default(), None));
            }
            Err(e) => return Err(ConfigError::new(path, None, e.to_string())),
        };
        let mut settings: Self = toml::from_str(&text).map_err(|e| {
            let line = e.span().map(|s| line_of(&text, s.start));
            ConfigError::new(path, line, e.message().to_string())
        })?;
        let problems = settings.clamp();
        let warning = (!problems.is_empty()).then(|| {
            let line = problems
                .iter()
                .find_map(|(field, _)| key_line(&text, field));
            let message = problems
                .iter()
                .map(|(_, m)| m.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            ConfigError::new(path, line, message)
        });
        Ok((settings, warning))
    }

    /// Bring every field into its allowed range; returns `(field, message)` for each change.
    pub fn clamp(&mut self) -> Vec<(&'static str, String)> {
        let mut problems = Vec::new();
        let (lo, hi) = FONT_SIZE_RANGE;
        if !(lo..=hi).contains(&self.font_size) {
            let used = if self.font_size.is_finite() {
                self.font_size.clamp(lo, hi)
            } else {
                Self::default().font_size
            };
            problems.push((
                "font_size",
                format!(
                    "font_size {} is out of range ({lo} to {hi}), using {used}",
                    self.font_size
                ),
            ));
            self.font_size = used;
        }
        let (lo, hi) = SCROLLBACK_RANGE;
        if !(lo..=hi).contains(&self.scrollback_lines) {
            let used = self.scrollback_lines.clamp(lo, hi);
            problems.push((
                "scrollback_lines",
                format!(
                    "scrollback_lines {} is out of range ({lo} to {hi}), using {used}",
                    self.scrollback_lines
                ),
            ));
            self.scrollback_lines = used;
        }
        let (lo, hi) = OPACITY_RANGE;
        if !(lo..=hi).contains(&self.background_opacity) {
            let used = if self.background_opacity.is_finite() {
                self.background_opacity.clamp(lo, hi)
            } else {
                Self::default().background_opacity
            };
            problems.push((
                "background_opacity",
                format!(
                    "background_opacity {} is out of range ({lo} to {hi}), using {used}",
                    self.background_opacity
                ),
            ));
            self.background_opacity = used;
        }
        problems
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

/// The commented template written as a new `settings.toml`: every setting with its default and
/// a one-line explanation. Removing the leading `#` of any setting line gives its default
/// value, so the file as written changes nothing.
pub const SETTINGS_TEMPLATE: &str = r##"# featherbit terminal settings.
# Every setting is shown with its default. Remove the leading "#" of a line to change it.
# Changes are picked up while the app runs.

# Terminal font family.
#font_family = "Geist Mono"

# Font size in points, from 6.0 to 72.0.
#font_size = 13.0

# Lines of history kept per tab, from 0 to 1000000.
#scrollback_lines = 10000

# Color theme: "dark" or "light".
#theme = "dark"

# Let programs turn on the kitty keyboard protocol (true or false).
#kitty_keyboard = false

# Opacity of the terminal background and tab bar, from 0.05 (nearly clear) to 1.0 (opaque).
#background_opacity = 1.0

# Blur what shows through a see-through background (true or false; macOS only for now).
#background_blur = false
"##;

impl Settings {
    /// Write [`SETTINGS_TEMPLATE`] to `path` unless a file is already there. Returns whether it
    /// was written. An existing file is never touched.
    pub fn write_template_if_missing(path: &Path) -> Result<bool, ConfigError> {
        match path.try_exists() {
            Ok(false) => {}
            Ok(true) => return Ok(false),
            Err(e) => return Err(ConfigError::new(path, None, e.to_string())),
        }
        atomic_write(path, SETTINGS_TEMPLATE.as_bytes())?;
        Ok(true)
    }
}

/// The 1-based line on which `key` is assigned at the top level, if any.
fn key_line(text: &str, key: &str) -> Option<usize> {
    text.lines()
        .position(|l| {
            l.trim_start()
                .strip_prefix(key)
                .is_some_and(|rest| rest.trim_start().starts_with('='))
        })
        .map(|i| i + 1)
}
