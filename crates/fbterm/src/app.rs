//! Application state and the update loop: windows of tabs, each tab one session.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fbterm_config::{
    ConfigError, Paths, ProfileId, ProfileKind, ProfileStore, SavedState, Settings, discover_shells,
};
use fbterm_secrets::{FallbackStore, MemoryStore, SecretStore};
use fbterm_session::ssh::{SshContext, SshPool};
use fbterm_session::{EventSink, SessionEvent, SessionHandle, SessionId};
use fbterm_term::{GridSize, SearchOutcome, TerminalOutput, TerminalStyle};
use iced::window::{self, Screenshot};
use iced::{Event, Subscription, Task, Theme};
use parking_lot::Mutex;
use tokio::sync::mpsc::unbounded_channel;

use crate::chrome::{self, BarAction};
use crate::display_rate;
use crate::editor::{EditorMsg, EditorState};
use crate::find::FindMsg;
use crate::find::all::{FindAll, FindAllMsg};
use crate::keys::{Action, Bindings};
use crate::layout::PaneId;
use crate::palette::{PaletteMsg, Palettes, Recency};
use crate::prompts::{
    PromptEvent, PromptMsg, PromptQueues, PromptReceiver, UiPrompter, prompt_stream,
};
use crate::restore;
use crate::schemes_window::{SchemesMsg, SchemesState};
use crate::sessions::{EventReceiver, session_stream, write_notice};
use crate::settings_window::{SettingsMsg, SettingsState};
use crate::tabs::{Pane, TabState, WindowState};
use crate::theme;
use crate::visibility;
use crate::watch::{self, Changed, ConfigDir};

/// What a tab bar asks of its own window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabBarMsg {
    Select(usize),
    Close(usize),
    New,
    Profiles,
    Settings,
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
    Terminal(window::Id, PaneId, TerminalOutput),
    /// Focus, divider drags and the area of a window's panes.
    Tiles(window::Id, crate::tiles::TilesMsg),
    /// A tab or tile carried between the tab bar and the panes.
    Drag(window::Id, crate::windows::DragMsg),
    /// A pane's resize quiet period passed; applies if no newer resize came (its generation).
    ResizeSettled(PaneId, u64),
    /// Tab-bar and shortcut actions act on the focused window.
    NewTab,
    CloseTab(usize),
    /// `--perf-close-after`: close every tab of the window but the active one.
    PerfCloseOthers(window::Id),
    SelectTab(usize),
    NextTab,
    PrevTab,
    OpenProfile {
        profile: ProfileId,
        window: window::Id,
    },
    /// A dial finished; carries the pty size it was opened with.
    Connected(PaneId, Delivered<SessionHandle>, GridSize),
    ConnectFailed(PaneId, String),
    /// A dial was refused because the host key changed: fail the pane and explain.
    HostKeyChanged {
        pane: PaneId,
        host: String,
        port: u16,
        reason: String,
    },
    NewWindow,
    WindowOpened(window::Id),
    WindowFocused(window::Id),
    WindowUnfocused(window::Id),
    /// A terminal window became visible (`true`) or stopped being visible.
    WindowVisible(window::Id, bool),
    /// The refresh rate of the display a terminal window is on (`None`: unknown).
    DisplayRate(window::Id, Option<u32>),
    WindowClosed(window::Id),
    /// A click in a window's tab bar; acts on that window whatever has focus.
    TabBar(window::Id, TabBarMsg),
    /// A press on empty title-bar space (unified title bar): move or zoom that window.
    TitleBar(window::Id, BarAction),
    Shortcut(window::Id, Action),
    DismissNotices,
    /// Palette input for one window.
    Palette(window::Id, PaletteMsg),
    /// The find bar of a window's active tab.
    Find(window::Id, FindMsg),
    /// A pane's find-bar search finished.
    Found {
        pane: PaneId,
        seq: u64,
        outcome: SearchOutcome,
    },
    /// Search a pane's find bar again for the output that arrived (one timer per burst).
    FindRefresh(PaneId),
    /// The find-in-all-tabs box, open in a window.
    FindAll(window::Id, FindAllMsg),
    /// One pane's part of a find-in-all-tabs search finished.
    FoundAll {
        seq: u64,
        pane: PaneId,
        outcome: SearchOutcome,
    },
    /// Debug aid: open the find bar or the box with a query once output has arrived.
    DebugFind {
        all: bool,
        query: String,
    },
    Editor(EditorMsg),
    Settings(SettingsMsg),
    /// The Color schemes window.
    Schemes(SchemesMsg),
    /// Exporting a tab to a file.
    Export(crate::export::ExportMsg),
    Prompt(PromptMsg),
    Shoot,
    Shot(Screenshot),
    /// The OS close button (or equivalent) was used on a window; see [`App::close_requested`].
    CloseRequested(window::Id),
    /// A window was resized (or has just reported its size).
    WindowResized(window::Id, iced::Size),
    /// The OS scale factor of a window (asked when it opens, and on a change).
    WindowScale(window::Id, f32),
    /// A window's mode, asked after a resize (entering or leaving full screen resizes).
    WindowMode(window::Id, window::Mode),
    /// The quiet period after a resize passed; saves if no newer resize happened (its count).
    SaveState(u64),
    /// `profiles.toml`, `settings.toml` and/or a file in `schemes/` changed on disk.
    ConfigChanged(Changed),
}

