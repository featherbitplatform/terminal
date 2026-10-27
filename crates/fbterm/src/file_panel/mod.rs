//! The file panel: a browser of the focused pane's server, docked at the active tab's right
//! edge (`window::dock`), outside the tiles.
//!
//! - **Toggling.** [`Action::FilePanel`](crate::keys::Action) (shortcut, palette `File panel`)
//!   shows or hides the panel of one terminal window; [`FilePanels::open`] lists the windows
//!   that show it. Only the user's shortcut, palette pick or the panel's own close button
//!   change it: nothing a remote program sends can.
//! - **Following the focused pane.** The panel shows the profile of its window's focused pane
//!   in the active tab. A local pane shows `Local session`, an SSH pane that is not connected
//!   `Not connected`. [`App::sync_file_panels`] runs after every message, so a focus change, a
//!   tab switch or a connect is picked up wherever it came from.
//! - **Sessions.** For a connected SSH pane the panel opens SFTP on the profile's shared
//!   connection ([`Sftp::open`]), asking any question over the panel's window
//!   ([`UiPrompter`]). The session holds the connection open. It is kept while a panel shows
//!   the profile, while a panel is open anywhere and the profile still has a connected shell
//!   (so moving focus back and forth does not reopen it), and while its transfers run; else it
//!   is let go (an open still under way is aborted, so it can raise no question after the
//!   panel is gone). Holding the channel while a panel is open elsewhere adds no connection
//!   lifetime: the profile's connected shell holds the connection anyway. A server that
//!   refuses SFTP shows `SFTP is not available: {reason}`; there is no fallback.
//! - **Folders.** Each profile's folder is kept for the app session ([`ProfilePanel`]); it
//!   starts as the server's answer for `.`. Answers that come back after their profile moved
//!   on (a newer open or read, a released session) are dropped by their token.
//! - **Width.** One width for every window, dragged at the panel's left edge, clamped to
//!   [`state::MIN_WIDTH`]..=[`state::MAX_WIDTH`] and saved in `state.toml` when the drag ends.

pub(crate) mod state;
#[cfg(test)]
mod tests;

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use fbterm_config::{ProfileId, ProfileKind, ProfileStore};
use fbterm_session::ssh::sftp::{Entry, Sftp, SftpError, SortKey, sort_entries};
use fbterm_session::ssh::{Prompter, SshError, SshPool};
use iced::futures::Stream;
use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Element, Length, Padding, Task, window};
use parking_lot::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::app::{App, Delivered, Message};
use crate::prompts::UiPrompter;
use crate::tabs::{TabState, WindowState};
use crate::theme::{Tokens, UI_FONT};
use crate::ui::{ICON_X, MONO, icon};
pub(crate) use state::{FilePanels, Listing, ProfilePanel};

/// An open SFTP session as the panel uses it: the real one, or a fake in tests.
#[async_trait::async_trait]
pub(crate) trait Remote: Send + Sync {
    async fn list(&self, path: &str) -> Result<Vec<Entry>, SftpError>;
    /// The session itself, for transfers (`None` for a test fake).
    #[allow(dead_code, reason = "downloads and uploads run on the session")]
    fn sftp(&self) -> Option<Arc<Sftp>>;
}

/// The real session.
struct Live(Arc<Sftp>);

#[async_trait::async_trait]
impl Remote for Live {
    async fn list(&self, path: &str) -> Result<Vec<Entry>, SftpError> {
        self.0.list(path).await
    }

    fn sftp(&self) -> Option<Arc<Sftp>> {
        Some(self.0.clone())
    }
}

/// What opening a profile's SFTP session takes.
pub(crate) struct OpenRequest {
    pub profile: ProfileId,
    pub token: u64,
    pub store: ProfileStore,
    /// Asks over [`OpenRequest::window`], the window whose panel needs the session.
    pub prompter: Arc<dyn Prompter>,
    #[cfg_attr(not(test), allow(dead_code, reason = "the prompter asks over it"))]
    pub window: window::Id,
}

/// What reading a folder takes.
pub(crate) struct ListRequest {
    pub profile: ProfileId,
    pub token: u64,
    pub folder: String,
    pub remote: Arc<dyn Remote>,
}

/// An opened session and its home folder (the server's answer for `.`).
pub(crate) struct Opened {
    pub remote: Arc<dyn Remote>,
    pub home: Result<String, String>,
}

