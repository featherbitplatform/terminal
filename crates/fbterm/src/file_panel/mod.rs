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
//! - **Browsing** (each profile's [`ProfilePanel`], drawn by [`view`]). Folders first, then
//!   files, by the clicked column (again reverses; `sort_entries`); a `..` row and Backspace
//!   go up, Enter or a double click opens a folder (or a link to one); one selected item. The
//!   path bar's segments go to their folder; a click on its empty end, or typing `/` or `~`,
//!   makes it a text field where Enter goes to the typed path. The filter box narrows the
//!   loaded folder by a case-insensitive part of the name; `Show hidden files` (on unless
//!   turned off, kept in `state.toml`) applies to every panel. Errors show inline with
//!   `Retry`, which reopens a session that is gone (asking over the window it was pressed
//!   in) or reads the folder again. Folders load in the background (`Loading…`), and only
//!   the rows on screen are built, so a folder of 100000 items stays responsive.
//! - **Keyboard.** A click in the panel gives it the keyboard; a click in the terminal,
//!   Escape, another pane or tab taking focus, or a panel with nothing to browse gives it
//!   back. While the panel has it, the terminal is drawn unfocused and the dock hands key
//!   presses to the panel only (`Mode::Panel` in `shortcuts.rs`), so a key is never both.