/// Where a settings change came from. The scrollback notice is for edits made in
/// `settings.toml` only; a change confirmed in the Settings window already told the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsSource {
    /// `settings.toml` was edited on disk and reloaded.
    File,
    /// The Settings window applied it (for a lowered scrollback: after the user confirmed).
    Window,
}

/// Size used for sessions started before their window has laid out.
pub(crate) const DEFAULT_GRID: GridSize = GridSize {
    cols: 80,
    rows: 24,
    cell_width: 8.0,
    cell_height: 16.0,
};

/// Which window `FBTERM_SCREENSHOT` captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShotWindow {
    Terminal,
    Editor,
    Settings,
    Schemes,
}

pub struct App {
    pub settings: Settings,
    /// Every action's chord: the platform defaults with `settings.keys` applied.
    pub bindings: Bindings,
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
    pub(crate) visibility: visibility::Changes,
    /// Terminal windows that cannot be seen (see [`crate::visibility`]).
    pub(crate) occluded: std::collections::HashSet<window::Id>,
    pub(crate) display: display_rate::Changes,
    /// The refresh rate of the display each terminal window is on, where known.
    pub(crate) display_rates: HashMap<window::Id, Option<u32>>,
    pub windows: BTreeMap<window::Id, WindowState>,
    /// Each pane's grid as its view last laid it out.
    pub(crate) grids: HashMap<PaneId, GridSize>,
    /// The area each terminal window lays its panes out in, as last drawn.
    pub(crate) areas: HashMap<window::Id, iced::Size>,
    /// Session resizes waiting for their quiet period: generation and grid, by pane.
    pub(crate) pending_resizes: HashMap<PaneId, (u64, GridSize)>,
    /// Counts pane resizes, so only the last of a burst reaches the session.
    pub(crate) resize_generation: u64,
    /// The window that last had focus: where unscoped actions go.
    pub focused: Option<window::Id>,
    /// Open quick-launch palettes, by window.
    pub palettes: Palettes,
    /// The tab or tile being dragged, and in which window.
    pub(crate) carry: Option<(window::Id, crate::windows::Carry)>,
    /// The tab button a carried tab hovers: window, index and the hover's generation.
    pub(crate) hover: Option<(window::Id, usize, u64)>,
    /// Counts hovers, so only the latest one's timer switches tabs.
    pub(crate) hover_generation: u64,
    /// The find-in-all-tabs box, when open (in one window at a time).
    pub find_box: Option<FindAll>,
    /// The profile editor window, when open.
    pub editor: Option<(window::Id, EditorState)>,
    /// The Settings window, when open.
    pub settings_win: Option<(window::Id, SettingsState)>,
    /// The Color schemes window, when open.
    pub schemes_win: Option<(window::Id, SchemesState)>,
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
    /// Windows in full screen (terminal windows and the editor).
    pub fullscreen: std::collections::HashSet<window::Id>,
    /// Counts resizes, so only the last one of a burst saves.
    pub(crate) resize_count: u64,
    /// The text of `profiles.toml` / `settings.toml` as last loaded or written by us: a change
    /// event for exactly this text is our own write (or a touch), not an edit.
    pub(crate) profiles_seen: Option<String>,
    pub(crate) settings_seen: Option<String>,
    /// The banner line for a `settings.toml` that failed to load.
    pub(crate) settings_notice: Option<String>,
    /// Every color scheme: the built-ins and the files in `schemes/`.
    pub schemes: fbterm_config::schemes::SchemeSet,
    /// The scheme files that did not load, as banner lines.
    pub(crate) scheme_errors: Vec<String>,
    /// The banner lines about schemes now in `notices` (see `App::refresh_scheme_notices`).
    pub(crate) scheme_notices: Vec<String>,
    /// Terminals of tabs the user closed that a session task still holds; the release is
    /// requested when `SessionEvent::Released` shows the last holder let go (`windows.rs`).
    pub(crate) pending_release: Vec<Vec<crate::memory::WeakTerminal>>,
    /// Whether one of our windows has focus at all (the app is in front).
    pub app_active: bool,
    pub style: TerminalStyle,
    pub theme: Theme,
    pub tokens: &'static theme::Tokens,
    pub(crate) screenshot: Option<PathBuf>,
    /// `FBTERM_SCREENSHOT_WINDOW=editor|settings|schemes` (debug aid): capture that window instead.
    pub(crate) screenshot_window: ShotWindow,
    pub(crate) shooting: bool,
    /// `FBTERM_DEMO_TOOLTIP=new|profiles|close` (debug aid): that tab-bar button's tooltip always
    /// shows.
    pub(crate) demo_tip: Option<crate::tooltip::DemoTip>,
    /// `FBTERM_SCALE` (debug aid): extra UI scale on top of the OS's, e.g. 0.625 on a 2x
    /// screen for a fractional 1.25 physical scale. 1.0 otherwise.
    pub(crate) ui_scale: f32,
    /// The OS scale factor of each window, where known.
    pub(crate) os_scales: HashMap<window::Id, f32>,
    /// The window whose tab export is under way (its Save dialog open or its file being
    /// written).
    pub(crate) exporting: Option<window::Id>,
    /// The last export's outcome, shown in the window it ran in until dismissed.
    pub(crate) export_notice: Option<crate::export::ExportNotice>,
}

