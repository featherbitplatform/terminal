//! The app side of settings: the Settings window, applying settings live, and reloading
//! `settings.toml` when it changes on disk.

use std::collections::HashMap;
use std::sync::OnceLock;

use fbterm_config::Settings;
use fbterm_term::{ColorScheme, TerminalStyle};
use iced::{Font, Task, window};
use parking_lot::Mutex;

use super::{SettingsMsg, SettingsState, WRITE_DEBOUNCE, Write, fonts, thousands};
use crate::app::{App, Message, SettingsSource};
use crate::chrome::Chrome;
use crate::tabs::{Pane, Tab};
use crate::theme;

/// `name` as a `'static` str: iced wants font names that live for the whole process. Each
/// distinct name is allocated once, however often the setting changes.
pub(crate) fn intern(name: &str) -> &'static str {
    static NAMES: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let mut names = NAMES.get_or_init(Default::default).lock();
    if let Some(n) = names.get(name) {
        return n;
    }
    let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.insert(name.to_owned(), leaked);
    leaked
}

/// The look of terminals for `settings`, before a pane's color scheme applies (see
/// [`App::pane_style`]).
pub(crate) fn terminal_style(settings: &Settings) -> TerminalStyle {
    TerminalStyle {
        scheme: ColorScheme::graphite(),
        font: Font::with_name(intern(&settings.font_family)),
        font_size: settings.font_size,
        accent: None,
        focused: true,
        // The window paints the default background (with the padding around the grid) at
        // `background_opacity`; see `window::view`.
        background_opacity: 0.0,
    }
}

impl App {
    /// The look of one pane: the colors of its scheme ([`App::scheme_for`]), the scheme's font
    /// and size where it sets them (else the settings'), at the pane's own zoom; focused (keys
    /// go to it, solid cursor) only for the focused pane of the focused window while its find
    /// bar is closed. No accent border: the tiles draw the focus outline.
    pub(crate) fn pane_style(&self, wid: window::Id, tab: &Tab, pane: &Pane) -> TerminalStyle {
        let scheme = self.scheme_for(pane);
        let (lo, hi) = fbterm_config::settings::FONT_SIZE_RANGE;
        let base = scheme
            .font_size
            .map_or(self.settings.font_size, |s| s.clamp(lo, hi));
        TerminalStyle {
            scheme: ColorScheme::from(&scheme.colors),
            font: match &scheme.font_family {
                Some(family) => Font::with_name(intern(family)),
                None => self.style.font,
            },
            // Unzoomed: the size as is (it may exceed the zoom range, up to 72).
            font_size: if pane.zoom == 0 {
                base
            } else {
                (base + f32::from(pane.zoom)).clamp(6.0, 48.0)
            },
            focused: self.app_active
                && self.focused == Some(wid)
                && tab.focused == pane.id
                && pane.find.is_none(),
            accent: None,
            ..self.style.clone()
        }
    }

    pub fn settings_window(&self) -> Option<window::Id> {
        self.settings_win.as_ref().map(|(id, _)| *id)
    }