pub(crate) mod state;
#[cfg(test)]
mod tests;
pub(crate) mod view;

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use fbterm_config::{ProfileId, ProfileKind, ProfileStore};
use fbterm_session::ssh::sftp::{Entry, Sftp, SftpError, SortKey, sort_entries};
use fbterm_session::ssh::{Prompter, SshError, SshPool};
use iced::futures::Stream;
use iced::{Task, window};
use parking_lot::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::app::{App, Delivered, Message};
use crate::prompts::UiPrompter;
use crate::tabs::{TabState, WindowState};
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
    /// `<folder>:<item>,<item>,...`; an item is `name` (a file), `name/` (a folder) or
    /// `name -> target` (a link; a target ending in `/` is a folder), optionally followed by
    /// `|size` in bytes and `|YYYY-MM-DDTHH:MM` (modified, UTC).
    pub(crate) fn parse(spec: &str) -> Self {
        use fbterm_session::ssh::sftp::EntryKind;
        let (home, names) = spec.split_once(':').unwrap_or(("/", spec));
        let entries = names
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(|item| {
                let mut fields = item.split('|');
                let n = fields.next().unwrap_or_default();
                let size = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                let modified = fields
                    .next()
                    .and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").ok())
                    .and_then(|t| u64::try_from(t.and_utc().timestamp()).ok())
                    .map(|secs| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs));
                let (name, kind) = if let Some((name, target)) = n.split_once(" -> ") {
                    let to = if target.ends_with('/') {
                        EntryKind::Dir
                    } else {
                        EntryKind::File
                    };
                    (
                        name,
                        EntryKind::Link {
                            target: target.trim_end_matches('/').to_owned(),
                            to: Some(Box::new(to)),
                        },
                    )
                } else if let Some(name) = n.strip_suffix('/') {
                    (name, EntryKind::Dir)
                } else {
                    (n, EntryKind::File)
                };
                Entry {
                    name: name.to_owned(),
                    kind,
                    size,
                    modified,
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
    /// The panel of this window takes the keyboard (`true`, a click in it) or gives it back to
    /// the terminal (`false`, a click in the terminal).
    Focus(window::Id, bool),
    /// A click on a row: select it (an entry's name, or [`state::PARENT`]).
    Select(window::Id, String),
    /// A double click on a row: open it when it is a folder ([`state::PARENT`] goes up).
    Activate(window::Id, String),
    /// Go to this folder (a path bar segment).
    Navigate(window::Id, String),
    /// The path bar becomes a text field (a click on its empty end).
    EditPath(window::Id),
    PathInput(window::Id, String),
    /// Enter in the path field: go where it says.
    PathSubmit(window::Id),
    Filter(window::Id, String),
    /// A column header: sort by it, or reverse when it already sorts.
    Sort(window::Id, SortKey),
    /// `Show hidden files`, for every panel.
    ShowHidden(bool),
    /// The `Retry` of an inline error.
    Retry(window::Id),
    /// The list scrolled: its offset and visible height.
    Scrolled(window::Id, f32, f32),
    /// A key the panel's fields did not take, while the panel has the keyboard.
    Key(window::Id, PanelKey),
}

/// The keys the panel acts on when it has the keyboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelKey {
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    /// Opens the selected folder, or goes where the path field says.
    Enter,
    /// Goes up.
    Backspace,
    /// Leaves the path field, else clears the filter, else gives the keyboard back.
    Escape,
    /// Printable text: `/` or `~` starts typing a path, anything else types in the filter.
    Type(String),
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
pub(crate) enum Body<'a> {
    /// A line instead of a listing (`Not connected`, `Local session`).
    Note(String),
    /// An error, inline with `Retry`: the server's message, or `SFTP is not available: ...`.
    Error(String),
    Loading,
    /// The loaded folder: its entries and which are shown.
    Entries(&'a ProfilePanel),
}

/// A remote name (or a server's message) as plain text: control characters (escape
/// sequences, new lines), bidirectional controls (which could reorder what is shown, e.g. a
/// `.exe` shown as `.pdf`), line and paragraph separators and invisible zero-width characters
/// become `?`. The joiners U+200C and U+200D stay: names in many scripts need them.
pub(crate) fn plain_name(name: &str) -> String {
    name.chars()
        .map(|c| if is_unsafe_char(c) { '?' } else { c })
        .collect()
}

fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{202A}'..='\u{202E}'
                | '\u{2066}'..='\u{2069}'
                | '\u{200E}'
                | '\u{200F}'
                | '\u{061C}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{200B}'
                | '\u{2060}'
                | '\u{FEFF}'
        )
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
        if self.file_panels.open.remove(&wid) {
            self.file_panels.ui.remove(&wid);
        } else {
            // Shown with the shortcut or the palette: the user means to browse.
            self.file_panels.show(wid);
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
                    return self.file_panel_event(event);
                }
            }
            FilePanelMsg::Resize(width) => self.file_panels.width = state::clamp_width(width),
            FilePanelMsg::ResizeEnd => self.save_state(),
            FilePanelMsg::Close(wid) => {
                self.file_panels.open.remove(&wid);
                self.file_panels.ui.remove(&wid);
            }
            FilePanelMsg::ShowHidden(on) => {
                self.file_panels.show_hidden = on;
                for panel in self.file_panels.by_profile.values_mut() {
                    panel.refresh(on);
                }
                self.save_state();
                return self.list_to_top(None);
            }
            FilePanelMsg::Focus(wid, focused) => return self.focus_file_panel(wid, focused),
            FilePanelMsg::Select(wid, name) => {
                if let Some(panel) = self.shown_panel_mut(wid) {
                    panel.selected = Some(name);
                }
            }
            FilePanelMsg::Activate(wid, name) => return self.activate(wid, &name),
            FilePanelMsg::Navigate(wid, path) => return self.go_to(wid, path, None),
            FilePanelMsg::EditPath(wid) => return self.edit_path(wid, None),
            FilePanelMsg::PathInput(wid, text) => {
                if let Some(ui) = self.file_panels.ui.get_mut(&wid)
                    && ui.editing.is_some()
                {
                    ui.editing = Some(text);
                }
            }
            FilePanelMsg::PathSubmit(wid) => return self.submit_path(wid),
            FilePanelMsg::Filter(wid, text) => return self.set_filter(wid, text),
            FilePanelMsg::Sort(wid, key) => {
                let show_hidden = self.file_panels.show_hidden;
                if let Some(panel) = self.shown_panel_mut(wid) {
                    if panel.sort == key {
                        panel.descending = !panel.descending;
                    } else {
                        panel.sort = key;
                        panel.descending = false;
                    }
                    panel.sort_and_refresh(show_hidden);
                }
            }
            FilePanelMsg::Retry(wid) => return self.retry(wid),
            FilePanelMsg::Scrolled(wid, offset, height) => {
                if let Some(ui) = self.file_panels.ui.get_mut(&wid) {
                    ui.offset = offset;
                    ui.height = height;
                }
            }
            FilePanelMsg::Key(wid, key) => {
                if self.file_panels.has_focus(wid) {
                    return self.panel_key(wid, key);
                }
            }
        }
        Task::none()
    }

    /// The profile `wid`'s panel shows, when it shows a server.
    fn shown_profile(&self, wid: window::Id) -> Option<ProfileId> {
        if !self.file_panels.is_open(wid) {
            return None;
        }
        match self.panel_shows(wid)? {
            Shown::Remote(p) => Some(p),
            _ => None,
        }
    }

    fn shown_panel_mut(&mut self, wid: window::Id) -> Option<&mut ProfilePanel> {
        let p = self.shown_profile(wid)?;
        self.file_panels.by_profile.get_mut(&p)
    }

    /// Give `wid`'s panel the keyboard, or give it back to the terminal; the panel's fields
    /// let go of it then, so nothing types into them or blinks there.
    fn focus_file_panel(&mut self, wid: window::Id, focused: bool) -> Task<Message> {
        let Some(ui) = self.file_panels.ui.get_mut(&wid) else {
            return Task::none();
        };
        ui.focused = focused;
        if focused {
            return Task::none();
        }
        ui.editing = None;
        view::unfocus_fields(ui)
    }

    /// Go to `path` in `wid`'s panel, selecting `select` there; the filter starts empty and
    /// the list at the top.
    fn go_to(&mut self, wid: window::Id, path: String, select: Option<String>) -> Task<Message> {
        let Some(profile) = self.shown_profile(wid) else {
            return Task::none();
        };
        let Some(panel) = self.file_panels.by_profile.get_mut(&profile) else {
            return Task::none();
        };
        panel.folder = path;
        panel.filter.clear();
        panel.selected = select;
        panel.shown.clear();
        if panel.sftp.is_some() {
            self.read_folder(profile);
        } else if matches!(panel.listing, Listing::Failed(_) | Listing::Unavailable(_)) {
            // The session is gone (the folder's error said so): open it again, over this
            // window, for the new folder rather than leave the old error under the new path.
            self.reopen(profile, wid);
        }
        // Otherwise a session is being opened (it lists `panel.folder` once open, and may be
        // asking a sign-in question that a restart would drop), or none is wanted yet.
        for ui in self.file_panels.ui.values_mut() {
            ui.editing = None;
        }
        self.list_to_top(Some(profile))
    }

    /// Scroll the list back to the top in every panel showing `profile` (every open panel
    /// with `None`): its rows changed, and an offset past their end would show none.
    fn list_to_top(&mut self, profile: Option<ProfileId>) -> Task<Message> {
        let showing: Vec<window::Id> = self
            .file_panels
            .open
            .iter()
            .copied()
            .filter(|&w| profile.is_none() || self.shown_profile(w) == profile)
            .collect();
        let mut tasks = Vec::new();
        for w in showing {
            if let Some(ui) = self.file_panels.ui.get_mut(&w) {
                ui.offset = 0.0;
                tasks.push(iced::widget::operation::snap_to(
                    ui.list_id.clone(),
                    iced::widget::operation::RelativeOffset::START,
                ));
            }
        }
        Task::batch(tasks)
    }

    /// Open `name` in `wid`'s panel: a folder (or a link to one) is entered, [`state::PARENT`]
    /// goes up; anything else stays selected.
    fn activate(&mut self, wid: window::Id, name: &str) -> Task<Message> {
        let Some(panel) = self.shown_panel_mut(wid) else {
            return Task::none();
        };
        if name == state::PARENT {
            return self.go_up(wid);
        }
        panel.selected = Some(name.to_owned());
        let Listing::Loaded(entries) = &panel.listing else {
            return Task::none();
        };
        if !entries.iter().any(|e| e.name == name && e.is_folder()) {
            return Task::none();
        }
        let path = state::resolve_path(&panel.folder, "/", "");
        let path = if path == "/" {
            format!("/{name}")
        } else {
            format!("{path}/{name}")
        };
        self.go_to(wid, path, None)
    }

    /// Up one folder, selecting the folder left.
    fn go_up(&mut self, wid: window::Id) -> Task<Message> {
        let Some(panel) = self.shown_panel_mut(wid) else {
            return Task::none();
        };
        if !state::has_parent(&panel.folder) {
            return Task::none();
        }
        let left = panel
            .folder
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|n| !n.is_empty())
            .map(str::to_owned);
        let parent = state::parent_of(&panel.folder);
        self.go_to(wid, parent, left)
    }

    /// The path bar becomes a text field holding `text` (the folder when `None`).
    fn edit_path(&mut self, wid: window::Id, text: Option<String>) -> Task<Message> {
        let Some(folder) = self.shown_panel_mut(wid).map(|p| p.folder.clone()) else {
            return Task::none();
        };
        let Some(ui) = self.file_panels.ui.get_mut(&wid) else {
            return Task::none();
        };
        ui.focused = true;
        ui.editing = Some(text.unwrap_or(folder));
        let id = ui.path_id.clone();
        Task::batch([
            iced::widget::operation::focus(id.clone()),
            iced::widget::operation::move_cursor_to_end(id),
        ])
    }

    fn submit_path(&mut self, wid: window::Id) -> Task<Message> {
        let Some(typed) = self
            .file_panels
            .ui
            .get_mut(&wid)
            .and_then(|ui| ui.editing.take())
        else {
            return Task::none();
        };
        let Some(panel) = self.shown_panel_mut(wid) else {
            return Task::none();
        };
        let path = state::resolve_path(&panel.folder, &panel.home, &typed);
        self.go_to(wid, path, None)
    }

    fn set_filter(&mut self, wid: window::Id, text: String) -> Task<Message> {
        let show_hidden = self.file_panels.show_hidden;
        let Some(profile) = self.shown_profile(wid) else {
            return Task::none();
        };
        if let Some(panel) = self.file_panels.by_profile.get_mut(&profile) {
            panel.filter = text;
            panel.refresh(show_hidden);
        }
        self.list_to_top(Some(profile))
    }

    /// `Retry` in `wid`'s panel: a session that is gone (or never opened) is opened again,
    /// asking over this window; otherwise the folder is read again.
    fn retry(&mut self, wid: window::Id) -> Task<Message> {
        let Some(profile) = self.shown_profile(wid) else {
            return Task::none();
        };
        let has_session = self
            .file_panels
            .by_profile
            .get(&profile)
            .is_some_and(|p| p.sftp.is_some());
        if has_session {
            self.read_folder(profile);
        } else {
            self.reopen(profile, wid);
        }
        self.list_to_top(Some(profile))
    }

    /// Open `profile`'s session again (it is gone), asking over `wid`; its folder is kept.
    fn reopen(&mut self, profile: ProfileId, wid: window::Id) {
        if let Some(panel) = self.file_panels.by_profile.get_mut(&profile) {
            panel.release();
        }
        self.open_sftp(profile, wid);
    }

    /// A key for `wid`'s panel, which has the keyboard.
    fn panel_key(&mut self, wid: window::Id, key: PanelKey) -> Task<Message> {
        let editing = self
            .file_panels
            .ui
            .get(&wid)
            .is_some_and(|ui| ui.editing.is_some());
        match key {
            PanelKey::Enter if editing => self.submit_path(wid),
            PanelKey::Escape if editing => {
                if let Some(ui) = self.file_panels.ui.get_mut(&wid) {
                    ui.editing = None;
                }
                Task::none()
            }
            // The path field takes its own typing.
            _ if editing => Task::none(),
            PanelKey::Enter => {
                let selected = self.shown_panel_mut(wid).and_then(|p| p.selected.clone());
                match selected {
                    Some(name) => self.activate(wid, &name),
                    None => Task::none(),
                }
            }
            PanelKey::Backspace => self.go_up(wid),
            PanelKey::Escape => {
                let filtered = self
                    .shown_panel_mut(wid)
                    .is_some_and(|p| !p.filter.is_empty());
                if filtered {
                    self.set_filter(wid, String::new())
                } else {
                    self.focus_file_panel(wid, false)
                }
            }
            PanelKey::Type(text) if text.starts_with('/') || text.starts_with('~') => {
                self.edit_path(wid, Some(text))
            }
            PanelKey::Type(text) => {
                let filter = match self.shown_panel_mut(wid) {
                    Some(panel) => format!("{}{text}", panel.filter),
                    None => return Task::none(),
                };
                let top = self.set_filter(wid, filter);
                match self.file_panels.ui.get(&wid) {
                    Some(ui) => Task::batch([
                        top,
                        iced::widget::operation::focus(ui.filter_id.clone()),
                        iced::widget::operation::move_cursor_to_end(ui.filter_id.clone()),
                    ]),
                    None => top,
                }
            }
            PanelKey::Up
            | PanelKey::Down
            | PanelKey::Home
            | PanelKey::End
            | PanelKey::PageUp
            | PanelKey::PageDown => self.move_selection(wid, &key),
        }
    }

    /// Move the selection by `key` and scroll it into view.
    fn move_selection(&mut self, wid: window::Id, key: &PanelKey) -> Task<Message> {
        let Some(ui) = self.file_panels.ui.get(&wid) else {
            return Task::none();
        };
        let (offset, height) = (ui.offset, ui.height);
        let list = ui.list_id.clone();
        let Some(panel) = self.shown_panel_mut(wid) else {
            return Task::none();
        };
        let names: Vec<&str> = panel.row_names().collect();
        if names.is_empty() {
            return Task::none();
        }
        let last = names.len() - 1;
        let current = panel
            .selected
            .as_deref()
            .and_then(|s| names.iter().position(|n| *n == s));
        let page = (view::visible_height(height) / view::ROW_HEIGHT)
            .floor()
            .max(1.0) as usize;
        let next = match (key, current) {
            (PanelKey::Home, _) => 0,
            (PanelKey::End, _) => last,
            (PanelKey::Down, None) | (PanelKey::PageDown, None) => 0,
            (PanelKey::Up, None) | (PanelKey::PageUp, None) => last,
            (PanelKey::Down, Some(i)) => (i + 1).min(last),
            (PanelKey::Up, Some(i)) => i.saturating_sub(1),
            (PanelKey::PageDown, Some(i)) => (i + page).min(last),
            (PanelKey::PageUp, Some(i)) => i.saturating_sub(page),
            _ => return Task::none(),
        };
        panel.selected = Some(names[next].to_owned());
        match view::scroll_into_view(next, offset, height) {
            Some(to) => {
                if let Some(ui) = self.file_panels.ui.get_mut(&wid) {
                    ui.offset = to;
                }
                iced::widget::operation::scroll_to(
                    list,
                    iced::widget::operation::AbsoluteOffset {
                        x: None,
                        y: Some(to),
                    },
                )
            }
            None => Task::none(),
        }
    }

    fn file_panel_event(&mut self, event: PanelEvent) -> Task<Message> {
        match event {
            PanelEvent::Opened {
                profile,
                token,
                result,
            } => {
                let Some(panel) = self.file_panels.by_profile.get_mut(&profile) else {
                    return Task::none();
                };
                if panel.token != token || panel.listing != Listing::Opening {
                    return Task::none();
                }
                panel.opening = None;
                match result {
                    Err(reason) => panel.listing = Listing::Unavailable(reason),
                    Ok(Opened { remote, home }) => {
                        panel.sftp = Some(remote);
                        if let Ok(home) = &home {
                            panel.home.clone_from(home);
                        }
                        if panel.folder.is_empty() {
                            match home {
                                Ok(home) => panel.folder = home,
                                Err(e) => {
                                    panel.listing = Listing::Failed(e);
                                    return Task::none();
                                }
                            }
                        }
                        self.read_folder(profile);
                    }
                }
                Task::none()
            }
            PanelEvent::Listed {
                profile,
                token,
                folder,
                result,
            } => {
                let Some(panel) = self.file_panels.by_profile.get_mut(&profile) else {
                    return Task::none();
                };
                if panel.token != token || panel.folder != folder {
                    return Task::none();
                }
                panel.listing = match result {
                    Ok(mut entries) => {
                        sort_entries(&mut entries, panel.sort, panel.descending);
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
                panel.refresh(self.file_panels.show_hidden);
                // New rows: from the top, as a shorter listing may end above the offset.
                self.list_to_top(Some(profile))
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
    /// A panel keeps the keyboard only while it shows a server and its window's focused pane
    /// stays the same: another pane or tab taking focus gives the keyboard back to the
    /// terminal.
    /// Returns the task that lets go of the fields of panels that lost the keyboard, if any.
    pub(crate) fn sync_panel_focus(&mut self) -> Option<Task<Message>> {
        if self.file_panels.ui.is_empty() {
            return None;
        }
        let mut tasks = Vec::new();
        let windows: Vec<window::Id> = self.file_panels.ui.keys().copied().collect();
        for wid in windows {
            let pane = self
                .windows
                .get(&wid)
                .and_then(WindowState::active_tab)
                .map(|tab| tab.focused);
            let shown = match self.panel_shows(wid) {
                Some(Shown::Remote(p)) => Some(p),
                _ => None,
            };
            let remote = shown.is_some();
            let Some(ui) = self.file_panels.ui.get_mut(&wid) else {
                continue;
            };
            let moved = ui.pane.is_some() && ui.pane != pane;
            ui.pane = pane;
            // Another profile's folder: its list starts at the top.
            if ui.profile != shown {
                ui.profile = shown;
                ui.offset = 0.0;
                tasks.push(iced::widget::operation::snap_to(
                    ui.list_id.clone(),
                    iced::widget::operation::RelativeOffset::START,
                ));
            }
            if ui.focused && (moved || !remote) {
                ui.focused = false;
                ui.editing = None;
                tasks.push(view::unfocus_fields(ui));
            }
        }
        (!tasks.is_empty()).then(|| Task::batch(tasks))
    }

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
                Some(Listing::Loaded(_)) => Body::Entries(&self.file_panels.by_profile[&p]),
                Some(Listing::Failed(e)) => Body::Error(plain_name(e)),
                Some(Listing::Unavailable(reason)) => {
                    Body::Error(format!("SFTP is not available: {}", plain_name(reason)))
                }
            },
        })
    }

    /// The folder `wid`'s panel shows, when it shows one.
    pub(crate) fn file_panel_folder(&self, wid: window::Id) -> Option<&str> {
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
