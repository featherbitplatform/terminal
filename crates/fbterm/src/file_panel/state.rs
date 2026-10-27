//! The file panel's state, free of the UI: which windows show the panel, each profile's SFTP
//! session, folder and listing, and the panel's width.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use fbterm_config::ProfileId;
use fbterm_session::ssh::sftp::Entry;
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
    pub listing: Listing,
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
            listing: Listing::Closed,
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
        self.token += 1;
    }
}

/// The panel of every window, and each profile's side of it.
pub(crate) struct FilePanels {
    /// Terminal windows with the panel shown.
    pub open: HashSet<window::Id>,
    pub by_profile: HashMap<ProfileId, ProfilePanel>,
    /// The panel's width, the same in every window (remembered in `state.toml`).
    pub width: f32,
    /// `FBTERM_DEMO_FILES` (debug aid): SSH panes count as connected.
    pub demo: bool,
}

impl FilePanels {
    pub(crate) fn new(width: Option<f32>) -> Self {
        Self {
            open: HashSet::new(),
            by_profile: HashMap::new(),
            width: width.map_or(DEFAULT_WIDTH, clamp_width),
            demo: false,
        }
    }

    /// Whether `window` shows the panel.
    pub(crate) fn is_open(&self, window: window::Id) -> bool {
        self.open.contains(&window)
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
        assert_eq!(FilePanels::new(None).width, DEFAULT_WIDTH);
        assert_eq!(FilePanels::new(Some(10.0)).width, MIN_WIDTH);
        assert_eq!(FilePanels::new(Some(5000.0)).width, MAX_WIDTH);
        assert_eq!(FilePanels::new(Some(f32::NAN)).width, DEFAULT_WIDTH);
        assert_eq!(FilePanels::new(Some(410.0)).width, 410.0);
        assert_eq!(FilePanels::new(None).saved_width(), None);
        assert_eq!(FilePanels::new(Some(410.0)).saved_width(), Some(410.0));
    }
}
