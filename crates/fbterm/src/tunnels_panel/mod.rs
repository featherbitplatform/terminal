//! The tunnels panel, docked at the right edge of a terminal window (one at a time with the
//! file panel), and the tunnels list it shares with the tab bar's indicator and the palette:
//! every tunnel of every SSH profile, grouped by profile (the highlighted profile first),
//! with its addresses and state. The rows are derived from the profiles and the tunnel
//! registry (`crate::tunnels`) every time they are drawn, as plain data so their rules are
//! testable without a GUI.
//!
//! - **Per window.** Each terminal window opens and closes its own panel ([`state`]); the
//!   tab bar's button, the `Tunnels` action and the palette's `Tunnels` row toggle it in the
//!   window they are used in. Used from a window without terminals (profile editor, Settings,
//!   Color schemes, export and import), the action opens the panel in the terminal window
//!   focused last and brings that window forward.
//! - **Questions** of a tunnel switched on (or retried) from the panel are asked over the
//!   panel's window.
//! - **Keyboard**, as in the file panel: the panel opens with its search field focused; a
//!   click in the panel gives it the keyboard, a click in the terminal, Escape on an empty
//!   search, another pane or tab taking focus or the panel closing gives it back. While the
//!   panel has it, typing goes to the search (never to the terminal) and app shortcuts act.

use fbterm_config::{ProfileId, ProfileKind, ProfileStore, TunnelDef, TunnelKind};
use fbterm_session::ssh::tunnel::{TunnelKey, TunnelState};
use iced::{Task, window};

use crate::app::{App, Message};
use crate::palette::{Fuzzy, PaletteItem};
use crate::tunnels::Tunnels;

pub(crate) mod state;
pub mod view;

#[cfg(test)]
mod tests;

pub(crate) use state::TunnelsPanels;

/// The search bar's placeholder.
pub const SEARCH_PLACEHOLDER: &str = "Search tunnels";
/// What the panel says when no profile has a tunnel.
pub const EMPTY: &str = "No tunnels yet. Add them to an SSH profile in the profile editor.";
/// What the panel says when the search matches nothing.
pub const NO_MATCHES: &str = "No matching tunnels";
/// The marker on a running tunnel whose definition changed.
pub const RESTART_TO_APPLY: &str = "Restart to apply";

/// What a window's tunnels panel asks for (it opens through `Action::Tunnels`).
#[derive(Debug, Clone)]
pub enum TunnelsMsg {
    /// The close button.
    Close(window::Id),
    Query(window::Id, String),
    /// The panel takes the keyboard (`true`, a click in it) or gives it back to the terminal
    /// (`false`, a click in the terminal).
    Focus(window::Id, bool),
    /// Escape while the panel has the keyboard: clears the search, else gives it back.
    Escape(window::Id),
    /// Text typed while the panel has the keyboard but its search field does not.
    Type(window::Id, String),
    /// The switch: on or off.
    Toggle(window::Id, TunnelKey),
    Retry(window::Id, TunnelKey),
    /// Dragging the resize handle: the new width.
    Resize(f32),
    /// The drag ended: the width is remembered.
    ResizeEnd,
}

impl App {
    /// The terminal window focused last; any terminal window when that one is gone.
    pub(crate) fn last_terminal_window(&self) -> Option<window::Id> {
        self.last_terminal
            .filter(|id| self.windows.contains_key(id))
            .or_else(|| self.windows.keys().next().copied())
    }

    /// Open `wid`'s panel if closed, close it if open.
    pub(crate) fn toggle_tunnels_panel(&mut self, wid: window::Id) -> Task<Message> {
        if self.tunnels_panels.is_open(wid) {
            self.close_tunnels_panel(wid);
            Task::none()
        } else {
            self.open_tunnels_panel(wid)
        }
    }

    /// Open `wid`'s panel (closing its file panel) with its search focused.
    pub(crate) fn open_tunnels_panel(&mut self, wid: window::Id) -> Task<Message> {
        if !self.windows.contains_key(&wid) {
            return Task::none();
        }
        if self.file_panels.is_open(wid) {
            self.close_file_panel(wid);
        }
        let input = self.tunnels_panels.show(wid);
        iced::widget::operation::focus(input)
    }

