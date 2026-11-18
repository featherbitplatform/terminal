use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::keys::KeyOverrides;
use crate::store::{ConfigError, atomic_write, line_of};

/// Where saved passwords and key passphrases live (`secret_storage`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretStorage {
    /// The system's secure storage where it works, else the encrypted vault (fbterm then
    /// writes `"vault"` and says why once).
    #[default]
    Auto,
    /// The system's secure storage: the macOS Keychain, the Linux Secret Service, or on
    /// Windows a DPAPI-protected file in the config directory.
    Keychain,
    /// The encrypted vault in the config directory, opened with a main password.
    Vault,
}

impl SecretStorage {
    /// The value as written in `settings.toml`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Keychain => "keychain",
            Self::Vault => "vault",
        }
    }
}

/// Who draws a window's title bar on Linux (`window_decorations`). Other systems ignore it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowDecorations {
    /// fbterm: the tab bar is the title bar, with drawn window controls.
    #[default]
    Fbterm,
    /// The desktop's own title bar, with the tab bar below it.
    System,
}

impl WindowDecorations {
    /// The value as written in `settings.toml`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fbterm => "fbterm",
            Self::System => "system",
        }
    }
}

/// Where a window lists its open sessions (`tab_bar`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabBar {
    /// A row of tabs along the top of the window.
    #[default]
    Top,
    /// A column down the left side of the window, scrolling when it overflows.
    Left,
}

impl TabBar {
    /// The value as written in `settings.toml`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Left => "left",
        }
    }
}

/// How a blurred window looks on macOS (`macos_blur_style`). Other systems ignore it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum MacosBlurStyle {
    /// The system's own blur look.
    #[default]
    Native,
    /// fbterm's earlier, clearer blur, recreated with the system's public blur view.
    Classic,
    /// A value from the file that is neither `"native"` nor `"classic"`, as written.
    /// [`Settings::clamp`] replaces it with [`MacosBlurStyle::Native`], so loaded settings
    /// never hold it.
    Invalid(String),
}

impl MacosBlurStyle {
    /// The value as written in `settings.toml` (an invalid value writes the default).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Native | Self::Invalid(_) => "native",
            Self::Classic => "classic",
        }
    }
}

impl Serialize for MacosBlurStyle {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for MacosBlurStyle {
    /// Lenient: any string, number or boolean loads, and what is not allowed is kept as
    /// [`MacosBlurStyle::Invalid`] for [`Settings::clamp`] to report, so a typo does not
    /// reject the whole file (a table or array is still a type error).
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged, expecting = "\"native\" or \"classic\"")]
        enum Raw {
            Int(i64),
            Float(f64),
            Bool(bool),
            Str(String),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Str(s) if s == "native" => Self::Native,
            Raw::Str(s) if s == "classic" => Self::Classic,
            Raw::Str(s) => Self::Invalid(format!("{s:?}")),
            Raw::Int(n) => Self::Invalid(n.to_string()),
            Raw::Float(f) => Self::Invalid(format!("{f:?}")),
            Raw::Bool(b) => Self::Invalid(b.to_string()),
        })
    }
}

/// The most redraws per second caused by program output (`max_fps`). Typing, scrolling and
/// resizing always redraw at once; this only caps how often streaming output is drawn.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MaxFps {
    /// A fixed rate, within [`MAX_FPS_RANGE`] once loaded.
    Fixed(u16),
    /// Follow the refresh rate of the displays the windows are on (`"display"`).
    Display,
    /// A value from the file that is not allowed (out of range, or neither a whole number nor
    /// `"display"`), as written. [`Settings::clamp`] replaces it, so loaded settings never
    /// hold it.
    Invalid(String),
}

impl Default for MaxFps {
    fn default() -> Self {
        Self::Fixed(DEFAULT_MAX_FPS)
    }
}

