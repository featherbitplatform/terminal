//! Application state and the update loop: windows of tabs, each tab one session.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fbterm_config::{
    ConfigError, Paths, ProfileId, ProfileKind, ProfileStore, SavedState, SavedWindow, Settings,
    discover_shells,
};
use fbterm_secrets::{FallbackStore, MemoryStore, SecretStore};
use fbterm_session::ssh::{SshContext, SshPool};
use fbterm_session::{EventSink, SessionEvent, SessionHandle, SessionId};
use fbterm_term::{ColorScheme, GridSize, TerminalOutput, TerminalStyle};
use iced::window::{self, Screenshot};
use iced::{Event, Font, Subscription, Task, Theme};
use parking_lot::Mutex;
use tokio::sync::mpsc::unbounded_channel;

use crate::chrome::{self, BarAction, Chrome};
use crate::editor::{EditorMsg, EditorState};
use crate::keys::Action;
use crate::palette::{self, PaletteItem, PaletteMsg, PaletteState, Palettes, Recency};
use crate::prompts::{
    PromptEvent, PromptMsg, PromptQueues, PromptReceiver, UiPrompter, prompt_stream,
};
use crate::restore;
use crate::sessions::{EventReceiver, session_stream, write_notice};
use crate::tabs::{self, Tab, TabId, TabState, WindowState};
use crate::theme;
use crate::watch::{self, Changed, ConfigDir};

/// What a tab bar asks of its own window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabBarMsg {
    Select(usize),
    Close(usize),
    New,
}

/// A value handed through a (cloneable) message exactly once.
pub struct Delivered<T>(Arc<Mutex<Option<T>>>);

impl<T> Delivered<T> {
    pub fn new(value: T) -> Self {
        Self(Arc::new(Mutex::new(Some(value))))
    }
    pub fn take(&self) -> Option<T> {
        self.0.lock().take()
    }
}

impl<T> Clone for Delivered<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> std::fmt::Debug for Delivered<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Delivered(..)")
    }
}

#[allow(
    dead_code,
    reason = "some variants are sent by the palette, editor and restore"
)]
#[derive(Debug, Clone)]
pub enum Message {
    Session(SessionId, SessionEvent),
    Terminal(window::Id, TerminalOutput),
    /// Tab-bar and shortcut actions act on the focused window.
    NewTab,
    CloseTab(usize),
    SelectTab(usize),
    NextTab,
    PrevTab,
    OpenProfile {
        profile: ProfileId,
        window: window::Id,
    },
    /// A dial finished; carries the pty size it was opened with.
    Connected(TabId, Delivered<SessionHandle>, GridSize),
    ConnectFailed(TabId, String),
    /// A dial was refused because the host key changed: fail the tab and explain.
    HostKeyChanged {
        tab: TabId,
        host: String,
        port: u16,
        reason: String,
    },
    NewWindow,
    WindowOpened(window::Id),
    WindowFocused(window::Id),
    WindowUnfocused(window::Id),
    WindowClosed(window::Id),
    /// A click in a window's tab bar; acts on that window whatever has focus.
    TabBar(window::Id, TabBarMsg),
    /// A press on empty title-bar space (unified title bar): move or zoom that window.
    TitleBar(window::Id, BarAction),
    Shortcut(window::Id, Action),
    DismissNotices,
    /// Palette input for one window.
    Palette(window::Id, PaletteMsg),
    Editor(EditorMsg),
    Prompt(PromptMsg),
    Shoot,
    Shot(Screenshot),
    /// The OS close button (or equivalent) was used on a window; see [`App::close_requested`].
    CloseRequested(window::Id),
    /// A window was resized (or has just reported its size).
    WindowResized(window::Id, iced::Size),
    /// The quiet period after a resize passed; saves if no newer resize happened (its count).
    SaveState(u64),
    /// `profiles.toml` and/or `settings.toml` changed on disk.
    ConfigChanged(Changed),
}

/// Size used for sessions started before their window has laid out.
pub(crate) const DEFAULT_GRID: GridSize = GridSize {
    cols: 80,
    rows: 24,
    cell_width: 8.0,
    cell_height: 16.0,
};

