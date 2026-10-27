//! The tunnel registry inside the app: toggles, "with the shell" lifetimes, profile edits,
//! failure notices and relaunch. A fake starter stands in for the tunnel runtime, so no
//! network is needed; one test at the end goes through the real runtime against a closed port.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use fbterm_config::{
    AuthMethod, Paths, Profile, ProfileKind, SavedState, SshProfile, StartMode, TunnelDef,
    TunnelKind,
};
use fbterm_session::SessionHandle;
use fbterm_session::ssh::tunnel::{TunnelHandle, TunnelKey, TunnelState};
use fbterm_term::TerminalOutput;
use iced::window;
use parking_lot::Mutex;

use crate::app::{App, DEFAULT_GRID, Delivered, Message};
use crate::layout::PaneId;
use crate::restore_tests::{paths_with, profile, saved};
use crate::tabs::{Pane, Tab, TabState, WindowState};
use crate::tunnels::TunnelStarter;

/// Records every start and hands back a handle with nothing behind it.
#[derive(Default)]
struct Fake {
    starts: Mutex<Vec<(TunnelKey, TunnelDef)>>,
}

impl TunnelStarter for Fake {
    fn start(&self, start: crate::tunnels::Start) -> TunnelHandle {
        self.starts.lock().push((start.key, start.def));
        TunnelHandle::inert()
    }
}

impl Fake {
    fn count(&self, name: &str) -> usize {
        self.starts
            .lock()
            .iter()
            .filter(|(k, _)| k.name == name)
            .count()
    }
}

fn local(name: &str, port: u16, start: StartMode) -> TunnelDef {
    TunnelDef {
        name: name.into(),
        kind: TunnelKind::Local {
            bind: "127.0.0.1".into(),
            port,
            host: "db.internal".into(),
            host_port: 5432,
        },
        start,
    }
}

/// An SSH profile `srv` with an on-demand tunnel `web` and a with-the-shell tunnel `db`.
fn srv() -> Profile {
    srv_on(1)
}

/// `srv`, its server at `127.0.0.1:port`.
fn srv_on(port: u16) -> Profile {
    Profile {
        kind: ProfileKind::Ssh(SshProfile {
            host: "127.0.0.1".into(),
            port,
            user: "user".into(),
            auth: AuthMethod::Agent,
            jump: vec![],
            tunnels: vec![
                local("web", 18080, StartMode::OnDemand),
                local("db", 15432, StartMode::WithShell),
            ],
        }),
        ..profile("srv")
    }
}

fn key(p: &Profile, name: &str) -> TunnelKey {
    TunnelKey {
        profile: p.id,
        name: name.into(),
    }
}

struct World {
    app: App,
    fake: Arc<Fake>,
    wid: window::Id,
    srv: Profile,
}

fn world() -> World {
    let srv = srv();
    // A local profile whose program does not exist keeps the default window from spawning a
    // real shell.
    let paths = paths_with(&[profile("local"), srv.clone()]);
    world_at(paths, srv)
}

fn world_at(paths: Paths, srv: Profile) -> World {
    let (mut app, _) = App::build(paths, true);
    app.state_saving_enabled = false;
    let fake = Arc::new(Fake::default());
    app.tunnels.set_starter(fake.clone());
    let wid = window::Id::unique();
    app.windows.clear();
    app.windows.insert(wid, WindowState::new());
    app.focused = Some(wid);
    World {
        app,
        fake,
        wid,
        srv,
    }
}

impl World {
    /// A new tab of `srv` whose shell has connected.
    fn open_shell(&mut self) -> PaneId {
        let pane = Pane::for_profile(&self.srv);
        let id = pane.id;
        self.app
            .windows
            .get_mut(&self.wid)
            .unwrap()
            .push(Tab::single(pane));
        self.connect(id);
        id
    }

    /// The dial of `pane` finished.
    fn connect(&mut self, pane: PaneId) {
        let handle = SessionHandle::inert(self.app.new_terminal(DEFAULT_GRID));
        let _ = self.app.update(Message::Connected(
            pane,
            Delivered::new(handle),
            DEFAULT_GRID,
        ));
        assert_eq!(self.pane(pane).state, TabState::Connected);
    }

    fn pane(&self, id: PaneId) -> &Pane {
        let w = &self.app.windows[&self.wid];
        let (t, p) = w.locate(id).unwrap();
        &w.tabs[t].panes[p]
    }

