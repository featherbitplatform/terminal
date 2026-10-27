//! Live tunnel tests (local forward, remote forward, SOCKS) against the containers in
//! `dev/ssh-test/compose.yaml`.
//!
//! Skipped (they return early and pass) unless `FBTERM_TEST_SSH=1`:
//!
//! ```sh
//! docker compose -f dev/ssh-test/compose.yaml up -d && dev/ssh-test/wait-ready.sh
//! FBTERM_TEST_SSH=1 cargo test -p fbterm-session --test ssh_tunnels
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fbterm_config::{
    AuthMethod, Profile, ProfileId, ProfileKind, ProfileStore, SshProfile, StartMode, TunnelDef,
    TunnelKind,
};
use fbterm_secrets::{MemoryStore, SecretKind};
use fbterm_session::ssh::tunnel::{TunnelHandle, TunnelKey, TunnelState, start_tunnel};
use fbterm_session::ssh::{Prompter, SecretAnswer, SshContext, SshPool};
use fbterm_session::{EventSink, SessionHandle};
use fbterm_term::GridSize;
use feathercritty::index::{Column, Line};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

const HOST: &str = "127.0.0.1";
const PORT: u16 = 2222;
const USER: &str = "fb";
/// Bound for every network await, so a broken server fails a test instead of hanging it.
const STEP: Duration = Duration::from_secs(20);

const SIZE: GridSize = GridSize {
    cols: 160,
    rows: 30,
    cell_width: 8.0,
    cell_height: 16.0,
};

fn enabled() -> bool {
    std::env::var("FBTERM_TEST_SSH").as_deref() == Ok("1")
}

fn keys_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dev/ssh-test/keys")
}

fn key_profile(name: &str, host: &str, jump: Vec<ProfileId>) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        group: None,
        accent: None,
        accent_style: Default::default(),
        color_scheme: None,
        kind: ProfileKind::Ssh(SshProfile {
            host: host.into(),
            port: PORT,
            user: USER.into(),
            auth: AuthMethod::Key {
                path: Some(keys_dir().join("fb_test_ed25519")),
                passphrase: false,
            },
            jump,
            tunnels: Vec::new(),
        }),
    }
}

/// Trusts every host (of any hop) and never expects a secret question.
struct TrustAny;

#[async_trait]
impl Prompter for TrustAny {
    async fn ask_key_file(&self, _profile_name: &str) -> Option<std::path::PathBuf> {
        None
    }

    async fn confirm_host_key(&self, _host: &str, _port: u16, _fp: &str) -> bool {
        true
    }
    async fn ask_secret(&self, name: &str, kind: SecretKind, _retry: bool) -> Option<SecretAnswer> {
        panic!("unexpected {kind:?} prompt for {name}");
    }
}

/// A temp `known_hosts`, a pool and the topology `bastion` -> `target` -> `target2`.
struct World {
    _dir: tempfile::TempDir,
    pool: Arc<SshPool>,
    store: ProfileStore,
    bastion: ProfileId,
    target: ProfileId,
    trust: Arc<dyn Prompter>,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let trust: Arc<dyn Prompter> = Arc::new(TrustAny);
        let pool = Arc::new(SshPool::new(SshContext {
            known_hosts: dir.path().join(".ssh").join("known_hosts"),
            secrets: Arc::new(MemoryStore::new()),
            prompter: trust.clone(),
        }));
        let bastion = key_profile("bastion", HOST, vec![]);
        let target = key_profile("target", "target", vec![bastion.id]);
        let target2 = key_profile("target2", "target2", vec![bastion.id, target.id]);
        let (b, t) = (bastion.id, target.id);
        Self {
            _dir: dir,
            pool,
            store: ProfileStore {
                profiles: vec![bastion, target, target2],
            },
            bastion: b,
            target: t,
            trust,
        }
    }

    /// Add a profile reached through `proxy` (straight to the bastion's sshd).
    fn add_via_proxy(&mut self, name: &str, proxy: &Proxy, auth: AuthMethod) -> ProfileId {
        let mut p = key_profile(name, HOST, vec![]);
        if let ProfileKind::Ssh(ssh) = &mut p.kind {
            ssh.port = proxy.port;
            ssh.auth = auth;
        }
        let id = p.id;
        self.store.profiles.push(p);
        id
    }

    fn start(&self, profile: ProfileId, def: TunnelDef) -> (TunnelHandle, Events) {
        self.start_with(profile, def, self.trust.clone(), Arc::default())
    }

    /// Start a tunnel with its own prompter (for the first connect) and retry limit.
    fn start_with(
        &self,
        profile: ProfileId,
        def: TunnelDef,
        prompter: Arc<dyn Prompter>,
        retries: Arc<AtomicU32>,
    ) -> (TunnelHandle, Events) {
        let (tx, rx) = unbounded_channel();
        let key = TunnelKey {
            profile,
            name: def.name.clone(),
        };
        let handle = start_tunnel(
            self.pool.clone(),
            self.store.clone(),
            key,
            def,
            prompter,
            retries,
            tx,
        );
        (handle, Events::new(rx))
    }

    async fn shell(&self, profile: ProfileId) -> SessionHandle {
        let (tx, _rx): (EventSink, _) = unbounded_channel();
        let terminal = Arc::new(parking_lot::Mutex::new(fbterm_term::Terminal::new(
            SIZE, 1000,
        )));
        tokio::time::timeout(
            STEP,
            self.pool
                .open_shell_prompting(&self.store, &profile, terminal, tx, self.trust.clone()),
        )
        .await
        .expect("open_shell timed out")
        .expect("open_shell")
    }

    async fn wait_disconnected(&self) {
        tokio::time::timeout(STEP, async {
            while !self.pool.connected_profiles().is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "still connected: {:?}",
                self.pool.connected_profiles().len()
            )
        });
    }
}