    /// Hide `wid`'s panel; the keyboard goes back to the terminal.
    pub(crate) fn close_tunnels_panel(&mut self, wid: window::Id) {
        self.tunnels_panels.hide(wid);
    }

    /// The tab bar's tunnels button shows its active style: `wid`'s panel is open.
    pub(crate) fn tunnels_button_active(&self, wid: window::Id) -> bool {
        self.tunnels_panels.is_open(wid)
    }

    /// The profile `wid`'s panel highlights: its focused pane's.
    pub(crate) fn tunnels_highlight(&self, wid: window::Id) -> Option<ProfileId> {
        let tab = self.windows.get(&wid)?.active_tab()?;
        Some(tab.focused_pane().profile)
    }

    /// Give `wid`'s panel the keyboard, or give it back to the terminal; the search field lets
    /// go of it then, so nothing types into it or blinks there.
    fn focus_tunnels_panel(&mut self, wid: window::Id, focused: bool) -> Task<Message> {
        if !self.tunnels_panels.is_open(wid) {
            return Task::none();
        }
        if focused {
            self.tunnels_panels.focused.insert(wid);
            return Task::none();
        }
        self.tunnels_panels.focused.remove(&wid);
        match self.tunnels_panels.input.get(&wid) {
            Some(input) => crate::ui::unfocus(vec![input.clone()]),
            None => Task::none(),
        }
    }

    /// A panel keeps the keyboard only while its window's focused pane stays the same:
    /// another pane or tab taking focus gives it back to the terminal (as for the file panel).
    /// Returns the task that lets go of the search fields of panels that lost it, if any.
    pub(crate) fn sync_tunnels_focus(&mut self) -> Option<Task<Message>> {
        if self.tunnels_panels.open.is_empty() {
            return None;
        }
        let mut tasks = Vec::new();
        let open: Vec<window::Id> = self.tunnels_panels.open.iter().copied().collect();
        for wid in open {
            let pane = self
                .windows
                .get(&wid)
                .and_then(crate::tabs::WindowState::active_tab)
                .map(|tab| tab.focused);
            let before = match pane {
                Some(p) => self.tunnels_panels.pane.insert(wid, p),
                None => self.tunnels_panels.pane.remove(&wid),
            };
            if before.is_some() && before != pane && self.tunnels_panels.has_focus(wid) {
                tasks.push(self.focus_tunnels_panel(wid, false));
            }
        }
        (!tasks.is_empty()).then(|| Task::batch(tasks))
    }

    /// The `Tunnels` action used in `from`: a terminal window toggles its own panel; any
    /// other window opens the panel in the terminal window focused last and raises it.
    pub(crate) fn tunnels_action(&mut self, from: Option<window::Id>) -> Task<Message> {
        if let Some(wid) = from.filter(|w| self.windows.contains_key(w)) {
            return self.toggle_tunnels_panel(wid);
        }
        match self.last_terminal_window() {
            Some(wid) => Task::batch([window::gain_focus(wid), self.open_tunnels_panel(wid)]),
            None => Task::none(),
        }
    }

    pub(crate) fn tunnels_msg(&mut self, msg: TunnelsMsg) -> Task<Message> {
        match msg {
            TunnelsMsg::Close(wid) => self.close_tunnels_panel(wid),
            TunnelsMsg::Query(wid, q) => {
                if self.windows.contains_key(&wid) {
                    self.tunnels_panels.query.insert(wid, q);
                }
            }
            TunnelsMsg::Focus(wid, focused) => return self.focus_tunnels_panel(wid, focused),
            TunnelsMsg::Escape(wid) => {
                if !self.tunnels_panels.has_focus(wid) {
                    return Task::none();
                }
                if self.tunnels_panels.query(wid).is_empty() {
                    return self.focus_tunnels_panel(wid, false);
                }
                self.tunnels_panels.query.insert(wid, String::new());
            }
            TunnelsMsg::Type(wid, typed) => {
                if !self.tunnels_panels.has_focus(wid) {
                    return Task::none();
                }
                let query = format!("{}{typed}", self.tunnels_panels.query(wid));
                self.tunnels_panels.query.insert(wid, query);
                if let Some(input) = self.tunnels_panels.input.get(&wid) {
                    return Task::batch([
                        iced::widget::operation::focus(input.clone()),
                        iced::widget::operation::move_cursor_to_end(input.clone()),
                    ]);
                }
            }
            TunnelsMsg::Toggle(wid, key) => return self.tunnel_toggle_in(wid, &key),
            TunnelsMsg::Retry(wid, key) => return self.tunnel_retry_in(wid, &key),
            TunnelsMsg::Resize(width) => self.tunnels_panels.width = state::clamp_width(width),
            TunnelsMsg::ResizeEnd => self.save_state(),
        }
        Task::none()
    }
}