    /// The shell of `pane` dropped (its server went away).
    fn disconnect(&mut self, pane: PaneId) {
        let sid = self.pane(pane).session.as_ref().unwrap().id;
        let _ = self.app.update(Message::Session(
            sid,
            fbterm_session::SessionEvent::Disconnected("connection lost".into()),
        ));
        assert!(matches!(self.pane(pane).state, TabState::Disconnected(_)));
    }

    /// Enter in a disconnected pane: it dials again.
    fn press_enter(&mut self, pane: PaneId) {
        let _ = self.app.update(Message::Terminal(
            self.wid,
            pane,
            TerminalOutput::Input(b"\r".to_vec()),
        ));
        assert_eq!(self.pane(pane).state, TabState::Connecting);
    }

    fn close_tab_of(&mut self, pane: PaneId) {
        let (t, _) = self.app.windows[&self.wid].locate(pane).unwrap();
        let _ = self.app.close_tab(self.wid, t);
    }

    fn running(&self, name: &str) -> bool {
        self.app.tunnels.running(&key(&self.srv, name))
    }

    fn state(&self, name: &str) -> TunnelState {
        self.app.tunnels.state(&key(&self.srv, name))
    }

    fn toggle(&mut self, name: &str) {
        let k = key(&self.srv, name);
        let _ = self.app.tunnel_toggle(&k);
    }

    fn edit(&mut self, f: impl FnOnce(&mut Profile)) {
        let p = self
            .app
            .profiles
            .profiles
            .iter_mut()
            .find(|p| p.id == self.srv.id)
            .unwrap();
        f(p);
        self.app.profiles_changed();
    }
}

fn tunnels_of(p: &mut Profile) -> &mut Vec<TunnelDef> {
    match &mut p.kind {
        ProfileKind::Ssh(s) => &mut s.tunnels,
        ProfileKind::Local(_) => unreachable!(),
    }
}

#[test]
fn toggle_starts_and_stops() {
    let mut w = world();
    assert!(!w.running("web"));
    assert_eq!(w.state("web"), TunnelState::Off);

    w.toggle("web");
    assert!(w.running("web"));
    assert_eq!(w.state("web"), TunnelState::Starting);
    assert_eq!(w.fake.count("web"), 1);
    let entry = &w.app.tunnels.entries[&key(&w.srv, "web")];
    assert_eq!(
        entry.def_started,
        Some(local("web", 18080, StartMode::OnDemand))
    );
    assert!(entry.manual);

    // States the runtime reports land in the registry.
    let _ = w.app.update(Message::Tunnel(
        key(&w.srv, "web"),
        TunnelState::Up { connections: 2 },
    ));
    assert_eq!(w.state("web"), TunnelState::Up { connections: 2 });

    w.toggle("web");
    assert!(!w.running("web"));
    // A stopped tunnel reports nothing more: the registry says Off itself.
    assert_eq!(w.state("web"), TunnelState::Off);
    assert_eq!(w.fake.count("web"), 1);
    // Nothing comes back to life on a late report.
    let _ = w.app.update(Message::Tunnel(
        key(&w.srv, "web"),
        TunnelState::Up { connections: 0 },
    ));
    assert_eq!(w.state("web"), TunnelState::Off);

    w.toggle("web");
    assert!(w.running("web"));
    assert_eq!(w.fake.count("web"), 2);
}

#[test]
fn with_shell_follows_last_shell() {
    let mut w = world();
    let first = w.open_shell();
    assert!(
        w.running("db"),
        "the first shell starts the with-shell tunnel"
    );
    assert!(
        !w.running("web"),
        "an on-demand tunnel waits to be switched on"
    );
    assert_eq!(w.fake.count("db"), 1);
    let entry = &w.app.tunnels.entries[&key(&w.srv, "db")];
    assert!(!entry.manual);

    let second = w.open_shell();
    assert_eq!(w.fake.count("db"), 1, "a second shell starts nothing more");

    w.close_tab_of(first);
    assert!(w.running("db"), "one shell is still open");

    w.disconnect(second);
    assert!(!w.running("db"), "the last shell disconnected");
    assert_eq!(w.state("db"), TunnelState::Off);

    // Closing the last shell (not only a disconnect) stops it as well.
    let third = w.open_shell();
    assert!(w.running("db"));
    assert_eq!(w.fake.count("db"), 2);
    w.close_tab_of(third);
    assert!(!w.running("db"));
    // So does closing the window that held it.
    let _fourth = w.open_shell();
    assert!(w.running("db"));
    let _ = w.app.update(Message::WindowClosed(w.wid));
    assert!(!w.running("db"));
}

