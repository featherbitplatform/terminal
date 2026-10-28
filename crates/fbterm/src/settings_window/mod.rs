//! The Settings window's state: one control per `settings.toml` option, as plain data so its
//! rules (ranges, typed numbers, when to write) are testable without a GUI. Changes apply at
//! once; the file is written in place (see [`Settings::write_field`]), right away for a click
//! and after a quiet period for a slider drag or typing. The window is drawn by [`view`] and
//! wired to the app in `glue`.

use std::time::Duration;

use fbterm_config::keys::{self as keymap, Action, Bindings, Chord};
use fbterm_config::settings::{
    DEFAULT_MAX_FPS, FONT_SIZE_RANGE, OPACITY_RANGE, SCROLLBACK_RANGE, SCROLLBACK_WARN_ABOVE,
    TUNNEL_RETRIES_RANGE,
};
use fbterm_config::{
    ConfigError, MaxFps, SecretStorage, SettingChange, Settings, ThemeMode, WindowDecorations,
};

mod fonts;
mod glue;
pub mod view;

pub use fonts::FontChoice;
pub(crate) use fonts::{BUNDLED as BUNDLED_FONT, choices as font_choices, installed_monospace};
pub(crate) use glue::{intern, terminal_style};

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
    /// Open the Color schemes window.
    EditSchemes,
    /// The quiet period after change number `n` passed.
    Flush(u64),
    /// A slider was let go: write now.
    Release,
    Theme(ThemeMode),
    Font(FontChoice),
    /// The id of the chosen global color scheme.
    ColorScheme(String),
    FontSize(f32),
    FontSizeText(String),
    FontSizeSubmit,
    /// Background opacity from the slider, as a fraction.
    Opacity(f32),
    /// Background opacity typed, in percent.
    OpacityText(String),
    OpacitySubmit,
    Blur(bool),
    /// Who draws the title bar (Linux).
    TitleBar(WindowDecorations),
    ScrollbackText(String),
    ScrollbackSubmit,
    /// Confirm lowering the scrollback below what an open tab holds: save and trim.
    ScrollbackApply,
    /// Drop the lowered value: the saved one comes back.
    ScrollbackCancel,
    /// Reconnect attempts for tunnels, typed.
    RetriesText(String),
    RetriesSubmit,
    Kitty(bool),
    MaxFps(MaxFps),
    /// Record a new chord for an action (a click on its chord).
    Record(Action),
    /// A key press while recording: its chord, or `None` for a key that cannot be one.
    Pressed(Option<Chord>),
    /// A key let go while recording whose press never reached fbterm: its chord, or `None`
    /// for a key that cannot be one.
    Withheld(Option<Chord>),
    CancelRecording,
    /// Give the recorded chord to the action being recorded; the action that had it is left
    /// without one.
    TakeOver,
    /// Put an action's default chord back.
    ResetKey(Action),
    /// Leave an action without a chord.
    ClearKey(Action),
    /// Put every default chord back.
    ResetKeys,
    /// A `Saved secrets` choice: the app moves the secrets (asking for the main password
    /// when the vault needs it) and then writes it.
    SecretStorage(SecretStorage),
    /// `Reset vault`: the app asks first.
    ResetVault,
}

/// A chord being recorded in the Keyboard section.
#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    pub action: Action,
    pub problem: Option<RecordProblem>,
}

/// Why the last chord pressed while recording was not taken.
#[derive(Debug, Clone, PartialEq)]
pub enum RecordProblem {
    /// It cannot be a shortcut; says why.
    NotAllowed(String),
    /// Another action has it: take it over or cancel.
    Taken { chord: Chord, other: Action },
}

/// Why the recorder never saw a key press: the system or another program kept it.
fn withheld_note(mac: bool, chord: Option<Chord>) -> String {
    let system = if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "The desktop"
    };
    let what = chord.map_or_else(|| "That key".to_string(), |c| c.label(mac));
    format!("{what} never reaches fbterm: {system} or another program uses it")
}

/// The note under the Scrollback field above [`SCROLLBACK_WARN_ABOVE`] lines.
pub const SCROLLBACK_NOTE: &str =
    "Large history: resizing a window can pause briefly while history rewraps.";

/// `n` with a comma between thousands (`2,000`).
pub(crate) fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Fixed redraw rates offered in the list.
pub const RATE_PRESETS: [u16; 3] = [30, 60, 120];

/// The redraw-rate row's hint when the rate in force is above the default.
pub const RATE_CPU_HINT: &str = "Higher rates use more CPU while output is streaming.";
/// The redraw-rate row's hint otherwise.
pub const RATE_HINT: &str = "Caps redraws from program output. Typing always redraws at once";

