//! The file panel's state, free of the UI: which windows show the panel, each profile's SFTP
//! session, folder and listing, and the panel's width.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fbterm_config::ProfileId;
use fbterm_session::ssh::sftp::{Entry, SortKey, sort_entries};
use fbterm_session::ssh::transfer::{TransferId, UploadMode};
use iced::window;

use super::Remote;

/// The panel's width when the user never changed it, in logical pixels.
pub(crate) const DEFAULT_WIDTH: f32 = 300.0;
/// The narrowest and widest the panel can be dragged.
pub(crate) const MIN_WIDTH: f32 = 220.0;
pub(crate) const MAX_WIDTH: f32 = 720.0;

/// `width` within [`MIN_WIDTH`]..=[`MAX_WIDTH`]; [`DEFAULT_WIDTH`] when it is not a number.
pub(crate) fn clamp_width(width: f32) -> f32 {
    if width.is_finite() {
        width.clamp(MIN_WIDTH, MAX_WIDTH)
    } else {
        DEFAULT_WIDTH
    }
}

/// What the panel shows of a profile's current folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Listing {
    /// No SFTP session: never opened, or let go while no panel needed it.
    Closed,
    /// The SFTP session is being opened.
    Opening,
    /// The check after a shell connected is opening the SFTP session on that shell's
    /// connection, asking nothing; a panel that shows the profile opens it its own way instead.
    Checking,
    /// The folder is being read.
    Loading,
    Loaded(Vec<Entry>),
    /// Reading the folder failed: the server's message, shown inline.
    Failed(String),
    /// The SFTP session could not be opened: the reason, shown as
    /// `SFTP is not available: {reason}`.
    Unavailable(String),
}

/// Which way a transfer goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Download,
    Upload,
}

/// Where a transfer stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowState {
    Running,
    /// Done; `local` is where a download was saved, `note` says how an upload replaced its
    /// target ([`fbterm_session::ssh::transfer::REPLACED_IN_PLACE`]).
    Finished {
        local: Option<PathBuf>,
        note: Option<String>,
    },
    /// Stopped, with the reason shown in the row.
    Failed(String),
    /// Never started, with the note shown in the row (a folder picked or dropped).
    Skipped(String),
}

/// Stops a running transfer.
pub(crate) type CancelFn = Arc<dyn Fn() + Send + Sync>;

/// What a row's transfer does, so `Retry` can do it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Job {
    /// The remote file `remote` (named `name` on the server) to `dest`. A destination the
    /// user `chosen` with `Download as…` is used again as it is; else a retry picks a free
    /// name in the same folder again.
    Download {
        remote: String,
        name: String,
        dest: PathBuf,
        chosen: bool,
    },
    /// The local file `local` as `name` into the remote `folder`.
    Upload {
        local: PathBuf,
        folder: String,
        name: String,
        mode: UploadMode,
    },
}

/// One transfer in a profile's queue. A cancelled transfer leaves the queue; finished, failed
/// and skipped ones stay until dismissed (finished and skipped ones also when no panel shows
/// the profile any more).
pub(crate) struct TransferRow {
    pub id: TransferId,
    /// The remote name as plain text ([`super::plain_name`]).
    pub name: String,
    pub direction: Direction,
    /// The local file a download writes: no other download may write it while this one runs.
    pub local: Option<PathBuf>,
    /// Bytes moved so far, of `total` when known.
    pub done: u64,
    pub total: Option<u64>,
    pub state: RowState,
    /// Stops the transfer while it runs.
    pub(crate) cancel: Option<CancelFn>,
    /// What the transfer does, for `Retry`; `None` for a row that cannot be retried.
    pub(crate) job: Option<Job>,
    /// An upload writing over the existing file in place: cancelling it may leave that file
    /// incomplete ([`fbterm_session::ssh::transfer::TransferEvent::InPlace`]).
    pub(crate) in_place: bool,
}