pub(crate) fn unix_now() -> i64 {
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

/// Ask for `id`'s mode now and once the full-screen animation is over (the last resize can
/// arrive before the OS reports the window as full screen).
fn query_mode(id: window::Id) -> Task<Message> {
    let now = window::mode(id).map(move |m| Message::WindowMode(id, m));
    let settled = Task::perform(
        async { tokio::time::sleep(Duration::from_millis(800)).await },
        |()| (),
    )
    .then(move |()| window::mode(id))
    .map(move |m| Message::WindowMode(id, m));
    Task::batch([now, settled])
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
        let (schemes, scheme_errors) = crate::schemes::load(&paths.schemes());
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

        let style = crate::settings_window::terminal_style(&settings);
        let (events, rx) = unbounded_channel();
        let mut app = Self {
            bindings: Bindings::current(&settings.keys.map),
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
            receiver: EventReceiver::new(rx),
            visibility: visibility::Changes::new(),
            occluded: Default::default(),
            display: display_rate::Changes::new(),
            display_rates: HashMap::new(),
            windows: BTreeMap::new(),
            grids: HashMap::new(),
            areas: HashMap::new(),
            pending_resizes: HashMap::new(),
            resize_generation: 0,
            focused: None,
            palettes: Palettes::new(),
            carry: None,
            hover: None,
            hover_generation: 0,
            find_box: None,
            editor: None,
            settings_win: None,
            schemes_win: None,
            prompts: PromptQueues::default(),
            prompt_tx,
            prompt_rx: PromptReceiver(Arc::new(Mutex::new(Some(prompt_rx)))),
            recency: Recency::from(saved.recency),
            state_saving_enabled: debug_env("FBTERM_OPEN_TABS").is_none()
                && debug_env("FBTERM_SPLITS").is_none()
                && debug_env("FBTERM_SCREENSHOT").is_none()
                && crate::perf::opts().is_none(),
            quitting: false,
            closing: Default::default(),
            sizes: HashMap::new(),
            fullscreen: Default::default(),
            resize_count: 0,
            profiles_seen,
            settings_seen,
            settings_notice,
            schemes,
            scheme_errors,
            scheme_notices: Vec::new(),
            pending_release: Vec::new(),
            app_active: true,
            style,
            screenshot: debug_env("FBTERM_SCREENSHOT").map(PathBuf::from),
            screenshot_window: match debug_env("FBTERM_SCREENSHOT_WINDOW") {
                Some(w) if w == "editor" => ShotWindow::Editor,
                Some(w) if w == "settings" => ShotWindow::Settings,
                Some(w) if w == "schemes" => ShotWindow::Schemes,
                _ => ShotWindow::Terminal,
            },
            shooting: false,
            demo_tip: debug_env("FBTERM_DEMO_TOOLTIP")
                .and_then(|v| crate::tooltip::DemoTip::parse(&v.to_string_lossy())),
            exporting: None,
            export_notice: None,
            ui_scale: debug_env("FBTERM_SCALE")
                .and_then(|v| v.to_string_lossy().parse::<f32>().ok())
                .filter(|s| s.is_finite() && (0.25..=4.0).contains(s))
                .unwrap_or(1.0),
            os_scales: HashMap::new(),
        };
        app.update_output_rate();
        app.refresh_scheme_notices();
        let mut task = match crate::perf::opts() {
            Some(o) => app.open_perf_tabs(o),
            None => app.restore_windows(&plan),
        };
        task = task.chain(app.open_debug_tabs());
        task = task.chain(app.open_debug_splits());
        task = task.chain(app.open_debug_palette());
        task = task.chain(app.open_debug_editor());
        task = task.chain(app.open_debug_settings());
        task = task.chain(app.open_debug_schemes());
        task = task.chain(app.open_debug_prompt());
        task = task.chain(app.open_debug_find());
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
        if self.settings_window() == Some(id) {
            return "Settings · fbterm".into();
        }
        if self.schemes_window() == Some(id) {
            return "Color schemes · fbterm".into();
        }
        match self.windows.get(&id).and_then(WindowState::active_tab) {
            Some(tab) => format!("{} · fbterm", tab.title()),
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
                Event::Window(window::Event::Rescaled(s)) => Some(Message::WindowScale(id, s)),
                _ => None,
            }),
            window::close_events().map(Message::WindowClosed),
            window::close_requests().map(Message::CloseRequested),
            Subscription::run_with(self.prompt_rx.clone(), prompt_stream).map(Message::Prompt),
            Subscription::run_with(self.visibility.clone(), visibility::stream)
                .map(|(id, visible)| Message::WindowVisible(id, visible)),
            Subscription::run_with(self.display.clone(), display_rate::stream)
                .map(|(id, hz)| Message::DisplayRate(id, hz)),
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
        match self.screenshot_window {
            ShotWindow::Editor if self.editor.is_none() => return false,
            ShotWindow::Settings if self.settings_win.is_none() => return false,
            ShotWindow::Schemes if self.schemes_win.is_none() => return false,
            _ => {}
        }
        self.windows
            .values()
            .next()
            .and_then(WindowState::active_tab)
            .is_some_and(|t| t.state() != &TabState::NotConnected)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Session(id, event) => self.session_event(id, event),
            Message::Terminal(wid, pane, output) => self.terminal_output(wid, pane, output),
            Message::Tiles(wid, msg) => self.tiles_msg(wid, msg),
            Message::Drag(wid, msg) => self.drag_msg(wid, msg),
            Message::ResizeSettled(pane, generation) => {
                self.resize_settled(pane, generation);
                Task::none()
            }
            Message::NewTab => self.action(None, Action::NewTab),
            Message::CloseTab(i) => match self.focused_window() {
                Some(wid) => self.close_tab_asking(wid, i),
                None => Task::none(),
            },
            Message::PerfCloseOthers(wid) => self.perf_close_others(wid),
            Message::SelectTab(i) => self.with_focused(|w| w.select(i)),
            Message::NextTab => self.with_focused(WindowState::next),
            Message::PrevTab => self.with_focused(WindowState::prev),
            Message::OpenProfile { profile, window } => self.open_tab(window, profile),
            Message::Connected(pane, delivered, requested) => {
                if let Some(handle) = delivered.take() {
                    self.connected(pane, handle, requested);
                }
                Task::none()
            }
            Message::ConnectFailed(pane, reason) => {
                self.connect_failed(pane, reason);
                Task::none()
            }
            Message::HostKeyChanged {
                pane,
                host,
                port,
                reason,
            } => {
                let task = self.host_key_changed(pane, host, port);
                self.connect_failed(pane, reason);
                task
            }
            Message::NewWindow => self.action(None, Action::NewWindow),
            Message::WindowOpened(id) => {
                self.focused.get_or_insert(id);
                if self.windows.contains_key(&id) {
                    Task::batch([
                        visibility::watch(id, &self.visibility),
                        display_rate::watch(id, &self.display),
                        window::scale_factor(id).map(move |s| Message::WindowScale(id, s)),
                    ])
                } else {
                    Task::none()
                }
            }
            Message::WindowVisible(id, visible) => {
                if visible {
                    // Take the redraw the active tab left pending while the window was unseen.
                    if self.occluded.remove(&id) {
                        return self.shown(id);
                    }
                } else if self.windows.contains_key(&id) {
                    self.occluded.insert(id);
                }
                Task::none()
            }
            Message::DisplayRate(id, hz) => {
                if self.windows.contains_key(&id) {
                    let hz = display_rate::merge(self.display_rates.get(&id).copied(), hz);
                    self.display_rates.insert(id, hz);
                    self.update_output_rate();
                }
                Task::none()
            }
            Message::WindowFocused(id) => {
                self.focused = Some(id);
                self.app_active = true;
                let detect = if self.windows.contains_key(&id) {
                    display_rate::detect(id, &self.display)
                } else {
                    Task::none()
                };
                Task::batch([self.shown(id), detect])
            }
            Message::WindowUnfocused(id) => {
                // Recording a chord ends when the Settings window loses focus: the keys go
                // elsewhere then.
                if let Some((sid, state)) = &mut self.settings_win
                    && *sid == id
                {
                    state.cancel_recording();
                }
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
                    TabBarMsg::Close(i) => self.close_tab_asking(wid, i),
                    TabBarMsg::New => self.action(Some(wid), Action::NewTab),
                    TabBarMsg::Profiles => self.action(Some(wid), Action::EditProfiles),
                    TabBarMsg::Settings => self.action(Some(wid), Action::Settings),
                }
            }
            Message::TitleBar(wid, action) => chrome::perform(wid, action),
            Message::WindowClosed(id) => {
                if self.editor_window() == Some(id) {
                    self.editor = None;
                }
                if self.settings_window() == Some(id) {
                    // Closed by the OS button or Quit: what is still waiting is written.
                    self.write_pending_settings();
                    self.settings_win = None;
                }
                if self.schemes_window() == Some(id) {
                    self.schemes_win = None;
                }
                // Quit and close rules: see `App::quit`. A close nobody asked for is the OS
                // ending the app; state.toml is current (saved on every change), so just
                // stop saving.
                if !self.closing.remove(&id) {
                    self.quitting = true;
                }
                // Dropping the window's tabs closes their sessions and abandons their dials;
                // its dialogs answer "no".
                let freed = self
                    .windows
                    .get(&id)
                    .map(|w| crate::memory::weak_terminals(w.tabs.iter().flat_map(|t| &t.panes)))
                    .unwrap_or_default();
                let gone: Vec<PaneId> = self
                    .windows
                    .get(&id)
                    .map(|w| w.tabs.iter().flat_map(|t| &t.panes).map(|p| p.id).collect())
                    .unwrap_or_default();
                self.forget_panes(&gone);
                let was_terminal = self.windows.remove(&id).is_some();
                visibility::unwatch(id);
                display_rate::unwatch(id);
                if self.display_rates.remove(&id).is_some() {
                    self.update_output_rate();
                }
                self.occluded.remove(&id);
                self.sizes.remove(&id);
                self.os_scales.remove(&id);
                self.fullscreen.remove(&id);
                self.palettes.remove(&id);
                if self.carry.is_some_and(|(w, _)| w == id) {
                    self.carry = None;
                    self.hover = None;
                }
                if self.find_box.as_ref().is_some_and(|f| f.window == id) {
                    self.close_find_all();
                }
                self.prompts.remove_window(id);
                self.areas.remove(&id);
                if self.export_notice.as_ref().is_some_and(|n| n.window == id) {
                    self.export_notice = None;
                }
                // A dialog that never answers (its window went away) must not block exports;
                // a write already running finishes and reports nowhere.
                if self.exporting == Some(id) {
                    self.exporting = None;
                }
                if self.focused == Some(id) {
                    self.focused = self.windows.keys().next().copied();
                }
                // A window the user closed is dropped from the layout (no-op while quitting).
                if was_terminal {
                    self.save_state();
                }
                if self.windows.is_empty()
                    && self.editor.is_none()
                    && self.settings_win.is_none()
                    && self.schemes_win.is_none()
                {
                    iced::exit()
                } else if self.quitting {
                    Task::none()
                } else {
                    self.release_closed(freed);
                    Task::none()
                }
            }
            Message::Shortcut(wid, action) => self.action(Some(wid), action),
            Message::DismissNotices => {
                self.notices_dismissed = true;
                Task::none()
            }
            Message::Palette(wid, msg) => self.palette(wid, msg),
            Message::Find(wid, msg) => self.find(wid, msg),
            Message::Found { pane, seq, outcome } => self.found(pane, seq, outcome),
            Message::FindRefresh(pane) => self.find_refresh(pane),
            Message::FindAll(wid, msg) => self.find_all(wid, msg),
            Message::FoundAll { seq, pane, outcome } => {
                self.found_all(seq, pane, outcome);
                Task::none()
            }
            Message::DebugFind { all, query } => self.debug_find(all, query),
            Message::Editor(msg) => self.editor_msg(msg),
            Message::Export(msg) => self.export_msg(msg),
            Message::Settings(msg) => self.settings_msg(msg),
            Message::Schemes(msg) => self.schemes_msg(msg),
            Message::Prompt(msg) => self.prompt_msg(msg),
            Message::WindowScale(id, s) => {
                if self.windows.contains_key(&id) && s.is_finite() && s > 0.0 {
                    self.os_scales.insert(id, s);
                }
                Task::none()
            }
            Message::WindowResized(id, size) => {
                Task::batch([self.window_resized(id, size), query_mode(id)])
            }
            Message::WindowMode(id, mode) => {
                if mode == window::Mode::Fullscreen {
                    self.fullscreen.insert(id);
                } else {
                    self.fullscreen.remove(&id);
                }
                Task::none()
            }
            Message::SaveState(n) => {
                if n == self.resize_count {
                    self.save_state();
                }
                Task::none()
            }
            Message::ConfigChanged(changed) => {
                let mut tasks = Vec::new();
                if changed.schemes {
                    tasks.push(self.reload_schemes());
                }
                if changed.profiles {
                    self.profiles_file_changed();
                }
                if changed.settings {
                    tasks.push(self.settings_file_changed());
                }
                Task::batch(tasks)
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

    fn connect_failed(&mut self, pane: PaneId, reason: String) {
        if let Some((_, t)) = self.pane_mut(pane) {
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

    /// Physical pixels per logical pixel in window `id` (the OS's scale times `ui_scale`).
    pub(crate) fn physical_scale(&self, id: window::Id) -> f32 {
        self.os_scales.get(&id).copied().unwrap_or(1.0) * self.ui_scale
    }

    pub(crate) fn focused_window(&self) -> Option<window::Id> {
        self.focused
            .filter(|id| self.windows.contains_key(id))
            .or_else(|| self.windows.keys().next().copied())
    }

    /// Change tab selection in the focused window.
    pub(crate) fn with_focused(
        &mut self,
        f: impl FnOnce(&mut WindowState) -> bool,
    ) -> Task<Message> {
        match self.focused_window() {
            Some(wid) => self.with_window(wid, f),
            None => Task::none(),
        }
    }

    /// Change tab selection in `wid`; the newly shown tab gets [`App::shown`].
    pub(crate) fn with_window(
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

    /// The pane `id`, wherever it is, with its window.
    pub(crate) fn pane_mut(&mut self, id: PaneId) -> Option<(window::Id, &mut Pane)> {
        self.windows.iter_mut().find_map(|(wid, w)| {
            let (t, p) = w.locate(id)?;
            Some((*wid, &mut w.tabs[t].panes[p]))
        })
    }

    fn shoot(&mut self) -> Task<Message> {
        if self.shooting {
            return Task::none();
        }
        let target = match self.screenshot_window {
            ShotWindow::Editor => self.editor_window(),
            ShotWindow::Settings => self.settings_window(),
            ShotWindow::Schemes => self.schemes_window(),
            ShotWindow::Terminal => self.windows.keys().next().copied(),
        };
        let Some(wid) = target else {
            return Task::none();
        };
        self.shooting = true;
        window::screenshot(wid).map(Message::Shot)
    }
}