/// What the panel's background work reports.
pub(crate) enum PanelEvent {
    Opened {
        profile: ProfileId,
        token: u64,
        /// The session, or why SFTP is not available.
        result: Result<Opened, String>,
    },
    Listed {
        profile: ProfileId,
        token: u64,
        folder: String,
        result: Result<Vec<Entry>, SftpError>,
    },
}

pub(crate) type PanelSink = UnboundedSender<PanelEvent>;

/// Opens sessions and reads folders: on the app's SSH pool, or a fake in tests.
pub(crate) trait Backend: Send + Sync {
    /// Start opening; the handle stops the open when the session is no longer wanted.
    fn open(&self, request: OpenRequest, sink: PanelSink) -> Option<tokio::task::AbortHandle>;
    fn list(&self, request: ListRequest, sink: PanelSink);
}

/// Runs the panel's work as tasks on the app's runtime, sessions on the app's SSH pool.
struct PoolBackend(Arc<SshPool>);

/// Why work could not start: the app always runs inside its runtime, so this is a bug, but the
/// panel must say so rather than show `Loading…` forever.
const NO_RUNTIME: &str = "the app's background runtime is not running";

/// The app's runtime, or a warning when there is none.
fn runtime() -> Option<tokio::runtime::Handle> {
    let rt = tokio::runtime::Handle::try_current().ok();
    if rt.is_none() {
        tracing::warn!("file panel: {NO_RUNTIME}");
    }
    rt
}

/// Why SFTP is not available, from the error opening it: a refused subsystem says so without
/// the generic prefix.
fn unavailable_reason(e: &SshError) -> String {
    match e {
        SshError::Protocol(reason) => reason.clone(),
        e => e.to_string(),
    }
}

impl Backend for PoolBackend {
    fn open(&self, r: OpenRequest, sink: PanelSink) -> Option<tokio::task::AbortHandle> {
        let Some(rt) = runtime() else {
            let _ = sink.send(PanelEvent::Opened {
                profile: r.profile,
                token: r.token,
                result: Err(NO_RUNTIME.into()),
            });
            return None;
        };
        let pool = self.0.clone();
        let task = rt.spawn(async move {
            let result = match Sftp::open(&pool, &r.store, &r.profile, r.prompter).await {
                Ok(sftp) => {
                    let sftp = Arc::new(sftp);
                    let home = sftp.home().await.map_err(|e| e.to_string());
                    Ok(Opened {
                        remote: Arc::new(Live(sftp)),
                        home,
                    })
                }
                Err(e) => Err(unavailable_reason(&e)),
            };
            let _ = sink.send(PanelEvent::Opened {
                profile: r.profile,
                token: r.token,
                result,
            });
        });
        Some(task.abort_handle())
    }

    fn list(&self, r: ListRequest, sink: PanelSink) {
        let Some(rt) = runtime() else {
            let _ = sink.send(PanelEvent::Listed {
                profile: r.profile,
                token: r.token,
                folder: r.folder,
                result: Err(SftpError::Other(NO_RUNTIME.into())),
            });
            return;
        };
        rt.spawn(async move {
            let result = r.remote.list(&r.folder).await;
            let _ = sink.send(PanelEvent::Listed {
                profile: r.profile,
                token: r.token,
                folder: r.folder,
                result,
            });
        });
    }
}

/// `FBTERM_DEMO_FILES` (debug aid): every session opens at once in `home` and every folder
/// lists `entries`; nothing touches the network.
pub(crate) struct Demo {
    pub home: String,
    pub entries: Vec<Entry>,
}

impl Demo {
    /// `<folder>:<name>,<name>/,...` (names ending in `/` are folders).
    pub(crate) fn parse(spec: &str) -> Self {
        let (home, names) = spec.split_once(':').unwrap_or(("/", spec));
        let entries = names
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(|n| {
                let (name, kind) = match n.strip_suffix('/') {
                    Some(name) => (name, fbterm_session::ssh::sftp::EntryKind::Dir),
                    None => (n, fbterm_session::ssh::sftp::EntryKind::File),
                };
                Entry {
                    name: name.to_owned(),
                    kind,
                    size: 0,
                    modified: None,
                    mode: None,
                }
            })
            .collect();
        Self {
            home: home.to_owned(),
            entries,
        }
    }
}

struct DemoRemote(Vec<Entry>);

#[async_trait::async_trait]
impl Remote for DemoRemote {
    async fn list(&self, _path: &str) -> Result<Vec<Entry>, SftpError> {
        Ok(self.0.clone())
    }

    fn sftp(&self) -> Option<Arc<Sftp>> {
        None
    }
}

