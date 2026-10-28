//! The app's tunnels: a registry of every tunnel that has run, keyed by profile and name, with
//! its state as the runtime reports it (`fbterm_session::ssh::tunnel`).
//!
//! - **Starting and stopping.** [`App::tunnel_toggle`] switches a tunnel on or off;
//!   [`App::tunnel_retry_in`] starts a failed (or changed) one again. A start from the UI asks its
//!   questions over the window it was used in: the tunnels panel's window
//!   ([`App::tunnel_toggle_in`]), else the focused terminal window, or the one used last while
//!   another window has focus, and raises it; with no terminal window open, a tunnel that
//!   needs to ask fails with [`NO_WINDOW`]. A tunnel whose definition does not validate is never started: it
//!   fails with the validation message. A stopped tunnel reports nothing more, so the registry
//!   sets `Off` itself.
//! - **Restarts wait for the old instance.** A stopped tunnel releases its port, remote forward
//!   and connection in its own task. A start under the same key while that is under way waits
//!   for it (or a fixed-port remote forward would be refused as taken): a waiter task sends
//!   `Off` on the tunnel sink once the old instance is gone, which no tunnel ever sends.
//! - **"With the shell".** [`App::sync_shell_tunnels`] counts the connected shell panes of a
//!   profile across all windows (always derived from the panes, never a counter of its own).
//!   When the count goes from 0 to 1 the profile's `with_shell` tunnels start; when it reaches 0
//!   they stop, except those switched on by hand with no shell open (`manual`), which run like
//!   on-demand tunnels until switched off. It runs when a shell connects (a reconnect with
//!   Enter included), ends, or its pane, tab or window closes. On relaunch nothing starts by
//!   itself except the `with_shell` tunnels of restored shells, as they connect.
//! - **Profile edits.** A running tunnel keeps the definition it started with. When its
//!   definition or its profile's connection (host, jump chain) changes it is marked
//!   `restart_to_apply`; a deleted tunnel or profile stops it. A running tunnel that was only
//!   renamed moves to its new name, marked `restart_to_apply`; its instance still reports
//!   under the old name (`reports_as`), and no other tunnel starts under that name until it
//!   has stopped.
//! - **Failures** put `Tunnel "{name}" ({profile}) failed: {reason}` on the banner; the line
//!   is remembered per tunnel and removed by its exact text when the tunnel starts again.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use fbterm_config::{
    Profile, ProfileId, ProfileKind, ProfileStore, SshProfile, StartMode, TunnelDef,
    ValidationError,
};
use fbterm_session::ssh::tunnel::{TunnelHandle, TunnelKey, TunnelSink, TunnelState, start_tunnel};
use fbterm_session::ssh::{Prompter, SshPool};
use iced::futures::Stream;
use iced::{Task, window};
use parking_lot::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::app::{App, Message};
use crate::prompts::UiPrompter;
use crate::tabs::TabState;

/// Why a tunnel that needed to ask something failed: no terminal window was open to ask in.
pub(crate) const NO_WINDOW: &str = "Open a terminal window to answer the sign-in question";

/// What starting one tunnel takes (see `start_tunnel`).
pub(crate) struct Start {
    pub store: ProfileStore,
    pub key: TunnelKey,
    pub def: TunnelDef,
    pub prompter: Arc<dyn Prompter>,
    pub retries: Arc<AtomicU32>,
    pub sink: TunnelSink,
}

/// Starts tunnels: the runtime in the app, a fake in tests.
pub(crate) trait TunnelStarter: Send + Sync {
    fn start(&self, start: Start) -> TunnelHandle;
}

/// Starts tunnels on the app's SSH pool.
struct PoolStarter(Arc<SshPool>);

impl TunnelStarter for PoolStarter {
    fn start(&self, s: Start) -> TunnelHandle {
        start_tunnel(
            self.0.clone(),
            s.store,
            s.key,
            s.def,
            s.prompter,
            s.retries,
            s.sink,
        )
    }
}

/// The connection a tunnel rides: its profile's SSH settings and its jump hops', in connection
/// order, tunnels left out.
type Route = Vec<SshProfile>;