    pub(crate) fn settings_msg(&mut self, msg: SettingsMsg) -> Task<Message> {
        match msg {
            SettingsMsg::Open => self.open_settings(),
            SettingsMsg::Close => {
                self.write_pending_settings();
                match self.settings_window() {
                    Some(id) => self.close_window(id),
                    None => Task::none(),
                }
            }
            SettingsMsg::OpenFile => {
                crate::config_files::open_settings_file(&self.paths);
                Task::none()
            }
            SettingsMsg::RevealFolder => {
                crate::config_files::reveal_config_folder(&self.paths);
                Task::none()
            }
            SettingsMsg::EditSchemes => {
                let current = self.settings.color_scheme.clone();
                self.open_schemes(Some(current))
            }
            other => {
                // Locking every open terminal is only worth it for what reads the answer: a typed
                // or submitted scrollback, and the debounce of a lowered one held back.
                let wants_history = match &other {
                    SettingsMsg::ScrollbackText(_) | SettingsMsg::ScrollbackSubmit => true,
                    SettingsMsg::Flush(_) => self
                        .settings_win
                        .as_ref()
                        .is_some_and(|(_, state)| state.holds_lower()),
                    _ => false,
                };
                let max_history = wants_history.then(|| self.max_open_history());
                let Some((_, state)) = &mut self.settings_win else {
                    return Task::none();
                };
                if let Some(max) = max_history {
                    state.max_open_history = max;
                }
                let effect = state.update(other);
                let mut tasks = Vec::new();
                if let Some(change) = effect.apply {
                    let mut next = self.settings.clone();
                    change.apply(&mut next);
                    tasks.push(self.apply_settings(next, SettingsSource::Window));
                }
                tasks.push(match effect.write {
                    Write::Not => Task::none(),
                    Write::Now => self.write_pending_settings_task(),
                    Write::After(n) => Task::perform(
                        async { tokio::time::sleep(WRITE_DEBOUNCE).await },
                        move |()| Message::Settings(SettingsMsg::Flush(n)),
                    ),
                });
                Task::batch(tasks)
            }
        }
    }

    /// The most history lines any open pane holds: what a lowered scrollback would cut.
    fn max_open_history(&self) -> usize {
        self.windows
            .values()
            .flat_map(|w| w.tabs.iter())
            .flat_map(|tab| &tab.panes)
            .filter_map(|pane| pane.terminal())
            .map(|term| term.lock().history_len())
            .max()
            .unwrap_or(0)
    }

    /// Open the Settings window, or bring it to the front.
    pub(crate) fn open_settings(&mut self) -> Task<Message> {
        if let Some(id) = self.settings_window() {
            return window::gain_focus(id);
        }
        let (id, open) = window::open(Chrome::current().window_settings(window::Settings {
            size: iced::Size::new(640.0, 720.0),
            min_size: Some(iced::Size::new(520.0, 420.0)),
            exit_on_close_request: false,
            ..window::Settings::default()
        }));
        let read_only = Settings::load(&self.paths.settings()).err();
        let mut state = SettingsState::new(&self.settings, fonts::installed_monospace(), read_only);
        state.display_hz = self.display_hz();
        state.max_open_history = self.max_open_history();
        self.settings_win = Some((id, state));
        open.discard()
    }

    /// Write the Settings window's unwritten changes to `settings.toml`, each key in place.
    /// Our own write is remembered so the file watcher does not reload it. If the file was
    /// edited on disk since we last saw it (and the watcher has not reported it yet), that edit
    /// is not ours to hide: after writing, the file is reloaded as for any edit on disk.
    /// A change that cannot be written stays pending (a later write retries it) and is reported.
    pub(crate) fn write_pending_settings(&mut self) {
        // Callers that drop the task are closing the window or the app: no find bar to refresh.
        drop(self.write_pending_settings_task());
    }

    /// [`Self::write_pending_settings`], returning the find refreshes a reload may ask for.
    fn write_pending_settings_task(&mut self) -> Task<Message> {
        let path = self.paths.settings();
        let Some((_, state)) = &mut self.settings_win else {
            return Task::none();
        };
        let pending = state.take_pending();
        if pending.is_empty() {
            return Task::none();
        }
        let external = std::fs::read_to_string(&path).ok() != self.settings_seen;
        let mut failed = Vec::new();
        let mut errors = Vec::new();
        for change in pending {
            if let Err(e) = Settings::write_field(&path, &change) {
                tracing::warn!(error = %e, key = change.key(), "settings.toml not written");
                let e = e.to_string();
                if !errors.contains(&e) {
                    errors.push(e);
                }
                failed.push(change);
            }
        }
        state.write_error = (!errors.is_empty())
            .then(|| format!("Could not save {}: {}", keys(&failed), errors.join("; ")));
        state.requeue(failed);
        if external {
            self.reload_settings()
        } else {
            self.settings_seen = std::fs::read_to_string(&path).ok();
            self.refresh_settings_notice();
            Task::none()
        }
    }