#[test]
fn reconnected_shell_restarts_with_shell_tunnels() {
    let mut w = world();
    let pane = w.open_shell();
    assert!(w.running("db"));
    w.disconnect(pane);
    assert!(!w.running("db"));

    w.press_enter(pane);
    assert!(!w.running("db"), "dialling is not connected yet");
    w.connect(pane);
    assert!(w.running("db"));
    assert_eq!(w.fake.count("db"), 2);
}

#[test]
fn manual_with_shell_tunnel_survives_last_shell() {
    let mut w = world();
    // Switched on by hand with no shell open: an on-demand run.
    w.toggle("db");
    assert!(w.running("db"));
    assert!(w.app.tunnels.entries[&key(&w.srv, "db")].manual);

    let pane = w.open_shell();
    assert_eq!(w.fake.count("db"), 1, "already running: not started twice");
    w.disconnect(pane);
    assert!(
        w.running("db"),
        "a run switched on by hand outlives the shells"
    );
    w.close_tab_of(pane);
    assert!(w.running("db"));

    // Switched off by hand, the mode applies again with the next shell.
    w.toggle("db");
    assert!(!w.running("db"));
    let pane = w.open_shell();
    assert!(w.running("db"));
    assert!(!w.app.tunnels.entries[&key(&w.srv, "db")].manual);
    w.close_tab_of(pane);
    assert!(!w.running("db"));
}

#[test]
fn edit_marks_restart_to_apply() {
    let mut w = world();
    w.toggle("web");
    let k = key(&w.srv, "web");
    assert!(!w.app.tunnels.entries[&k].restart_to_apply);

    // Another tunnel's edit does not touch this one.
    w.edit(|p| tunnels_of(p)[1].kind = local("db", 15433, StartMode::WithShell).kind);
    assert!(!w.app.tunnels.entries[&k].restart_to_apply);

    w.edit(|p| tunnels_of(p)[0].kind = local("web", 18081, StartMode::OnDemand).kind);
    let entry = &w.app.tunnels.entries[&k];
    assert!(entry.restart_to_apply);
    assert!(w.running("web"), "the running tunnel keeps its definition");
    assert_eq!(
        entry.def_started,
        Some(local("web", 18080, StartMode::OnDemand))
    );

    // Editing it back clears the marker.
    w.edit(|p| tunnels_of(p)[0].kind = local("web", 18080, StartMode::OnDemand).kind);
    assert!(!w.app.tunnels.entries[&k].restart_to_apply);

    // The profile's own connection (host, jump chain) counts as part of the definition.
    w.edit(|p| match &mut p.kind {
        ProfileKind::Ssh(s) => s.host = "10.0.0.9".into(),
        ProfileKind::Local(_) => unreachable!(),
    });
    assert!(w.app.tunnels.entries[&k].restart_to_apply);

    // A restart applies the new definition and clears the marker.
    w.edit(|p| tunnels_of(p)[0].kind = local("web", 18081, StartMode::OnDemand).kind);
    let _ = w.app.tunnel_retry(&k);
    let entry = &w.app.tunnels.entries[&k];
    assert!(!entry.restart_to_apply);
    assert_eq!(
        entry.def_started,
        Some(local("web", 18081, StartMode::OnDemand))
    );
    assert_eq!(w.fake.count("web"), 2);
}

#[test]
fn delete_stops_tunnel() {
    let mut w = world();
    w.toggle("web");
    w.toggle("db");
    assert!(w.running("web") && w.running("db"));

    w.edit(|p| tunnels_of(p).retain(|t| t.name != "web"));
    assert!(!w.running("web"), "a deleted tunnel stops");
    assert!(w.running("db"));
    assert!(!w.app.tunnels.entries.contains_key(&key(&w.srv, "web")));

    let id = w.srv.id;
    w.app.profiles.profiles.retain(|p| p.id != id);
    w.app.profiles_changed();
    assert!(!w.running("db"), "a deleted profile's tunnels stop");
    assert!(w.app.tunnels.entries.is_empty());
}

#[test]
fn failure_pushes_banner() {
    let mut w = world();
    w.app.notices_dismissed = true;
    w.toggle("web");
    let k = key(&w.srv, "web");
    let _ = w.app.update(Message::Tunnel(
        k.clone(),
        TunnelState::Failed("port 18080 already in use".into()),
    ));
    assert_eq!(
        w.state("web"),
        TunnelState::Failed("port 18080 already in use".into())
    );
    let line = "Tunnel \"web\" (srv) failed: port 18080 already in use";
    assert!(!w.app.notices_dismissed);
    assert!(w.app.visible_notices().iter().any(|n| n == line));
    assert!(!w.running("web"), "a failed tunnel is not running");

    // Retry starts it again; the same failure again does not repeat the line.
    let _ = w.app.tunnel_retry(&k);
    assert!(w.running("web"));
    assert_eq!(w.state("web"), TunnelState::Starting);
    assert_eq!(w.fake.count("web"), 2);
    let _ = w.app.update(Message::Tunnel(
        k.clone(),
        TunnelState::Failed("port 18080 already in use".into()),
    ));
    assert_eq!(w.app.notices.iter().filter(|n| *n == line).count(), 1);

    // Reconnecting is not a failure.
    let _ = w.app.tunnel_retry(&k);
    let _ = w.app.update(Message::Tunnel(
        k,
        TunnelState::Reconnecting {
            attempt: 1,
            max: 10,
        },
    ));
    assert!(w.running("web"));
}