fn route(store: &ProfileStore, id: ProfileId) -> Option<Route> {
    let ssh = |p: &Profile| match &p.kind {
        ProfileKind::Ssh(s) => Some(SshProfile {
            tunnels: Vec::new(),
            ..s.clone()
        }),
        ProfileKind::Local(_) => None,
    };
    let mut out: Route = store
        .jump_chain(id)
        .ok()?
        .into_iter()
        .filter_map(ssh)
        .collect();
    out.push(ssh(store.get(id)?)?);
    Some(out)
}

/// The saved definition of `key`, if its profile still has it.
fn definition(store: &ProfileStore, key: &TunnelKey) -> Option<TunnelDef> {
    match &store.get(key.profile)?.kind {
        ProfileKind::Ssh(s) => s.tunnels.iter().find(|t| t.name == key.name).cloned(),
        ProfileKind::Local(_) => None,
    }
}

/// Why `key`'s saved definition cannot run (a hand-edited profiles file can hold one): the
/// first validation error that names it.
fn invalid(store: &ProfileStore, key: &TunnelKey) -> Option<String> {
    let alone = ProfileStore {
        profiles: vec![store.get(key.profile)?.clone()],
    };
    let name = key.name.trim();
    alone
        .validate()
        .into_iter()
        .find(|e| match e {
            ValidationError::EmptyTunnelName { .. } => name.is_empty(),
            ValidationError::DuplicateTunnel { name: n, .. }
            | ValidationError::TunnelPort { name: n, .. }
            | ValidationError::TunnelTargetPort { name: n, .. }
            | ValidationError::EmptyTunnelBind { name: n, .. }
            | ValidationError::EmptyTunnelHost { name: n, .. } => n.trim() == name,
            _ => false,
        })
        .map(|e| e.to_string())
}

/// One tunnel the registry knows.
pub(crate) struct Entry {
    /// The definition the running instance started with.
    pub def_started: Option<TunnelDef>,
    /// The running instance; `None` while off or failed.
    pub handle: Option<TunnelHandle>,
    pub state: TunnelState,
    /// Switched on by hand: it does not stop with the last shell of its profile.
    pub manual: bool,
    /// The definition (or its profile's connection) changed since this instance started.
    pub restart_to_apply: bool,
    /// The connection the running instance started on.
    route_started: Option<Route>,
    /// A stopped instance is still releasing what it held; a start waits for it.
    releasing: bool,
    /// A start waiting for that release, with the window its questions go to.
    queued: Option<Option<window::Id>>,
    /// The key the running instance reports under: its own, or the name it had before a
    /// rename carried it over.
    reports_as: Option<TunnelKey>,
}

impl Entry {
    fn off() -> Self {
        Self {
            def_started: None,
            handle: None,
            state: TunnelState::Off,
            manual: false,
            restart_to_apply: false,
            route_started: None,
            releasing: false,
            queued: None,
            reports_as: None,
        }
    }

    /// Running, or about to start once its previous instance has let go.
    fn running(&self) -> bool {
        self.handle.is_some() || self.queued.is_some()
    }
}

type Events = UnboundedReceiver<(TunnelKey, TunnelState)>;

/// Tunnel states from every tunnel, delivered to the update loop by one subscription.
#[derive(Clone)]
pub(crate) struct TunnelReceiver(Arc<Mutex<Option<Events>>>);

impl Hash for TunnelReceiver {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "fbterm-tunnel-events".hash(state);
    }
}

pub(crate) fn tunnel_stream(
    rx: &TunnelReceiver,
) -> impl Stream<Item = (TunnelKey, TunnelState)> + use<> {
    let rx = rx.0.lock().take();
    iced::futures::stream::unfold(rx, |rx| async move {
        let mut rx = rx?;
        let event = rx.recv().await?;
        Some((event, Some(rx)))
    })
}

/// Every tunnel that has run, and which profiles had a connected shell at the last count.
pub(crate) struct Tunnels {
    pub entries: BTreeMap<TunnelKey, Entry>,
    sink: TunnelSink,
    pub(crate) receiver: TunnelReceiver,
    starter: Arc<dyn TunnelStarter>,
    /// Profiles with at least one connected shell when last counted: the 0-to-1 and 1-to-0
    /// transitions of [`App::sync_shell_tunnels`] are against this.
    shells_up: BTreeSet<ProfileId>,
    /// `with_shell` tunnels the user switched off by hand: they stay off, also through profile
    /// edits, until the next first shell of their profile (or until switched on again).
    off_by_hand: BTreeSet<TunnelKey>,
    /// The banner line each failed tunnel put up, removed by its exact text.
    banner: BTreeMap<TunnelKey, String>,
}