impl Backend for Demo {
    fn open(&self, r: OpenRequest, sink: PanelSink) -> Option<tokio::task::AbortHandle> {
        let _ = sink.send(PanelEvent::Opened {
            profile: r.profile,
            token: r.token,
            result: Ok(Opened {
                remote: Arc::new(DemoRemote(self.entries.clone())),
                home: Ok(self.home.clone()),
            }),
        });
        None
    }

    fn list(&self, r: ListRequest, sink: PanelSink) {
        let _ = sink.send(PanelEvent::Listed {
            profile: r.profile,
            token: r.token,
            folder: r.folder,
            result: Ok(self.entries.clone()),
        });
    }
}

/// The panel's events, delivered to the update loop by one subscription.
#[derive(Clone)]
pub(crate) struct PanelReceiver(Arc<Mutex<Option<UnboundedReceiver<PanelEvent>>>>);

impl Hash for PanelReceiver {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "fbterm-file-panel-events".hash(state);
    }
}

pub(crate) fn panel_stream(rx: &PanelReceiver) -> impl Stream<Item = PanelEvent> + use<> {
    let rx = rx.0.lock().take();
    iced::futures::stream::unfold(rx, |rx| async move {
        let mut rx = rx?;
        let event = rx.recv().await?;
        Some((event, Some(rx)))
    })
}

/// What the panel asks of the app.
#[derive(Debug, Clone)]
pub enum FilePanelMsg {
    Event(Delivered<PanelEvent>),
    /// The left edge is being dragged: the panel's new width.
    Resize(f32),
    /// The drag ended: remember the width.
    ResizeEnd,
    /// The panel's close button in this window.
    Close(window::Id),
}

/// The panel's background work and where it reports.
pub(crate) struct Work {
    pub(crate) backend: Arc<dyn Backend>,
    pub(crate) sink: PanelSink,
    pub(crate) receiver: PanelReceiver,
}

impl Work {
    pub(crate) fn new(pool: Arc<SshPool>) -> Self {
        let (sink, rx) = unbounded_channel();
        Self {
            backend: Arc::new(PoolBackend(pool)),
            sink,
            receiver: PanelReceiver(Arc::new(Mutex::new(Some(rx)))),
        }
    }
}

/// Whose files a window's panel shows: its focused pane's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    Local,
    NotConnected,
    Remote(ProfileId),
}

/// What the panel of a window shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Body<'a> {
    /// A line instead of a listing (`Not connected`, `Local session`, an error).
    Note(String),
    Loading,
    Entries(&'a [Entry]),
}

/// A remote name as plain text: control characters (escape sequences, new lines) become `?`.
pub(crate) fn plain_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

impl App {
    /// Whose files `wid`'s panel shows; `None` without an active tab.
    fn panel_shows(&self, wid: window::Id) -> Option<Shown> {
        let pane = self
            .windows
            .get(&wid)
            .and_then(WindowState::active_tab)?
            .focused_pane();
        Some(match self.profiles.get(pane.profile).map(|p| &p.kind) {
            Some(ProfileKind::Local(_)) => Shown::Local,
            Some(ProfileKind::Ssh(_))
                if pane.state == TabState::Connected || self.file_panels.demo =>
            {
                Shown::Remote(pane.profile)
            }
            _ => Shown::NotConnected,
        })
    }

    /// Show or hide `wid`'s panel. Showing it again after SFTP failed tries again.
    pub(crate) fn toggle_file_panel(&mut self, wid: window::Id) -> Task<Message> {
        if !self.windows.contains_key(&wid) {
            return Task::none();
        }
        if !self.file_panels.open.remove(&wid) {
            self.file_panels.open.insert(wid);
            if let Some(Shown::Remote(p)) = self.panel_shows(wid)
                && let Some(panel) = self.file_panels.by_profile.get_mut(&p)
                && panel.sftp.is_none()
                && matches!(panel.listing, Listing::Failed(_) | Listing::Unavailable(_))
            {
                panel.release();
            }
        }
        Task::none()
    }

    pub(crate) fn file_panel_msg(&mut self, msg: FilePanelMsg) -> Task<Message> {
        match msg {
            FilePanelMsg::Event(event) => {
                if let Some(event) = event.take() {
                    self.file_panel_event(event);
                }
            }
            FilePanelMsg::Resize(width) => self.file_panels.width = state::clamp_width(width),
            FilePanelMsg::ResizeEnd => self.save_state(),
            FilePanelMsg::Close(wid) => {
                self.file_panels.open.remove(&wid);
            }
        }
        Task::none()
    }

