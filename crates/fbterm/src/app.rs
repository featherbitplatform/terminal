//! Application state and the update loop: windows of tabs, each tab one session.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fbterm_config::{Paths, ProfileId, ProfileKind, ProfileStore, Settings, discover_shells};
use fbterm_secrets::{FallbackStore, MemoryStore, SecretStore};
use fbterm_session::ssh::{SshContext, SshPool};
use fbterm_session::{EventSink, SessionEvent, SessionHandle, SessionId};
use fbterm_term::{ColorScheme, GridSize, TerminalOutput, TerminalStyle};
use iced::window::{self, Screenshot};
use iced::{Event, Font, Subscription, Task, Theme};
use parking_lot::Mutex;
use tokio::sync::mpsc::unbounded_channel;

use crate::keys::Action;
use crate::palette::{self, PaletteItem, PaletteMsg, PaletteState, Palettes, Recency};
use crate::sessions::{EventReceiver, prompter, session_stream, write_notice};
use crate::tabs::{self, Tab, TabId, TabState, WindowState};
use crate::theme;

/// Profile editor messages. Placeholder until the editor exists.
#[allow(
    dead_code,
    reason = "variants used by the palette, editor and prompt tasks"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorMsg {
    Open,
    Close,
}

/// Connection prompt messages. Placeholder until prompts exist.
#[allow(
    dead_code,
    reason = "variants used by the palette, editor and prompt tasks"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptMsg {
    Dismiss,
}

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
    NewWindow,
    WindowOpened(window::Id),
    WindowFocused(window::Id),
    WindowUnfocused(window::Id),
    WindowClosed(window::Id),
    /// A click in a window's tab bar; acts on that window whatever has focus.
    TabBar(window::Id, TabBarMsg),
    Shortcut(window::Id, Action),
    DismissNotices,
    /// Palette input for one window.
    Palette(window::Id, PaletteMsg),
    Editor(EditorMsg),
    Prompt(PromptMsg),
    Shoot,
    Shot(Screenshot),
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
    pub profiles_load_failed: bool,
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
    /// When each profile was last opened (in memory until state persistence).
    pub recency: Recency,
    /// Whether one of our windows has focus at all (the app is in front).
    pub app_active: bool,
    pub style: TerminalStyle,
    pub style_unfocused: TerminalStyle,
    pub theme: Theme,
    pub tokens: &'static theme::Tokens,
    pub(crate) screenshot: Option<PathBuf>,
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
        _ => Paths::from_env(),
    }
}