pub struct App {
    pub settings: Settings,
    pub profiles: ProfileStore,
    /// Config problems found at startup (path:line: message), shown until dismissed.
    pub notices: Vec<String>,
    pub notices_dismissed: bool,
    /// `profiles.toml` exists but could not be parsed; the in-memory profiles are a stand-in.
    /// While set, nothing may save the profile store: that would overwrite the user's file.
    pub profiles_error: Option<ConfigError>,
    pub(crate) paths: Paths,
    pub(crate) secrets: Arc<dyn fbterm_secrets::SecretStore>,
    pub(crate) fallback: Option<Arc<FallbackStore>>,
    pub(crate) pool: Arc<SshPool>,
    pub(crate) events: EventSink,
    pub(crate) receiver: EventReceiver,
    pub windows: BTreeMap<window::Id, WindowState>,
    pub(crate) grids: HashMap<window::Id, GridSize>,
    /// The window that last had focus: where unscoped actions go.
    pub focused: Option<window::Id>,
    /// Open quick-launch palettes, by window.
    pub palettes: Palettes,
    /// The profile editor window, when open.
    pub editor: Option<(window::Id, EditorState)>,
    /// Connection dialogs waiting per window.
    pub prompts: PromptQueues,
    pub(crate) prompt_tx: tokio::sync::mpsc::UnboundedSender<PromptEvent>,
    pub(crate) prompt_rx: PromptReceiver,
    /// When each profile was last opened (persisted in `state.toml`).
    pub recency: Recency,
    /// Whether `state.toml` is written. Off for debug aids that open tabs (`FBTERM_OPEN_TABS`,
    /// `FBTERM_SCREENSHOT`, the perf runs): they would overwrite the user's real layout.
    pub(crate) state_saving_enabled: bool,
    /// The app is quitting: windows closing from now on are not the user dropping them, so
    /// the layout saved at quit must stay as it is.
    pub(crate) quitting: bool,
    /// Windows we asked to close. A window that closes without being asked is the OS tearing
    /// the app down (macOS Cmd+Q from the menu): that is a quit too.
    pub(crate) closing: std::collections::HashSet<window::Id>,
    /// Last known inner size of each terminal window.
    pub(crate) sizes: HashMap<window::Id, (f32, f32)>,
    /// Counts resizes, so only the last one of a burst saves.
    pub(crate) resize_count: u64,
    /// The text of `profiles.toml` / `settings.toml` as last loaded or written by us: a change
    /// event for exactly this text is our own write (or a touch), not an edit.
    pub(crate) profiles_seen: Option<String>,
    pub(crate) settings_seen: Option<String>,
    /// The banner line for a `settings.toml` that failed to load.
    pub(crate) settings_notice: Option<String>,
    /// Whether one of our windows has focus at all (the app is in front).
    pub app_active: bool,
    pub style: TerminalStyle,
    pub style_unfocused: TerminalStyle,
    pub theme: Theme,
    pub tokens: &'static theme::Tokens,
    pub(crate) screenshot: Option<PathBuf>,
    /// `FBTERM_SCREENSHOT_WINDOW=editor` (debug aid): capture the editor window instead.
    pub(crate) screenshot_editor: bool,
    pub(crate) shooting: bool,
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Configuration directory: `FBTERM_CONFIG_DIR` (debug aid: keeps manual runs away from the
/// real config) or the platform default.
fn paths() -> Paths {
    match std::env::var_os("FBTERM_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => Paths {
            config_dir: dir.into(),
        },
        _ if crate::perf::opts().is_some() => {
            // Measurement runs never touch the real config.
            let dir = std::env::temp_dir().join(format!("fbterm-perf-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            Paths { config_dir: dir }
        }
        _ => Paths::from_env(),
    }
}

/// Load profiles without ever writing the file. A missing file is seeded with the discovered
/// shells; a malformed one is reported and replaced in memory by them.
/// Returns the store and whether the file failed to load.
pub(crate) fn load_profiles(
    paths: &Paths,
    notices: &mut Vec<String>,
) -> (ProfileStore, Option<ConfigError>) {
    let path = paths.profiles();
    let mut failed = None;
    let mut store = if path.exists() {
        match ProfileStore::load(&path) {
            Ok(store) => store,
            Err(e) => {
                notices.push(e.to_string());
                failed = Some(e);
                ProfileStore::default()
            }
        }
    } else {
        ProfileStore::default()
    };
    if !store
        .profiles
        .iter()
        .any(|p| matches!(p.kind, ProfileKind::Local(_)))
    {
        store.profiles.extend(discover_shells());
    }
    for e in store.validate() {
        notices.push(format!("{}: {e}", path.display()));
    }
    (store, failed)
}

/// The look of terminals for `settings` (the font name lives for the whole process: iced wants a
/// `'static` name, and a changed font in `settings.toml` costs one small allocation).
fn terminal_style(settings: &Settings) -> TerminalStyle {
    let family: &'static str = Box::leak(settings.font_family.clone().into_boxed_str());
    TerminalStyle {
        scheme: ColorScheme::graphite(),
        font: Font::with_name(family),
        font_size: settings.font_size,
        accent: None,
        focused: true,
        // The window paints the default background (with the padding around the grid) at
        // `background_opacity`; see `window::view`.
        background_opacity: 0.0,
    }
}

/// A debug-only environment variable (`FBTERM_SCREENSHOT`, `FBTERM_OPEN_TABS`, ...); release
/// builds ignore them.
pub(crate) fn debug_env(name: &str) -> Option<std::ffi::OsString> {
    if cfg!(debug_assertions) {
        std::env::var_os(name).filter(|v| !v.is_empty())
    } else {
        None
    }
}

impl App {
    pub fn boot() -> (Self, Task<Message>) {
        // FBTERM_SECRETS=memory (debug aid) keeps secrets in memory and never touches the keychain.
        let memory_secrets = std::env::var("FBTERM_SECRETS").as_deref() == Ok("memory")
            || crate::perf::opts().is_some();
        Self::build(paths(), memory_secrets)
    }

    pub(crate) fn build(paths: Paths, memory_secrets: bool) -> (Self, Task<Message>) {
        let mut notices = Vec::new();
        let mut settings_notice = None;
        // A settings warning (a value out of range, already clamped) is shown like an error,
        // but the clamped settings are used.
        let settings = match Settings::load_checked(&paths.settings()) {
            Ok((settings, warning)) => {
                if let Some(w) = warning {
                    notices.push(w.to_string());
                    settings_notice = Some(w.to_string());
                }
                settings
            }
            Err(e) => {
                notices.push(e.to_string());
                settings_notice = Some(e.to_string());
                Settings::default()
            }
        };
        let (profiles, profiles_error) = load_profiles(&paths, &mut notices);
        let saved = SavedState::load_or_default(&paths.state());
        // With a profiles.toml that failed to load every saved tab would look orphaned: restore
        // nothing, and (see `save_state`) leave state.toml alone until the file loads.
        let plan = if profiles_error.is_some() || crate::perf::opts().is_some() {
            Vec::new()
        } else {
            restore::plan(&saved, &profiles)
        };
        let profiles_seen = std::fs::read_to_string(paths.profiles()).ok();
        let settings_seen = std::fs::read_to_string(paths.settings()).ok();

        let (secrets, fallback): (Arc<dyn SecretStore>, _) = if memory_secrets {
            (Arc::new(MemoryStore::new()), None)
        } else {
            let store = Arc::new(FallbackStore::new());
            (store.clone(), Some(store))
        };
        let known_hosts = directories::BaseDirs::new()
            .map(|d| d.home_dir().join(".ssh").join("known_hosts"))
            .unwrap_or_else(|| PathBuf::from("known_hosts"));
        let (prompt_tx, prompt_rx) = unbounded_channel();
        // Dials pass a prompter for their own window; this one asks over the focused window.
        let pool = Arc::new(SshPool::new(SshContext {
            known_hosts,
            secrets: secrets.clone(),
            prompter: Arc::new(UiPrompter::new(None, prompt_tx.clone())),
        }));

        let style = terminal_style(&settings);
        let (events, rx) = unbounded_channel();
        let mut app = Self {
            theme: theme::graphite_theme(settings.theme),
            tokens: theme::tokens(settings.theme),
            settings,
            profiles,
            notices,
            notices_dismissed: false,
            profiles_error,
            paths,
            secrets,
            fallback,
            pool,
            events,
            receiver: EventReceiver(Arc::new(Mutex::new(Some(rx)))),
            windows: BTreeMap::new(),
            grids: HashMap::new(),
            focused: None,
            palettes: Palettes::new(),
            editor: None,
            prompts: PromptQueues::default(),
            prompt_tx,
            prompt_rx: PromptReceiver(Arc::new(Mutex::new(Some(prompt_rx)))),
            recency: Recency::from(saved.recency),
            state_saving_enabled: debug_env("FBTERM_OPEN_TABS").is_none()
                && debug_env("FBTERM_SCREENSHOT").is_none()
                && crate::perf::opts().is_none(),
            quitting: false,
            closing: Default::default(),
            sizes: HashMap::new(),
            resize_count: 0,
            profiles_seen,
            settings_seen,
            settings_notice,
            app_active: true,
            style_unfocused: TerminalStyle {
                focused: false,
                ..style.clone()
            },
            style,
            screenshot: debug_env("FBTERM_SCREENSHOT").map(PathBuf::from),
            screenshot_editor: debug_env("FBTERM_SCREENSHOT_WINDOW").is_some_and(|w| w == "editor"),
            shooting: false,
        };
        let mut task = match crate::perf::opts() {
            Some(o) => app.open_perf_tabs(o),
            None => app.restore_windows(&plan),
        };
        task = task.chain(app.open_debug_tabs());
        task = task.chain(app.open_debug_palette());
        task = task.chain(app.open_debug_editor());
        task = task.chain(app.open_debug_prompt());
        (app, task)
    }

    /// The first local profile, preferring the user's login shell.
    pub fn default_profile(&self) -> Option<ProfileId> {
        let shell = std::env::var_os("SHELL").map(PathBuf::from);
        let locals = || {
            self.profiles.profiles.iter().filter_map(|p| match &p.kind {
                ProfileKind::Local(l) => Some((p.id, &l.program)),
                ProfileKind::Ssh(_) => None,
            })
        };
        locals()
            .find(|(_, program)| shell.as_ref() == Some(*program))
            .or_else(|| locals().next())
            .map(|(id, _)| id)
    }

    /// Notices to show: config problems and a degraded keychain.
    pub fn visible_notices(&self) -> Vec<String> {
        if self.notices_dismissed {
            return Vec::new();
        }
        let mut out = self.notices.clone();
        if self.profiles_error.is_some() {
            out.push("Profiles are read-only until profiles.toml is fixed".into());
        }
        if self.fallback.as_ref().is_some_and(|f| f.degraded()) {
            out.push("Keychain unavailable: passwords are kept for this session only".into());
        }
        out
    }

    pub fn title(&self, id: window::Id) -> String {
        if self.editor_window() == Some(id) {
            return "Profiles · fbterm".into();
        }
        match self.windows.get(&id).and_then(WindowState::active_tab) {
            Some(tab) => format!("{} · fbterm", tab.title),
            None => "fbterm".into(),
        }
    }

    /// The color windows are cleared with: clear when the background is translucent (every
    /// window then paints its own background), the theme's background otherwise.
    pub fn style(&self, theme: &Theme) -> iced::theme::Style {
        let p = theme.palette();
        iced::theme::Style {
            background_color: if self.settings.background_opacity < 1.0 {
                iced::Color::TRANSPARENT
            } else {
                p.background
            },
            text_color: p.text,
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            Subscription::run_with(self.receiver.clone(), session_stream)
                .map(|(id, ev)| Message::Session(id, ev)),
            iced::event::listen_with(|event, _, id| match event {
                Event::Window(window::Event::Focused) => Some(Message::WindowFocused(id)),
                Event::Window(window::Event::Unfocused) => Some(Message::WindowUnfocused(id)),
                _ => None,
            }),
            window::close_events().map(Message::WindowClosed),
            window::close_requests().map(Message::CloseRequested),
            Subscription::run_with(self.prompt_rx.clone(), prompt_stream).map(Message::Prompt),
        ];
        if self.screenshot.is_some() && !self.shooting && self.first_window_ready() {
            // Give the renderer time to load fonts and present a few frames.
            subs.push(iced::time::every(Duration::from_millis(1500)).map(|_| Message::Shoot));
        }
        subs.push(window::resize_events().map(|(id, size)| Message::WindowResized(id, size)));
        subs.push(
            Subscription::run_with(
                ConfigDir(self.paths.config_dir.clone()),
                watch::config_stream,
            )
            .map(Message::ConfigChanged),
        );
        Subscription::batch(subs)
    }

    fn first_window_ready(&self) -> bool {
        if self.screenshot_editor && self.editor.is_none() {
            return false;
        }
        self.windows
            .values()
            .next()
            .and_then(WindowState::active_tab)
            .is_some_and(|t| t.state != TabState::NotConnected)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Session(id, event) => self.session_event(id, event),
            Message::Terminal(wid, output) => self.terminal_output(wid, output),
            Message::NewTab => self.action(None, Action::NewTab),
            Message::CloseTab(i) => match self.focused_window() {
                Some(wid) => self.close_tab(wid, i),
                None => Task::none(),
            },
            Message::SelectTab(i) => self.with_focused(|w| w.select(i)),
            Message::NextTab => self.with_focused(WindowState::next),
            Message::PrevTab => self.with_focused(WindowState::prev),
            Message::OpenProfile { profile, window } => self.open_tab(window, profile),
            Message::Connected(tab, delivered, requested) => {
                if let Some(handle) = delivered.take() {
                    self.connected(tab, handle, requested);
                }
                Task::none()
            }
            Message::ConnectFailed(tab, reason) => {
                self.connect_failed(tab, reason);
                Task::none()
            }
            Message::HostKeyChanged {
                tab,
                host,
                port,
                reason,
            } => {
                let task = self.host_key_changed(tab, host, port);
                self.connect_failed(tab, reason);
                task
            }
            Message::NewWindow => self.action(None, Action::NewWindow),
            Message::WindowOpened(id) => {
                self.focused.get_or_insert(id);
                Task::none()
            }
            Message::WindowFocused(id) => {
                self.focused = Some(id);
                self.app_active = true;
                self.shown(id)
            }
            Message::WindowUnfocused(id) => {
                // Focus moving to another of our windows reports Focused for it right after.
                if self.focused == Some(id) {
                    self.app_active = false;
                }
                Task::none()
            }
            Message::TabBar(wid, msg) => {
                self.focused = Some(wid);
                match msg {
                    TabBarMsg::Select(i) => self.with_window(wid, |w| w.select(i)),
                    TabBarMsg::Close(i) => self.close_tab(wid, i),
                    TabBarMsg::New => self.action(Some(wid), Action::NewTab),
                }
            }
            Message::TitleBar(wid, action) => chrome::perform(wid, action),
            Message::WindowClosed(id) => {
                if self.editor_window() == Some(id) {
                    self.editor = None;
                }
                // Quit and close rules: see `App::quit`. A close nobody asked for is the OS
                // ending the app; state.toml is current (saved on every change), so just
                // stop saving.
                if !self.closing.remove(&id) {
                    self.quitting = true;
                }
                // Dropping the window's tabs closes their sessions and abandons their dials;
                // its dialogs answer "no".
                let was_terminal = self.windows.remove(&id).is_some();
                self.sizes.remove(&id);
                self.palettes.remove(&id);
                self.prompts.remove_window(id);
                self.grids.remove(&id);
                if self.focused == Some(id) {
                    self.focused = self.windows.keys().next().copied();
                }
                // A window the user closed is dropped from the layout (no-op while quitting).
                if was_terminal {
                    self.save_state();
                }
                if self.windows.is_empty() && self.editor.is_none() {
                    iced::exit()
                } else {
                    Task::none()
                }
            }
            Message::Shortcut(wid, action) => self.action(Some(wid), action),
            Message::DismissNotices => {
                self.notices_dismissed = true;
                Task::none()
            }
            Message::Palette(wid, msg) => self.palette(wid, msg),
            Message::Editor(msg) => self.editor_msg(msg),
            Message::Prompt(msg) => self.prompt_msg(msg),
            Message::WindowResized(id, size) => self.window_resized(id, size),
            Message::SaveState(n) => {
                if n == self.resize_count {
                    self.save_state();
                }
                Task::none()
            }
            Message::ConfigChanged(changed) => {
                if changed.profiles {
                    self.profiles_file_changed();
                }
                if changed.settings {
                    self.settings_file_changed();
                }
                Task::none()
            }
            Message::CloseRequested(id) => self.close_requested(id),
            Message::Shoot => self.shoot(),
            Message::Shot(shot) => {
                if let Some(path) = &self.screenshot {
                    match crate::window::save_png(path, &shot) {
                        Ok(()) => println!("saved {}", path.display()),
                        Err(e) => eprintln!("screenshot failed: {e}"),
                    }
                }
                iced::exit()
            }
        }
    }

    fn connect_failed(&mut self, tab: TabId, reason: String) {
        if let Some(t) = self.tab_mut(tab) {
            t.connecting = None;
            let text = if reason.starts_with("could not connect") {
                format!("{reason}, press Enter to retry")
            } else {
                format!("could not connect: {reason}, press Enter to retry")
            };
            t.state = TabState::Disconnected(reason);
            write_notice(t, &text, false);
        }
    }

    pub(crate) fn focused_window(&self) -> Option<window::Id> {
        self.focused
            .filter(|id| self.windows.contains_key(id))
            .or_else(|| self.windows.keys().next().copied())
    }

    /// Change tab selection in the focused window.
    fn with_focused(&mut self, f: impl FnOnce(&mut WindowState) -> bool) -> Task<Message> {
        match self.focused_window() {
            Some(wid) => self.with_window(wid, f),
            None => Task::none(),
        }
    }

    /// Change tab selection in `wid`; the newly shown tab gets [`App::shown`].
    fn with_window(
        &mut self,
        wid: window::Id,
        f: impl FnOnce(&mut WindowState) -> bool,
    ) -> Task<Message> {
        if self.windows.get_mut(&wid).is_some_and(f) {
            self.save_state();
            self.shown(wid)
        } else {
            Task::none()
        }
    }

    fn action(&mut self, window: Option<window::Id>, action: Action) -> Task<Message> {
        if let Some(wid) = window {
            self.focused = Some(wid);
        }
        let wid = self.focused_window();
        match action {
            // New tab asks which profile to open: the palette, with nothing preselected.
            Action::NewTab => match wid {
                Some(wid) => self.open_palette(wid, String::new()),
                None => self.open_window(self.default_profile()),
            },
            Action::CloseTab => match wid {
                Some(wid) => {
                    let active = self.windows.get(&wid).map_or(0, |w| w.active);
                    self.close_tab(wid, active)
                }
                None => Task::none(),
            },
            Action::NextTab => self.with_focused(WindowState::next),
            Action::PrevTab => self.with_focused(WindowState::prev),
            Action::SelectTab(n) => self.with_focused(|w| w.select_number(n)),
            Action::Palette => match wid {
                Some(wid) if self.palettes.contains_key(&wid) => {
                    self.palette(wid, PaletteMsg::Close)
                }
                Some(wid) => self.palette(wid, PaletteMsg::Open),
                None => Task::none(),
            },
            Action::NewWindow => self.open_window(self.default_profile()),
            Action::EditProfiles => self.editor_msg(EditorMsg::Open),
            Action::Quit => self.quit(),
            Action::Settings => {
                tracing::info!("settings are not available yet");
                Task::none()
            }
        }
    }

    /// Open `wid`'s palette (or reset it) with `query`, and focus its input.
    pub(crate) fn open_palette(&mut self, wid: window::Id, query: String) -> Task<Message> {
        if !self.windows.contains_key(&wid) {
            return Task::none();
        }
        let state = PaletteState::new(palette::items(&self.profiles), query, &self.recency);
        let input = state.input.clone();
        self.palettes.insert(wid, state);
        iced::widget::operation::focus(input)
    }

    fn palette(&mut self, wid: window::Id, msg: PaletteMsg) -> Task<Message> {
        let recency = &self.recency;
        let Some(state) = self.palettes.get_mut(&wid) else {
            return match msg {
                PaletteMsg::Open => self.open_palette(wid, String::new()),
                _ => Task::none(),
            };
        };
        let chosen = match msg {
            PaletteMsg::Open => return self.open_palette(wid, String::new()),
            PaletteMsg::Query(q) => {
                state.set_query(q, recency);
                return Task::none();
            }
            PaletteMsg::Up => {
                state.up();
                return Task::none();
            }
            PaletteMsg::Down => {
                state.down();
                return Task::none();
            }
            PaletteMsg::Close => None,
            PaletteMsg::Submit => state.selected_item().cloned(),
            PaletteMsg::Pick(position) => state.at(position).cloned(),
        };
        if matches!(msg, PaletteMsg::Submit | PaletteMsg::Pick(_)) && chosen.is_none() {
            // Nothing matches: Enter does nothing, the palette stays open.
            return Task::none();
        }
        self.palettes.remove(&wid);
        match chosen {
            Some(PaletteItem::Profile { id, .. }) => self.open_tab(wid, id),
            Some(PaletteItem::Action { action, .. }) => self.run_palette_action(wid, action),
            None => Task::none(),
        }
    }

    fn run_palette_action(&mut self, wid: window::Id, action: Action) -> Task<Message> {
        match action {
            // From the palette, "New tab" is the default shell (the palette itself just closed).
            Action::NewTab => match self.default_profile() {
                Some(profile) => self.open_tab(wid, profile),
                None => Task::none(),
            },
            other => self.action(Some(wid), other),
        }
    }

    /// A new terminal window of `size` (logical pixels), with no tabs yet.
    pub(crate) fn new_window(&mut self, size: (f32, f32)) -> (window::Id, Task<Message>) {
        let chrome = Chrome::current();
        let backdrop = chrome.backdrop(
            self.settings.background_opacity,
            self.settings.background_blur,
        );
        let (id, open) = window::open(chrome.window_settings(window::Settings {
            size: iced::Size::new(size.0, size.1),
            min_size: Some(iced::Size::new(360.0, 200.0)),
            exit_on_close_request: false,
            transparent: backdrop.transparent,
            blur: backdrop.blur,
            ..window::Settings::default()
        }));
        self.windows.insert(id, WindowState::new());
        self.sizes.insert(id, size);
        (id, open.map(Message::WindowOpened))
    }

    /// Reopen the saved windows with their tabs not connected, then connect the active tab of
    /// each; the others connect when first selected (see [`App::shown`]). With nothing to
    /// restore, the default single window opens.
    fn restore_windows(&mut self, plan: &[SavedWindow]) -> Task<Message> {
        if plan.is_empty() {
            return self.open_window(self.default_profile());
        }
        let mut tasks = Vec::new();
        for saved in plan {
            let (wid, open) = self.new_window(saved.size);
            let window = self.windows.get_mut(&wid).expect("just inserted");
            for &profile in &saved.tabs {
                if let Some(p) = self.profiles.get(profile) {
                    window
                        .tabs
                        .push(Tab::new(profile, p.name.clone(), p.accent));
                }
            }
            window.active = saved.active.min(window.tabs.len().saturating_sub(1));
            tracing::info!(
                tabs = window.tabs.len(),
                active = window.active,
                "restoring window"
            );
            tasks.push(open);
            tasks.push(self.shown(wid));
        }
        Task::batch(tasks)
    }

    pub fn open_window(&mut self, profile: Option<ProfileId>) -> Task<Message> {
        let (id, open) = self.new_window(restore::DEFAULT_SIZE);
        let start = match profile {
            Some(p) => self.open_tab(id, p),
            None => Task::none(),
        };
        open.chain(start)
    }

    pub fn open_tab(&mut self, wid: window::Id, profile: ProfileId) -> Task<Message> {
        let Some(p) = self.profiles.get(profile) else {
            return Task::none();
        };
        let tab = Tab::new(profile, p.name.clone(), p.accent);
        self.recency.bump(profile, unix_now());
        let id = tab.id;
        let Some(window) = self.windows.get_mut(&wid) else {
            return Task::none();
        };
        window.push(tab);
        self.save_state();
        self.start(wid, id)
    }

    fn close_tab(&mut self, wid: window::Id, index: usize) -> Task<Message> {
        let Some(window) = self.windows.get_mut(&wid) else {
            return Task::none();
        };
        let Some(closed) = window.close(index) else {
            return Task::none();
        };
        drop(closed.tab);
        self.save_state();
        if closed.window_empty {
            self.close_window(wid)
        } else {
            self.shown(wid)
        }
    }

    /// The active tab of `wid` was just shown (selected, created, or its window focused): take
    /// the redraw its session left pending while hidden (the redraw follows this update), and
    /// start it if it never was (restored tabs connect on first focus).
    fn shown(&mut self, wid: window::Id) -> Task<Message> {
        let Some(window) = self.windows.get(&wid) else {
            return Task::none();
        };
        tabs::on_shown(window);
        match window.active_tab() {
            Some(t) if t.state == TabState::NotConnected => {
                let id = t.id;
                self.start(wid, id)
            }
            _ => Task::none(),
        }
    }

    /// Write `state.toml`: windows, tabs, selection, sizes and recency. Does nothing for debug
    /// runs (see `state_saving_enabled`) and while `profiles.toml` is unreadable (the tabs would
    /// look orphaned; the file keeps the layout until the profiles load again).
    pub(crate) fn save_state(&self) {
        if !self.state_saving_enabled || self.quitting || self.profiles_error.is_some() {
            return;
        }
        let state = restore::snapshot(&self.windows, &self.sizes, &self.recency);
        if let Err(e) = state.save(&self.paths.state()) {
            tracing::warn!(error = %e, "could not save the session state");
        }
    }

    /// Close a window on purpose (its `Closed` event then is not an OS teardown).
    pub(crate) fn close_window(&mut self, id: window::Id) -> Task<Message> {
        self.closing.insert(id);
        window::close(id)
    }

    /// How windows close and the app quits, as far as `state.toml` is concerned:
    /// - Quit (Cmd+Q / Ctrl+Shift+Q / palette) saves once while every window still exists, then
    ///   closes them all and exits; nothing saves after that, so every window comes back.
    /// - The close button on the last terminal window is Quit. On any other terminal window it
    ///   drops that window from the layout. Closing the last tab of the last window leaves no
    ///   tabs to save, so the next launch opens the default window.
    /// - macOS Cmd+Q from the app menu is `terminate:`: winit closes the windows without a
    ///   close request and the process ends; windows closing unasked count as a quit, and
    ///   state.toml is already current because it is saved on every layout change.
    pub(crate) fn quit(&mut self) -> Task<Message> {
        if self.quitting {
            return Task::none();
        }
        self.save_state();
        self.quitting = true;
        let ids: Vec<_> = self
            .windows
            .keys()
            .copied()
            .chain(self.editor_window())
            .collect();
        self.closing.extend(ids.iter().copied());
        Task::batch(ids.into_iter().map(window::close)).chain(iced::exit())
    }

    fn close_requested(&mut self, id: window::Id) -> Task<Message> {
        if self.editor_window() == Some(id) || self.windows.len() > 1 {
            self.close_window(id)
        } else {
            self.quit()
        }
    }

    fn window_resized(&mut self, id: window::Id, size: iced::Size) -> Task<Message> {
        if !self.windows.contains_key(&id) {
            return Task::none();
        }
        self.sizes.insert(id, (size.width, size.height));
        self.resize_count += 1;
        let n = self.resize_count;
        // Dragging a window edge resizes continuously: save once it settles.
        Task::perform(
            async { tokio::time::sleep(Duration::from_millis(500)).await },
            move |()| Message::SaveState(n),
        )
    }

    /// `settings.toml` changed on disk: apply it, or keep the previous settings and say why.
    pub(crate) fn settings_file_changed(&mut self) {
        let path = self.paths.settings();
        let text = std::fs::read_to_string(&path).ok();
        if text == self.settings_seen {
            return;
        }
        self.settings_seen = text;
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
            }
            Err(e) => {
                tracing::warn!(error = %e, "settings.toml not applied");
                self.show_settings_notice(e.to_string());
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
    fn apply_settings(&mut self, new: Settings) {
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
        if backdrop(&new) != backdrop(&self.settings) && !self.windows.is_empty() {
            tracing::info!("window transparency and blur apply to windows opened from now on");
        }
        tracing::info!("settings.toml reloaded");
        self.settings = new;
    }

    pub(crate) fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.windows
            .values_mut()
            .find_map(|w| w.find(id).map(|i| &mut w.tabs[i]))
    }

    fn shoot(&mut self) -> Task<Message> {
        if self.shooting {
            return Task::none();
        }
        let target = if self.screenshot_editor {
            self.editor_window()
        } else {
            self.windows.keys().next().copied()
        };
        let Some(wid) = target else {
            return Task::none();
        };
        self.shooting = true;
        window::screenshot(wid).map(Message::Shot)
    }
}