impl Tunnels {
    pub(crate) fn new(pool: Arc<SshPool>) -> Self {
        let (sink, rx) = unbounded_channel();
        Self {
            entries: BTreeMap::new(),
            sink,
            receiver: TunnelReceiver(Arc::new(Mutex::new(Some(rx)))),
            starter: Arc::new(PoolStarter(pool)),
            shells_up: BTreeSet::new(),
            off_by_hand: BTreeSet::new(),
            banner: BTreeMap::new(),
        }
    }

    /// The registry key of the tunnel that reported as `reported`: the entry whose running
    /// instance reports under it (it may have been renamed since), else `reported` itself.
    pub(crate) fn resolve(&self, reported: &TunnelKey) -> TunnelKey {
        self.entries
            .iter()
            .find(|(_, e)| e.handle.is_some() && e.reports_as.as_ref() == Some(reported))
            .map_or_else(|| reported.clone(), |(k, _)| k.clone())
    }

    #[cfg(test)]
    pub(crate) fn set_starter(&mut self, starter: Arc<dyn TunnelStarter>) {
        self.starter = starter;
    }

    #[cfg(test)]
    pub(crate) fn take_receiver(&self) -> Option<Events> {
        self.receiver.0.lock().take()
    }

    /// Whether `key` is running (or starting).
    pub(crate) fn running(&self, key: &TunnelKey) -> bool {
        self.entries.get(key).is_some_and(Entry::running)
    }

    /// The state of `key`; `Off` for a tunnel that never ran.
    pub(crate) fn state(&self, key: &TunnelKey) -> TunnelState {
        self.entries
            .get(key)
            .map_or(TunnelState::Off, |e| e.state.clone())
    }

    /// How many tunnels are up (the tab bar's indicator).
    pub(crate) fn up_count(&self) -> usize {
        self.entries
            .values()
            .filter(|e| e.handle.is_some() && matches!(e.state, TunnelState::Up { .. }))
            .count()
    }

    /// Whether `key`, defined as `def` and off, will start with its profile's next shell: a
    /// `with_shell` tunnel, unless the user switched it off by hand while a shell is open.
    pub(crate) fn waits_for_shell(&self, key: &TunnelKey, def: &TunnelDef) -> bool {
        def.start == StartMode::WithShell
            && !(self.off_by_hand.contains(key) && self.shells_up.contains(&key.profile))
    }

    /// Stop `key` now: it is `Off` at once; its task releases what it held right after.
    fn stop(&mut self, key: &TunnelKey) {
        let Some(entry) = self.entries.get_mut(key) else {
            return;
        };
        entry.queued = None;
        entry.manual = false;
        entry.restart_to_apply = false;
        entry.state = TunnelState::Off;
        if let Some(handle) = entry.handle.take() {
            Self::retire(entry, key, handle, &self.sink);
        }
    }

    /// Let go of an instance that is stopped or has ended. A waiter sends `Off` once it has
    /// released what it held; starts in its profile wait until then. The waiter runs even for
    /// an instance that has already ended: its `Off` then queues behind any report the instance
    /// sent last, so no such report reaches the next instance.
    fn retire(entry: &mut Entry, key: &TunnelKey, handle: TunnelHandle, sink: &TunnelSink) {
        handle.stop();
        // Outside a runtime (unit tests) there is no task to wait for.
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        entry.releasing = true;
        let (key, sink) = (key.clone(), sink.clone());
        rt.spawn(async move {
            handle.stopped().await;
            let _ = sink.send((key, TunnelState::Off));
        });
    }
}

/// The banner line for a failed tunnel, up to the reason.
fn failure_prefix(name: &str, profile: &str) -> String {
    format!("Tunnel \"{name}\" ({profile}) failed: ")
}

impl App {
    /// Switch `key` on or off. Switching on a `with_shell` tunnel with no shell of its profile
    /// connected makes that run on-demand (`manual`): it outlives the shells.
    pub(crate) fn tunnel_toggle(&mut self, key: &TunnelKey) -> Task<Message> {
        self.tunnel_toggle_over(key, self.focused_window())
    }