impl TransferRow {
    /// A download that started writing `local`.
    pub(crate) fn download(id: TransferId, name: String, local: PathBuf, cancel: CancelFn) -> Self {
        Self {
            id,
            name,
            direction: Direction::Download,
            local: Some(local),
            done: 0,
            total: None,
            state: RowState::Running,
            cancel: Some(cancel),
            job: None,
            in_place: false,
        }
    }

    /// An upload that started.
    pub(crate) fn upload(id: TransferId, name: String, cancel: CancelFn) -> Self {
        Self {
            direction: Direction::Upload,
            local: None,
            ..Self::download(id, name, PathBuf::new(), cancel)
        }
    }

    /// A download that could not start, and why.
    pub(crate) fn refused(name: String, reason: String) -> Self {
        Self::stopped(name, Direction::Download, RowState::Failed(reason))
    }

    /// A row for something that never ran.
    pub(crate) fn stopped(name: String, direction: Direction, state: RowState) -> Self {
        Self {
            id: TransferId::next(),
            name,
            direction,
            local: None,
            done: 0,
            total: None,
            state,
            cancel: None,
            job: None,
            in_place: false,
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        self.state == RowState::Running
    }

    /// The saved file a finished download can show in the file manager.
    pub(crate) fn revealable(&self) -> Option<&Path> {
        match &self.state {
            RowState::Finished { local, .. } => local.as_deref(),
            _ => None,
        }
    }

    /// Why the transfer stopped, when it failed.
    #[cfg(test)]
    pub(crate) fn failed(&self) -> Option<&str> {
        match &self.state {
            RowState::Failed(reason) => Some(reason),
            _ => None,
        }
    }

    /// The row's note: how a finished upload replaced its target, or why it was skipped.
    pub(crate) fn note(&self) -> Option<&str> {
        match &self.state {
            RowState::Finished { note, .. } => note.as_deref(),
            RowState::Skipped(note) => Some(note),
            _ => None,
        }
    }

    /// Whether `Retry` is offered: a failed transfer that can be done again.
    pub(crate) fn retryable(&self) -> bool {
        self.job.is_some() && matches!(self.state, RowState::Failed(_))
    }

    /// The local file this row is writing now, if it is a running download.
    pub(crate) fn writing(&self) -> Option<&Path> {
        match (self.direction, &self.state) {
            (Direction::Download, RowState::Running) => self.local.as_deref(),
            _ => None,
        }
    }

    /// The remote name this row is writing in `folder` now, if it is a running upload.
    pub(crate) fn uploading_into(&self, folder: &str) -> Option<&str> {
        match (&self.job, &self.state) {
            (
                Some(Job::Upload {
                    folder: f, name, ..
                }),
                RowState::Running,
            ) if f == folder => Some(name),
            _ => None,
        }
    }
}

/// What a `Download as…` dialog was opened for.
pub(crate) struct PendingSave {
    pub profile: ProfileId,
    /// The remote file's path and its name on the server.
    pub remote: String,
    pub name: String,
}

/// One profile's side of the panel. Kept for the app session, so its folder survives focus
/// moving away and back; the SFTP session is let go when no panel needs it.
pub(crate) struct ProfilePanel {
    /// The open SFTP session; it holds the profile's connection open.
    pub sftp: Option<Arc<dyn Remote>>,
    /// The folder shown: the server's answer for `.` at first, empty until known.
    pub folder: String,
    /// The server's answer for `.`, where `~` leads; empty until known.
    pub home: String,
    pub listing: Listing,
    /// The column the listing is sorted by, and whether it is reversed.
    pub sort: SortKey,
    pub descending: bool,
    /// The filter box: a case-insensitive part of the names to show.
    pub filter: String,
    /// The one selected item: an entry's name, or [`PARENT`] for the `..` row.
    pub selected: Option<String>,
    /// Which of the loaded entries are shown (indices into [`Listing::Loaded`]), after hidden
    /// files and the filter; kept up to date by [`ProfilePanel::refresh`].
    pub shown: Vec<usize>,
    /// Transfers of this profile's connection; they keep the session while they run.
    pub transfers: Vec<TransferRow>,
    /// Rows whose `Retry` waits for the session to open again (their ids).
    pub(crate) waiting: Vec<TransferId>,
    /// Counts opens and folder reads, so only the latest answer is taken.
    pub(crate) token: u64,
    /// Stops the open under way, if any: a session no panel wants any more must not go on to
    /// ask a sign-in question.
    pub(crate) opening: Option<tokio::task::AbortHandle>,
}

impl ProfilePanel {
    pub(crate) fn new() -> Self {
        Self {
            sftp: None,
            folder: String::new(),
            home: String::new(),
            listing: Listing::Closed,
            sort: SortKey::Name,
            descending: false,
            filter: String::new(),
            selected: None,
            shown: Vec::new(),
            transfers: Vec::new(),
            waiting: Vec::new(),
            token: 0,
            opening: None,
        }
    }