/// Allowed range of a fixed `max_fps`.
pub const MAX_FPS_RANGE: (u16, u16) = (30, 240);
/// The default `max_fps`, also the rate used for `"display"` when it cannot be detected.
pub const DEFAULT_MAX_FPS: u16 = 60;

impl MaxFps {
    /// The rate in force for this setting; `display`: the detected refresh rate, if known.
    /// Always within [`MAX_FPS_RANGE`].
    pub fn effective(&self, display: Option<u32>) -> u16 {
        let (lo, hi) = MAX_FPS_RANGE;
        match self {
            Self::Fixed(n) => (*n).clamp(lo, hi),
            Self::Display => display.filter(|hz| *hz > 0).map_or(DEFAULT_MAX_FPS, |hz| {
                u16::try_from(hz.min(u32::from(hi)))
                    .unwrap_or(hi)
                    .clamp(lo, hi)
            }),
            Self::Invalid(_) => DEFAULT_MAX_FPS,
        }
    }
}

impl Serialize for MaxFps {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Fixed(n) => s.serialize_u16(*n),
            Self::Display => s.serialize_str("display"),
            Self::Invalid(_) => s.serialize_u16(DEFAULT_MAX_FPS),
        }
    }
}

impl<'de> Deserialize<'de> for MaxFps {
    /// Lenient: any number, string or boolean loads, and what is not allowed is kept as
    /// [`MaxFps::Invalid`] for [`Settings::clamp`] to report (a table or array is still a
    /// type error).
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged, expecting = "a whole number from 30 to 240, or \"display\"")]
        enum Raw {
            Int(i64),
            Float(f64),
            Bool(bool),
            Str(String),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Int(n) => {
                u16::try_from(n).map_or_else(|_| Self::Invalid(n.to_string()), Self::Fixed)
            }
            Raw::Str(s) if s == "display" => Self::Display,
            Raw::Str(s) => Self::Invalid(format!("{s:?}")),
            Raw::Float(f) => Self::Invalid(format!("{f:?}")),
            Raw::Bool(b) => Self::Invalid(b.to_string()),
        })
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub font_family: String,
    pub font_size: f32,
    /// Draw ligatures (`->`, `!=` ...) in fonts that have them, including ligatures a font keeps
    /// in a stylistic set. Off turns every ligature feature off.
    pub ligatures: bool,
    /// Extra OpenType features for the terminal font, applied after `ligatures`: `"ss11"`,
    /// `"-calt"`, `"cv01=2"`. Entries that do not parse are reported and skipped.
    pub font_features: Vec<String>,
    pub scrollback_lines: usize,
    /// Keep the history that is not near the screen compressed, saving memory at the cost of
    /// CPU. Off by default.
    pub history_compression: bool,
    /// With `history_compression`: how many of the newest history lines stay uncompressed.
    /// `None`: absent from the file, which counts as [`UNCOMPRESSED_DEFAULT`] (less than
    /// `scrollback_lines`). Always below `scrollback_lines` once loaded with compression on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uncompressed_history_lines: Option<usize>,
    /// Id of the terminal color scheme used by every session without its own (a built-in id or
    /// `user-<uuid>`). Not validated here: the app reports ids it does not know.
    pub color_scheme: String,
    /// Let programs turn on the kitty keyboard protocol (off: requests are ignored and keys keep
    /// their legacy encoding).
    pub kitty_keyboard: bool,
    /// Show pictures programs send (on). Off, every picture sequence a program writes is consumed
    /// and discarded, and what is already on screen goes away at once.
    pub inline_images: bool,
    /// Keep pictures when they scroll off the screen (`false`: they become a line saying how big
    /// they were and free their memory). Only counts with `inline_images` on.
    pub scrollback_images: bool,
    /// Start every new terminal with grapheme clusters on (DEC private mode 2027): joined emoji,
    /// flags and skin tones in one cell. Programs can turn it on and off themselves; a change
    /// applies to new terminals and after a reset or restart, never to a running program.
    pub grapheme_clusters: bool,
    /// Opacity of the terminal's default background and the window chrome around it, from
    /// `0.05` (nearly clear) to `1.0` (opaque). Text, the cursor, the selection and cells with their own
    /// background stay opaque.
    pub background_opacity: f32,
    /// Blur what shows through a translucent background, where the platform supports it.
    pub background_blur: bool,
    /// How blur looks on macOS: the system's look or fbterm's earlier, clearer one. Ignored
    /// elsewhere.
    pub macos_blur_style: MacosBlurStyle,
    /// Who draws the window title bar on Linux; ignored elsewhere.
    pub window_decorations: WindowDecorations,
    /// Where open sessions are listed. `None`: never answered (absent from the file), which
    /// counts as [`TabBar::Top`]; the Welcome tab asks once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_bar: Option<TabBar>,
    /// Fold the sidebar's rows into the profiles' groups (only with tabs on the left).
    #[serde(default)]
    pub tab_groups: bool,
    /// Draw each pane in its color scheme's suggested font when that font is installed (a
    /// scheme's own `font_family` still wins; a missing font falls back to `font_family`).
    pub use_theme_fonts: bool,
    /// Most redraws per second caused by program output.
    pub max_fps: MaxFps,
    /// Automatic reconnect attempts for a tunnel whose connection dropped (0 = off).
    pub tunnel_max_retries: u32,
    /// Check GitHub once a day for a newer release. `None`: never answered (absent from the
    /// file), which counts as off; the app asks once at launch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_updates: Option<bool>,
    /// Where saved secrets live. Changed through the Settings tab, which moves them; a
    /// hand edit applies on the next launch and moves nothing.
    pub secret_storage: SecretStorage,
    /// Keyboard shortcuts changed from their defaults (the `[keys]` table).
    pub keys: KeyOverrides,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_family: "Geist Mono".into(),
            font_size: 13.0,
            ligatures: true,
            font_features: Vec::new(),
            scrollback_lines: 10_000,
            history_compression: false,
            uncompressed_history_lines: None,
            color_scheme: "graphite".into(),
            kitty_keyboard: false,
            inline_images: true,
            scrollback_images: false,
            grapheme_clusters: false,
            background_opacity: 1.0,
            background_blur: false,
            macos_blur_style: MacosBlurStyle::Native,
            window_decorations: WindowDecorations::Fbterm,
            tab_bar: None,
            tab_groups: false,
            use_theme_fonts: false,
            max_fps: MaxFps::default(),
            tunnel_max_retries: 10,
            check_updates: None,
            secret_storage: SecretStorage::Auto,
            keys: KeyOverrides::default(),
        }
    }
}