#[test]
fn relaunch_starts_only_with_shell_tunnels() {
    let srv = srv();
    let other = profile("local");
    let paths = paths_with(&[other.clone(), srv.clone()]);
    SavedState {
        windows: vec![saved(&[&srv], 0)],
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let (mut app, _) = App::build(paths, true);
    let fake = Arc::new(Fake::default());
    app.tunnels.set_starter(fake.clone());
    app.state_saving_enabled = false;
    let (wid, window) = app.windows.iter().next().unwrap();
    let wid = *wid;
    let pane = window.tabs[0].panes[0].id;
    assert_eq!(window.tabs[0].panes[0].state, TabState::Connecting);
    assert!(
        fake.starts.lock().is_empty(),
        "nothing starts before a shell is up"
    );

    let mut w = World {
        app,
        fake,
        wid,
        srv,
    };
    w.connect(pane);
    assert!(
        w.running("db"),
        "the restored shell brings its with-shell tunnel"
    );
    assert!(!w.running("web"), "on-demand tunnels stay off on relaunch");
    assert_eq!(w.fake.starts.lock().len(), 1);
}

#[test]
fn retry_limit_follows_settings() {
    let mut w = world();
    assert_eq!(w.app.tunnel_retries.load(Ordering::Relaxed), 10);
    let mut s = w.app.settings.clone();
    s.tunnel_max_retries = 3;
    let _ = w.app.apply_settings(s, crate::app::SettingsSource::Window);
    assert_eq!(w.app.tunnel_retries.load(Ordering::Relaxed), 3);
}

/// Through the real runtime: a tunnel switched on for a server that refuses the connection
/// reports `Starting`, then fails with a banner. Needs no server: port 1 on loopback is closed.
#[test]
fn real_runtime_failure_reaches_the_banner() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();
    let srv = srv();
    let paths = paths_with(&[profile("local"), srv.clone()]);
    let (mut app, _) = App::build(paths, true);
    app.state_saving_enabled = false;
    let mut rx = app.tunnels.take_receiver().unwrap();
    let k = key(&srv, "web");
    let _ = app.tunnel_toggle(&k);
    let mut seen = Vec::new();
    rt.block_on(async {
        while !matches!(seen.last(), Some(TunnelState::Failed(_))) {
            let (got, state) = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv())
                .await
                .expect("a state within 30 s")
                .expect("the sink stays open");
            assert_eq!(got, k);
            seen.push(state.clone());
            let _ = app.update(Message::Tunnel(got, state));
        }
    });
    assert_eq!(seen[0], TunnelState::Starting);
    let TunnelState::Failed(reason) = app.tunnels.state(&k) else {
        panic!("not failed: {seen:?}");
    };
    let line = format!("Tunnel \"web\" (srv) failed: {reason}");
    assert!(app.visible_notices().contains(&line), "{line}");
    drop(app);
}

/// Through the real runtime: a restart waits until the stopped instance has let go before the
/// new one starts. The server accepts TCP but never answers, so the first instance is still
/// connecting (and must be wound down by its task) when it is stopped.
#[test]
fn restart_waits_for_the_old_instance() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let srv = srv_on(silent.local_addr().unwrap().port());
    let paths = paths_with(&[profile("local"), srv.clone()]);
    let (mut app, _) = App::build(paths, true);
    app.state_saving_enabled = false;
    let mut rx = app.tunnels.take_receiver().unwrap();
    let k = key(&srv, "web");
    let mut next = |app: &mut App| {
        let (got, state) = rt.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
                .await
                .expect("a state within 10 s")
                .expect("the sink stays open")
        });
        assert_eq!(got, k);
        let _ = app.update(Message::Tunnel(got, state.clone()));
        state
    };

    let _ = app.tunnel_toggle(&k);
    assert_eq!(next(&mut app), TunnelState::Starting);
    let _ = app.tunnel_retry(&k);
    let entry = &app.tunnels.entries[&k];
    assert!(
        entry.handle.is_none(),
        "the new instance waits for the old one"
    );
    assert!(app.tunnels.running(&k));
    assert_eq!(app.tunnels.state(&k), TunnelState::Starting);

    // The old instance has let go: the waiting start goes ahead.
    assert_eq!(next(&mut app), TunnelState::Off);
    assert!(app.tunnels.entries[&k].handle.is_some());
    assert_eq!(next(&mut app), TunnelState::Starting);

    let _ = app.tunnel_toggle(&k);
    assert!(!app.tunnels.running(&k));
    assert_eq!(next(&mut app), TunnelState::Off, "released after the stop");
    assert_eq!(app.tunnels.state(&k), TunnelState::Off);
    drop(app);
}