    /// After our own write: the banner keeps a warning only if the file still has one.
    fn refresh_settings_notice(&mut self) {
        let warning = match Settings::load_checked(&self.paths.settings()) {
            Ok((_, w)) => w.map(|w| w.to_string()),
            Err(e) => Some(e.to_string()),
        };
        if warning == self.settings_notice {
            return;
        }
        if let Some(old) = self.settings_notice.take() {
            self.notices.retain(|n| *n != old);
        }
        if let Some(w) = warning {
            self.show_settings_notice(w);
        }
    }

    /// `settings.toml` changed on disk: apply it, or keep the previous settings and say why.
    /// An open Settings window shows the new values (or turns read-only while it does not load).
    pub(crate) fn settings_file_changed(&mut self) -> Task<Message> {
        let path = self.paths.settings();
        let text = std::fs::read_to_string(&path).ok();
        if text == self.settings_seen {
            return Task::none();
        }
        self.reload_settings()
    }

    /// Load `settings.toml` and apply it (or keep the previous settings and show why), and
    /// remember its text as seen.
    fn reload_settings(&mut self) -> Task<Message> {
        let path = self.paths.settings();
        self.settings_seen = std::fs::read_to_string(&path).ok();
        if let Some(old) = self.settings_notice.take() {
            self.notices.retain(|n| *n != old);
        }
        match Settings::load_checked(&path) {
            Ok((new, warning)) => {
                if let Some(w) = warning {
                    tracing::warn!(warning = %w, "settings.toml values clamped");
                    self.show_settings_notice(w.to_string());
                }
                let task = self.apply_settings(new, SettingsSource::File);
                tracing::info!("settings.toml reloaded");
                if let Some((_, state)) = &mut self.settings_win {
                    state.read_only = None;
                    state.sync(&self.settings);
                }
                task
            }
            Err(e) => {
                tracing::warn!(error = %e, "settings.toml not applied");
                self.show_settings_notice(e.to_string());
                if let Some((_, state)) = &mut self.settings_win {
                    state.read_only = Some(e);
                    state.sync(&self.settings);
                }
                Task::none()
            }
        }
    }

    fn show_settings_notice(&mut self, line: String) {
        self.notices.push(line.clone());
        self.settings_notice = Some(line);
        self.notices_dismissed = false;
    }

    /// Font, size and theme take effect in every open terminal and window at once (they are
    /// read each frame). The scrollback length applies to every open terminal's normal screen
    /// and to new ones: raising it keeps everything, lowering it drops the oldest lines at once
    /// and then releases the freed memory (a lowered value read from `settings.toml` also sets
    /// the settings notice; one the Settings window applied after the user confirmed does not,
    /// see [`SettingsSource`]).
    /// `kitty_keyboard` applies to every open terminal at once: turning it off drops the
    /// protocol flags programs have set, turning it on affects their requests from then on.
    /// `color_scheme` recolors the open panes that follow it (those without a scheme of their
    /// own, or whose own is not found).
    /// `background_opacity` is read each frame too; a window can only show through if it was
    /// created transparent (always on macOS, see [`Chrome::backdrop`]), and `background_blur`
    /// is fixed when a window opens, so those apply to windows opened from now on.
    ///
    /// Returns the find refreshes a scrollback lowering asks for.
    pub(crate) fn apply_settings(
        &mut self,
        new: Settings,
        source: SettingsSource,
    ) -> Task<Message> {
        if new == self.settings {
            return Task::none();
        }
        if new.font_family != self.settings.font_family || new.font_size != self.settings.font_size
        {
            self.style = terminal_style(&new);
        }
        if new.theme != self.settings.theme {
            self.theme = theme::graphite_theme(new.theme);
            self.tokens = theme::tokens(new.theme);
        }
        if new.kitty_keyboard != self.settings.kitty_keyboard {
            let panes = self
                .windows
                .values()
                .flat_map(|w| &w.tabs)
                .flat_map(|t| &t.panes);
            for pane in panes {
                if let Some(term) = pane.terminal() {
                    term.lock().set_kitty_keyboard(new.kitty_keyboard);
                }
            }
        }
        let task = if new.scrollback_lines != self.settings.scrollback_lines {
            self.apply_scrollback(new.scrollback_lines, source)
        } else {
            Task::none()
        };
        let backdrop =
            |s: &Settings| Chrome::current().backdrop(s.background_opacity, s.background_blur);
        let (old_b, new_b) = (backdrop(&self.settings), backdrop(&new));
        if !self.windows.is_empty() {
            if new_b.blur != old_b.blur {
                tracing::info!("background blur applies to windows opened from now on");
            }
            if new_b.transparent && !old_b.transparent {
                tracing::info!(
                    "a see-through background shows in windows opened from now on; open windows \
                     were created opaque"
                );
            }
        }
        if new.keys.map != self.settings.keys.map {
            self.bindings = crate::keys::Bindings::current(&new.keys.map);
        }
        let rate_changed = new.max_fps != self.settings.max_fps;
        let scheme_changed = new.color_scheme != self.settings.color_scheme;
        let before = scheme_changed.then(|| self.pane_colors());
        self.settings = new;
        if rate_changed {
            self.update_output_rate();
        }
        if let Some(before) = before {
            self.schemes_moved(&before);
        }
        task
    }

