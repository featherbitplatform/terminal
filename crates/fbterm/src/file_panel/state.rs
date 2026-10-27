//! The file panel's state, free of the UI: which windows show the panel, each profile's SFTP
//! session, folder and listing, and the panel's width.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use fbterm_config::ProfileId;
use fbterm_session::ssh::sftp::{Entry, SortKey, sort_entries};
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
    /// The folder is being read.
    Loading,
    Loaded(Vec<Entry>),
    /// Reading the folder failed: the server's message, shown inline.
    Failed(String),
    /// The SFTP session could not be opened: the reason, shown as
    /// `SFTP is not available: {reason}`.
    Unavailable(String),
}

/// One transfer in a profile's queue (filled by downloads and uploads).
#[allow(dead_code, reason = "rows are added by downloads and uploads")]
pub(crate) struct TransferRow {
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
            token: 0,
            opening: None,
        }
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
                .filter(|(_, e)| filter.is_empty() || e.name.to_lowercase().contains(&filter))
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
