//! The tunnels panel's state, free of the UI: which terminal windows show it, each window's
//! search (kept while its panel is closed, forgotten with the window) and the panel's width,
//! the same in every window. The width limits are the file panel's.

use std::collections::{HashMap, HashSet};

use iced::window;

pub(crate) use crate::file_panel::state::clamp_width;
#[cfg(test)]
pub(crate) use crate::file_panel::state::{MAX_WIDTH, MIN_WIDTH};

/// The panel's width when the user never changed it, in logical pixels (the file panel's).
pub(crate) const DEFAULT_WIDTH: f32 = crate::file_panel::state::DEFAULT_WIDTH;

pub(crate) struct TunnelsPanels {
    /// Terminal windows with the panel shown.
    pub open: HashSet<window::Id>,
    /// Each window's search text, for the app session.
    pub query: HashMap<window::Id, String>,
    /// Each window's search field, focused when its panel opens.
    pub input: HashMap<window::Id, iced::widget::Id>,
    /// The panel's width, the same in every window (remembered in `state.toml`).
    pub width: f32,
}

impl TunnelsPanels {
    pub(crate) fn new(width: Option<f32>) -> Self {
        Self {
            open: HashSet::new(),
            query: HashMap::new(),
            input: HashMap::new(),
            width: width.map_or(DEFAULT_WIDTH, clamp_width),
        }
    }

    pub(crate) fn is_open(&self, window: window::Id) -> bool {
        self.open.contains(&window)
    }

    /// Show `window`'s panel; its search field, to focus.
    pub(crate) fn show(&mut self, window: window::Id) -> iced::widget::Id {
        self.open.insert(window);
        self.input
            .entry(window)
            .or_insert_with(iced::widget::Id::unique)
            .clone()
    }

    /// Hide `window`'s panel; its search stays.
    pub(crate) fn hide(&mut self, window: window::Id) {
        self.open.remove(&window);
    }

    /// `window`'s search text.
    // Until the dock draws the panel, only tests use it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn query(&self, window: window::Id) -> &str {
        self.query.get(&window).map_or("", String::as_str)
    }

    /// `window` closed: everything about its panel goes.
    pub(crate) fn forget(&mut self, window: window::Id) {
        self.open.remove(&window);
        self.query.remove(&window);
        self.input.remove(&window);
    }

    /// The width to remember: `None` while it is the default.
    pub(crate) fn saved_width(&self) -> Option<f32> {
        (self.width != DEFAULT_WIDTH).then_some(self.width)
    }
}