/// How a state reads at a glance: the color of its dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Up,
    /// Starting or reconnecting.
    Busy,
    Failed,
    Off,
}

/// One tunnel as the panel lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub key: TunnelKey,
    pub name: String,
    /// The addresses in plain form, e.g. `127.0.0.1:5432 → db.internal:5432 via bastion`.
    pub address: String,
    pub state: String,
    pub tone: Tone,
    pub restart_to_apply: bool,
    /// The switch: running or starting.
    pub on: bool,
    /// Failed: Retry is offered.
    pub retry: bool,
}

/// The tunnels of one profile.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub profile_id: ProfileId,
    pub profile: String,
    pub rows: Vec<Row>,
    /// The profile the panel was opened for: its group is listed first.
    pub highlighted: bool,
}

/// A state as the panel, the palette and the tunnel's row say it.
pub fn state_label(state: &TunnelState) -> String {
    match state {
        TunnelState::Off => "Off".into(),
        TunnelState::Starting => "Starting".into(),
        TunnelState::Up { connections: 0 } => "Up".into(),
        TunnelState::Up { connections: 1 } => "Up \u{b7} 1 connection".into(),
        TunnelState::Up { connections } => format!("Up \u{b7} {connections} connections"),
        TunnelState::Reconnecting { attempt, max } => {
            format!("Reconnecting (attempt {attempt} of {max})")
        }
        TunnelState::Failed(reason) => format!("Failed: {reason}"),
    }
}

fn tone(state: &TunnelState) -> Tone {
    match state {
        TunnelState::Up { .. } => Tone::Up,
        TunnelState::Starting | TunnelState::Reconnecting { .. } => Tone::Busy,
        TunnelState::Failed(_) => Tone::Failed,
        TunnelState::Off => Tone::Off,
    }
}

/// `bind:port`, an IPv6 bind in brackets.
fn endpoint(bind: &str, port: u16) -> String {
    let bind = bind.trim();
    if bind.contains(':') && !bind.starts_with('[') {
        format!("[{bind}]:{port}")
    } else {
        format!("{bind}:{port}")
    }
}

/// The addresses of `def` in plain form: `{bind}:{port} → {host}:{port}` for a local forward,
/// `remote …` for a remote one, `SOCKS {bind}:{port}`, then `via` the jump hops, if any.
/// `listening`: the port a running instance listens on, shown for a definition that let the
/// system (or the server) pick one.
pub fn address(
    store: &ProfileStore,
    key: &TunnelKey,
    def: &TunnelDef,
    listening: Option<u16>,
) -> String {
    let pick = |port: u16| match (port, listening) {
        (0, Some(p)) => p,
        _ => port,
    };
    let mut out = match &def.kind {
        TunnelKind::Local {
            bind,
            port,
            host,
            host_port,
        } => format!(
            "{} \u{2192} {}",
            endpoint(bind, pick(*port)),
            endpoint(host, *host_port)
        ),
        TunnelKind::Remote {
            bind,
            port,
            host,
            host_port,
        } => format!(
            "remote {} \u{2192} {}",
            endpoint(bind, pick(*port)),
            endpoint(host, *host_port)
        ),
        TunnelKind::Socks { bind, port } => format!("SOCKS {}", endpoint(bind, pick(*port))),
    };
    if let Ok(hops) = store.jump_chain(key.profile)
        && !hops.is_empty()
    {
        let names: Vec<&str> = hops.iter().map(|h| h.name.as_str()).collect();
        out.push_str(" via ");
        out.push_str(&names.join(" \u{2192} "));
    }
    out
}