/// Allowed range of `font_size`, in points.
pub const FONT_SIZE_RANGE: (f32, f32) = (6.0, 72.0);
/// Allowed range of `background_opacity`. Not quite zero: fully clear pixels can let clicks
/// through to what is behind the window and leave nothing of it visible.
pub const OPACITY_RANGE: (f32, f32) = (0.05, 1.0);
/// Allowed range of `tunnel_max_retries`.
pub const TUNNEL_RETRIES_RANGE: (u32, u32) = (0, 100);
/// Allowed range of `scrollback_lines`.
pub const SCROLLBACK_RANGE: (usize, usize) = (0, 1_000_000);
/// Above this many lines the Settings tab notes that resizing can pause while history
/// rewraps. Measured (feathercritty `benches/RESULTS.md`): a 100,000-line rewrap takes 3 to
/// 33 ms (one first full-width pass 57 ms); the next measured size, 1,000,000, takes about
/// 0.3 s. The owner chose to warn above 100,000.
pub const SCROLLBACK_WARN_ABOVE: usize = 100_000;

/// The fewest uncompressed history lines `uncompressed_history_lines` may be. With
/// `scrollback_lines` at or below it there is nothing worth compressing.
pub const UNCOMPRESSED_FLOOR: usize = 1_000;
/// The default `uncompressed_history_lines`, lowered to stay below `scrollback_lines`.
pub const UNCOMPRESSED_DEFAULT: usize = 10_000;