fn def(name: &str, kind: TunnelKind) -> TunnelDef {
    TunnelDef {
        name: name.into(),
        kind,
        start: StartMode::OnDemand,
    }
}

fn local(port: u16, host: &str, host_port: u16) -> TunnelKind {
    TunnelKind::Local {
        bind: HOST.into(),
        port,
        host: host.into(),
        host_port,
    }
}

/// The states a tunnel reported, with when they were sent (a task stamps each as it arrives,
/// so a test busy elsewhere does not blur the timing).
struct Events(UnboundedReceiver<(Instant, TunnelState)>);

impl Events {
    fn new(mut rx: UnboundedReceiver<(TunnelKey, TunnelState)>) -> Self {
        let (tx, stamped) = unbounded_channel();
        tokio::spawn(async move {
            while let Some((_, state)) = rx.recv().await {
                if tx.send((Instant::now(), state)).is_err() {
                    break;
                }
            }
        });
        Self(stamped)
    }

    /// The next reported state that satisfies `pick`; panics on a `Failed` it did not pick.
    async fn until(&mut self, what: &str, pick: impl Fn(&TunnelState) -> bool) -> TunnelState {
        tokio::time::timeout(STEP, async {
            loop {
                let (_, state) = self.0.recv().await.expect("tunnel events ended");
                if pick(&state) {
                    return state;
                }
                if let TunnelState::Failed(reason) = &state {
                    panic!("tunnel failed while waiting for {what}: {reason}");
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no {what} reported"))
    }

    async fn up(&mut self) {
        self.until("Up", |s| matches!(s, TunnelState::Up { .. }))
            .await;
    }

    async fn failed(&mut self) -> String {
        match self
            .until("Failed", |s| matches!(s, TunnelState::Failed(_)))
            .await
        {
            TunnelState::Failed(reason) => reason,
            _ => unreachable!(),
        }
    }
}

/// Connect to the tunnel's local port.
async fn dial(handle: &TunnelHandle) -> TcpStream {
    let port = handle.port().expect("an Up tunnel knows its port");
    tokio::time::timeout(STEP, TcpStream::connect((HOST, port)))
        .await
        .expect("connect timed out")
        .expect("connect")
}

/// Read until `n` bytes arrived.
async fn read_n(stream: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut buf = vec![0; n];
    tokio::time::timeout(STEP, stream.read_exact(&mut buf))
        .await
        .expect("read timed out")
        .expect("read");
    buf
}

/// Whether the peer closed `stream` (end of file or reset) within `STEP`.
async fn closed_by_peer(stream: &mut TcpStream) -> bool {
    let mut buf = [0u8; 256];
    tokio::time::timeout(STEP, async {
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => return true,
                Ok(_) => {}
            }
        }
    })
    .await
    .unwrap_or(false)
}

fn grid_text(handle: &SessionHandle) -> String {
    let t = handle.terminal.lock();
    let g = t.term().grid();
    let size = t.size();
    let mut out = String::new();
    for row in 0..size.rows as i32 {
        for col in 0..size.cols as usize {
            out.push(g.screen()[Line(row)][Column(col)].c);
        }
        out.push('\n');
    }
    out
}

/// Run `command` in the shell and return what it printed as `<<TAG value>>` (the echoed
/// command line has a `$` right after the tag, so it is skipped).
async fn shell_value(handle: &SessionHandle, tag: &str, command: &str) -> String {
    handle.send(format!("{command}; echo \"<<{tag} $R>>\"\r").into_bytes());
    let open = format!("<<{tag} ");
    tokio::time::timeout(STEP, async {
        loop {
            let text = grid_text(handle);
            if let Some(v) = text.split(open.as_str()).skip(1).find_map(|s| {
                let v = s.split(">>").next()?;
                (!v.starts_with('$')).then(|| v.to_owned())
            }) {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no <<{tag} ...>> in:\n{}", grid_text(handle)))
}

#[tokio::test]
async fn local_forward_through_jump_reaches_target() {
    if !enabled() {
        return;
    }
    let w = World::new();
    // Through the bastion to `target`, which only the bastion can reach.
    let (a, mut ea) = w.start(w.bastion, def("ssh-target", local(0, "target", PORT)));
    ea.up().await;
    let mut s = dial(&a).await;
    assert_eq!(read_n(&mut s, 4).await, b"SSH-");

    // A profile behind a jump host: the tunnel rides its whole chain, to `target2`, which only
    // `target` can reach.
    let (b, mut eb) = w.start(w.target, def("ssh-target2", local(0, "target2", PORT)));
    eb.up().await;
    let mut s2 = dial(&b).await;
    assert_eq!(read_n(&mut s2, 4).await, b"SSH-");

    // A destination the server cannot reach closes that one client and leaves the tunnel up.
    let (c, mut ec) = w.start(w.bastion, def("nowhere", local(0, "target", 1)));
    ec.up().await;
    let mut bad = dial(&c).await;
    assert!(closed_by_peer(&mut bad).await);
    let mut s3 = dial(&a).await;
    assert_eq!(read_n(&mut s3, 4).await, b"SSH-");
    assert!(c.port().is_some());
}

#[tokio::test]
async fn remote_forward_reaches_local_listener() {
    if !enabled() {
        return;
    }
    let w = World::new();
    let listener = TcpListener::bind((HOST, 0)).await.unwrap();
    let here = listener.local_addr().unwrap().port();
    let served = tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            let _ = s.write_all(b"remote-ok\n").await;
            let _ = s.shutdown().await;
        }
    });

    let remote = TunnelKind::Remote {
        bind: HOST.into(),
        port: 0,
        host: HOST.into(),
        host_port: here,
    };
    let (t, mut events) = w.start(w.target, def("back", remote));
    events.up().await;
    let port = t.port().expect("the server chose a port");
    assert_ne!(port, 0);

    // From a shell on `target`, the server's port reaches the listener here.
    let shell = w.shell(w.target).await;
    let got = shell_value(
        &shell,
        "A",
        &format!("R=$(timeout 5 cat < /dev/tcp/127.0.0.1/{port})"),
    )
    .await;
    assert_eq!(got, "remote-ok");

    // Stopped, the server no longer listens there.
    t.stop();
    let mut refused = false;
    for attempt in 0..40 {
        let got = shell_value(
            &shell,
            &format!("B{attempt}"),
            &format!("R=$(timeout 5 cat < /dev/tcp/127.0.0.1/{port} 2>/dev/null; echo rc=$?)"),
        )
        .await;
        if got != "remote-okrc=0" && got.ends_with("rc=1") {
            refused = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(refused, "the remote forward was not cancelled");
    served.abort();
}

/// SOCKS5: greeting with "no authentication", then CONNECT to `host:port` by name. Returns
/// the reply code and the stream.
async fn socks_connect(handle: &TunnelHandle, host: &str, port: u16) -> (u8, TcpStream) {
    let mut s = dial(handle).await;
    s.write_all(&[5, 1, 0]).await.unwrap();
    assert_eq!(read_n(&mut s, 2).await, [5, 0]);
    let mut req = vec![5, 1, 0, 3, u8::try_from(host.len()).unwrap()];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req).await.unwrap();
    let reply = read_n(&mut s, 10).await;
    assert_eq!((reply[0], reply[2], reply[3]), (5, 0, 1), "{reply:?}");
    (reply[1], s)
}

#[tokio::test]
async fn socks_reaches_target_by_name() {
    if !enabled() {
        return;
    }
    let w = World::new();
    let socks = TunnelKind::Socks {
        bind: HOST.into(),
        port: 0,
    };
    let (t, mut events) = w.start(w.bastion, def("proxy", socks));
    events.up().await;
    // `target` is a name only the bastion can resolve.
    let (rep, mut s) = socks_connect(&t, "target", PORT).await;
    assert_eq!(rep, 0);
    assert_eq!(read_n(&mut s, 4).await, b"SSH-");
    // A closed port on the far side: "connection refused".
    let (rep, mut refused) = socks_connect(&t, "target", 1).await;
    assert_eq!(rep, 5);
    assert!(closed_by_peer(&mut refused).await);
}

#[tokio::test]
async fn bind_in_use_fails_with_reason() {
    if !enabled() {
        return;
    }
    let w = World::new();
    let taken = TcpListener::bind((HOST, 0)).await.unwrap();
    let p = taken.local_addr().unwrap().port();
    let (_t, mut events) = w.start(w.bastion, def("clash", local(p, "target", PORT)));
    assert_eq!(events.failed().await, format!("port {p} already in use"));

    // The server's own port is taken on the server: the forward is refused.
    let remote = TunnelKind::Remote {
        bind: HOST.into(),
        port: PORT,
        host: HOST.into(),
        host_port: 9,
    };
    let (_r, mut events) = w.start(w.bastion, def("clash-remote", remote));
    let reason = events.failed().await;
    assert!(
        reason.starts_with("server refused the forward: "),
        "{reason}"
    );
    drop(taken);
}

#[tokio::test]
async fn stop_releases_connection() {
    if !enabled() {
        return;
    }
    let w = World::new();
    let (t, mut events) = w.start(w.bastion, def("ssh-target", local(0, "target", PORT)));
    events.up().await;
    assert_eq!(w.pool.connected_profiles(), vec![w.bastion]);
    let port = t.port().unwrap();
    let mut live = dial(&t).await;
    assert_eq!(read_n(&mut live, 4).await, b"SSH-");

    // Stopping ends the live client, closes the listener and lets the connection go.
    t.stop();
    assert!(closed_by_peer(&mut live).await, "live client kept open");
    w.wait_disconnected().await;
    assert!(TcpStream::connect((HOST, port)).await.is_err());

    // Dropping the handle stops it too.
    let (t, mut events) = w.start(w.bastion, def("ssh-target", local(0, "target", PORT)));
    events.up().await;
    assert_eq!(w.pool.connected_profiles(), vec![w.bastion]);
    drop(t);
    w.wait_disconnected().await;
    // Nothing more is reported once stopped (nothing was pending after `Up` either).
    tokio::time::sleep(Duration::from_millis(300)).await;
    if let Ok((_, state)) = events.0.try_recv() {
        panic!("reported after stop: {state:?}");
    }
}

/// A port nothing listens on right now.
async fn free_port() -> u16 {
    let l = TcpListener::bind((HOST, 0)).await.unwrap();
    l.local_addr().unwrap().port()
}

#[tokio::test]
async fn stop_and_wait_releases_port_for_restart() {
    if !enabled() {
        return;
    }
    let w = World::new();
    // A fixed local port, with a client connected when the tunnel is restarted.
    let port = free_port().await;
    let d = def("fixed", local(port, "target", PORT));
    let (t, mut events) = w.start(w.bastion, d.clone());
    events.up().await;
    let mut live = dial(&t).await;
    assert_eq!(read_n(&mut live, 4).await, b"SSH-");
    tokio::time::timeout(STEP, t.stop_and_wait())
        .await
        .expect("stop_and_wait did not finish");
    // Everything is released when it returns: the connection had no other user.
    assert!(w.pool.connected_profiles().is_empty());
    let (t2, mut events) = w.start(w.bastion, d);
    events.up().await;
    assert_eq!(t2.port(), Some(port));
    let mut s = dial(&t2).await;
    assert_eq!(read_n(&mut s, 4).await, b"SSH-");
    assert!(closed_by_peer(&mut live).await);

    // A fixed remote port: the restart's request is granted and routed, because the old
    // forward is cancelled and its claim released before stop_and_wait returns.
    let listener = TcpListener::bind((HOST, 0)).await.unwrap();
    let here = listener.local_addr().unwrap().port();
    let served = tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            let _ = s.write_all(b"remote-ok\n").await;
            let _ = s.shutdown().await;
        }
    });
    let remote = |port| TunnelKind::Remote {
        bind: HOST.into(),
        port,
        host: HOST.into(),
        host_port: here,
    };
    let (r, mut events) = w.start(w.target, def("back", remote(0)));
    events.up().await;
    let p = r.port().unwrap();
    tokio::time::timeout(STEP, r.stop_and_wait())
        .await
        .expect("stop_and_wait did not finish");
    // Only the restarted local tunnel's bastion connection is left.
    assert_eq!(w.pool.connected_profiles(), vec![w.bastion]);
    let (r2, mut events) = w.start(w.target, def("back", remote(p)));
    events.up().await;
    let shell = w.shell(w.target).await;
    let got = shell_value(
        &shell,
        "A",
        &format!("R=$(timeout 5 cat < /dev/tcp/127.0.0.1/{p})"),
    )
    .await;
    assert_eq!(got, "remote-ok");
    drop(r2);
    served.abort();
}

#[tokio::test]
async fn second_remote_tunnel_on_same_port_is_refused_and_first_keeps_working() {
    if !enabled() {
        return;
    }
    let w = World::new();
    let listener = TcpListener::bind((HOST, 0)).await.unwrap();
    let here = listener.local_addr().unwrap().port();
    let served = tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            let _ = s.write_all(b"first-ok\n").await;
            let _ = s.shutdown().await;
        }
    });
    let remote = |port, host_port| TunnelKind::Remote {
        bind: HOST.into(),
        port,
        host: HOST.into(),
        host_port,
    };
    let (first, mut events) = w.start(w.target, def("first", remote(0, here)));
    events.up().await;
    let p = first.port().unwrap();
    // Same server port, another tunnel of the profile: refused without touching the first.
    let (_second, mut events2) = w.start(w.target, def("second", remote(p, 9)));
    assert_eq!(
        events2.failed().await,
        format!("server refused the forward: 127.0.0.1 port {p} is already used by another tunnel")
    );
    let shell = w.shell(w.target).await;
    let got = shell_value(
        &shell,
        "A",
        &format!("R=$(timeout 5 cat < /dev/tcp/127.0.0.1/{p})"),
    )
    .await;
    assert_eq!(got, "first-ok");
    drop(first);
    served.abort();
}

