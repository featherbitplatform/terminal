//! The Settings window's state: one control per `settings.toml` option, as plain data so its
//! rules (ranges, typed numbers, when to write) are testable without a GUI. Changes apply at
//! once; the file is written in place (see [`Settings::write_field`]), right away for a click
//! and after a quiet period for a slider drag or typing. The window is drawn by [`view`] and
//! wired to the app in `glue`.

use std::time::Duration;

use fbterm_config::settings::{FONT_SIZE_RANGE, OPACITY_RANGE, SCROLLBACK_RANGE};
use fbterm_config::{ConfigError, SettingChange, Settings, ThemeMode};

mod fonts;
mod glue;
pub mod view;

pub use fonts::FontChoice;
pub(crate) use glue::terminal_style;

/// Quiet period after the last slider or typing change before the file is written.
pub const WRITE_DEBOUNCE: Duration = Duration::from_millis(300);

/// What the Settings window asks for. `Open`, `Close`, `OpenFile`, `RevealFolder` and `Flush`
/// are the app's; the rest change a value.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsMsg {
    Open,
    Close,
    OpenFile,
    RevealFolder,
    /// The quiet period after change number `n` passed.
    Flush(u64),
    /// A slider was let go: write now.
    Release,
    Theme(ThemeMode),
    Font(FontChoice),
    FontSize(f32),
    FontSizeText(String),
    FontSizeSubmit,
    /// Background opacity from the slider, as a fraction.
    Opacity(f32),
    /// Background opacity typed, in percent.
    OpacityText(String),
    OpacitySubmit,
    Blur(bool),
    ScrollbackText(String),
    ScrollbackSubmit,
    Kitty(bool),
}

/// When the pending changes go to disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Write {
    /// Nothing to write.
    Not,
    /// Now.
    Now,
    /// After [`WRITE_DEBOUNCE`], if no change came after this one (its number).
    After(u64),
}

/// What an update asks of the app: a value to apply at once (live) and when to write.
#[derive(Debug, Clone, PartialEq)]
pub struct Effect {
    pub apply: Option<SettingChange>,
    pub write: Write,
}

impl Effect {
    const NONE: Self = Self {
        apply: None,
        write: Write::Not,
    };
}

pub struct SettingsState {
    /// The values the controls show (the app's settings, with our changes applied).
    pub values: Settings,
    pub font_size_text: String,
    /// Background opacity in percent, as typed.
    pub opacity_text: String,
    pub scrollback_text: String,
    pub fonts: Vec<FontChoice>,
    /// `settings.toml` does not load: the window only shows, it never writes.
    pub read_only: Option<ConfigError>,
    /// Why the last write failed, until the next one works.
    pub write_error: Option<String>,
    /// Changes applied but not yet written, one per key, oldest first.
    pending: Vec<SettingChange>,
    /// Counts debounced changes, so only the last of a burst writes.
    generation: u64,
}

fn size_text(v: f32) -> String {
    format!("{v}")
}

fn percent_text(v: f32) -> String {
    format!("{}", (v * 100.0).round())
}

/// `v` rounded to steps of `1 / per_unit` (divided last, so 0.85 stays the nearest float to it).
fn snap(v: f32, per_unit: f32) -> f32 {
    (v * per_unit).round() / per_unit
}

impl SettingsState {
    pub fn new(values: &Settings, installed: Vec<String>, read_only: Option<ConfigError>) -> Self {
        let fonts = fonts::choices(installed, &values.font_family);
        let mut state = Self {
            values: values.clone(),
            font_size_text: String::new(),
            opacity_text: String::new(),
            scrollback_text: String::new(),
            fonts,
            read_only,
            write_error: None,
            pending: Vec::new(),
            generation: 0,
        };
        state.sync(values);
        state
    }

    /// Show `values` (the file changed on disk): every control and typed number follows, and
    /// unwritten changes are dropped (the file wins).
    pub fn sync(&mut self, values: &Settings) {
        self.values = values.clone();
        self.font_size_text = size_text(values.font_size);
        self.opacity_text = percent_text(values.background_opacity);
        self.scrollback_text = values.scrollback_lines.to_string();
        if !self.fonts.iter().any(|f| f.family == values.font_family) {
            let installed = self
                .fonts
                .iter()
                .filter(|f| f.installed)
                .map(|f| f.family.clone())
                .collect();
            self.fonts = fonts::choices(installed, &values.font_family);
        }
        self.pending.clear();
    }