    /// [`App::tunnel_toggle`] from the tunnels panel of `wid`: its questions are asked there.
    pub(crate) fn tunnel_toggle_in(&mut self, wid: window::Id, key: &TunnelKey) -> Task<Message> {
        let over = self.over(wid);
        self.tunnel_toggle_over(key, over)
    }

    /// [`App::tunnel_retry`] from the tunnels panel of `wid`: its questions are asked there.
    pub(crate) fn tunnel_retry_in(&mut self, wid: window::Id, key: &TunnelKey) -> Task<Message> {
        let over = self.over(wid);
        self.tunnel_retry_over(key, over)
    }

    /// The window a start from `wid` asks over: `wid` while it is a terminal window.
    fn over(&self, wid: window::Id) -> Option<window::Id> {
        Some(wid)
            .filter(|w| self.windows.contains_key(w))
            .or_else(|| self.focused_window())
    }

    fn tunnel_toggle_over(&mut self, key: &TunnelKey, over: Option<window::Id>) -> Task<Message> {
        if self.tunnels.running(key) {
            self.tunnels.stop(key);
            self.tunnels.off_by_hand.insert(key.clone());
        } else if let Some(def) = definition(&self.profiles, key) {
            let manual = def.start == StartMode::OnDemand || self.shell_count(key.profile) == 0;
            self.launch(key, over, manual);
        }
        self.refresh_palettes();
        Task::none()
    }

    /// Start `key` again with its current definition (Retry, or restart to apply): the old
    /// instance, if any, is stopped and released first.
    #[cfg(test)]
    pub(crate) fn tunnel_retry(&mut self, key: &TunnelKey) -> Task<Message> {
        self.tunnel_retry_over(key, self.focused_window())
    }

    fn tunnel_retry_over(&mut self, key: &TunnelKey, over: Option<window::Id>) -> Task<Message> {
        let Some(def) = definition(&self.profiles, key) else {
            return Task::none();
        };
        let manual = self.tunnels.entries.get(key).is_some_and(|e| e.manual)
            || def.start == StartMode::OnDemand
            || self.shell_count(key.profile) == 0;
        self.tunnels.stop(key);
        self.launch(key, over, manual);
        self.refresh_palettes();
        Task::none()
    }

    /// Start `key` with its current definition, asking over `window`. Waits (queued) while a
    /// stopped instance of the same profile is still releasing: its own previous instance, or
    /// the one it was renamed from, may hold the same port.
    fn launch(&mut self, key: &TunnelKey, window: Option<window::Id>, manual: bool) {
        let Some(def) = definition(&self.profiles, key) else {
            return;
        };
        self.tunnels.off_by_hand.remove(key);
        // Blocked while a stopped instance of the profile is releasing, or while a renamed
        // tunnel still runs an instance that reports under this key.
        let blocked = self.tunnels.entries.iter().any(|(k, e)| {
            k.profile == key.profile
                && (e.releasing
                    || (k != key && e.handle.is_some() && e.reports_as.as_ref() == Some(key)))
        });
        self.clear_tunnel_banner(key);
        let entry = self
            .tunnels
            .entries
            .entry(key.clone())
            .or_insert_with(Entry::off);
        entry.manual = manual;
        entry.restart_to_apply = false;
        if let Some(reason) = invalid(&self.profiles, key) {
            entry.queued = None;
            self.tunnel_failed(key, reason);
            return;
        }
        entry.state = TunnelState::Starting;
        if blocked {
            entry.queued = Some(window);
            return;
        }
        entry.queued = None;
        entry.def_started = Some(def.clone());
        entry.route_started = route(&self.profiles, key.profile);
        entry.reports_as = Some(key.clone());
        entry.handle = Some(self.tunnels.starter.start(Start {
            store: self.profiles.clone(),
            key: key.clone(),
            def,
            prompter: Arc::new(UiPrompter::for_tunnel(
                window,
                key.clone(),
                self.prompt_tx.clone(),
            )),
            retries: self.tunnel_retries.clone(),
            sink: self.tunnels.sink.clone(),
        }));
    }