#[tokio::test]
async fn remote_forward_to_closed_local_port_closes_the_server_client() {
    if !enabled() {
        return;
    }
    let w = World::new();
    let nothing = free_port().await;
    let remote = TunnelKind::Remote {
        bind: HOST.into(),
        port: 0,
        host: HOST.into(),
        host_port: nothing,
    };
    let (t, mut events) = w.start(w.target, def("dead-end", remote));
    events.up().await;
    let p = t.port().unwrap();
    // The server's client sees the channel close (end of file, cat exits 0) instead of hanging
    // until `timeout` gives up (124).
    let shell = w.shell(w.target).await;
    let got = shell_value(
        &shell,
        "A",
        &format!("R=$(timeout 5 cat < /dev/tcp/127.0.0.1/{p}; echo rc=$?)"),
    )
    .await;
    assert_eq!(got, "rc=0");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_counts_are_reported() {
    if !enabled() {
        return;
    }
    let w = World::new();
    let (t, mut events) = w.start(w.bastion, def("ssh-target", local(0, "target", PORT)));
    assert_eq!(
        events.until("Starting", |_| true).await,
        TunnelState::Starting
    );
    assert_eq!(
        events
            .until("Up", |s| matches!(s, TunnelState::Up { .. }))
            .await,
        TunnelState::Up { connections: 0 }
    );

    // Twelve clients in quick succession: the count arrives coalesced, at most every 250 ms.
    let mut clients = Vec::new();
    for _ in 0..12 {
        let mut s = dial(&t).await;
        assert_eq!(read_n(&mut s, 4).await, b"SSH-");
        clients.push(s);
    }
    let mut seen = Vec::new();
    tokio::time::timeout(STEP, async {
        loop {
            let (at, state) = events.0.recv().await.unwrap();
            seen.push((at, state.clone()));
            if state == (TunnelState::Up { connections: 12 }) {
                break;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("never reported 12 connections: {seen:?}"));
    // However long the clients took, no more than one update per 200 ms (plus the first).
    let span = seen.last().unwrap().0 - seen[0].0;
    assert!(
        seen.len() as u128 <= span.as_millis() / 200 + 1,
        "too many updates for 12 quick clients: {seen:?}"
    );
    for pair in seen.windows(2) {
        let gap = pair[1].0 - pair[0].0;
        assert!(gap >= Duration::from_millis(200), "updates {gap:?} apart");
    }

    // Reaching zero is always reported.
    drop(clients);
    assert_eq!(
        events
            .until("0 connections", |s| *s
                == TunnelState::Up { connections: 0 })
            .await,
        TunnelState::Up { connections: 0 }
    );
}

/// A TCP proxy to the bastion's sshd that a test can cut (every live connection drops, as
/// when the server restarts), cut on this side only (the server notices later, as when the
/// network drops) or shut (nothing listens any more: the server is unreachable).
struct Proxy {
    port: u16,
    accept: tokio::task::JoinHandle<()>,
    pipes: Arc<parking_lot::Mutex<Vec<Pipe>>>,
}

/// One proxied connection.
struct Pipe {
    task: tokio::task::AbortHandle,
    /// Closes the client side and keeps the server side open for the given time.
    linger: tokio::sync::watch::Sender<Option<Duration>>,
}

impl Proxy {
    async fn new() -> Self {
        let listener = TcpListener::bind((HOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let pipes: Arc<parking_lot::Mutex<Vec<Pipe>>> = Arc::default();
        let tracked = pipes.clone();
        let accept = tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let (linger, mut lingering) = tokio::sync::watch::channel(None);
                let task = tokio::spawn(async move {
                    let Ok(mut server) = TcpStream::connect((HOST, PORT)).await else {
                        return;
                    };
                    let copied = tokio::select! {
                        _ = tokio::io::copy_bidirectional(&mut client, &mut server) => None,
                        r = lingering.wait_for(Option::is_some) => r.ok().and_then(|l| *l),
                    };
                    if let Some(linger) = copied {
                        drop(client);
                        tokio::time::sleep(linger).await;
                    }
                });
                tracked.lock().push(Pipe {
                    task: task.abort_handle(),
                    linger,
                });
            }
        });
        Self {
            port,
            accept,
            pipes,
        }
    }

    /// Drop every connection made through the proxy.
    fn cut(&self) {
        for pipe in self.pipes.lock().drain(..) {
            pipe.task.abort();
        }
    }

    /// Drop every connection here; the server keeps its side (and what it holds for it, a
    /// remote forward's port say) for `linger` more.
    fn cut_here(&self, linger: Duration) {
        for pipe in self.pipes.lock().drain(..) {
            pipe.linger.send_replace(Some(linger));
        }
    }

    /// Stop listening, then cut: the server can no longer be reached.
    fn shut(&self) {
        self.accept.abort();
        self.cut();
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.shut();
    }
}

/// Trusts every host and types the password when asked, without saving it; counts the asks.
#[derive(Default)]
struct TypesPassword(AtomicUsize);

#[async_trait]
impl Prompter for TypesPassword {
    async fn ask_key_file(&self, _profile_name: &str) -> Option<std::path::PathBuf> {
        None
    }

    async fn confirm_host_key(&self, _host: &str, _port: u16, _fp: &str) -> bool {
        true
    }
    async fn ask_secret(
        &self,
        _name: &str,
        kind: SecretKind,
        _retry: bool,
    ) -> Option<SecretAnswer> {
        assert_eq!(kind, SecretKind::Password);
        self.0.fetch_add(1, Ordering::SeqCst);
        Some(SecretAnswer {
            value: "fb-pass".into(),
            remember: false,
        })
    }
}

fn reconnecting(state: &TunnelState) -> bool {
    matches!(state, TunnelState::Reconnecting { .. })
}

/// The very next state reported.
async fn next_state(events: &mut Events) -> TunnelState {
    tokio::time::timeout(STEP, events.0.recv())
        .await
        .expect("no state reported")
        .expect("tunnel events ended")
        .1
}

#[tokio::test]
async fn server_restart_reconnects_tunnel() {
    if !enabled() {
        return;
    }
    let w = World::new();
    let retries = Arc::new(AtomicU32::new(5));
    let listener = TcpListener::bind((HOST, 0)).await.unwrap();
    let here = listener.local_addr().unwrap().port();
    let served = tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            let _ = s.write_all(b"remote-ok\n").await;
            let _ = s.shutdown().await;
        }
    });

    let (l, mut el) = w.start_with(
        w.bastion,
        def("ssh-target", local(0, "target", PORT)),
        w.trust.clone(),
        retries.clone(),
    );
    el.up().await;
    let port = l.port().unwrap();
    let remote = TunnelKind::Remote {
        bind: HOST.into(),
        port: 0,
        host: HOST.into(),
        host_port: here,
    };
    let (r, mut er) = w.start_with(w.bastion, def("back", remote), w.trust.clone(), retries);
    er.up().await;

    // Kill the server process of the shared connection (the shell's parent), as a server
    // restart would.
    let shell = w.shell(w.bastion).await;
    shell.send(b"kill -9 $PPID\r".to_vec());
    assert_eq!(
        el.until("Reconnecting", reconnecting).await,
        TunnelState::Reconnecting { attempt: 1, max: 5 }
    );
    // The local port stays ours while reconnecting; a client meanwhile is closed at once.
    let mut early = dial(&l).await;
    assert!(closed_by_peer(&mut early).await);
    assert_eq!(
        er.until("Reconnecting", reconnecting).await,
        TunnelState::Reconnecting { attempt: 1, max: 5 }
    );

    // Back up on a new connection, the same local port forwarding again.
    el.up().await;
    assert_eq!(l.port(), Some(port));
    let mut s = dial(&l).await;
    assert_eq!(read_n(&mut s, 4).await, b"SSH-");

    // The remote forward was requested again on the new connection.
    er.up().await;
    let rport = r.port().expect("the server chose a port again");
    let shell = w.shell(w.bastion).await;
    let got = shell_value(
        &shell,
        "A",
        &format!("R=$(timeout 5 cat < /dev/tcp/127.0.0.1/{rport})"),
    )
    .await;
    assert_eq!(got, "remote-ok");
    served.abort();
}