    /// The changes to write now, oldest first.
    pub fn take_pending(&mut self) -> Vec<SettingChange> {
        std::mem::take(&mut self.pending)
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The option the font list shows as chosen.
    pub fn selected_font(&self) -> Option<&FontChoice> {
        self.fonts
            .iter()
            .find(|f| f.family == self.values.font_family)
    }

    pub fn update(&mut self, msg: SettingsMsg) -> Effect {
        if self.read_only.is_some() {
            return Effect::NONE;
        }
        match msg {
            SettingsMsg::Open
            | SettingsMsg::Close
            | SettingsMsg::OpenFile
            | SettingsMsg::RevealFolder => Effect::NONE,
            SettingsMsg::Flush(n) => Effect {
                apply: None,
                write: if n == self.generation && self.has_pending() {
                    Write::Now
                } else {
                    Write::Not
                },
            },
            SettingsMsg::Release => Effect {
                apply: None,
                write: if self.has_pending() {
                    Write::Now
                } else {
                    Write::Not
                },
            },
            SettingsMsg::Theme(v) => self.change(SettingChange::Theme(v), false),
            SettingsMsg::Font(f) => self.change(SettingChange::FontFamily(f.family), false),
            SettingsMsg::Blur(v) => self.change(SettingChange::BackgroundBlur(v), false),
            SettingsMsg::Kitty(v) => self.change(SettingChange::KittyKeyboard(v), false),
            SettingsMsg::FontSize(v) => {
                let v = snap(v, 2.0).clamp(FONT_SIZE_RANGE.0, FONT_SIZE_RANGE.1);
                self.font_size_text = size_text(v);
                self.change(SettingChange::FontSize(v), true)
            }
            SettingsMsg::FontSizeText(s) => {
                let parsed = s.trim().parse::<f32>().ok();
                self.font_size_text = s;
                match parsed {
                    Some(v) if (FONT_SIZE_RANGE.0..=FONT_SIZE_RANGE.1).contains(&v) => {
                        self.change(SettingChange::FontSize(v), true)
                    }
                    _ => Effect::NONE,
                }
            }
            SettingsMsg::FontSizeSubmit => {
                let parsed = self
                    .font_size_text
                    .trim()
                    .parse::<f32>()
                    .ok()
                    .filter(|v| v.is_finite());
                match parsed {
                    Some(v) => {
                        let v = v.clamp(FONT_SIZE_RANGE.0, FONT_SIZE_RANGE.1);
                        self.font_size_text = size_text(v);
                        self.change(SettingChange::FontSize(v), false)
                    }
                    None => {
                        self.font_size_text = size_text(self.values.font_size);
                        self.flush_now()
                    }
                }
            }
            SettingsMsg::Opacity(v) => {
                let v = snap(v, 100.0).clamp(OPACITY_RANGE.0, OPACITY_RANGE.1);
                self.opacity_text = percent_text(v);
                self.change(SettingChange::BackgroundOpacity(v), true)
            }
            SettingsMsg::OpacityText(s) => {
                let parsed = parse_percent(&s);
                self.opacity_text = s;
                match parsed {
                    Some(v) if (OPACITY_RANGE.0..=OPACITY_RANGE.1).contains(&v) => {
                        self.change(SettingChange::BackgroundOpacity(v), true)
                    }
                    _ => Effect::NONE,
                }
            }
            SettingsMsg::OpacitySubmit => match parse_percent(&self.opacity_text) {
                Some(v) => {
                    let v = v.clamp(OPACITY_RANGE.0, OPACITY_RANGE.1);
                    self.opacity_text = percent_text(v);
                    self.change(SettingChange::BackgroundOpacity(v), false)
                }
                None => {
                    self.opacity_text = percent_text(self.values.background_opacity);
                    self.flush_now()
                }
            },
            SettingsMsg::ScrollbackText(s) => {
                let digits: String = s.chars().filter(|c| *c != ',' && *c != '_').collect();
                let parsed = digits.trim().parse::<usize>().ok();
                self.scrollback_text = s;
                match parsed {
                    Some(v) if (SCROLLBACK_RANGE.0..=SCROLLBACK_RANGE.1).contains(&v) => {
                        self.change(SettingChange::ScrollbackLines(v), true)
                    }
                    _ => Effect::NONE,
                }
            }
            SettingsMsg::ScrollbackSubmit => {
                let digits: String = self
                    .scrollback_text
                    .chars()
                    .filter(|c| *c != ',' && *c != '_')
                    .collect();
                match digits.trim().parse::<u64>() {
                    Ok(v) => {
                        let v = usize::try_from(v)
                            .unwrap_or(usize::MAX)
                            .clamp(SCROLLBACK_RANGE.0, SCROLLBACK_RANGE.1);
                        self.scrollback_text = v.to_string();
                        self.change(SettingChange::ScrollbackLines(v), false)
                    }
                    Err(_) => {
                        self.scrollback_text = self.values.scrollback_lines.to_string();
                        self.flush_now()
                    }
                }
            }
        }
    }

    /// Write whatever is pending now (Enter in a field that holds no new value).
    fn flush_now(&mut self) -> Effect {
        Effect {
            apply: None,
            write: if self.has_pending() {
                Write::Now
            } else {
                Write::Not
            },
        }
    }

    /// Apply `change` to the shown values and queue it; `debounce` for a slider or typing.
    /// A value equal to the current one changes nothing (but still lets a pending write go).
    fn change(&mut self, change: SettingChange, debounce: bool) -> Effect {
        let mut next = self.values.clone();
        change.apply(&mut next);
        if next == self.values {
            return if debounce {
                Effect::NONE
            } else {
                self.flush_now()
            };
        }
        self.values = next;
        self.pending.retain(|p| p.key() != change.key());
        self.pending.push(change.clone());
        self.generation += 1;
        Effect {
            apply: Some(change),
            write: if debounce {
                Write::After(self.generation)
            } else {
                Write::Now
            },
        }
    }
}

/// A typed percentage (`85`, `85%`, `85.5`) as a fraction.
fn parse_percent(s: &str) -> Option<f32> {
    let v = s.trim().trim_end_matches('%').trim().parse::<f32>().ok()?;
    v.is_finite().then_some(v / 100.0)
}

#[cfg(test)]
mod app_tests;
#[cfg(test)]
mod tests;