#[test]
fn added_with_shell_tunnel_starts_when_shell_open() {
    let mut w = world();
    let pane = w.open_shell();
    w.edit(|p| tunnels_of(p).push(local("cache", 16379, StartMode::WithShell)));
    assert!(
        w.running("cache"),
        "a shell is connected: it starts at once"
    );
    assert!(!w.app.tunnels.entries[&key(&w.srv, "cache")].manual);
    assert!(!w.running("web"), "on-demand tunnels still wait");
    w.close_tab_of(pane);
    assert!(!w.running("cache"), "and it stops with the last shell");
}

#[test]
fn renamed_with_shell_tunnel_keeps_running() {
    let mut w = world();
    let _pane = w.open_shell();
    assert!(w.running("db"));
    w.edit(|p| tunnels_of(p)[1].name = "postgres".into());
    assert!(!w.running("db"));
    assert!(!w.app.tunnels.entries.contains_key(&key(&w.srv, "db")));
    assert!(w.running("postgres"), "the renamed tunnel runs on");
    assert_eq!(w.fake.count("postgres"), 1);
}

#[test]
fn switching_to_with_shell_starts_when_shell_open() {
    let mut w = world();
    let pane = w.open_shell();
    assert!(!w.running("web"));
    w.edit(|p| tunnels_of(p)[0].start = StartMode::WithShell);
    assert!(w.running("web"));
    assert!(!w.app.tunnels.entries[&key(&w.srv, "web")].manual);
    w.close_tab_of(pane);
    assert!(!w.running("web"));
}

#[test]
fn tunnel_switched_off_by_hand_stays_off_after_edit() {
    let mut w = world();
    let pane = w.open_shell();
    assert!(w.running("db"));
    w.toggle("db");
    assert!(!w.running("db"));
    // An edit to the tunnel, or to another one, does not bring it back.
    w.edit(|p| tunnels_of(p)[0].kind = local("web", 18081, StartMode::OnDemand).kind);
    w.edit(|p| tunnels_of(p)[1].kind = local("db", 15433, StartMode::WithShell).kind);
    assert!(!w.running("db"));
    // A second shell is not a first shell.
    let second = w.open_shell();
    assert!(!w.running("db"));
    // The next first shell is.
    w.close_tab_of(pane);
    w.close_tab_of(second);
    let _third = w.open_shell();
    assert!(w.running("db"));
    assert_eq!(w.fake.count("db"), 2);
}

#[test]
fn failed_with_shell_tunnel_waits_through_edits() {
    let mut w = world();
    let _pane = w.open_shell();
    let k = key(&w.srv, "db");
    let _ = w
        .app
        .update(Message::Tunnel(k, TunnelState::Failed("refused".into())));
    w.edit(|p| tunnels_of(p)[0].kind = local("web", 18081, StartMode::OnDemand).kind);
    assert!(!w.running("db"), "an edit does not retry a failed tunnel");
    assert_eq!(w.fake.count("db"), 1);
}

#[test]
fn closing_the_last_pane_stops_with_shell_tunnels() {
    let mut w = world();
    let (a, b) = (Pane::for_profile(&w.srv), Pane::for_profile(&w.srv));
    let (ia, ib) = (a.id, b.id);
    let mut tab = Tab::single(a);
    tab.layout.split(ia, crate::layout::Side::Right, ib);
    tab.panes.push(b);
    tab.focused = ib;
    w.app.windows.get_mut(&w.wid).unwrap().push(tab);
    w.connect(ia);
    w.connect(ib);
    assert!(w.running("db"));
    let _ = w.app.close_focused_pane(w.wid);
    assert!(w.running("db"), "one pane is still connected");
    w.disconnect(ia);
    assert!(!w.running("db"));
}

// The tunnels panel, the tab-bar indicator and the palette rows.