    /// A tunnel reported `state` (or, for `Off`, a stopped instance finished releasing).
    pub(crate) fn tunnel_event(&mut self, key: TunnelKey, state: TunnelState) -> Task<Message> {
        let key = self.tunnels.resolve(&key);
        let Some(entry) = self.tunnels.entries.get_mut(&key) else {
            return Task::none();
        };
        match state {
            TunnelState::Off => {
                entry.releasing = false;
                if !entry.running() && definition(&self.profiles, &key).is_none() {
                    self.tunnels.entries.remove(&key);
                }
                self.launch_queued(key.profile);
            }
            // A late report of an instance already stopped.
            _ if entry.handle.is_none() => {}
            TunnelState::Failed(reason) => self.tunnel_failed(&key, reason),
            state => entry.state = state,
        }
        self.refresh_palettes();
        Task::none()
    }

    /// `key` (a registry key) failed for `reason`: its instance is let go, and the banner says
    /// so (replacing the line of its previous failure, if any).
    pub(crate) fn tunnel_failed(&mut self, key: &TunnelKey, reason: String) {
        let Some(entry) = self.tunnels.entries.get_mut(key) else {
            return;
        };
        entry.queued = None;
        if let Some(handle) = entry.handle.take() {
            Tunnels::retire(entry, key, handle, &self.tunnels.sink);
        }
        entry.state = TunnelState::Failed(reason.clone());
        let profile = self
            .profiles
            .get(key.profile)
            .map_or_else(|| key.profile.to_string(), |p| p.name.clone());
        self.clear_tunnel_banner(key);
        let line = format!("{}{reason}", failure_prefix(&key.name, &profile));
        self.notices.push(line.clone());
        self.tunnels.banner.insert(key.clone(), line);
        self.notices_dismissed = false;
        self.refresh_palettes();
    }

    /// Take down the banner line `key`'s last failure put up, if it is still there.
    fn clear_tunnel_banner(&mut self, key: &TunnelKey) {
        if let Some(line) = self.tunnels.banner.remove(key)
            && let Some(i) = self.notices.iter().position(|n| *n == line)
        {
            self.notices.remove(i);
        }
    }

    /// Start the queued tunnels of `profile` once none of its stopped instances is releasing.
    fn launch_queued(&mut self, profile: ProfileId) {
        let entries = &self.tunnels.entries;
        if entries
            .iter()
            .any(|(k, e)| k.profile == profile && e.releasing)
        {
            return;
        }
        let ready: Vec<(TunnelKey, Option<window::Id>, bool)> = entries
            .iter()
            .filter(|(k, _)| k.profile == profile)
            .filter_map(|(k, e)| e.queued.map(|w| (k.clone(), w, e.manual)))
            .collect();
        for (key, window, manual) in ready {
            self.launch(&key, window, manual);
        }
    }

    /// Start `profile`'s `with_shell` tunnels that are not running, except those switched off
    /// by hand and, with `keep_failed`, those that failed.
    fn start_with_shell(&mut self, profile: ProfileId, keep_failed: bool) {
        let names: Vec<String> = match self.profiles.get(profile).map(|p| &p.kind) {
            Some(ProfileKind::Ssh(s)) => s
                .tunnels
                .iter()
                .filter(|t| t.start == StartMode::WithShell)
                .map(|t| t.name.clone())
                .collect(),
            _ => Vec::new(),
        };
        let window = self.focused_window();
        for name in names {
            let key = TunnelKey { profile, name };
            let failed = matches!(self.tunnels.state(&key), TunnelState::Failed(_));
            if !self.tunnels.running(&key)
                && !self.tunnels.off_by_hand.contains(&key)
                && !(keep_failed && failed)
            {
                self.launch(&key, window, false);
            }
        }
    }