/// One option of the redraw-rate list.
#[derive(Debug, Clone, PartialEq)]
pub struct RateChoice {
    pub value: MaxFps,
    pub label: String,
}

impl std::fmt::Display for RateChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
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
    /// Tunnel reconnect attempts, as typed.
    pub retries_text: String,
    /// A lower scrollback typed but not saved, because an open tab holds more lines than it
    /// and applying it would cut them: the window asks (Apply / Cancel) first.
    pub pending_lower: Option<usize>,
    /// A lower scrollback typed that no open tab exceeds, with the change number of its
    /// debounce: it applies when that quiet period passes (`Flush`) or on Enter, never per
    /// keystroke, so `2` on the way to `20000` cannot cut output scrolling in that moment.
    held_lower: Option<(usize, u64)>,
    /// The most history lines any open tab holds (set by the app when the window opens and
    /// before a typed scrollback is evaluated).
    pub max_open_history: usize,
    pub fonts: Vec<FontChoice>,
    /// `settings.toml` does not load: the window only shows, it never writes.
    pub read_only: Option<ConfigError>,
    /// Why the last write failed, until the next one works.
    pub write_error: Option<String>,
    /// The refresh rate `max_fps = "display"` follows, if detected (set by the app).
    pub display_hz: Option<u32>,
    /// The chord being recorded, if any: the window takes every key press meanwhile.
    pub recording: Option<Recording>,
    /// Changes applied but not yet written, one per key, oldest first.
    pending: Vec<SettingChange>,
    /// Counts debounced changes, so only the last of a burst writes.
    generation: u64,
    /// The scrolling page, for the `FBTERM_OPEN_SETTINGS=secrets` debug aid.
    pub page: iced::widget::Id,
}

