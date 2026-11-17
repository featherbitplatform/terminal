//! The app side of settings: the Settings tab, applying settings live, and reloading
//! `settings.toml` when it changes on disk.

use fbterm_config::font_features::parse_features;
use std::collections::HashMap;
use std::sync::OnceLock;

use fbterm_config::{SettingChange, Settings, WindowDecorations};
use fbterm_term::{ColorScheme, TerminalStyle};
use iced::{Font, Task, window};
use parking_lot::Mutex;

use super::{SettingsMsg, SettingsState, WRITE_DEBOUNCE, Write, thousands};
use crate::app::{App, Message, SettingsSource};
use crate::chrome::Chrome;
use crate::tabs::{Page, Pane, Tab};

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

/// The look of terminals for `settings`, before a scheme applies and before a window's physical
/// scale is known (see [`App::pane_style`], which sets the scale of the window drawn in).
pub(crate) fn terminal_style(settings: &Settings) -> TerminalStyle {
    TerminalStyle {
        scheme: ColorScheme::graphite(),
        features: fbterm_term::FontFeatures {
            ligatures: settings.ligatures,
            extra: parse_features(&settings.font_features).0.into(),
        },
        font: Font::with_name(intern(&settings.font_family)),
        font_size: settings.font_size,
        accent: None,
        focused: true,
        // The window paints the default background (with the padding around the grid) at
        // `background_opacity`; see `window::view`.
        background_opacity: 0.0,
        scale: 1.0,
    }
}

