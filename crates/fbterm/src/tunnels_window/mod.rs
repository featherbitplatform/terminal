//! The Tunnels window: the list from `crate::tunnels_panel` under a search bar that filters by
//! tunnel and profile name (fuzzy, like the palette). The window keeps only its query.

use fbterm_session::ssh::tunnel::TunnelKey;
use iced::{Task, window};

use crate::app::{App, Message};

pub mod view;

/// The search bar's placeholder.
pub const SEARCH_PLACEHOLDER: &str = "Search tunnels";
/// What the window says when no profile has a tunnel.
pub const EMPTY: &str = "No tunnels yet. Add them to an SSH profile in the profile editor.";
/// What the window says when the search matches nothing.
pub const NO_MATCHES: &str = "No matching tunnels";
/// The marker on a running tunnel whose definition changed.
pub const RESTART_TO_APPLY: &str = "Restart to apply";

/// What the Tunnels window asks for (it opens through `Action::Tunnels`).
#[derive(Debug, Clone)]
pub enum TunnelsMsg {
    Close,
    Query(String),
    /// The switch: on or off.
    Toggle(TunnelKey),
    Retry(TunnelKey),
}

/// The open window's own state: the search.
#[derive(Debug)]
pub struct TunnelsState {
    pub query: String,
    pub input: iced::widget::Id,
}

impl App {
    pub fn tunnels_window(&self) -> Option<window::Id> {
        self.tunnels_win.as_ref().map(|(id, _)| *id)
    }

    /// Open the Tunnels window with its search focused, or bring it to the front.
    pub(crate) fn open_tunnels(&mut self) -> Task<Message> {
        if let Some((id, state)) = &self.tunnels_win {
            let input = state.input.clone();
            return Task::batch([
                window::gain_focus(*id),
                iced::widget::operation::focus(input),
            ]);
        }
        let (id, open) = crate::chrome::open_window(window::Settings {
            size: iced::Size::new(640.0, 560.0),
            min_size: Some(iced::Size::new(460.0, 320.0)),
            exit_on_close_request: false,
            ..window::Settings::default()
        });
        let state = TunnelsState {
            query: String::new(),
            input: iced::widget::Id::unique(),
        };
        let input = state.input.clone();
        self.tunnels_win = Some((id, state));
        open.then(move |_| iced::widget::operation::focus(input.clone()))
    }

    pub(crate) fn tunnels_msg(&mut self, msg: TunnelsMsg) -> Task<Message> {
        match msg {
            TunnelsMsg::Close => match self.tunnels_window() {
                Some(id) => self.close_window(id),
                None => Task::none(),
            },
            TunnelsMsg::Query(q) => {
                if let Some((_, state)) = &mut self.tunnels_win {
                    state.query = q;
                }
                Task::none()
            }
            TunnelsMsg::Toggle(key) => self.tunnel_toggle(&key),
            TunnelsMsg::Retry(key) => self.tunnel_retry(&key),
        }
    }
}