impl Settings {
    /// How many of the newest history lines stay uncompressed, or `None` when history is not
    /// compressed: the switch is off, or `scrollback_lines` is at most [`UNCOMPRESSED_FLOOR`].
    /// Otherwise the setting (or, when absent, [`UNCOMPRESSED_DEFAULT`] capped below
    /// `scrollback_lines`), within `UNCOMPRESSED_FLOOR..=scrollback_lines - 1`.
    pub fn history_compression_keep(&self) -> Option<usize> {
        if !self.history_compression || self.scrollback_lines <= UNCOMPRESSED_FLOOR {
            return None;
        }
        let top = self.scrollback_lines - 1;
        let wanted = self
            .uncompressed_history_lines
            .unwrap_or_else(|| UNCOMPRESSED_DEFAULT.min(top));
        Some(wanted.clamp(UNCOMPRESSED_FLOOR, top))
    }

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
        let removed_theme = if problems.is_empty() {
            top_level_key_line(&text, "theme")
        } else {
            None
        };
        if let Some(line) = removed_theme {
            return Ok((
                settings,
                Some(ConfigError::new(
                    path,
                    Some(line),
                    THEME_REMOVED.to_string(),
                )),
            ));
        }
        let warning = (!problems.is_empty()).then(|| {
            let line = problems
                .iter()
                .find_map(|(field, _)| key_line(&text, field.as_str()));
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
    /// `[keys]` entries that are not taken (an unknown action, a chord that does not parse or
    /// may not be bound, or one another action already has) are dropped, so those actions keep
    /// their default; the field is then the entry's name.
    pub fn clamp(&mut self) -> Vec<(String, String)> {
        let mut problems: Vec<(String, String)> = Vec::new();
        let (lo, hi) = FONT_SIZE_RANGE;
        if !(lo..=hi).contains(&self.font_size) {
            let used = if self.font_size.is_finite() {
                self.font_size.clamp(lo, hi)
            } else {
                Self::default().font_size
            };
            problems.push((
                "font_size".into(),
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
                "scrollback_lines".into(),
                format!(
                    "scrollback_lines {} is out of range ({lo} to {hi}), using {used}",
                    self.scrollback_lines
                ),
            ));
            self.scrollback_lines = used;
        }
        // Only checked while compression is on and applies: with it off the value is kept as
        // written, so switching it on later does not surprise.
        if let (Some(keep), Some(set)) = (
            self.history_compression_keep(),
            self.uncompressed_history_lines,
        ) && set != keep
        {
            problems.push((
                "uncompressed_history_lines".into(),
                format!(
                    "uncompressed_history_lines {set} is out of range ({UNCOMPRESSED_FLOOR} to {}), using {keep}",
                    self.scrollback_lines - 1
                ),
            ));
            self.uncompressed_history_lines = Some(keep);
        }
        let (lo, hi) = TUNNEL_RETRIES_RANGE;
        if !(lo..=hi).contains(&self.tunnel_max_retries) {
            let used = self.tunnel_max_retries.clamp(lo, hi);
            problems.push((
                "tunnel_max_retries".into(),
                format!(
                    "tunnel_max_retries {} is out of range ({lo} to {hi}), using {used}",
                    self.tunnel_max_retries
                ),
            ));
            self.tunnel_max_retries = used;
        }
        let (lo, hi) = OPACITY_RANGE;
        if !(lo..=hi).contains(&self.background_opacity) {
            let used = if self.background_opacity.is_finite() {
                self.background_opacity.clamp(lo, hi)
            } else {
                Self::default().background_opacity
            };
            problems.push((
                "background_opacity".into(),
                format!(
                    "background_opacity {} is out of range ({lo} to {hi}), using {used}",
                    self.background_opacity
                ),
            ));
            self.background_opacity = used;
        }
        let (lo, hi) = MAX_FPS_RANGE;
        let fps_problem = match &self.max_fps {
            MaxFps::Fixed(n) if !(lo..=hi).contains(n) => Some(n.to_string()),
            MaxFps::Invalid(raw) => Some(raw.clone()),
            _ => None,
        };
        if let Some(raw) = fps_problem {
            // A whole number is clamped; anything else takes the default.
            let used = raw.parse::<i64>().map_or(DEFAULT_MAX_FPS, |n| {
                u16::try_from(n.clamp(i64::from(lo), i64::from(hi))).unwrap_or(DEFAULT_MAX_FPS)
            });
            let why = if raw.parse::<i64>().is_ok() {
                format!("is out of range ({lo} to {hi})")
            } else {
                format!("is not a whole number from {lo} to {hi} or \"display\"")
            };
            problems.push((
                "max_fps".into(),
                format!("max_fps {raw} {why}, using {used}"),
            ));
            self.max_fps = MaxFps::Fixed(used);
        }
        if let MacosBlurStyle::Invalid(raw) = &self.macos_blur_style {
            problems.push((
                "macos_blur_style".into(),
                format!(
                    "macos_blur_style {raw} is not \"native\" or \"classic\", using \"native\""
                ),
            ));
            self.macos_blur_style = MacosBlurStyle::Native;
        }
        for (name, why) in std::mem::take(&mut self.keys.invalid) {
            let message = format!("keys.{name} {why}");
            problems.push((name, message));
        }
        let mac = cfg!(target_os = "macos");
        for (action, why) in self.keys.drop_reserved(mac) {
            problems.push((
                action.name().to_owned(),
                format!("keys.{} {why}, using the default", action.name()),
            ));
        }
        for (action, chord, other) in self.keys.resolve_conflicts(mac) {
            problems.push((
                action.name().to_owned(),
                format!(
                    "keys.{} {:?} is already used by {}, using the default",
                    action.name(),
                    chord.to_config(mac),
                    other.name()
                ),
            ));
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

# Draw each tab in its color scheme's suggested font when that font is installed (true or
# false). A scheme that sets its own font keeps it; a missing font falls back to font_family.
# fbterm never downloads fonts.
#use_theme_fonts = false

# Draw sequences such as -> and != as one symbol when the font has them (true or false).
#ligatures = true

# Extra OpenType features for the terminal font, such as "ss02", "-calt" or "cv01=2".
#font_features = []

# Lines of history kept per tab, from 0 to 1000000. Applies to open tabs at once: lowering it
# drops their oldest lines (the Settings tab asks first). Above 100000, resizing a window
# can pause briefly while history rewraps.
#scrollback_lines = 10000

# Keep older history compressed to save memory, at the cost of CPU when it is read and when
# output scrolls into it. Recommended for very large histories (100000 lines or more).
#history_compression = false

# With history_compression: how many of the newest lines stay uncompressed.
#uncompressed_history_lines = 10000  # 1000 to scrollback_lines - 1

# Terminal color scheme for every session (a scheme id; see the Color schemes tab).
#color_scheme = "graphite"

# Let programs turn on the kitty keyboard protocol (true or false).
#kitty_keyboard = false

# Show pictures that programs send (true or false). Off, every picture a program writes is
# dropped, and what is already on screen disappears with it.
#inline_images = true

# Keep a picture when its rows scroll off the screen (true or false): off replaces it with a
# line saying how big it was. Only counts while inline_images is true.
#scrollback_images = false

# Keep joined emoji, flags and skin tones in one cell in new tabs (true or false). Programs can
# turn this on themselves.
#grapheme_clusters = false

# Opacity of the terminal background and tab bar, from 0.05 (nearly clear) to 1.0 (opaque).
#background_opacity = 1.0

# Blur what shows through a see-through background (true or false), where the system
# supports it: macOS, Windows 11 and Linux desktops that offer blur.
#background_blur = false

# How blur looks on macOS: "native" (the system's look, with its own tint) or "classic"
# (fbterm's earlier, clearer blur: no tint of its own, so the opacity drives the look).
# Ignored on other systems.
#macos_blur_style = "native"  # or "classic"; macOS only

# Window title bar on Linux: "fbterm" draws it in the tab bar, "system" uses the desktop's.
#window_decorations = "fbterm"

# Where open sessions are listed: "top" (a row of tabs along the top of the window) or
# "left" (a column down the left side, scrolling when it overflows). Left out, the Welcome
# tab asks once.
#tab_bar = "top"

# Fold the sidebar into the profiles' groups when tabs are on the left: a collapsible header
# per group, tabs reorder within their group. Ignored with tabs on top.
#tab_groups = false

# Most redraws per second caused by program output: a whole number from 30 to 240, or
# "display" to follow the refresh rate of the screen (60 when it cannot be detected).
# Caps redraws caused by program output; typing always redraws immediately. Higher values
# use more CPU while output streams.
#max_fps = 60

# Automatic reconnect attempts for a tunnel whose connection dropped (0 = off).
#tunnel_max_retries = 10

# Check GitHub once a day for a newer fbterm and show a dot on the settings button when one
# is out (true or false). Nothing is downloaded or installed. Left out, fbterm asks once.
#check_updates = false

# Where saved passwords and key passphrases live: "auto" (the system keychain where it
# works, else the encrypted vault), "keychain" or "vault" (an encrypted file opened with a
# main password). Change it in the Settings tab, which moves the saved secrets across.
#secret_storage = "auto"

# Keyboard shortcuts changed from their defaults, by action. The Keyboard section of the
# Settings tab lists every action with its chord and changes them for you.
# A chord is modifiers and a key joined by "+": ctrl, alt, shift and cmd (written super
# outside macOS), then a key such as t, 1, ",", f5, up, pageup, space or plus.
# "none" leaves an action without a chord. For example:
#   new_tab = "ctrl+shift+t"
#   close_tab = "none"
#[keys]
"##;

impl Settings {
    /// Tabs are listed down the left side (`tab_bar = "left"`); unanswered counts as top.
    pub fn tabs_left(&self) -> bool {
        self.tab_bar == Some(TabBar::Left)
    }

    /// The sidebar folds its rows into groups: `tab_groups`, which only counts with tabs on
    /// the left.
    pub fn tab_groups_on(&self) -> bool {
        self.tab_groups && self.tabs_left()
    }

    /// Whether the update check runs: only when the user said yes.
    pub fn checks_updates(&self) -> bool {
        self.check_updates == Some(true)
    }

    /// Write [`SETTINGS_TEMPLATE`] to `path` unless something is already there (a file, or a
    /// symlink, even a dangling one). Returns whether it was written. The target is created
    /// exclusively, so an existing file is never touched, even if it appears concurrently.
    pub fn write_template_if_missing(path: &Path) -> Result<bool, ConfigError> {
        use std::io::Write;
        let io = |e: std::io::Error| ConfigError::new(path, None, e.to_string());
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(e) => return Err(io(e)),
        };
        let written = file
            .write_all(SETTINGS_TEMPLATE.as_bytes())
            .and_then(|()| file.sync_all());
        if let Err(e) = written {
            // Do not leave a partial template behind.
            let _ = std::fs::remove_file(path);
            return Err(io(e));
        }
        Ok(true)
    }
}

/// Shown when a settings file still has the removed `theme` key.
const THEME_REMOVED: &str = "theme is no longer used: the window follows color_scheme. For a light window, choose Graphite Light.";

/// Line of `key` among the top-level keys (before the first `[table]`).
fn top_level_key_line(text: &str, key: &str) -> Option<usize> {
    let end = text
        .lines()
        .position(|l| l.trim_start().starts_with('['))
        .unwrap_or(usize::MAX);
    key_line(text, key).filter(|&line| line - 1 < end)
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