    fn file_panel_event(&mut self, event: PanelEvent) {
        match event {
            PanelEvent::Opened {
                profile,
                token,
                result,
            } => {
                let Some(panel) = self.file_panels.by_profile.get_mut(&profile) else {
                    return;
                };
                if panel.token != token || panel.listing != Listing::Opening {
                    return;
                }
                panel.opening = None;
                match result {
                    Err(reason) => panel.listing = Listing::Unavailable(reason),
                    Ok(Opened { remote, home }) => {
                        panel.sftp = Some(remote);
                        if panel.folder.is_empty() {
                            match home {
                                Ok(home) => panel.folder = home,
                                Err(e) => {
                                    panel.listing = Listing::Failed(e);
                                    return;
                                }
                            }
                        }
                        self.read_folder(profile);
                    }
                }
            }
            PanelEvent::Listed {
                profile,
                token,
                folder,
                result,
            } => {
                let Some(panel) = self.file_panels.by_profile.get_mut(&profile) else {
                    return;
                };
                if panel.token != token || panel.folder != folder {
                    return;
                }
                panel.listing = match result {
                    Ok(mut entries) => {
                        sort_entries(&mut entries, SortKey::Name, false);
                        Listing::Loaded(entries)
                    }
                    Err(e) => {
                        // The session is gone: showing the panel again opens a new one.
                        if matches!(e, SftpError::Unavailable(_)) {
                            panel.sftp = None;
                        }
                        Listing::Failed(e.to_string())
                    }
                };
            }
        }
    }

    /// Read `profile`'s folder on its session.
    fn read_folder(&mut self, profile: ProfileId) {
        let Some(panel) = self.file_panels.by_profile.get_mut(&profile) else {
            return;
        };
        let Some(remote) = panel.sftp.clone() else {
            return;
        };
        panel.token += 1;
        panel.listing = Listing::Loading;
        self.file_panel_work.backend.list(
            ListRequest {
                profile,
                token: panel.token,
                folder: panel.folder.clone(),
                remote,
            },
            self.file_panel_work.sink.clone(),
        );
    }

    /// Open `profile`'s session for the panel of `wid`, asking over that window.
    fn open_sftp(&mut self, profile: ProfileId, wid: window::Id) {
        let panel = self
            .file_panels
            .by_profile
            .entry(profile)
            .or_insert_with(ProfilePanel::new);
        if let Some(old) = panel.opening.take() {
            old.abort();
        }
        panel.token += 1;
        panel.listing = Listing::Opening;
        panel.opening = self.file_panel_work.backend.open(
            OpenRequest {
                profile,
                token: panel.token,
                store: self.profiles.clone(),
                prompter: Arc::new(UiPrompter::new(Some(wid), self.prompt_tx.clone())),
                window: wid,
            },
            self.file_panel_work.sink.clone(),
        );
    }