    /// Set every open terminal's scrollback limit. If any lost lines, release the freed memory
    /// and, for a change from `settings.toml`, say how many tabs lost their oldest lines. A pane
    /// that lost lines and has a find bar open searches again, so the bar does not keep
    /// counting or stepping to matches that are gone.
    fn apply_scrollback(&mut self, lines: usize, source: SettingsSource) -> Task<Message> {
        let mut trimmed_panes = Vec::new();
        let mut trimmed = 0;
        for tab in self.windows.values().flat_map(|w| &w.tabs) {
            let before = trimmed_panes.len();
            for pane in &tab.panes {
                if let Some(term) = pane.terminal()
                    && term.lock().set_scrollback(lines) > 0
                {
                    trimmed_panes.push(pane.id);
                }
            }
            if trimmed_panes.len() > before {
                trimmed += 1;
            }
        }
        if trimmed == 0 {
            return Task::none();
        }
        crate::memory::release_freed();
        if source == SettingsSource::File {
            let tabs = if trimmed == 1 {
                "1 open tab lost its oldest lines".to_owned()
            } else {
                format!("{trimmed} open tabs lost their oldest lines")
            };
            let line = format!("Scrollback lowered to {} lines: {tabs}.", thousands(lines));
            // A clamp warning from the same reload stays; the two share the banner.
            let line = match self.settings_notice.take() {
                Some(old) => {
                    self.notices.retain(|n| *n != old);
                    format!("{old} {line}")
                }
                None => line,
            };
            self.show_settings_notice(line);
        }
        Task::batch(trimmed_panes.into_iter().map(|pane| self.find_output(pane)))
    }

    /// The refresh rate `max_fps = "display"` follows: the fastest display a terminal window is
    /// on, if any is known.
    pub(crate) fn display_hz(&self) -> Option<u32> {
        crate::display_rate::fastest(self.display_rates.values().copied())
    }

    /// Set the output redraw rate from `max_fps` and the displays; pacing picks it up with the
    /// next output. The Settings window shows the detected display rate.
    pub(crate) fn update_output_rate(&mut self) {
        let hz = self.display_hz();
        let fps = u32::from(self.settings.max_fps.effective(hz));
        if fps != self.receiver.rate.get() {
            tracing::info!(fps, display_hz = ?hz, "output redraw rate");
            self.receiver.rate.set(fps);
        }
        if let Some((_, state)) = &mut self.settings_win {
            state.display_hz = hz;
        }
    }
}

/// The keys of `changes`, for a message.
fn keys(changes: &[fbterm_config::SettingChange]) -> String {
    changes
        .iter()
        .map(fbterm_config::SettingChange::key)
        .collect::<Vec<_>>()
        .join(", ")
}