pub(crate) fn size_text(v: f32) -> String {
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
            retries_text: String::new(),
            pending_lower: None,
            held_lower: None,
            max_open_history: 0,
            fonts,
            read_only,
            write_error: None,
            display_hz: None,
            recording: None,
            pending: Vec::new(),
            generation: 0,
            page: iced::widget::Id::unique(),
        };
        state.sync(values);
        state
    }

    /// Show `values` (the file changed on disk): every control and typed number follows, and
    /// unwritten changes are dropped (the file wins).
    pub fn sync(&mut self, values: &Settings) {
        // A recording (and any question it asks) was about the old chords.
        if values.keys.map != self.values.keys.map {
            self.recording = None;
        }
        self.values = values.clone();
        self.font_size_text = size_text(values.font_size);
        self.opacity_text = percent_text(values.background_opacity);
        self.scrollback_text = values.scrollback_lines.to_string();
        self.retries_text = values.tunnel_max_retries.to_string();
        self.pending_lower = None;
        self.held_lower = None;
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

    /// Put back changes that could not be written, unless a newer change to the same key is
    /// already waiting.
    pub fn requeue(&mut self, failed: Vec<SettingChange>) {
        for change in failed.into_iter().rev() {
            if !self.pending.iter().any(|p| p.key() == change.key()) {
                self.pending.insert(0, change);
            }
        }
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// A lowered scrollback is typed and waiting for its quiet period.
    pub fn holds_lower(&self) -> bool {
        self.held_lower.is_some()
    }

    /// The option the font list shows as chosen.
    pub fn selected_font(&self) -> Option<&FontChoice> {
        self.fonts
            .iter()
            .find(|f| f.family == self.values.font_family)
    }

    /// The redraw-rate options: the presets, the file's own number if it is none of them, and
    /// following the display.
    pub fn rate_choices(&self) -> Vec<RateChoice> {
        let mut fixed: Vec<(u16, bool)> = RATE_PRESETS.iter().map(|n| (*n, false)).collect();
        if let MaxFps::Fixed(n) = self.values.max_fps
            && !RATE_PRESETS.contains(&n)
        {
            fixed.push((n, true));
            fixed.sort_unstable();
        }
        let mut choices: Vec<RateChoice> = fixed
            .into_iter()
            .map(|(n, custom)| RateChoice {
                value: MaxFps::Fixed(n),
                label: if custom {
                    format!("{n} (custom)")
                } else {
                    n.to_string()
                },
            })
            .collect();
        choices.push(RateChoice {
            value: MaxFps::Display,
            label: match self.display_hz {
                Some(hz) => format!("Match display ({hz} Hz)"),
                None => "Match display".into(),
            },
        });
        choices
    }

    /// The option the redraw-rate list shows as chosen.
    pub fn selected_rate(&self) -> Option<RateChoice> {
        self.rate_choices()
            .into_iter()
            .find(|c| c.value == self.values.max_fps)
    }

    /// The redraw-rate row's hint: a CPU note once the rate in force is above the default.
    pub fn rate_hint(&self) -> &'static str {
        if self.values.max_fps.effective(self.display_hz) > DEFAULT_MAX_FPS {
            RATE_CPU_HINT
        } else {
            RATE_HINT
        }
    }

    /// The chord of every action as the window shows it.
    pub fn bindings(&self) -> Bindings {
        Bindings::current(&self.values.keys.map)
    }

    /// Stop recording a chord, keeping the one the action had.
    pub fn cancel_recording(&mut self) {
        self.recording = None;
    }

    /// A chord pressed while recording for `action`: taken, or the reason it is not.
    fn pressed(&mut self, action: Action, chord: Option<Chord>) -> Effect {
        let bindings = self.bindings();
        let problem = match chord {
            None => RecordProblem::NotAllowed("That key cannot be part of a shortcut".into()),
            Some(chord) if keymap::reserved(bindings.mac(), chord).is_some() => {
                let what = keymap::reserved(bindings.mac(), chord).unwrap_or_default();
                RecordProblem::NotAllowed(format!(
                    "{} is used by {what}",
                    chord.label(bindings.mac())
                ))
            }
            Some(chord) => match chord.allowed() {
                Err(why) => {
                    RecordProblem::NotAllowed(format!("{}: {why}", chord.label(bindings.mac())))
                }
                Ok(()) => match bindings.action(chord) {
                    Some(other) if other != action => RecordProblem::Taken { chord, other },
                    Some(_) => {
                        // Its own chord already: nothing changes.
                        self.recording = None;
                        return Effect::NONE;
                    }
                    None => {
                        self.recording = None;
                        return self.set_keys(&[(action, Some(chord))]);
                    }
                },
            },
        };
        self.recording = Some(Recording {
            action,
            problem: Some(problem),
        });
        Effect::NONE
    }

    /// Set the chords of `changes` (`None`: unbound) and write the overrides.
    fn set_keys(&mut self, changes: &[(Action, Option<Chord>)]) -> Effect {
        let mac = self.bindings().mac();
        let mut overrides = self.values.keys.map.clone();
        for (action, chord) in changes {
            keymap::set_override(mac, &mut overrides, *action, *chord);
        }
        self.change(SettingChange::Keys(overrides), false)
    }

    pub fn update(&mut self, msg: SettingsMsg) -> Effect {
        if self.read_only.is_some() {
            self.recording = None;
            return Effect::NONE;
        }
        match msg {
            SettingsMsg::Open
            | SettingsMsg::Close
            | SettingsMsg::OpenFile
            | SettingsMsg::RevealFolder
            | SettingsMsg::EditSchemes
            | SettingsMsg::SecretStorage(_)
            | SettingsMsg::ResetVault => Effect::NONE,
            SettingsMsg::Flush(n) => match self.held_lower {
                Some((v, at)) if at == n => {
                    self.held_lower = None;
                    // The tabs may have grown since it was typed.
                    self.set_scrollback(v, false)
                }
                _ => Effect {
                    apply: None,
                    write: if n == self.generation && self.has_pending() {
                        Write::Now
                    } else {
                        Write::Not
                    },
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
            SettingsMsg::ColorScheme(id) => self.change(SettingChange::ColorScheme(id), false),
            SettingsMsg::Blur(v) => self.change(SettingChange::BackgroundBlur(v), false),
            SettingsMsg::TitleBar(v) => self.change(SettingChange::WindowDecorations(v), false),
            SettingsMsg::Kitty(v) => self.change(SettingChange::KittyKeyboard(v), false),
            SettingsMsg::MaxFps(v) => self.change(SettingChange::MaxFps(v), false),
            SettingsMsg::Record(action)
            | SettingsMsg::ResetKey(action)
            | SettingsMsg::ClearKey(action)
                if action.is_fixed(self.bindings().mac()) =>
            {
                Effect::NONE
            }
            SettingsMsg::Record(action) => {
                self.recording = Some(Recording {
                    action,
                    problem: None,
                });
                Effect::NONE
            }
            SettingsMsg::CancelRecording => {
                self.recording = None;
                Effect::NONE
            }
            SettingsMsg::Pressed(chord) => match &self.recording {
                Some(r) => self.pressed(r.action, chord),
                None => Effect::NONE,
            },
            SettingsMsg::Withheld(chord) => {
                let mac = self.bindings().mac();
                if let Some(r) = &mut self.recording {
                    r.problem = Some(RecordProblem::NotAllowed(withheld_note(mac, chord)));
                }
                Effect::NONE
            }
            SettingsMsg::TakeOver => match self.recording.take() {
                Some(Recording {
                    action,
                    problem: Some(RecordProblem::Taken { chord, other }),
                }) => {
                    // The bindings may have changed since the question (an edit on disk):
                    // take over only if `other` still has the chord, else ask again.
                    if self.bindings().action(chord) == Some(other) {
                        self.set_keys(&[(other, None), (action, Some(chord))])
                    } else {
                        self.pressed(action, Some(chord))
                    }
                }
                other => {
                    self.recording = other;
                    Effect::NONE
                }
            },
            SettingsMsg::ResetKey(action) => {
                // Like pressing the default chord: another action may have taken it since.
                match keymap::default_chord(self.bindings().mac(), action) {
                    Some(chord) => self.pressed(action, Some(chord)),
                    None => {
                        self.recording = None;
                        self.set_keys(&[(action, None)])
                    }
                }
            }
            SettingsMsg::ClearKey(action) => {
                self.recording = None;
                self.set_keys(&[(action, None)])
            }
            SettingsMsg::ResetKeys => {
                self.recording = None;
                self.change(SettingChange::Keys(Default::default()), false)
            }
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
                let parsed = parse_percent(&s).map(|v| snap(v, 100.0));
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
                    let v = snap(v, 100.0).clamp(OPACITY_RANGE.0, OPACITY_RANGE.1);
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
                // Whatever was asked about is not what the field holds any more.
                self.pending_lower = None;
                self.held_lower = None;
                match parsed {
                    Some(v) if (SCROLLBACK_RANGE.0..=SCROLLBACK_RANGE.1).contains(&v) => {
                        self.set_scrollback(v, true)
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
                        self.pending_lower = None;
                        self.held_lower = None;
                        self.set_scrollback(v, false)
                    }
                    Err(_) => {
                        self.scrollback_text = self.values.scrollback_lines.to_string();
                        self.pending_lower = None;
                        self.held_lower = None;
                        self.flush_now()
                    }
                }
            }
            SettingsMsg::RetriesText(s) => {
                let parsed = s.trim().parse::<u32>().ok();
                self.retries_text = s;
                match parsed {
                    Some(v) if (TUNNEL_RETRIES_RANGE.0..=TUNNEL_RETRIES_RANGE.1).contains(&v) => {
                        self.change(SettingChange::TunnelMaxRetries(v), true)
                    }
                    _ => Effect::NONE,
                }
            }
            SettingsMsg::RetriesSubmit => match self.retries_text.trim().parse::<u64>() {
                Ok(v) => {
                    let v = u32::try_from(v)
                        .unwrap_or(u32::MAX)
                        .clamp(TUNNEL_RETRIES_RANGE.0, TUNNEL_RETRIES_RANGE.1);
                    self.retries_text = v.to_string();
                    self.change(SettingChange::TunnelMaxRetries(v), false)
                }
                Err(_) => {
                    self.retries_text = self.values.tunnel_max_retries.to_string();
                    self.flush_now()
                }
            },
            SettingsMsg::ScrollbackApply => match self.pending_lower.take() {
                Some(v) => {
                    self.scrollback_text = v.to_string();
                    self.change(SettingChange::ScrollbackLines(v), false)
                }
                None => Effect::NONE,
            },
            SettingsMsg::ScrollbackCancel => {
                self.pending_lower = None;
                self.held_lower = None;
                self.scrollback_text = self.values.scrollback_lines.to_string();
                Effect::NONE
            }
        }
    }

    /// A scrollback value from the field. A value below the saved one that would cut an open
    /// tab's history is not saved: it waits for the user's confirmation (so typing `5` on the
    /// way to `50000` never trims anything, and Enter keeps the question open). A typed lower
    /// value nothing exceeds waits for the debounce (`held_lower`); raising applies at once.
    fn set_scrollback(&mut self, v: usize, debounce: bool) -> Effect {
        if v < self.values.scrollback_lines && self.max_open_history > v {
            self.pending_lower = Some(v);
            return Effect::NONE;
        }
        if debounce && v < self.values.scrollback_lines {
            // Nothing would be cut now, but this may be a prefix of a larger number: wait.
            self.generation += 1;
            self.held_lower = Some((v, self.generation));
            return Effect {
                apply: None,
                write: Write::After(self.generation),
            };
        }
        self.change(SettingChange::ScrollbackLines(v), debounce)
    }

    /// The question shown under the Scrollback field while a lowered value waits.
    pub fn scrollback_warning(&self) -> Option<String> {
        self.pending_lower.map(|v| {
            format!(
                "Open tabs will lose their oldest lines beyond {}. This can't be undone.",
                thousands(v)
            )
        })
    }

    /// The quiet note under the field for a very large value.
    pub fn scrollback_note(&self) -> Option<&'static str> {
        let shown = self.pending_lower.unwrap_or(self.values.scrollback_lines);
        (shown > SCROLLBACK_WARN_ABOVE).then_some(SCROLLBACK_NOTE)
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