    /// Every open panel shows its focused pane's profile: sessions are opened for the
    /// profiles shown and let go for those no panel needs any more (see the module docs).
    pub(crate) fn sync_file_panels(&mut self) {
        let panels = &self.file_panels;
        let holding = |p: &ProfilePanel| p.sftp.is_some() || p.listing == Listing::Opening;
        if panels.open.is_empty() && !panels.by_profile.values().any(holding) {
            return;
        }
        // The profiles shown, each with the first window (in a stable order) showing it.
        let mut shown: Vec<(ProfileId, window::Id)> = Vec::new();
        for &wid in self.windows.keys() {
            if !panels.is_open(wid) {
                continue;
            }
            if let Some(Shown::Remote(p)) = self.panel_shows(wid)
                && !shown.iter().any(|(q, _)| *q == p)
            {
                shown.push((p, wid));
            }
        }
        for &(profile, wid) in &shown {
            let closed = self
                .file_panels
                .by_profile
                .get(&profile)
                .is_none_or(|p| p.listing == Listing::Closed);
            if closed {
                self.open_sftp(profile, wid);
            }
        }
        let any_open = !self.file_panels.open.is_empty();
        let release: Vec<ProfileId> = self
            .file_panels
            .by_profile
            .iter()
            .filter(|(id, p)| {
                holding(p)
                    && p.transfers.is_empty()
                    && !shown.iter().any(|(q, _)| q == *id)
                    && !(any_open && self.shell_count(**id) > 0)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in release {
            if let Some(p) = self.file_panels.by_profile.get_mut(&id) {
                p.release();
            }
        }
    }

    /// What `wid`'s panel shows; `None` while it is hidden or the window has no tab.
    pub(crate) fn file_panel_body(&self, wid: window::Id) -> Option<Body<'_>> {
        if !self.file_panels.is_open(wid) {
            return None;
        }
        Some(match self.panel_shows(wid)? {
            Shown::Local => Body::Note("Local session".into()),
            Shown::NotConnected => Body::Note("Not connected".into()),
            Shown::Remote(p) => match self.file_panels.by_profile.get(&p).map(|p| &p.listing) {
                None | Some(Listing::Closed | Listing::Opening | Listing::Loading) => Body::Loading,
                Some(Listing::Loaded(entries)) => Body::Entries(entries),
                Some(Listing::Failed(e)) => Body::Note(e.clone()),
                Some(Listing::Unavailable(reason)) => {
                    Body::Note(format!("SFTP is not available: {reason}"))
                }
            },
        })
    }

    /// The folder `wid`'s panel shows, when it shows one.
    fn file_panel_folder(&self, wid: window::Id) -> Option<&str> {
        match self.panel_shows(wid)? {
            Shown::Remote(p) => self
                .file_panels
                .by_profile
                .get(&p)
                .map(|p| p.folder.as_str())
                .filter(|f| !f.is_empty()),
            _ => None,
        }
    }
}

/// The most rows the panel builds at once (browsing large folders comes with the list view).
const MAX_ROWS: usize = 500;

/// `wid`'s panel and its width, when it is open.
pub(crate) fn view(app: &App, wid: window::Id) -> Option<(Element<'_, Message>, f32)> {
    let body = app.file_panel_body(wid)?;
    let t = app.tokens;
    let title = app
        .windows
        .get(&wid)
        .and_then(WindowState::active_tab)
        .map(|tab| tab.focused_pane().name.clone())
        .unwrap_or_default();
    let close = button(icon(ICON_X, 14.0, t.text_muted))
        .on_press(Message::FilePanel(FilePanelMsg::Close(wid)))
        .padding(4)
        .style(move |_, status| crate::ui::ghost(t, status));
    let mut heading = column![
        text(title)
            .font(UI_FONT)
            .size(12)
            .color(t.text_secondary)
            .wrapping(text::Wrapping::None)
    ]
    .spacing(2)
    .width(Length::Fill);
    if let Some(folder) = app.file_panel_folder(wid) {
        heading = heading.push(
            text(plain_name(folder))
                .font(MONO)
                .size(12)
                .color(t.text)
                .wrapping(text::Wrapping::None),
        );
    }
    let header = row![heading, close]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .padding(Padding::from([8.0, 12.0]));
    let content: Element<'_, Message> = match body {
        Body::Note(line) => note(t, line),
        Body::Loading => note(t, "Loading\u{2026}".into()),
        Body::Entries(entries) => entry_list(t, entries),
    };
    let rule = container(text(""))
        .width(1)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(t.border.into()),
            ..container::Style::default()
        });
    let opacity = app.settings.background_opacity;
    let surface = t.surface.scale_alpha(opacity);
    let panel = container(column![header, content])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(surface.into()),
            ..container::Style::default()
        });
    Some((row![rule, panel].into(), app.file_panels.width))
}

fn note<'a>(t: &Tokens, line: String) -> Element<'a, Message> {
    container(text(line).font(UI_FONT).size(13).color(t.text_muted))
        .padding(Padding::from([8.0, 12.0]))
        .width(Length::Fill)
        .into()
}

/// The folder's items, one per row, folders first (as sorted).
fn entry_list<'a>(t: &Tokens, entries: &'a [Entry]) -> Element<'a, Message> {
    let mut rows = column![].width(Length::Fill);
    for entry in entries.iter().take(MAX_ROWS) {
        let (name, color) = if entry.is_folder() {
            (format!("{}/", plain_name(&entry.name)), t.text)
        } else {
            (plain_name(&entry.name), t.text_secondary)
        };
        rows = rows.push(
            container(
                text(name)
                    .font(MONO)
                    .size(12)
                    .color(color)
                    .wrapping(text::Wrapping::None),
            )
            .padding(Padding::from([3.0, 12.0]))
            .width(Length::Fill),
        );
    }
    if entries.len() > MAX_ROWS {
        rows = rows.push(note(t, format!("{} more items", entries.len() - MAX_ROWS)));
    }
    scrollable(rows).height(Length::Fill).into()
}