#[tokio::test]
async fn retries_run_out_then_fail() {
    if !enabled() {
        return;
    }
    let proxy = Proxy::new().await;
    let mut w = World::new();
    let auth = AuthMethod::Key {
        path: Some(keys_dir().join("fb_test_ed25519")),
        passphrase: false,
    };
    let p = w.add_via_proxy("proxied", &proxy, auth);
    let retries = Arc::new(AtomicU32::new(10));
    let (t, mut e) = w.start_with(
        p,
        def("ssh", local(0, HOST, PORT)),
        w.trust.clone(),
        retries.clone(),
    );
    e.up().await;
    let port = t.port().unwrap();

    // The limit is read live, when the attempt is made.
    retries.store(1, Ordering::SeqCst);
    proxy.shut();
    assert_eq!(
        next_state(&mut e).await,
        TunnelState::Reconnecting { attempt: 1, max: 1 }
    );
    // The one attempt finds nothing listening; its reason is the one reported.
    match next_state(&mut e).await {
        TunnelState::Failed(reason) => assert!(
            reason.starts_with("connection lost: could not connect"),
            "{reason}"
        ),
        other => panic!("expected Failed, got {other:?}"),
    }
    t.stopped().await;
    assert!(
        TcpStream::connect((HOST, port)).await.is_err(),
        "port still bound"
    );
    w.wait_disconnected().await;
}