/// `srv` (tunnels `web` and `db`), a `bastion` without tunnels, and `warehouse` reached through
/// it with a remote forward `hook` and a SOCKS proxy `proxy`.
fn panel_world() -> (World, Profile) {
    let srv = srv();
    let bastion = Profile {
        kind: ProfileKind::Ssh(SshProfile {
            host: "bastion.example".into(),
            port: 22,
            user: "ops".into(),
            auth: AuthMethod::Agent,
            jump: vec![],
            tunnels: vec![],
        }),
        ..profile("bastion")
    };
    let warehouse = Profile {
        kind: ProfileKind::Ssh(SshProfile {
            host: "warehouse.internal".into(),
            port: 22,
            user: "ops".into(),
            auth: AuthMethod::Agent,
            jump: vec![bastion.id],
            tunnels: vec![
                TunnelDef {
                    name: "hook".into(),
                    kind: TunnelKind::Remote {
                        bind: "127.0.0.1".into(),
                        port: 9000,
                        host: "localhost".into(),
                        host_port: 3000,
                    },
                    start: StartMode::OnDemand,
                },
                TunnelDef {
                    name: "proxy".into(),
                    kind: TunnelKind::Socks {
                        bind: "127.0.0.1".into(),
                        port: 1080,
                    },
                    start: StartMode::OnDemand,
                },
            ],
        }),
        ..profile("warehouse")
    };
    let paths = paths_with(&[
        profile("local"),
        srv.clone(),
        bastion.clone(),
        warehouse.clone(),
    ]);
    (world_at(paths, srv), warehouse)
}

fn groups(w: &World, query: &str) -> Vec<crate::tunnels_window::Group> {
    crate::tunnels_window::groups(&w.app.profiles, &w.app.tunnels, query)
}

fn names(groups: &[crate::tunnels_window::Group]) -> Vec<(String, Vec<String>)> {
    groups
        .iter()
        .map(|g| {
            (
                g.profile.clone(),
                g.rows.iter().map(|r| r.name.clone()).collect(),
            )
        })
        .collect()
}

#[test]
fn panel_lists_tunnels_grouped_by_profile() {
    use crate::tunnels_window::Tone;
    let (mut w, _) = panel_world();
    let g = groups(&w, "");
    assert_eq!(
        names(&g),
        [
            ("srv".to_string(), vec!["web".to_string(), "db".to_string()]),
            (
                "warehouse".to_string(),
                vec!["hook".to_string(), "proxy".to_string()]
            ),
        ],
        "profiles without tunnels are left out; definition order within a profile"
    );
    let web = &g[0].rows[0];
    assert_eq!(web.address, "127.0.0.1:18080 \u{2192} db.internal:5432");
    assert_eq!(web.state, "Off");
    assert_eq!(web.tone, Tone::Off);
    assert!(!web.on && !web.retry && !web.restart_to_apply);
    // A with-the-shell tunnel waiting for its shell says so.
    assert_eq!(g[0].rows[1].state, "Starts with the shell");
    assert_eq!(
        g[1].rows[0].address,
        "remote 127.0.0.1:9000 \u{2192} localhost:3000 via bastion"
    );
    assert_eq!(g[1].rows[1].address, "SOCKS 127.0.0.1:1080 via bastion");

    // Every state, its label and tone.
    w.toggle("web");
    let row = |w: &World| groups(w, "")[0].rows[0].clone();
    assert_eq!(
        (row(&w).state.as_str(), row(&w).tone, row(&w).on),
        ("Starting", Tone::Busy, true)
    );
    let web_key = key(&w.srv, "web");
    for (state, label, tone) in [
        (TunnelState::Up { connections: 0 }, "Up", Tone::Up),
        (
            TunnelState::Up { connections: 1 },
            "Up \u{b7} 1 connection",
            Tone::Up,
        ),
        (
            TunnelState::Up { connections: 3 },
            "Up \u{b7} 3 connections",
            Tone::Up,
        ),
        (
            TunnelState::Reconnecting {
                attempt: 2,
                max: 10,
            },
            "Reconnecting (attempt 2 of 10)",
            Tone::Busy,
        ),
    ] {
        let _ = w.app.update(Message::Tunnel(web_key.clone(), state));
        assert_eq!((row(&w).state.as_str(), row(&w).tone), (label, tone));
    }
    // A changed definition marks the running tunnel.
    w.edit(|p| {
        if let TunnelKind::Local { host_port, .. } = &mut tunnels_of(p)[0].kind {
            *host_port = 5433;
        }
    });
    assert!(row(&w).restart_to_apply);
    assert_eq!(row(&w).address, "127.0.0.1:18080 \u{2192} db.internal:5433");
    // A failure: red, switched off, with Retry.
    let _ = w.app.update(Message::Tunnel(
        web_key,
        TunnelState::Failed("port 18080 already in use".into()),
    ));
    let r = row(&w);
    assert_eq!(r.state, "Failed: port 18080 already in use");
    assert_eq!(r.tone, Tone::Failed);
    assert!(!r.on && r.retry && !r.restart_to_apply);

    // A with-the-shell tunnel switched off by hand while its shell is open is plainly off.
    let _ = w.open_shell();
    assert!(w.running("db"));
    w.toggle("db");
    assert_eq!(groups(&w, "")[0].rows[1].state, "Off");
}