    /// Connected shell panes of `profile`, in every window.
    pub(crate) fn shell_count(&self, profile: ProfileId) -> usize {
        self.windows
            .values()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| &t.panes)
            .filter(|p| p.profile == profile && p.state == TabState::Connected)
            .count()
    }

    /// Recount `profile`'s connected shells: at the first one its `with_shell` tunnels start,
    /// at none those not switched on by hand stop.
    pub(crate) fn sync_shell_tunnels(&mut self, profile: ProfileId) {
        let up = self.shell_count(profile) > 0;
        let was = self.tunnels.shells_up.contains(&profile);
        if up && !was {
            self.tunnels.shells_up.insert(profile);
            // The first shell: the mode applies again to tunnels switched off by hand.
            self.tunnels.off_by_hand.retain(|k| k.profile != profile);
            self.start_with_shell(profile, false);
        } else if !up && was {
            self.tunnels.shells_up.remove(&profile);
            let stop: Vec<TunnelKey> = self
                .tunnels
                .entries
                .iter()
                .filter(|(k, e)| k.profile == profile && e.running() && !e.manual)
                .map(|(k, _)| k.clone())
                .collect();
            for key in &stop {
                self.tunnels.stop(key);
            }
        }
        self.refresh_palettes();
    }

    /// [`App::sync_shell_tunnels`] for every profile that had or has a connected shell (after
    /// panes, tabs or windows closed).
    pub(crate) fn sync_all_shell_tunnels(&mut self) {
        let mut profiles = self.tunnels.shells_up.clone();
        profiles.extend(
            self.windows
                .values()
                .flat_map(|w| &w.tabs)
                .flat_map(|t| &t.panes)
                .filter(|p| p.state == TabState::Connected)
                .map(|p| p.profile),
        );
        for profile in profiles {
            self.sync_shell_tunnels(profile);
        }
    }

    /// The profiles changed: running tunnels whose definition or connection changed are marked
    /// `restart_to_apply`; deleted tunnels (or tunnels of deleted profiles) stop. A `with_shell`
    /// tunnel that appears while a shell of its profile is connected (added, renamed, or
    /// switched from on demand) starts at once, unless switched off by hand; a failed one waits
    /// for Retry or the next first shell.
    pub(crate) fn tunnels_profiles_changed(&mut self) {
        let keys: Vec<TunnelKey> = self.tunnels.entries.keys().cloned().collect();
        let mut carried = BTreeSet::new();
        for key in keys {
            match definition(&self.profiles, &key) {
                None => {
                    if let Some(new) = self.renamed_to(&key, &carried) {
                        self.carry_over(&key, &new);
                        carried.insert(new);
                        continue;
                    }
                    self.tunnels.stop(&key);
                    self.tunnels.banner.remove(&key);
                    self.tunnels.off_by_hand.remove(&key);
                    if self.tunnels.entries.get(&key).is_some_and(|e| !e.releasing) {
                        self.tunnels.entries.remove(&key);
                    }
                }
                Some(def) => {
                    let now = route(&self.profiles, key.profile);
                    if let Some(entry) = self.tunnels.entries.get_mut(&key)
                        && entry.handle.is_some()
                    {
                        entry.restart_to_apply =
                            entry.def_started.as_ref() != Some(&def) || entry.route_started != now;
                    }
                }
            }
        }
        let up: Vec<ProfileId> = self.tunnels.shells_up.iter().copied().collect();
        for profile in up {
            self.start_with_shell(profile, true);
        }
    }

    /// The new name of `old`, a running tunnel whose name is gone from its profile, when it
    /// was only renamed: exactly one tunnel of the profile defines what it started with (kind,
    /// addresses and ports) under a name that is not running and not taken by another rename.
    fn renamed_to(&self, old: &TunnelKey, carried: &BTreeSet<TunnelKey>) -> Option<TunnelKey> {
        let entry = self.tunnels.entries.get(old)?;
        let started = entry
            .def_started
            .as_ref()
            .filter(|_| entry.handle.is_some())?;
        let ProfileKind::Ssh(ssh) = &self.profiles.get(old.profile)?.kind else {
            return None;
        };
        let mut candidates = ssh
            .tunnels
            .iter()
            .filter(|t| t.kind == started.kind)
            .map(|t| TunnelKey {
                profile: old.profile,
                name: t.name.clone(),
            })
            .filter(|k| {
                !carried.contains(k)
                    && self
                        .tunnels
                        .entries
                        .get(k)
                        .is_none_or(|e| !e.running() && !e.releasing)
            });
        let new = candidates.next()?;
        candidates.next().is_none().then_some(new)
    }

    /// Move the running tunnel `old` to its new name `new`; it applies the new definition at
    /// its next start.
    fn carry_over(&mut self, old: &TunnelKey, new: &TunnelKey) {
        let Some(mut entry) = self.tunnels.entries.remove(old) else {
            return;
        };
        let def = definition(&self.profiles, new);
        entry.restart_to_apply =
            entry.def_started != def || entry.route_started != route(&self.profiles, new.profile);
        self.tunnels.entries.insert(new.clone(), entry);
        self.tunnels.off_by_hand.remove(old);
        if let Some(line) = self.tunnels.banner.remove(old) {
            self.tunnels.banner.insert(new.clone(), line);
        }
    }
}
