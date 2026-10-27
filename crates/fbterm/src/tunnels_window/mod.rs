//! The Tunnels window: every tunnel of every SSH profile, grouped by profile, with its
//! addresses, state, an on/off switch and Retry when it failed, under a search bar that
//! filters by tunnel and profile name (fuzzy, like the palette). Also what the tab bar's
//! indicator and the palette's tunnel rows show. The rows are derived from the profiles and the
//! tunnel registry (`crate::tunnels`) every time they are drawn, as plain data so their rules
//! are testable without a GUI; the window keeps only its query.

use fbterm_config::{ProfileKind, ProfileStore, TunnelDef, TunnelKind};
use fbterm_session::ssh::tunnel::{TunnelKey, TunnelState};
use iced::{Task, window};

use crate::app::{App, Message};
use crate::chrome::Chrome;
use crate::palette::{Fuzzy, PaletteItem};
use crate::tunnels::Tunnels;

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

/// How a state reads at a glance: the color of its dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Up,
    /// Starting or reconnecting.
    Busy,
    Failed,
    Off,
}

/// One tunnel as the window lists it.
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
    pub profile: String,
    pub rows: Vec<Row>,
}

/// A state as the window, the palette and the tunnel's row say it.
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

/// Whether any profile has a tunnel (else the window shows how to add one).
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

/// The window's list: the profiles that have tunnels, each with the tunnels whose name or
/// profile name matches `query` (all of them for a blank query).
pub fn groups(store: &ProfileStore, tunnels: &Tunnels, query: &str) -> Vec<Group> {
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
                profile: profile.to_owned(),
                rows: vec![row],
            }),
        }
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
        let (id, open) = window::open(Chrome::current().window_settings(window::Settings {
            size: iced::Size::new(640.0, 560.0),
            min_size: Some(iced::Size::new(460.0, 320.0)),
            exit_on_close_request: false,
            ..window::Settings::default()
        }));
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