#[test]
fn search_filters_by_tunnel_and_profile() {
    let (w, _) = panel_world();
    // A tunnel name: only that row, under its profile.
    assert_eq!(
        names(&groups(&w, "proxy")),
        [("warehouse".to_string(), vec!["proxy".to_string()])]
    );
    // A profile name: every tunnel of that profile.
    assert_eq!(
        names(&groups(&w, "wareh")),
        [(
            "warehouse".to_string(),
            vec!["hook".to_string(), "proxy".to_string()]
        )]
    );
    // Fuzzy, like the palette, and words may span profile and tunnel.
    assert_eq!(
        names(&groups(&w, "srv db")),
        [("srv".to_string(), vec!["db".to_string()])]
    );
    assert_eq!(
        names(&groups(&w, "wb")),
        [("srv".to_string(), vec!["web".to_string()])]
    );
    assert!(groups(&w, "nothing-like-this").is_empty());
    // Addresses are not searched.
    assert!(groups(&w, "1080").is_empty());
    assert_eq!(groups(&w, "  ").len(), 2, "a blank query lists everything");
}

#[test]
fn toggle_from_panel_and_palette() {
    use crate::palette::{PaletteItem, PaletteMsg};
    use crate::tunnels_window::TunnelsMsg;
    let (mut w, _) = panel_world();
    let web = key(&w.srv, "web");

    // The panel's switch.
    let _ = w
        .app
        .update(Message::Tunnels(TunnelsMsg::Toggle(web.clone())));
    assert!(w.running("web"));
    let _ = w
        .app
        .update(Message::Tunnels(TunnelsMsg::Toggle(web.clone())));
    assert!(!w.running("web"));
    assert_eq!(w.fake.count("web"), 1);

    // Retry on a failed tunnel.
    let _ = w
        .app
        .update(Message::Tunnels(TunnelsMsg::Toggle(web.clone())));
    let _ = w.app.update(Message::Tunnel(
        web.clone(),
        TunnelState::Failed("refused".into()),
    ));
    assert!(!w.running("web"));
    let _ = w
        .app
        .update(Message::Tunnels(TunnelsMsg::Retry(web.clone())));
    assert!(w.running("web"));
    assert_eq!(w.fake.count("web"), 3);

    // The palette: choosing the row toggles the tunnel and closes the palette.
    let wid = w.wid;
    let _ = w.app.update(Message::Palette(wid, PaletteMsg::Open));
    let _ = w.app.update(Message::Palette(
        wid,
        PaletteMsg::Query("Tunnel: srv / web".into()),
    ));
    assert!(matches!(
        w.app.palettes[&wid].selected_item(),
        Some(PaletteItem::Tunnel { key, .. }) if *key == web
    ));
    let _ = w
        .app
        .update(Message::Palette(wid, PaletteMsg::Submit { split: false }));
    assert!(!w.running("web"));
    assert!(!w.app.palettes.contains_key(&wid));
    let _ = w.app.update(Message::Palette(wid, PaletteMsg::Open));
    let _ = w.app.update(Message::Palette(
        wid,
        PaletteMsg::Query("Tunnel: srv / web".into()),
    ));
    let _ = w.app.update(Message::Palette(wid, PaletteMsg::Pick(0)));
    assert!(w.running("web"));
}

