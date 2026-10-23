//! The app side of settings: the Settings window, applying settings live, and reloading
//! `settings.toml` when it changes on disk.

use std::collections::HashMap;
use std::sync::OnceLock;

use fbterm_config::Settings;
use fbterm_term::{ColorScheme, TerminalStyle};
use iced::{Font, Task, window};
use parking_lot::Mutex;

use super::{SettingsMsg, SettingsState, WRITE_DEBOUNCE, Write, fonts};
use crate::app::{App, Message};
use crate::chrome::Chrome;
use crate::theme;

/// `name` as a `'static` str: iced wants font names that live for the whole process. Each
/// distinct name is allocated once, however often the setting changes.
fn intern(name: &str) -> &'static str {
    static NAMES: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let mut names = NAMES.get_or_init(Default::default).lock();
    if let Some(n) = names.get(name) {
        return n;
    }
    let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.insert(name.to_owned(), leaked);
    leaked
}

/// The look of terminals for `settings`.
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
            other => {
                let Some((_, state)) = &mut self.settings_win else {
                    return Task::none();
                };
                let effect = state.update(other);
                if let Some(change) = effect.apply {
                    let mut next = self.settings.clone();
                    change.apply(&mut next);
                    self.apply_settings(next);
                }
                match effect.write {
                    Write::Not => Task::none(),
                    Write::Now => {
                        self.write_pending_settings();
                        Task::none()
                    }
                    Write::After(n) => Task::perform(
                        async { tokio::time::sleep(WRITE_DEBOUNCE).await },
                        move |()| Message::Settings(SettingsMsg::Flush(n)),
                    ),
                }
            }
        }
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
        let state = SettingsState::new(&self.settings, fonts::installed_monospace(), read_only);
        self.settings_win = Some((id, state));
        open.discard()
    }

    /// Write the Settings window's unwritten changes to `settings.toml`, each key in place.
    /// Our own write is remembered so the file watcher does not reload it. If the file was
    /// edited on disk since we last saw it (and the watcher has not reported it yet), that edit
    /// is not ours to hide: after writing, the file is reloaded as for any edit on disk.
    /// A change that cannot be written stays pending (a later write retries it) and is reported.
    pub(crate) fn write_pending_settings(&mut self) {
        let path = self.paths.settings();
        let Some((_, state)) = &mut self.settings_win else {
            return;
        };
        let pending = state.take_pending();
        if pending.is_empty() {
            return;
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
            self.reload_settings();
        } else {
            self.settings_seen = std::fs::read_to_string(&path).ok();
            self.refresh_settings_notice();
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
    pub(crate) fn settings_file_changed(&mut self) {
        let path = self.paths.settings();
        let text = std::fs::read_to_string(&path).ok();
        if text == self.settings_seen {
            return;
        }
        self.reload_settings();
    }

    /// Load `settings.toml` and apply it (or keep the previous settings and show why), and
    /// remember its text as seen.
    fn reload_settings(&mut self) {
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
                self.apply_settings(new);
                tracing::info!("settings.toml reloaded");
                if let Some((_, state)) = &mut self.settings_win {
                    state.read_only = None;
                    state.sync(&self.settings);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "settings.toml not applied");
                self.show_settings_notice(e.to_string());
                if let Some((_, state)) = &mut self.settings_win {
                    state.read_only = Some(e);
                    state.sync(&self.settings);
                }
            }
        }
    }

    fn show_settings_notice(&mut self, line: String) {
        self.notices.push(line.clone());
        self.settings_notice = Some(line);
        self.notices_dismissed = false;
    }

    /// Font, size and theme take effect in every open terminal and window at once (they are
    /// read each frame). The scrollback length applies to terminals opened from now on.
    /// `kitty_keyboard` applies to every open terminal at once: turning it off drops the
    /// protocol flags programs have set, turning it on affects their requests from then on.
    /// `background_opacity` is read each frame too; a window can only show through if it was
    /// created transparent (always on macOS, see [`Chrome::backdrop`]), and `background_blur`
    /// is fixed when a window opens, so those apply to windows opened from now on.
    pub(crate) fn apply_settings(&mut self, new: Settings) {
        if new == self.settings {
            return;
        }
        if new.font_family != self.settings.font_family || new.font_size != self.settings.font_size
        {
            self.style = terminal_style(&new);
            self.style_unfocused = TerminalStyle {
                focused: false,
                ..self.style.clone()
            };
        }
        if new.theme != self.settings.theme {
            self.theme = theme::graphite_theme(new.theme);
            self.tokens = theme::tokens(new.theme);
        }
        if new.kitty_keyboard != self.settings.kitty_keyboard {
            for tab in self.windows.values().flat_map(|w| w.tabs.iter()) {
                if let Some(term) = tab.terminal() {
                    term.lock().set_kitty_keyboard(new.kitty_keyboard);
                }
            }
        }
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
        self.settings = new;
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