#[tokio::test]
async fn zero_retries_fail_at_once() {
    if !enabled() {
        return;
    }
    let proxy = Proxy::new().await;
    let mut w = World::new();
    let auth = AuthMethod::Key {
        path: Some(keys_dir().join("fb_test_ed25519")),
        passphrase: false,
    };
    let p = w.add_via_proxy("proxied", &proxy, auth);
    let (t, mut e) = w.start_with(
        p,
        def("ssh", local(0, HOST, PORT)),
        w.trust.clone(),
        Arc::new(AtomicU32::new(0)),
    );
    e.up().await;
    let cut = Instant::now();
    proxy.cut();
    match next_state(&mut e).await {
        TunnelState::Failed(reason) => assert!(reason.starts_with("connection lost: "), "{reason}"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(
        cut.elapsed() < Duration::from_millis(900),
        "waited for a retry"
    );
    t.stopped().await;
}

#[tokio::test]
async fn reconnect_needing_input_fails() {
    if !enabled() {
        return;
    }
    let proxy = Proxy::new().await;
    let mut w = World::new();
    let p = w.add_via_proxy("typed", &proxy, AuthMethod::Password);
    let typed = Arc::new(TypesPassword::default());
    let (t, mut e) = w.start_with(
        p,
        def("ssh", local(0, HOST, PORT)),
        typed.clone(),
        Arc::new(AtomicU32::new(5)),
    );
    e.up().await;
    assert_eq!(typed.0.load(Ordering::SeqCst), 1);

    // The password was typed, not saved: the automatic attempt cannot ask for it.
    proxy.cut();
    assert_eq!(
        next_state(&mut e).await,
        TunnelState::Reconnecting { attempt: 1, max: 5 }
    );
    assert_eq!(
        next_state(&mut e).await,
        TunnelState::Failed("needs your input".into())
    );
    assert_eq!(typed.0.load(Ordering::SeqCst), 1, "a reconnect prompted");
    t.stopped().await;
}

#[tokio::test]
async fn stop_while_reconnecting_ends_promptly() {
    if !enabled() {
        return;
    }
    let proxy = Proxy::new().await;
    let mut w = World::new();
    let auth = AuthMethod::Key {
        path: Some(keys_dir().join("fb_test_ed25519")),
        passphrase: false,
    };
    let p = w.add_via_proxy("proxied", &proxy, auth);
    let (t, mut e) = w.start_with(
        p,
        def("ssh", local(0, HOST, PORT)),
        w.trust.clone(),
        Arc::new(AtomicU32::new(10)),
    );
    e.up().await;
    let port = t.port().unwrap();
    proxy.shut();
    e.until("Reconnecting", reconnecting).await;

    // Stopped in the middle of the backoff: it ends at once and lets go of its port.
    let asked = Instant::now();
    t.stop_and_wait().await;
    assert!(asked.elapsed() < Duration::from_millis(500), "stop waited");
    TcpListener::bind((HOST, port))
        .await
        .expect("the port was released");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(e.0.try_recv().is_err(), "a state arrived after stop");
}

#[tokio::test]
async fn fixed_port_remote_forward_survives_reconnect() {
    if !enabled() {
        return;
    }
    let proxy = Proxy::new().await;
    let mut w = World::new();
    let auth = AuthMethod::Key {
        path: Some(keys_dir().join("fb_test_ed25519")),
        passphrase: false,
    };
    let p = w.add_via_proxy("proxied", &proxy, auth);
    let listener = TcpListener::bind((HOST, 0)).await.unwrap();
    let here = listener.local_addr().unwrap().port();
    let served = tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            let _ = s.write_all(b"fixed-ok\n").await;
            let _ = s.shutdown().await;
        }
    });
    // A fixed server port, distinct per test run, below the server's ephemeral range
    // (32768-60999 on Linux), so no outgoing connection there can hold it.
    let fixed = 20000 + u16::try_from(std::process::id() % 10000).unwrap();
    let remote = TunnelKind::Remote {
        bind: HOST.into(),
        port: fixed,
        host: HOST.into(),
        host_port: here,
    };
    let (t, mut e) = w.start_with(
        p,
        def("fixed", remote),
        w.trust.clone(),
        Arc::new(AtomicU32::new(5)),
    );
    e.up().await;
    assert_eq!(t.port(), Some(fixed));

    // The link drops here while the server's old session keeps the port for 2 s more: the
    // first attempt (after 1 s) is refused, the second (after 2 s more) gets the port back,
    // each a second away from the port's release.
    proxy.cut_here(Duration::from_secs(2));
    assert_eq!(
        next_state(&mut e).await,
        TunnelState::Reconnecting { attempt: 1, max: 5 }
    );
    assert_eq!(
        next_state(&mut e).await,
        TunnelState::Reconnecting { attempt: 2, max: 5 }
    );
    assert_eq!(next_state(&mut e).await, TunnelState::Up { connections: 0 });
    assert_eq!(t.port(), Some(fixed));
    let shell = w.shell(p).await;
    let got = shell_value(
        &shell,
        "F",
        &format!("R=$(timeout 5 cat < /dev/tcp/127.0.0.1/{fixed})"),
    )
    .await;
    assert_eq!(got, "fixed-ok");
    served.abort();
}