#[test]
fn indicator_counts_up_tunnels() {
    use crate::tunnels_window::indicator_count;
    let (mut w, warehouse) = panel_world();
    assert_eq!(w.app.tunnels.up_count(), 0);
    assert_eq!(indicator_count(0), None, "nothing up: no count");

    w.toggle("web");
    let proxy = key(&warehouse, "proxy");
    let _ = w.app.tunnel_toggle(&proxy);
    assert_eq!(w.app.tunnels.up_count(), 0, "starting is not up");
    let web = key(&w.srv, "web");
    let _ = w.app.update(Message::Tunnel(
        web.clone(),
        TunnelState::Up { connections: 0 },
    ));
    let _ = w.app.update(Message::Tunnel(
        proxy.clone(),
        TunnelState::Up { connections: 4 },
    ));
    assert_eq!(w.app.tunnels.up_count(), 2);
    assert_eq!(indicator_count(2).as_deref(), Some("2"));

    let _ = w.app.update(Message::Tunnel(
        proxy.clone(),
        TunnelState::Reconnecting {
            attempt: 1,
            max: 10,
        },
    ));
    assert_eq!(w.app.tunnels.up_count(), 1);
    let _ = w
        .app
        .update(Message::Tunnel(web, TunnelState::Failed("gone".into())));
    assert_eq!(w.app.tunnels.up_count(), 0);

    // The indicator opens the panel.
    assert!(w.app.tunnels_window().is_none());
    let wid = w.wid;
    let _ = w
        .app
        .update(Message::TabBar(wid, crate::app::TabBarMsg::Tunnels));
    assert!(w.app.tunnels_window().is_some());
    // And so does the action, which brings the open panel forward instead of a second one.
    let first = w.app.tunnels_window();
    let _ = w
        .app
        .update(Message::Shortcut(wid, crate::keys::Action::Tunnels));
    assert_eq!(w.app.tunnels_window(), first);
}

#[test]
fn palette_rows_show_state_and_refresh() {
    use crate::palette::{PaletteItem, PaletteMsg};
    let (mut w, warehouse) = panel_world();
    let wid = w.wid;
    let _ = w.app.update(Message::Palette(wid, PaletteMsg::Open));
    let rows = |w: &World| -> Vec<(String, String)> {
        w.app.palettes[&wid]
            .ranked_items()
            .into_iter()
            .filter_map(|i| match i {
                PaletteItem::Tunnel { label, state, .. } => Some((label.clone(), state.clone())),
                _ => None,
            })
            .collect()
    };
    let s = |a: &str, b: &str| (a.to_string(), b.to_string());
    assert_eq!(
        rows(&w),
        [
            s("Tunnel: srv / web", "Off"),
            s("Tunnel: srv / db", "Starts with the shell"),
            s("Tunnel: warehouse / hook", "Off"),
            s("Tunnel: warehouse / proxy", "Off"),
        ]
    );
    // After the profiles, before the actions.
    let kinds: Vec<u8> = w.app.palettes[&wid]
        .ranked_items()
        .into_iter()
        .map(|i| match i {
            PaletteItem::Profile { .. } => 0,
            PaletteItem::Tunnel { .. } => 1,
            _ => 2,
        })
        .collect();
    let mut sorted = kinds.clone();
    sorted.sort_unstable();
    assert_eq!(kinds, sorted);
    assert!(kinds.contains(&1) && kinds.contains(&2));

    // An open palette follows the tunnels as they change.
    let proxy = key(&warehouse, "proxy");
    let _ = w.app.tunnel_toggle(&proxy);
    assert_eq!(rows(&w)[3], s("Tunnel: warehouse / proxy", "Starting"));
    let _ = w.app.update(Message::Tunnel(
        proxy.clone(),
        TunnelState::Up { connections: 1 },
    ));
    assert_eq!(
        rows(&w)[3],
        s("Tunnel: warehouse / proxy", "Up \u{b7} 1 connection")
    );
    let _ = w.app.update(Message::Tunnel(
        proxy,
        TunnelState::Failed("refused".into()),
    ));
    assert_eq!(
        rows(&w)[3],
        s("Tunnel: warehouse / proxy", "Failed: refused")
    );

    // A query keeps working across a refresh.
    let _ = w
        .app
        .update(Message::Palette(wid, PaletteMsg::Query("proxy".into())));
    let web = key(&w.srv, "web");
    let _ = w.app.tunnel_toggle(&web);
    assert_eq!(
        rows(&w),
        [s("Tunnel: warehouse / proxy", "Failed: refused")]
    );

    // The Tunnels action is listed and opens the panel.
    let _ = w
        .app
        .update(Message::Palette(wid, PaletteMsg::Query("Tunnels".into())));
    let at = w.app.palettes[&wid]
        .ranked_items()
        .into_iter()
        .position(|i| {
            matches!(
                i,
                PaletteItem::Action { action: crate::keys::Action::Tunnels, label }
                    if label == "Tunnels"
            )
        })
        .expect("the Tunnels action is listed");
    let _ = w.app.update(Message::Palette(wid, PaletteMsg::Pick(at)));
    assert!(w.app.tunnels_window().is_some());
}