/// Every tunnel definition, by profile in the store's order and by tunnel in the profile's.
fn defined(store: &ProfileStore) -> impl Iterator<Item = (&str, TunnelKey, &TunnelDef)> {
    store.profiles.iter().flat_map(|p| {
        let tunnels: &[TunnelDef] = match &p.kind {
            ProfileKind::Ssh(s) => &s.tunnels,
            ProfileKind::Local(_) => &[],
        };
        tunnels.iter().map(move |t| {
            let key = TunnelKey {
                profile: p.id,
                name: t.name.clone(),
            };
            (p.name.as_str(), key, t)
        })
    })
}

/// Whether any profile has a tunnel (else the panel says where to add one).
pub fn any_defined(store: &ProfileStore) -> bool {
    defined(store).next().is_some()
}

/// What `key` (defined as `def`) is doing, as a label, a tone and its switch.
fn status(tunnels: &Tunnels, key: &TunnelKey, def: &TunnelDef) -> (String, Tone, bool) {
    let state = tunnels.state(key);
    let on = tunnels.running(key);
    if state == TunnelState::Off && !on && tunnels.waits_for_shell(key, def) {
        return ("Starts with the shell".into(), Tone::Off, false);
    }
    (state_label(&state), tone(&state), on)
}

/// The list: the profiles that have tunnels, each with the tunnels whose name or profile name
/// matches `query` (all of them for a blank query). The group of `highlight` comes first and is
/// marked; the others keep the profiles' order. A `highlight` that matches no group (unknown
/// profile, a local one, one without tunnels, or filtered out by `query`) marks nothing.
pub fn groups(
    store: &ProfileStore,
    tunnels: &Tunnels,
    query: &str,
    highlight: Option<ProfileId>,
) -> Vec<Group> {
    let mut fuzzy = (!query.trim().is_empty()).then(|| Fuzzy::new(query));
    let mut out: Vec<Group> = Vec::new();
    for (profile, key, def) in defined(store) {
        if let Some(f) = &mut fuzzy
            && f.score(&format!("{profile} {}", def.name)).is_none()
        {
            continue;
        }
        let entry = tunnels.entries.get(&key);
        let listening = entry.and_then(|e| e.handle.as_ref()).and_then(|h| h.port());
        // While it runs, what is actually listening (an edit waits for the restart); off, what
        // the next start uses.
        let shown = entry
            .filter(|e| e.handle.is_some())
            .and_then(|e| e.def_started.as_ref())
            .unwrap_or(def);
        let (state, tone, on) = status(tunnels, &key, def);
        let row = Row {
            name: def.name.clone(),
            address: address(store, &key, shown, listening),
            state,
            tone,
            restart_to_apply: on && entry.is_some_and(|e| e.restart_to_apply),
            on,
            retry: tone == Tone::Failed,
            key,
        };
        match out.last_mut() {
            Some(g)
                if g.rows
                    .first()
                    .is_some_and(|r| r.key.profile == row.key.profile) =>
            {
                g.rows.push(row);
            }
            _ => out.push(Group {
                profile_id: row.key.profile,
                profile: profile.to_owned(),
                rows: vec![row],
                highlighted: false,
            }),
        }
    }
    if let Some(at) = highlight.and_then(|h| out.iter().position(|g| g.profile_id == h)) {
        out[at].highlighted = true;
        out[..=at].rotate_right(1);
    }
    out
}

/// The palette's tunnel rows: `Tunnel: {profile} / {name}`, with the state.
pub fn palette_rows(store: &ProfileStore, tunnels: &Tunnels) -> Vec<PaletteItem> {
    defined(store)
        .map(|(profile, key, def)| {
            let (state, _, _) = status(tunnels, &key, def);
            PaletteItem::Tunnel {
                label: format!("Tunnel: {profile} / {}", def.name),
                state,
                key,
            }
        })
        .collect()
}

/// What the tab bar's indicator shows next to its icon: the number of tunnels up, nothing when
/// none is.
pub fn indicator_count(up: usize) -> Option<String> {
    (up > 0).then(|| up.to_string())
}