#[tokio::test]
async fn refused_re_forward_fails_with_its_reason_when_retries_run_out() {
    if !enabled() {
        return;
    }
    let proxy = Proxy::new().await;
    let mut w = World::new();
    let auth = AuthMethod::Key {
        path: Some(keys_dir().join("fb_test_ed25519")),
        passphrase: false,
    };
    let p = w.add_via_proxy("proxied", &proxy, auth);
    // Fixed, per run and below the server's ephemeral range, apart from the test above.
    let fixed = 30000 + u16::try_from(std::process::id() % 10000).unwrap();
    let remote = TunnelKind::Remote {
        bind: HOST.into(),
        port: fixed,
        host: HOST.into(),
        host_port: 9,
    };
    let (_t, mut e) = w.start_with(
        p,
        def("fixed", remote),
        w.trust.clone(),
        Arc::new(AtomicU32::new(1)),
    );
    e.up().await;
    // The old session holds the port longer than the one attempt waits.
    proxy.cut_here(Duration::from_secs(5));
    assert_eq!(
        next_state(&mut e).await,
        TunnelState::Reconnecting { attempt: 1, max: 1 }
    );
    match next_state(&mut e).await {
        TunnelState::Failed(reason) => assert_eq!(
            reason,
            format!(
                "connection lost: server refused the forward: 127.0.0.1 port {fixed} is taken \
                 or not allowed"
            )
        ),
        other => panic!("expected Failed, got {other:?}"),
    }
}