    /// Whether the session is open and its connection still up: the tab bar's files button
    /// shows for the profile.
    pub(crate) fn available(&self) -> bool {
        self.sftp.as_ref().is_some_and(|s| s.is_alive())
    }

    /// Let go of the SFTP session: an open under way stops, and anything still on its way is
    /// ignored.
    pub(crate) fn release(&mut self) {
        if let Some(open) = self.opening.take() {
            open.abort();
        }
        self.sftp = None;
        self.listing = Listing::Closed;
        self.shown.clear();
        self.token += 1;
    }

    /// Sort the loaded entries by the panel's column, then work out which are shown.
    pub(crate) fn sort_and_refresh(&mut self, show_hidden: bool) {
        if let Listing::Loaded(entries) = &mut self.listing {
            sort_entries(entries, self.sort, self.descending);
        }
        self.refresh(show_hidden);
    }

    /// Work out which loaded entries are shown: hidden files only with `show_hidden`, and only
    /// names containing the filter, ignoring case.
    pub(crate) fn refresh(&mut self, show_hidden: bool) {
        self.shown.clear();
        let Listing::Loaded(entries) = &self.listing else {
            return;
        };
        let filter = self.filter.to_lowercase();
        self.shown.extend(
            entries
                .iter()
                .enumerate()
                .filter(|(_, e)| show_hidden || !e.name.starts_with('.'))
                // Against the name as shown, so `?` matches a control character.
                .filter(|(_, e)| {
                    filter.is_empty() || super::plain_name(&e.name).to_lowercase().contains(&filter)
                })
                .map(|(i, _)| i),
        );
    }