impl App {
    /// The look of one pane: the colors of its scheme ([`App::scheme_for`]), the scheme's font
    /// (its own, or with `use_theme_fonts` its installed suggestion: [`App::scheme_font`]) and
    /// size where it sets them (else the settings'), at the pane's own zoom, at the window's
    /// physical scale (so the grid its view reports tells programs physical pixels); focused (keys
    /// go to it, solid cursor) only for the focused pane of the focused window while its find
    /// bar is closed and neither docked panel has the keyboard. No accent border: the tiles
    /// draw the focus outline.
    pub(crate) fn pane_style(&self, wid: window::Id, tab: &Tab, pane: &Pane) -> TerminalStyle {
        let scheme = self.scheme_for(pane);
        let (lo, hi) = fbterm_config::settings::FONT_SIZE_RANGE;
        let base = scheme
            .font_size
            .map_or(self.settings.font_size, |s| s.clamp(lo, hi));
        TerminalStyle {
            scheme: ColorScheme::from(&scheme.colors),
            features: self.style.features.clone(),
            font: match self.scheme_font(scheme) {
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
                && pane.find.is_none()
                && !self.renaming_in(wid)
                && !self.file_panels.has_focus(wid)
                && !self.tunnels_panels.has_focus(wid),
            accent: None,
            // The window's own scale: a pane moved to another display tells its program new pixels.
            scale: self.physical_scale(wid),
            ..self.style.clone()
        }
    }

    pub fn settings_window(&self) -> Option<window::Id> {
        self.settings_win.as_ref().map(|(id, _)| *id)
    }

    pub(crate) fn settings_msg(&mut self, msg: SettingsMsg) -> Task<Message> {
        match msg {
            SettingsMsg::Open => self.open_settings(),
            SettingsMsg::Close => self.close_page(Page::Settings),
            SettingsMsg::OpenFile => {
                crate::config_files::open_settings_file(&self.paths);
                Task::none()
            }
            SettingsMsg::RevealFolder => {
                crate::config_files::reveal_config_folder(&self.paths);
                Task::none()
            }
            SettingsMsg::FontsOpened => self.scan_monospace_fonts(),
            SettingsMsg::EditSchemes => {
                let current = self.settings.color_scheme.clone();
                self.open_schemes(Some(current))
            }
            SettingsMsg::SecretStorage(choice) => self.choose_secret_storage(choice),
            SettingsMsg::ResetVault => self.ask_reset_vault(),
            SettingsMsg::DownloadUpdate => {
                self.open_update_page();
                Task::none()
            }
            // The row shows `System` after the desktop refused drawn chrome while the setting
            // stays "fbterm": choosing fbterm tries drawn chrome again (nothing to write).
            SettingsMsg::TitleBar(WindowDecorations::Fbterm)
                if self.chrome_fell_back
                    && self.settings.window_decorations == WindowDecorations::Fbterm =>
            {
                self.chrome_fell_back = false;
                self.reapply_chrome()
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

    /// Change one setting from outside the Settings tab (an action, the Welcome tab): written
    /// to `settings.toml` and applied as a Settings tab change is, the open Settings tab
    /// following. An edit that came in meanwhile from someone else is loaded with it.
    pub(crate) fn set_setting(&mut self, change: SettingChange) -> Task<Message> {
        let path = self.paths.settings();
        let external = std::fs::read_to_string(&path).ok() != self.settings_seen;
        if let Err(e) = Settings::write_field(&path, &change) {
            tracing::warn!(error = %e, key = change.key(), "settings.toml not written");
        }
        if let Some((_, state)) = &mut self.settings_win {
            change.apply(&mut state.values);
        }
        let mut next = self.settings.clone();
        change.apply(&mut next);
        let task = self.apply_settings(next, SettingsSource::Window);
        if external {
            return Task::batch([task, self.settings_file_changed()]);
        }
        self.settings_seen = std::fs::read_to_string(&path).ok();
        task
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

    /// Open the Settings tab, or select it where it is open.
    pub(crate) fn open_settings(&mut self) -> Task<Message> {
        if self.settings_win.is_some() {
            return self.show_page(Page::Settings).1;
        }
        let (id, open) = self.show_page(Page::Settings);
        let read_only = Settings::load(&self.paths.settings()).err();
        let mut state = SettingsState::new(&self.settings, (self.font_io.monospace)(), read_only);
        state.display_hz = self.display_hz();
        state.max_open_history = self.max_open_history();
        self.settings_win = Some((id, state));
        open
    }

    /// Apply a value typed into a Settings field and still waiting on its quiet period, as
    /// Enter would (the tab is closing). Leaves the write to the caller.
    pub(crate) fn flush_typed_setting(&mut self) -> Task<Message> {
        let Some((_, state)) = &mut self.settings_win else {
            return Task::none();
        };
        let Some(change) = state.flush_typed().apply else {
            return Task::none();
        };
        let mut next = self.settings.clone();
        change.apply(&mut next);
        self.apply_settings(next, SettingsSource::Window)
    }

    /// Write the Settings tab's unwritten changes to `settings.toml`, each key in place.
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
    /// An open Settings tab shows the new values (or turns read-only while it does not load).
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
    /// the settings notice; one the Settings tab applied after the user confirmed does not,
    /// see [`SettingsSource`]).
    /// `kitty_keyboard` applies to every open terminal at once: turning it off drops the
    /// protocol flags programs have set, turning it on affects their requests from then on.
    /// `inline_images` shows or drops pictures in every open terminal at once: off clears what is
    /// on screen there and then (each picture released), and programs get "not supported" from
    /// then on; on again does not bring the pictures back. `scrollback_images` decides whether a
    /// picture whose rows have all scrolled off the screen stays or becomes a line saying how big
    /// it was, and only counts while `inline_images` is on.
    /// `grapheme_clusters` becomes every open terminal's default for mode 2027, used by new
    /// terminals and after a reset or restart; a running program's mode is never flipped.
    /// `color_scheme` recolors the open panes that follow it (those without a scheme of their
    /// own, or whose own is not found).
    /// `use_theme_fonts` changes the font of the panes whose scheme suggests an installed font
    /// (read each frame, like a scheme font: the views measure again and report their grids).
    /// `background_opacity` is read each frame too; a window can only show through if it was
    /// created transparent (always on macOS, see [`Chrome::backdrop`]), and `background_blur`
    /// is fixed when a window opens, so those apply to windows opened from now on.
    /// `macos_blur_style` re-tunes the blur view of every open window that has one, in place
    /// (the post-update `sync_blur_views`, which also follows scheme and tab changes).
    /// `window_decorations` (Linux) switches every open window's title bar at once.
    /// `check_updates` starts or stops the update check (its subscription); off also drops
    /// what it found.
    ///
    /// Returns the find refreshes a scrollback lowering asks for.
    pub(crate) fn apply_settings(
        &mut self,
        mut new: Settings,
        source: SettingsSource,
    ) -> Task<Message> {
        // Saved secrets move only through the Settings tab (which writes the key itself):
        // a hand edit of `secret_storage` applies on the next launch.
        new.secret_storage = self.settings.secret_storage;
        if new == self.settings {
            return Task::none();
        }
        let restyle = new.font_family != self.settings.font_family
            || new.font_size != self.settings.font_size
            || new.ligatures != self.settings.ligatures
            || new.font_features != self.settings.font_features;
        if restyle {
            self.style = terminal_style(&new);
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
        // Either switch reaches every open terminal: the pair goes to `set_images` together so a
        // terminal never sees one half of a change it has to guess the other half of.
        if new.inline_images != self.settings.inline_images
            || new.scrollback_images != self.settings.scrollback_images
        {
            let panes = self
                .windows
                .values()
                .flat_map(|w| &w.tabs)
                .flat_map(|t| &t.panes);
            for pane in panes {
                if let Some(term) = pane.terminal() {
                    term.lock()
                        .set_images(new.inline_images, new.scrollback_images);
                }
            }
        }
        if new.grapheme_clusters != self.settings.grapheme_clusters {
            let panes = self
                .windows
                .values()
                .flat_map(|w| &w.tabs)
                .flat_map(|t| &t.panes);
            for pane in panes {
                if let Some(term) = pane.terminal() {
                    term.lock().set_grapheme_clusters(new.grapheme_clusters);
                }
            }
        }
        let task = if new.scrollback_lines != self.settings.scrollback_lines {
            self.apply_scrollback(new.scrollback_lines, source)
        } else {
            Task::none()
        };
        let (old_keep, new_keep) = (
            self.settings.history_compression_keep(),
            new.history_compression_keep(),
        );
        let task = if new_keep == old_keep {
            task
        } else {
            Task::batch([task, self.apply_history_compression(old_keep, new_keep)])
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
        // Turning the update check off forgets what it found: no dot, no message.
        if !new.checks_updates() {
            self.update_available = None;
        }
        if new.keys.map != self.settings.keys.map {
            self.bindings = crate::keys::Bindings::current(&new.keys.map);
        }
        // Running tunnels read the limit at each reconnect attempt.
        self.tunnel_retries
            .store(new.tunnel_max_retries, std::sync::atomic::Ordering::Relaxed);
        let rate_changed = new.max_fps != self.settings.max_fps;
        let decorations_changed = new.window_decorations != self.settings.window_decorations;
        let scheme_changed = new.color_scheme != self.settings.color_scheme;
        let before = scheme_changed.then(|| self.pane_colors());
        let theme_fonts_changed = new.use_theme_fonts != self.settings.use_theme_fonts;
        self.settings = new;
        if let Some((_, state)) = &mut self.schemes_win {
            state.sync_settings(&self.settings);
        }
        let mut task = task;
        if theme_fonts_changed && self.settings.use_theme_fonts {
            // Turned on (a user action): read what is installed, and look for suggested fonts
            // installed since fbterm started.
            self.refresh_installed_fonts();
            self.warm_ligature_sets();
            task = Task::batch([task, self.scan_fonts()]);
        } else if theme_fonts_changed {
            self.rebuild_scheme_fonts();
        }
        self.refresh_feature_notices();
        if restyle {
            self.warm_ligature_sets();
        }
        if rate_changed {
            self.update_output_rate();
        }
        if let Some(before) = before {
            self.schemes_moved(&before);
            self.retheme();
        }
        if decorations_changed {
            let chrome = self.window_decorations_changed(self.settings.window_decorations);
            return Task::batch([task, chrome]);
        }
        task
    }

    /// Set every open terminal's history compression (`None`: off) and queue each for the
    /// background worker. The memory that compressing frees goes back to the OS once, after the
    /// worker drains, and only for a change the user made that frees memory: off to on, or a
    /// lower number of uncompressed lines. Raising it or turning it off frees nothing.
    fn apply_history_compression(
        &mut self,
        old: Option<usize>,
        new: Option<usize>,
    ) -> Task<Message> {
        let panes = self
            .windows
            .values()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| &t.panes);
        for term in panes.filter_map(|p| p.terminal()) {
            let queued = {
                let mut t = term.lock();
                t.set_history_compression(new);
                t.has_history_jobs()
            };
            if queued {
                fbterm_term::history_worker::enqueue(term);
            }
        }
        let frees = match (old, new) {
            (None, Some(_)) => true,
            (Some(old), Some(new)) => new < old,
            _ => false,
        };
        if !frees {
            return Task::none();
        }
        release_when_worker_idle()
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
    /// next output. The Settings tab shows the detected display rate.
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

impl App {
    /// The banner lines for `font_features` entries that do not parse, one per distinct entry,
    /// kept in step with the settings like the scheme notices.
    pub(crate) fn refresh_feature_notices(&mut self) {
        let mut lines: Vec<String> = Vec::new();
        for entry in parse_features(&self.settings.font_features).1 {
            let line = format!("font_features: \"{entry}\" is not a feature tag; it is ignored.");
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
        if lines == self.feature_notices {
            return;
        }
        let old = std::mem::take(&mut self.feature_notices);
        self.notices.retain(|n| !old.contains(n));
        if !lines.is_empty() {
            self.notices_dismissed = false;
        }
        self.notices.extend(lines.iter().cloned());
        self.feature_notices = lines;
    }
}

impl App {
    /// Detects the ligature-only stylistic sets of the terminal font and every scheme's font in
    /// the background, so the first frame drawn with one never reads a font file itself.
    pub(crate) fn warm_ligature_sets(&self) {
        if !self.settings.ligatures {
            return;
        }
        let mut fonts = vec![self.style.font];
        for scheme in self.schemes.all() {
            if let Some(family) = self.scheme_font(scheme) {
                let font = Font::with_name(intern(family));
                if !fonts.contains(&font) {
                    fonts.push(font);
                }
            }
        }
        // The thread is detached: drawing detects anything it has not reached yet.
        drop(fbterm_term::ligature_sets::warm_ligature_sets(fonts));
    }
}

/// Ask for the memory release once the history worker has drained (`release_when_idle` on the
/// blocking pool), for a user change that leaves the worker compressing history: switching
/// compression on, fewer uncompressed lines, a reflow. A release already waiting covers this
/// one too: none is added then.
pub(crate) fn release_when_worker_idle() -> Task<Message> {
    if !mark_release_pending() {
        return Task::none();
    }
    Task::perform(
        async {
            let _ = tokio::task::spawn_blocking(release_when_idle).await;
        },
        |()| (),
    )
    .discard()
}

/// How long the release waits for the history worker to drain before it releases anyway.
const RELEASE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Mark a release as waiting; `false` if one already was.
fn mark_release_pending() -> bool {
    !release_pending().swap(true, std::sync::atomic::Ordering::SeqCst)
}

fn clear_release_pending() {
    release_pending().store(false, std::sync::atomic::Ordering::SeqCst);
}

/// Whether a release is waiting: process-wide in the app. Tests get one flag per thread (they
/// run the release on their own thread, and run in parallel), so only where the flag lives
/// differs and the code that sets and clears it is the same.
fn release_pending() -> &'static std::sync::atomic::AtomicBool {
    #[cfg(not(test))]
    {
        static PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        &PENDING
    }
    #[cfg(test)]
    {
        thread_local! {
            static PENDING: &'static std::sync::atomic::AtomicBool =
                Box::leak(Box::new(std::sync::atomic::AtomicBool::new(false)));
        }
        PENDING.with(|p| *p)
    }
}

/// Wait up to [`RELEASE_WAIT`] for the history worker to drain, then ask for the memory release
/// (once) whether it did or not, and let the next freeing change start a new wait. Runs on the
/// blocking pool: the wait sleeps on a condition variable.
pub(super) fn release_when_idle() {
    fbterm_term::history_worker::wait_idle_for(RELEASE_WAIT);
    // Cleared even if the release panics.
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            clear_release_pending();
        }
    }
    let _clear = Clear;
    crate::memory::release_freed();
}