/// Load profiles without ever writing the file. A missing file is seeded with the discovered
/// shells; a malformed one is reported and replaced in memory by them.
/// Returns the store and whether the file failed to load.
fn load_profiles(paths: &Paths, notices: &mut Vec<String>) -> (ProfileStore, bool) {
    let path = paths.profiles();
    let mut failed = false;
    let mut store = if path.exists() {
        match ProfileStore::load(&path) {
            Ok(store) => store,
            Err(e) => {
                notices.push(e.to_string());
                failed = true;
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

/// A debug-only environment variable (`FBTERM_SCREENSHOT`, `FBTERM_OPEN_TABS`); release builds
/// ignore them.
fn debug_env(name: &str) -> Option<std::ffi::OsString> {
    if cfg!(debug_assertions) {
        std::env::var_os(name).filter(|v| !v.is_empty())
    } else {
        None
    }
}

impl App {
    pub fn boot() -> (Self, Task<Message>) {
        let paths = paths();
        let mut notices = Vec::new();
        let settings = Settings::load(&paths.settings()).unwrap_or_else(|e| {
            notices.push(e.to_string());
            Settings::default()
        });
        let (profiles, profiles_load_failed) = load_profiles(&paths, &mut notices);

        // FBTERM_SECRETS=memory (debug aid) keeps secrets in memory and never touches the keychain.
        let (secrets, fallback): (Arc<dyn SecretStore>, _) =
            if std::env::var("FBTERM_SECRETS").as_deref() == Ok("memory") {
                (Arc::new(MemoryStore::new()), None)
            } else {
                let store = Arc::new(FallbackStore::new());
                (store.clone(), Some(store))
            };
        let known_hosts = directories::BaseDirs::new()
            .map(|d| d.home_dir().join(".ssh").join("known_hosts"))
            .unwrap_or_else(|| PathBuf::from("known_hosts"));
        let pool = Arc::new(SshPool::new(SshContext {
            known_hosts,
            secrets,
            prompter: prompter(),
        }));

        let family: &'static str = Box::leak(settings.font_family.clone().into_boxed_str());
        let style = TerminalStyle {
            scheme: ColorScheme::graphite(),
            font: Font::with_name(family),
            font_size: settings.font_size,
            accent: None,
            focused: true,
        };
        let (events, rx) = unbounded_channel();
        let mut app = Self {
            theme: theme::graphite_theme(settings.theme),
            tokens: theme::tokens(settings.theme),
            settings,
            profiles,
            notices,
            notices_dismissed: false,
            profiles_load_failed,
            fallback,
            pool,
            events,
            receiver: EventReceiver(Arc::new(Mutex::new(Some(rx)))),
            windows: BTreeMap::new(),
            grids: HashMap::new(),
            focused: None,
            palettes: Palettes::new(),
            recency: Recency::new(),
            app_active: true,
            style_unfocused: TerminalStyle {
                focused: false,
                ..style.clone()
            },
            style,
            screenshot: debug_env("FBTERM_SCREENSHOT").map(PathBuf::from),
            shooting: false,
        };
        let mut task = app.open_window(app.default_profile());
        task = task.chain(app.open_debug_tabs());
        task = task.chain(app.open_debug_palette());
        (app, task)
    }

    /// `FBTERM_OPEN_TABS=name,name` (debug aid until restore exists): extra tabs opened in the
    /// first window at startup, by profile name.
    fn open_debug_tabs(&mut self) -> Task<Message> {
        let Some(list) = debug_env("FBTERM_OPEN_TABS") else {
            return Task::none();
        };
        let list = list.to_string_lossy().into_owned();
        let Some(&wid) = self.windows.keys().next() else {
            return Task::none();
        };
        let ids: Vec<ProfileId> = list
            .split(',')
            .filter_map(|n| self.profiles.profiles.iter().find(|p| p.name == n.trim()))
            .map(|p| p.id)
            .collect();
        let tasks: Vec<_> = ids.into_iter().map(|id| self.open_tab(wid, id)).collect();
        // They open in the background; the first tab stays active.
        if let Some(w) = self.windows.get_mut(&wid) {
            w.active = 0;
        }
        Task::batch(tasks)
    }

    /// `FBTERM_PALETTE_QUERY=text` (debug aid): the palette open and pre-filled in the first window.
    fn open_debug_palette(&mut self) -> Task<Message> {
        let Some(query) = debug_env("FBTERM_PALETTE_QUERY") else {
            return Task::none();
        };
        match self.windows.keys().next().copied() {
            Some(wid) => self.open_palette(wid, query.to_string_lossy().into_owned()),
            None => Task::none(),
        }
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
        if self.profiles_load_failed {
            out.push("Profiles are read-only until profiles.toml is fixed".into());
        }
        if self.fallback.as_ref().is_some_and(|f| f.degraded()) {
            out.push("Keychain unavailable: passwords are kept for this session only".into());
        }
        out
    }

    pub fn title(&self, id: window::Id) -> String {
        match self.windows.get(&id).and_then(WindowState::active_tab) {
            Some(tab) => format!("{} · fbterm", tab.title),
            None => "fbterm".into(),
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
        ];
        if self.screenshot.is_some() && !self.shooting && self.first_window_ready() {
            // Give the renderer time to load fonts and present a few frames.
            subs.push(iced::time::every(Duration::from_millis(1500)).map(|_| Message::Shoot));
        }
        Subscription::batch(subs)
    }

    fn first_window_ready(&self) -> bool {
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
                Task::none()
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
            Message::WindowClosed(id) => {
                // Dropping the window's tabs closes their sessions and abandons their dials.
                self.windows.remove(&id);
                self.palettes.remove(&id);
                self.grids.remove(&id);
                if self.focused == Some(id) {
                    self.focused = self.windows.keys().next().copied();
                }
                if self.windows.is_empty() {
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
            Message::Editor(msg) => {
                tracing::info!(?msg, "profile editor not available yet");
                Task::none()
            }
            Message::Prompt(msg) => {
                tracing::info!(?msg, "prompts not available yet");
                Task::none()
            }
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

    fn focused_window(&self) -> Option<window::Id> {
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
            Action::Settings | Action::EditProfiles => {
                self.update(Message::Editor(EditorMsg::Open))
            }
        }
    }

    /// Open `wid`'s palette (or reset it) with `query`, and focus its input.
    fn open_palette(&mut self, wid: window::Id, query: String) -> Task<Message> {
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

    pub fn open_window(&mut self, profile: Option<ProfileId>) -> Task<Message> {
        let (id, open) = window::open(window::Settings {
            size: iced::Size::new(1000.0, 640.0),
            min_size: Some(iced::Size::new(360.0, 200.0)),
            ..window::Settings::default()
        });
        self.windows.insert(id, WindowState::new());
        let start = match profile {
            Some(p) => self.open_tab(id, p),
            None => Task::none(),
        };
        open.map(Message::WindowOpened).chain(start)
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
        if closed.window_empty {
            window::close(wid)
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

    pub(crate) fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.windows
            .values_mut()
            .find_map(|w| w.find(id).map(|i| &mut w.tabs[i]))
    }

    fn shoot(&mut self) -> Task<Message> {
        if self.shooting {
            return Task::none();
        }
        let Some(&wid) = self.windows.keys().next() else {
            return Task::none();
        };
        self.shooting = true;
        window::screenshot(wid).map(Message::Shot)
    }
}