    /// The rows in order, by the name a selection uses: the `..` row (unless the folder is
    /// `/`), then the shown entries.
    pub(crate) fn row_names(&self) -> impl Iterator<Item = &str> {
        let entries: &[Entry] = match &self.listing {
            Listing::Loaded(entries) => entries,
            _ => &[],
        };
        let parent = has_parent(&self.folder) && matches!(self.listing, Listing::Loaded(_));
        parent
            .then_some(PARENT)
            .into_iter()
            .chain(self.shown.iter().map(|&i| entries[i].name.as_str()))
    }
}

/// How a selection names the `..` row (no entry has this name: listings leave it out).
pub(crate) const PARENT: &str = "..";

/// Whether `folder` has a folder above it (anything but `/`).
pub(crate) fn has_parent(folder: &str) -> bool {
    folder != "/" && !folder.is_empty()
}

/// Where typing `typed` in the path bar leads from `current`: `~` is `home`, a path without a
/// leading `/` is inside `current`, and `.` and `..` are resolved by name. Always absolute.
pub(crate) fn resolve_path(current: &str, home: &str, typed: &str) -> String {
    let typed = typed.trim();
    let joined = if typed.is_empty() {
        current.to_owned()
    } else if typed == "~" {
        home.to_owned()
    } else if let Some(rest) = typed.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else if typed.starts_with('/') {
        typed.to_owned()
    } else {
        format!("{current}/{typed}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}

/// The path of `name` inside `folder`.
pub(crate) fn child(folder: &str, name: &str) -> String {
    let folder = resolve_path(folder, "/", "");
    if folder == "/" {
        format!("/{name}")
    } else {
        format!("{folder}/{name}")
    }
}

/// The folder above `folder` (`/` stays `/`).
pub(crate) fn parent_of(folder: &str) -> String {
    resolve_path(folder, "/", "..")
}

/// Each step of `folder` for the path bar: its label and the folder it leads to, `/` first.
pub(crate) fn path_segments(folder: &str) -> Vec<(String, String)> {
    let mut segments = vec![("/".to_owned(), "/".to_owned())];
    let mut path = String::new();
    for part in folder.split('/').filter(|p| !p.is_empty()) {
        path.push('/');
        path.push_str(part);
        segments.push((part.to_owned(), path.clone()));
    }
    segments
}

/// One window's panel: keyboard focus, the path field and the list's scroll position.
pub(crate) struct WindowPanel {
    /// Keys go to the panel, not the terminal (a click in the panel; a click in the terminal
    /// or Escape gives them back).
    pub focused: bool,
    /// The focused pane the panel last followed: when another pane or tab takes focus, the
    /// keyboard goes back to the terminal.
    pub pane: Option<crate::layout::PaneId>,
    /// The profile the panel last showed.
    pub profile: Option<ProfileId>,
    /// The path bar as a text field, with what is typed so far.
    pub editing: Option<String>,
    /// The list's scroll offset and visible height, in logical pixels (height 0 until the
    /// list reports it).
    pub offset: f32,
    pub height: f32,
    pub filter_id: iced::widget::Id,
    pub path_id: iced::widget::Id,
    pub list_id: iced::widget::Id,
}

impl WindowPanel {
    pub(crate) fn new() -> Self {
        Self {
            focused: false,
            pane: None,
            profile: None,
            editing: None,
            offset: 0.0,
            height: 0.0,
            filter_id: iced::widget::Id::unique(),
            path_id: iced::widget::Id::unique(),
            list_id: iced::widget::Id::unique(),
        }
    }
}

/// The panel of every window, and each profile's side of it.
pub(crate) struct FilePanels {
    /// Terminal windows with the panel shown.
    pub open: HashSet<window::Id>,
    /// Each open panel's own state.
    pub ui: HashMap<window::Id, WindowPanel>,
    /// `Show hidden files` (on unless the user turned it off; remembered in `state.toml`).
    pub show_hidden: bool,
    pub by_profile: HashMap<ProfileId, ProfilePanel>,
    /// The panel's width, the same in every window (remembered in `state.toml`).
    pub width: f32,
    /// `FBTERM_DEMO_FILES` (debug aid): SSH panes count as connected.
    pub demo: bool,
    /// The `Download as…` dialog open in each window (one at a time per window).
    pub saving: HashMap<window::Id, PendingSave>,
    /// Windows with the `Upload…` picker open (one at a time per window).
    pub picking: HashSet<window::Id>,
    /// Files dropped on each window in the current burst, uploaded together once it settles.
    pub dropped: HashMap<window::Id, Vec<PathBuf>>,
    /// Each window's uploads waiting for a decision, oldest first: the front one may be
    /// asking about a name clash.
    pub batches: HashMap<window::Id, VecDeque<Batch>>,
    /// Quitting once the transfers cancelled for it have stopped.
    pub quit_when_stopped: bool,
    /// `FBTERM_DEMO_UPLOAD` (debug aid): files uploaded in this window once a listing loads.
    pub demo_upload: Option<(window::Id, Vec<PathBuf>)>,
}

/// Files picked or dropped together for one folder, decided one at a time.
pub(crate) struct Batch {
    pub profile: ProfileId,
    pub folder: String,
    /// The files still to decide, each with the name it gets in the folder.
    pub items: VecDeque<(PathBuf, String)>,
    /// The file the clash dialog asks about, and the name it would have.
    pub asking: Option<(PathBuf, String)>,
    /// `Apply to all` was chosen with this answer.
    pub all: Option<AllChoice>,
}

/// An answer `Apply to all` repeats for the rest of a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AllChoice {
    Replace,
    Cancel,
}

impl FilePanels {
    pub(crate) fn new(width: Option<f32>, show_hidden: bool) -> Self {
        Self {
            open: HashSet::new(),
            ui: HashMap::new(),
            show_hidden,
            by_profile: HashMap::new(),
            width: width.map_or(DEFAULT_WIDTH, clamp_width),
            demo: false,
            saving: HashMap::new(),
            picking: HashSet::new(),
            dropped: HashMap::new(),
            batches: HashMap::new(),
            quit_when_stopped: false,
            demo_upload: None,
        }
    }

    /// Whether `window` shows the panel.
    pub(crate) fn is_open(&self, window: window::Id) -> bool {
        self.open.contains(&window)
    }

    /// Show `window`'s panel, with the keyboard.
    pub(crate) fn show(&mut self, window: window::Id) {
        self.open.insert(window);
        let mut ui = WindowPanel::new();
        ui.focused = true;
        self.ui.insert(window, ui);
    }

    /// Whether `window`'s panel has the keyboard.
    pub(crate) fn has_focus(&self, window: window::Id) -> bool {
        self.is_open(window) && self.ui.get(&window).is_some_and(|ui| ui.focused)
    }

    /// The `Show hidden files` setting to remember: `None` while it is the default (on).
    pub(crate) fn saved_show_hidden(&self) -> Option<bool> {
        (!self.show_hidden).then_some(false)
    }

    /// The width to remember: `None` while it is the default.
    pub(crate) fn saved_width(&self) -> Option<f32> {
        (self.width != DEFAULT_WIDTH).then_some(self.width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_is_clamped_and_defaults() {
        assert_eq!(FilePanels::new(None, true).width, DEFAULT_WIDTH);
        assert_eq!(FilePanels::new(Some(10.0), true).width, MIN_WIDTH);
        assert_eq!(FilePanels::new(Some(5000.0), true).width, MAX_WIDTH);
        assert_eq!(FilePanels::new(Some(f32::NAN), true).width, DEFAULT_WIDTH);
        assert_eq!(FilePanels::new(Some(410.0), true).width, 410.0);
        assert_eq!(FilePanels::new(None, true).saved_width(), None);
        assert_eq!(
            FilePanels::new(Some(410.0), true).saved_width(),
            Some(410.0)
        );
    }

    #[test]
    fn paths_resolve_like_a_shell() {
        assert_eq!(
            resolve_path("/home/deploy", "/home/deploy", "logs"),
            "/home/deploy/logs"
        );
        assert_eq!(
            resolve_path("/home/deploy", "/home/deploy", "/var/log/"),
            "/var/log"
        );
        assert_eq!(resolve_path("/var", "/home/deploy", "~"), "/home/deploy");
        assert_eq!(
            resolve_path("/var", "/home/deploy", "~/app"),
            "/home/deploy/app"
        );
        assert_eq!(resolve_path("/var/log", "/", "../tmp/./x"), "/var/tmp/x");
        assert_eq!(resolve_path("/", "/", ".."), "/");
        assert_eq!(resolve_path("/srv", "/", "  "), "/srv");
        assert_eq!(parent_of("/home/deploy"), "/home");
        assert_eq!(parent_of("/"), "/");
        assert_eq!(
            path_segments("/home/deploy"),
            [
                ("/".to_owned(), "/".to_owned()),
                ("home".to_owned(), "/home".to_owned()),
                ("deploy".to_owned(), "/home/deploy".to_owned()),
            ]
        );
        assert_eq!(path_segments("/"), [("/".to_owned(), "/".to_owned())]);
    }
}
