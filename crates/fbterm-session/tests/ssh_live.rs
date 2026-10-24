//! Live SSH tests against the containers in `dev/ssh-test/compose.yaml`.
//!
//! Skipped (they return early and pass) unless `FBTERM_TEST_SSH=1`:
//!
//! ```sh
//! docker compose -f dev/ssh-test/compose.yaml up -d && dev/ssh-test/wait-ready.sh
//! FBTERM_TEST_SSH=1 cargo test -p fbterm-session --test ssh_live
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use fbterm_config::{AuthMethod, Profile, ProfileId, ProfileKind, SshProfile};
use fbterm_secrets::{MemoryStore, SecretKind, SecretStore};
use fbterm_session::ssh::HostKeyStatus;
use fbterm_session::ssh::{
    ClientHandler, Prompter, SecretAnswer, SshContext, SshError, check_host_key, connect_direct,
};
use russh::ChannelMsg;
use russh::client::Handle;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 2222;
const USER: &str = "fb";
const PASSWORD: &str = "fb-pass";
const KEY_PASSPHRASE: &str = "fb-test-pass";
/// Bound for every network await, so a broken server fails a test instead of hanging it.
const STEP: Duration = Duration::from_secs(20);

fn enabled() -> bool {
    std::env::var("FBTERM_TEST_SSH").as_deref() == Ok("1")
}

fn keys_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dev/ssh-test/keys")
}

fn profile(auth: AuthMethod) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: "bastion".into(),
        group: None,
        accent: None,
        accent_style: Default::default(),
        kind: ProfileKind::Ssh(SshProfile {
            host: HOST.into(),
            port: PORT,
            user: USER.into(),
            auth,
            jump: vec![],
        }),
    }
}

/// Scripted prompter: answers host-key questions with `trust` and secret questions from
/// `answers` in order (`None` once they run out); counts every call.
struct Script {
    trust: bool,
    answers: parking_lot::Mutex<Vec<Option<SecretAnswer>>>,
    host_key_calls: AtomicUsize,
    secret_calls: parking_lot::Mutex<Vec<(SecretKind, bool)>>,
}

impl Script {
    fn new(trust: bool, answers: Vec<Option<SecretAnswer>>) -> Arc<Self> {
        Arc::new(Self {
            trust,
            answers: parking_lot::Mutex::new(answers),
            host_key_calls: AtomicUsize::new(0),
            secret_calls: parking_lot::Mutex::new(vec![]),
        })
    }
    fn host_key_calls(&self) -> usize {
        self.host_key_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Prompter for Script {
    async fn confirm_host_key(&self, host: &str, port: u16, fingerprint_sha256: &str) -> bool {
        assert_eq!((host, port), (HOST, PORT));
        assert!(
            fingerprint_sha256.starts_with("SHA256:") && !fingerprint_sha256.ends_with('='),
            "unexpected fingerprint format {fingerprint_sha256}"
        );
        self.host_key_calls.fetch_add(1, Ordering::SeqCst);
        self.trust
    }

    async fn ask_secret(
        &self,
        _profile_name: &str,
        kind: SecretKind,
        retry: bool,
    ) -> Option<SecretAnswer> {
        self.secret_calls.lock().push((kind, retry));
        let mut answers = self.answers.lock();
        if answers.is_empty() {
            None
        } else {
            answers.remove(0)
        }
    }
}

fn answer(value: &str, remember: bool) -> Option<SecretAnswer> {
    Some(SecretAnswer {
        value: value.into(),
        remember,
    })
}

struct Fixture {
    _dir: tempfile::TempDir,
    known_hosts: PathBuf,
    store: Arc<MemoryStore>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let known_hosts = dir.path().join(".ssh").join("known_hosts");
        Self {
            _dir: dir,
            known_hosts,
            store: Arc::new(MemoryStore::new()),
        }
    }

    fn ctx(&self, prompter: Arc<dyn Prompter>) -> SshContext {
        SshContext {
            known_hosts: self.known_hosts.clone(),
            secrets: self.store.clone(),
            prompter,
        }
    }

    fn known_hosts_lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.known_hosts)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

async fn connect(profile: &Profile, ctx: &SshContext) -> Result<Handle<ClientHandler>, SshError> {
    tokio::time::timeout(STEP, connect_direct(profile, ctx))
        .await
        .expect("connect timed out")
}

/// Run `command` on a fresh session channel and return its stdout.
async fn exec(handle: &Handle<ClientHandler>, command: &str) -> String {
    let mut channel = tokio::time::timeout(STEP, handle.channel_open_session())
        .await
        .expect("channel open timed out")
        .expect("channel open");
    tokio::time::timeout(STEP, channel.exec(true, command))
        .await
        .expect("exec timed out")
        .expect("exec");
    let mut out = Vec::new();
    loop {
        match tokio::time::timeout(STEP, channel.wait())
            .await
            .expect("channel read timed out")
        {
            Some(ChannelMsg::Data { data }) => out.extend_from_slice(&data),
            Some(ChannelMsg::Eof | ChannelMsg::Close) | None => break,
            Some(_) => {}
        }
    }
    String::from_utf8(out).unwrap()
}

async fn assert_echo_ok(handle: &Handle<ClientHandler>) {
    assert_eq!(exec(handle, "echo ok").await.trim(), "ok");
}

#[tokio::test]
async fn password_auth_then_shell_echo() {
    if !enabled() {
        return;
    }
    let fx = Fixture::new();
    let p = profile(AuthMethod::Password);
    fx.store.set(&p.id, SecretKind::Password, PASSWORD).unwrap();
    let script = Script::new(true, vec![]);
    let handle = connect(&p, &fx.ctx(script.clone())).await.unwrap();
    assert_echo_ok(&handle).await;
    assert!(
        script.secret_calls.lock().is_empty(),
        "saved password must not prompt"
    );
}

#[tokio::test]
async fn key_auth_with_passphrase() {
    if !enabled() {
        return;
    }
    let fx = Fixture::new();
    let p = profile(AuthMethod::Key {
        path: keys_dir().join("fb_test_ed25519_pass"),
        passphrase: true,
    });
    fx.store
        .set(&p.id, SecretKind::KeyPassphrase, KEY_PASSPHRASE)
        .unwrap();
    let script = Script::new(true, vec![]);
    let handle = connect(&p, &fx.ctx(script.clone())).await.unwrap();
    assert_echo_ok(&handle).await;
    assert!(script.secret_calls.lock().is_empty());
}

#[tokio::test]
async fn key_auth_prompts_for_passphrase_and_remembers() {
    if !enabled() {
        return;
    }
    let fx = Fixture::new();
    let p = profile(AuthMethod::Key {
        path: keys_dir().join("fb_test_ed25519_pass"),
        passphrase: true,
    });
    let script = Script::new(
        true,
        vec![answer("wrong", true), answer(KEY_PASSPHRASE, true)],
    );
    let handle = connect(&p, &fx.ctx(script.clone())).await.unwrap();
    assert_echo_ok(&handle).await;
    assert_eq!(
        *script.secret_calls.lock(),
        vec![
            (SecretKind::KeyPassphrase, false),
            (SecretKind::KeyPassphrase, true)
        ]
    );
    assert_eq!(
        fx.store
            .get(&p.id, SecretKind::KeyPassphrase)
            .unwrap()
            .as_deref(),
        Some(KEY_PASSPHRASE)
    );
}

#[tokio::test]
async fn key_auth_without_passphrase() {
    if !enabled() {
        return;
    }
    let fx = Fixture::new();
    let p = profile(AuthMethod::Key {
        path: keys_dir().join("fb_test_ed25519"),
        passphrase: false,
    });
    let script = Script::new(true, vec![]);
    let handle = connect(&p, &fx.ctx(script.clone())).await.unwrap();
    assert_echo_ok(&handle).await;
    assert!(script.secret_calls.lock().is_empty());
}

#[tokio::test]
async fn unknown_host_prompts_and_learns() {
    if !enabled() {
        return;
    }
    let fx = Fixture::new();
    let p = profile(AuthMethod::Password);
    fx.store.set(&p.id, SecretKind::Password, PASSWORD).unwrap();
    assert!(!fx.known_hosts.exists());

    let script = Script::new(true, vec![]);
    let handle = connect(&p, &fx.ctx(script.clone())).await.unwrap();
    assert_echo_ok(&handle).await;
    assert_eq!(script.host_key_calls(), 1);
    let lines = fx.known_hosts_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("[127.0.0.1]:2222 "), "{lines:?}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&fx.known_hosts), 0o600);
        assert_eq!(mode(fx.known_hosts.parent().unwrap()), 0o700);
    }

    let again = Script::new(false, vec![]);
    let handle = connect(&p, &fx.ctx(again.clone())).await.unwrap();
    assert_echo_ok(&handle).await;
    assert_eq!(again.host_key_calls(), 0, "a known host must not prompt");
    assert_eq!(fx.known_hosts_lines(), lines, "known_hosts must not change");
}

#[tokio::test]
async fn unknown_host_rejected() {
    if !enabled() {
        return;
    }
    let fx = Fixture::new();
    let p = profile(AuthMethod::Password);
    fx.store.set(&p.id, SecretKind::Password, PASSWORD).unwrap();
    let script = Script::new(false, vec![]);
    let err = connect(&p, &fx.ctx(script.clone())).await.err().unwrap();
    assert!(matches!(err, SshError::HostKeyRejected), "{err:?}");
    assert_eq!(script.host_key_calls(), 1);
    assert!(fx.known_hosts_lines().is_empty());
    assert!(
        script.secret_calls.lock().is_empty(),
        "no auth before the host is trusted"
    );
}

#[tokio::test]
async fn changed_host_key_refused_without_prompt() {
    if !enabled() {
        return;
    }
    let fx = Fixture::new();
    let p = profile(AuthMethod::Password);
    fx.store.set(&p.id, SecretKind::Password, PASSWORD).unwrap();
    // A key that is certainly not the container's host key: one of our test user keys.
    let foreign = std::fs::read_to_string(keys_dir().join("pub/fb_test_ed25519.pub")).unwrap();
    let foreign = foreign
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    std::fs::create_dir_all(fx.known_hosts.parent().unwrap()).unwrap();
    let original = format!("# kept\n[127.0.0.1]:2222 {foreign}\n");
    std::fs::write(&fx.known_hosts, &original).unwrap();

    let script = Script::new(true, vec![answer(PASSWORD, true)]);
    let err = connect(&p, &fx.ctx(script.clone())).await.err().unwrap();
    match err {
        SshError::HostKeyChanged { ref host, port } => {
            assert_eq!((host.as_str(), port), (HOST, PORT))
        }
        other => panic!("expected HostKeyChanged, got {other:?}"),
    }
    assert_eq!(
        script.host_key_calls(),
        0,
        "a changed key must never prompt"
    );
    assert!(script.secret_calls.lock().is_empty());
    assert_eq!(std::fs::read_to_string(&fx.known_hosts).unwrap(), original);
}

#[tokio::test]
async fn wrong_saved_password_reprompts_and_replaces_only_on_success() {
    if !enabled() {
        return;
    }
    let fx = Fixture::new();
    let p = profile(AuthMethod::Password);
    fx.store.set(&p.id, SecretKind::Password, "bad").unwrap();
    let script = Script::new(true, vec![answer("also-bad", true), answer(PASSWORD, true)]);
    let handle = connect(&p, &fx.ctx(script.clone())).await.unwrap();
    assert_echo_ok(&handle).await;
    assert_eq!(
        *script.secret_calls.lock(),
        vec![(SecretKind::Password, true), (SecretKind::Password, true)]
    );
    assert_eq!(
        fx.store
            .get(&p.id, SecretKind::Password)
            .unwrap()
            .as_deref(),
        Some(PASSWORD)
    );
}

#[tokio::test]
async fn rejected_password_keeps_saved_one() {
    if !enabled() {
        return;
    }
    let fx = Fixture::new();
    let p = profile(AuthMethod::Password);
    fx.store.set(&p.id, SecretKind::Password, "bad").unwrap();
    // Every prompted password is wrong too: the saved one must survive untouched.
    let script = Script::new(
        true,
        vec![answer("x1", true), answer("x2", true), answer("x3", true)],
    );
    let err = connect(&p, &fx.ctx(script.clone())).await.err().unwrap();
    assert!(
        matches!(err, SshError::AuthFailed { ref profile } if profile == "bastion"),
        "{err:?}"
    );
    assert_eq!(script.secret_calls.lock().len(), 3);
    assert_eq!(
        fx.store
            .get(&p.id, SecretKind::Password)
            .unwrap()
            .as_deref(),
        Some("bad")
    );
}

#[tokio::test]
async fn password_not_saved_without_remember() {
    if !enabled() {
        return;
    }
    let fx = Fixture::new();
    let p = profile(AuthMethod::Password);
    let script = Script::new(true, vec![answer(PASSWORD, false)]);
    let handle = connect(&p, &fx.ctx(script.clone())).await.unwrap();
    assert_echo_ok(&handle).await;
    assert_eq!(
        *script.secret_calls.lock(),
        vec![(SecretKind::Password, false)]
    );
    assert_eq!(fx.store.get(&p.id, SecretKind::Password).unwrap(), None);
}

#[tokio::test]
async fn cancelled_prompt_gives_cancelled() {
    if !enabled() {
        return;
    }
    let fx = Fixture::new();
    let p = profile(AuthMethod::Password);
    let script = Script::new(true, vec![None]);
    let err = connect(&p, &fx.ctx(script.clone())).await.err().unwrap();
    assert!(matches!(err, SshError::Cancelled), "{err:?}");
    assert_eq!(fx.store.get(&p.id, SecretKind::Password).unwrap(), None);
}

#[tokio::test]
async fn learned_key_is_known_to_check_host_key() {
    if !enabled() {
        return;
    }
    // learn_host_key writes what check_host_key reads: after a trusted connect, a key read back
    // from the file is reported Known for this host and port and Unknown for another port.
    let fx = Fixture::new();
    let p = profile(AuthMethod::Password);
    fx.store.set(&p.id, SecretKind::Password, PASSWORD).unwrap();
    let _handle = connect(&p, &fx.ctx(Script::new(true, vec![])))
        .await
        .unwrap();
    let line = fx.known_hosts_lines().remove(0);
    let key = russh::keys::PublicKey::from_openssh(
        &line
            .split_whitespace()
            .skip(1)
            .collect::<Vec<_>>()
            .join(" "),
    )
    .unwrap();
    assert_eq!(
        check_host_key(&fx.known_hosts, HOST, PORT, &key).unwrap(),
        HostKeyStatus::Known
    );
    assert_eq!(
        check_host_key(&fx.known_hosts, HOST, 2223, &key).unwrap(),
        HostKeyStatus::Unknown
    );
}

#[cfg(unix)]
#[tokio::test]
async fn agent_auth() {
    if !enabled() {
        return;
    }
    // One test owns SSH_AUTH_SOCK (no other test in this binary reads it): first an empty
    // agent (AuthFailed), then one holding the authorized key (success), then no agent at all.
    let fx = Fixture::new();
    let p = profile(AuthMethod::Agent);
    let sock = fx._dir.path().join("agent.sock");
    // Killed on drop, also when an assertion fails, so it never outlives the test run.
    struct Agent(std::process::Child);
    impl Drop for Agent {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _agent = Agent(
        std::process::Command::new("ssh-agent")
            .arg("-D")
            .arg("-a")
            .arg(&sock)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("ssh-agent must be installed for the live tests"),
    );
    for _ in 0..100 {
        if sock.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // SAFETY: no other test in this binary reads or writes SSH_AUTH_SOCK.
    unsafe { std::env::set_var("SSH_AUTH_SOCK", &sock) };

    let err = connect(&p, &fx.ctx(Script::new(true, vec![])))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, SshError::AuthFailed { .. }), "{err:?}");

    // ssh-add refuses keys readable by others; git checkouts are usually 0644.
    let key = fx._dir.path().join("id");
    std::fs::copy(keys_dir().join("fb_test_ed25519"), &key).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let added = std::process::Command::new("ssh-add")
        .arg(&key)
        .env("SSH_AUTH_SOCK", &sock)
        .output()
        .unwrap();
    assert!(added.status.success(), "{added:?}");
    let handle = connect(&p, &fx.ctx(Script::new(true, vec![])))
        .await
        .unwrap();
    assert_echo_ok(&handle).await;
    drop(handle);

    // SAFETY: as above.
    unsafe { std::env::remove_var("SSH_AUTH_SOCK") };
    let err = connect(&p, &fx.ctx(Script::new(true, vec![])))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, SshError::AuthFailed { .. }), "{err:?}");
}

// ---------------------------------------------------------------------------------------------
// Jump chains and the shared connection pool.
//
// Topology: 127.0.0.1:2222 -> bastion -> target -> target2 (sshd listens on 2222 everywhere).
// Every test uses the unencrypted test key, so no secret prompts happen; host keys are trusted
// on first use into a per-test known_hosts.
//
// Tests run in parallel against the same containers, so nothing here counts or kills
// connections globally: a connection is identified by `$SSH_CONNECTION` (its client address and
// port as the server sees them), and the drop test kills only its own connection.

mod pool {
    use super::*;

    use alacritty_terminal::index::{Column, Line};
    use fbterm_config::ProfileStore;
    use fbterm_session::ssh::SshPool;
    use fbterm_session::{AuthWarning, EventSink, SessionEvent, SessionHandle, SessionId};
    use fbterm_term::GridSize;
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    type Events = UnboundedReceiver<(SessionId, SessionEvent)>;

    const SIZE: GridSize = GridSize {
        cols: 100,
        rows: 30,
        cell_width: 8.0,
        cell_height: 16.0,
    };

    /// Trusts every host key and counts the questions; never expects a secret prompt.
    #[derive(Default)]
    struct TrustAll {
        host_key_calls: parking_lot::Mutex<Vec<(String, u16)>>,
    }

    #[async_trait]
    impl Prompter for TrustAll {
        async fn confirm_host_key(&self, host: &str, port: u16, _fp: &str) -> bool {
            self.host_key_calls.lock().push((host.to_owned(), port));
            true
        }
        async fn ask_secret(
            &self,
            name: &str,
            kind: SecretKind,
            _retry: bool,
        ) -> Option<SecretAnswer> {
            panic!("unexpected {kind:?} prompt for {name}");
        }
    }

    fn ssh(name: &str, host: &str, jump: Vec<ProfileId>) -> Profile {
        Profile {
            id: ProfileId::new(),
            name: name.into(),
            group: None,
            accent: None,
            accent_style: Default::default(),
            kind: ProfileKind::Ssh(SshProfile {
                host: host.into(),
                port: PORT,
                user: USER.into(),
                auth: AuthMethod::Key {
                    path: keys_dir().join("fb_test_ed25519"),
                    passphrase: false,
                },
                jump,
            }),
        }
    }

    struct World {
        fx: Fixture,
        prompter: Arc<TrustAll>,
        store: ProfileStore,
        bastion: ProfileId,
        target: ProfileId,
        target2: ProfileId,
    }

    impl World {
        fn new() -> Self {
            let bastion = ssh("bastion", HOST, vec![]);
            let target = ssh("target", "target", vec![bastion.id]);
            let target2 = ssh("target2", "target2", vec![target.id]);
            Self {
                fx: Fixture::new(),
                prompter: Arc::new(TrustAll::default()),
                bastion: bastion.id,
                target: target.id,
                target2: target2.id,
                store: ProfileStore {
                    profiles: vec![bastion, target, target2],
                },
            }
        }

        fn pool(&self) -> SshPool {
            SshPool::new(self.fx.ctx(self.prompter.clone()))
        }
    }

    async fn open(pool: &SshPool, w: &World, id: ProfileId) -> (SessionHandle, Events) {
        let (tx, rx): (EventSink, _) = unbounded_channel();
        let h = tokio::time::timeout(STEP, pool.open_shell(&w.store, &id, SIZE, 1000, tx))
            .await
            .expect("open_shell timed out")
            .expect("open_shell");
        (h, rx)
    }

    fn grid_text(handle: &SessionHandle) -> String {
        let t = handle.terminal.lock();
        let g = t.term().grid();
        let size = t.size();
        let mut out = String::new();
        for row in 0..size.rows as i32 {
            for col in 0..size.cols as usize {
                out.push(g[Line(row)][Column(col)].c);
            }
            out.push('\n');
        }
        out
    }

    /// Wait until the screen shows `needle`; returns the screen.
    async fn wait_for(handle: &SessionHandle, needle: &str) -> String {
        tokio::time::timeout(STEP, async {
            loop {
                let text = grid_text(handle);
                if text.contains(needle) {
                    return text;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {needle:?}:\n{}", grid_text(handle)))
    }

    /// Type a command line into the shell.
    fn run(handle: &SessionHandle, line: &str) {
        handle.send(format!("{line}\r").into_bytes());
    }

    /// The remote `$SSH_CONNECTION` (`client-ip client-port server-ip server-port`): the same
    /// value in two shells means the same TCP connection.
    async fn ssh_connection(handle: &SessionHandle) -> String {
        // The output is `<<C 10.0.0.1 4242 ...>>`; the echoed command line never has `<<C ` next
        // to a digit, so the match below finds only the output.
        run(handle, "echo \"<<C $SSH_CONNECTION>>\"");
        tokio::time::timeout(STEP, async {
            loop {
                let text = grid_text(handle);
                if let Some(v) = text.split("<<C ").skip(1).find_map(|s| {
                    let v = s.split(">>").next()?;
                    v.starts_with(|c: char| c.is_ascii_digit())
                        .then(|| v.to_owned())
                }) {
                    return v;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no $SSH_CONNECTION in:\n{}", grid_text(handle)))
    }

    /// Wait for the first `Exited`/`Disconnected` event.
    async fn wait_end(rx: &mut Events) -> SessionEvent {
        tokio::time::timeout(STEP, async {
            loop {
                if let (_, e @ (SessionEvent::Exited(_) | SessionEvent::Disconnected(_))) =
                    rx.recv().await.expect("event sink closed")
                {
                    return e;
                }
            }
        })
        .await
        .expect("timed out waiting for the session to end")
    }

    /// Whether the bastion still has an established TCP connection from `client_port`, checked
    /// over a separate connection.
    async fn bastion_has_connection_from(w: &World, client_port: &str) -> bool {
        let p = w.store.get(w.bastion).unwrap();
        let handle = connect(p, &w.fx.ctx(w.prompter.clone())).await.unwrap();
        let tables = exec(&handle, "cat /proc/net/tcp /proc/net/tcp6").await;
        let port_hex = format!("{:04X}", client_port.parse::<u16>().unwrap());
        // `sl local rem st ...`; `rem` is `ADDR:PORT` in hex and `01` is ESTABLISHED.
        tables.lines().any(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            f.len() > 3 && f[2].ends_with(&format!(":{port_hex}")) && f[3] == "01"
        })
    }

    #[tokio::test]
    async fn one_hop_reaches_unpublished_target() {
        if !enabled() {
            return;
        }
        let w = World::new();
        let pool = w.pool();
        let (h, _rx) = open(&pool, &w, w.target).await;
        run(&h, "echo \"@@$(hostname)@@\"");
        wait_for(&h, "@@target@@").await;
        // Each hop's key was checked under its own name, in connection order.
        assert_eq!(
            *w.prompter.host_key_calls.lock(),
            vec![(HOST.to_owned(), PORT), ("target".to_owned(), PORT)]
        );
        assert_eq!(pool.connected_profiles(), vec![w.target]);
    }

    #[tokio::test]
    async fn two_hops_reach_target2() {
        if !enabled() {
            return;
        }
        let w = World::new();
        let pool = w.pool();
        let (h, _rx) = open(&pool, &w, w.target2).await;
        run(&h, "echo \"@@$(hostname)@@\"");
        wait_for(&h, "@@target2@@").await;
        assert_eq!(w.prompter.host_key_calls.lock().len(), 3);
    }

    #[tokio::test]
    async fn rsa_user_key_is_reported_per_tab_and_ed25519_is_not() {
        if !enabled() {
            return;
        }
        let mut rsa = ssh("rsa", HOST, vec![]);
        if let ProfileKind::Ssh(s) = &mut rsa.kind {
            s.auth = AuthMethod::Key {
                path: keys_dir().join("fb_test_rsa"),
                passphrase: false,
            };
        }
        let rsa_id = rsa.id;
        let w = World::new();
        let w = World {
            store: ProfileStore {
                profiles: vec![rsa],
            },
            ..w
        };
        let pool = w.pool();
        let (a, _ra) = open(&pool, &w, rsa_id).await;
        let (b, _rb) = open(&pool, &w, rsa_id).await;
        // Both tabs share one connection and each reports the warning exactly once.
        assert_eq!(ssh_connection(&a).await, ssh_connection(&b).await);
        assert_eq!(a.auth_warnings(), [AuthWarning::RsaUserKey]);
        assert_eq!(b.auth_warnings(), [AuthWarning::RsaUserKey]);

        let w = World::new();
        let pool = w.pool();
        let (c, _rc) = open(&pool, &w, w.target2).await;
        assert!(c.auth_warnings().is_empty());
    }

    #[tokio::test]
    async fn two_tabs_share_one_connection() {
        if !enabled() {
            return;
        }
        let w = World::new();
        let pool = w.pool();
        // Opened concurrently: the second must wait for the first's connect, not dial again.
        let ((a, _ra), (b, _rb)) =
            tokio::join!(open(&pool, &w, w.bastion), open(&pool, &w, w.bastion));
        assert_ne!(a.id, b.id);
        let (ca, cb) = (ssh_connection(&a).await, ssh_connection(&b).await);
        assert_eq!(ca, cb, "both tabs must ride the same TCP connection");
        assert_eq!(
            w.prompter.host_key_calls.lock().len(),
            1,
            "dialled more than once"
        );
        // And a third tab, opened later, joins it too.
        let (c, _rc) = open(&pool, &w, w.bastion).await;
        assert_eq!(ssh_connection(&c).await, ca);
        // The server sees exactly that connection from our client port.
        let client_port = ca.split_whitespace().nth(1).unwrap().to_owned();
        assert!(bastion_has_connection_from(&w, &client_port).await);
        assert_eq!(pool.connected_profiles(), vec![w.bastion]);
    }

    #[tokio::test]
    async fn connection_closes_after_last_tab() {
        if !enabled() {
            return;
        }
        let w = World::new();
        let pool = w.pool();
        let (a, _ra) = open(&pool, &w, w.bastion).await;
        let (b, _rb) = open(&pool, &w, w.bastion).await;
        let conn = ssh_connection(&a).await;
        assert_eq!(ssh_connection(&b).await, conn);
        let client_port = conn.split_whitespace().nth(1).unwrap().to_owned();

        a.close();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            pool.connected_profiles(),
            vec![w.bastion],
            "one tab is still open"
        );
        run(&b, "echo \"@@$((40+2))@@\"");
        wait_for(&b, "@@42@@").await;

        drop(b);
        drop(a);
        tokio::time::timeout(Duration::from_secs(2), async {
            while !pool.connected_profiles().is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("connection still listed 2 s after its last tab closed");
        // The server sees the TCP connection go away as well.
        tokio::time::timeout(STEP, async {
            while bastion_has_connection_from(&w, &client_port).await {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .expect("server still has the connection");
    }

    #[tokio::test]
    async fn server_drop_emits_disconnected() {
        if !enabled() {
            return;
        }
        let w = World::new();
        let pool = w.pool();
        let (a, mut ra) = open(&pool, &w, w.target).await;
        let (_b, mut rb) = open(&pool, &w, w.target).await;
        let conn = ssh_connection(&a).await;
        run(&a, "echo \"@@keep-$((6*7))@@\"");
        wait_for(&a, "@@keep-42@@").await;

        // The shell's parent is the server process of this one connection: killing it drops
        // the connection (and only this one) without a clean channel close.
        run(&a, "kill -9 $PPID");
        // Both tabs on the dead connection report it.
        for rx in [&mut ra, &mut rb] {
            match wait_end(rx).await {
                SessionEvent::Disconnected(reason) => assert!(!reason.is_empty()),
                other => panic!("expected Disconnected, got {other:?}"),
            }
            // Exactly once, and never followed by Exited.
            tokio::time::sleep(Duration::from_millis(300)).await;
            while let Ok((_, e)) = rx.try_recv() {
                assert!(
                    !matches!(e, SessionEvent::Exited(_) | SessionEvent::Disconnected(_)),
                    "second end event {e:?}"
                );
            }
        }
        assert!(matches!(
            a.ended(),
            Some(fbterm_session::SessionEnd::Disconnected(_))
        ));
        assert!(
            grid_text(&a).contains("@@keep-42@@"),
            "terminal content lost"
        );
        assert!(
            pool.connected_profiles().is_empty(),
            "a dead connection is listed"
        );

        // The dead connection is not reused: the next tab dials afresh.
        let (c, _rc) = open(&pool, &w, w.target).await;
        let fresh = ssh_connection(&c).await;
        assert_ne!(fresh, conn);
        assert_eq!(pool.connected_profiles(), vec![w.target]);
    }

    #[tokio::test]
    async fn remote_exit_reports_status() {
        if !enabled() {
            return;
        }
        let w = World::new();
        let pool = w.pool();
        let (h, mut rx) = open(&pool, &w, w.bastion).await;
        run(&h, "exit 3");
        assert_eq!(wait_end(&mut rx).await, SessionEvent::Exited(Some(3)));
        // The handle remembers it, for a tab that installs it only after the event.
        assert_eq!(h.ended(), Some(fbterm_session::SessionEnd::Exited(Some(3))));
    }

    #[tokio::test]
    async fn reconnect_into_existing_terminal_keeps_old_content() {
        if !enabled() {
            return;
        }
        let w = World::new();
        let pool = w.pool();
        let (h, mut rx) = open(&pool, &w, w.bastion).await;
        run(&h, "echo \"@@first-$((40+2))@@\"");
        wait_for(&h, "@@first-42@@").await;
        run(&h, "exit");
        wait_end(&mut rx).await;
        let term = h.terminal.clone();
        drop(h);

        let (tx, _rx2): (EventSink, _) = unbounded_channel();
        let h2 = tokio::time::timeout(
            STEP,
            pool.open_shell_into(&w.store, &w.bastion, term.clone(), tx),
        )
        .await
        .expect("open_shell_into timed out")
        .expect("open_shell_into");
        assert!(Arc::ptr_eq(&h2.terminal, &term));
        run(&h2, "echo \"@@second-$((40+3))@@\"");
        let text = wait_for(&h2, "@@second-43@@").await;
        let first = text.find("@@first-42@@").expect("old content kept");
        let second = text.find("@@second-43@@").unwrap();
        assert!(
            first < second,
            "new output must follow the old content:\n{text}"
        );
    }

    #[tokio::test]
    async fn resize_reaches_the_remote_pty() {
        if !enabled() {
            return;
        }
        let w = World::new();
        let pool = w.pool();
        let (h, _rx) = open(&pool, &w, w.bastion).await;
        run(&h, "echo \"@@$(stty size)@@\"");
        wait_for(&h, "@@30 100@@").await;
        h.resize(GridSize {
            cols: 90,
            rows: 25,
            ..SIZE
        });
        run(&h, "echo \"@@$(stty size)@@\"");
        wait_for(&h, "@@25 90@@").await;
    }

    #[tokio::test]
    async fn unreachable_hop_error_names_the_hop() {
        if !enabled() {
            return;
        }
        let mut w = World::new();
        let ghost = ssh("ghost", "no-such-host.invalid", vec![w.bastion]);
        let ghost_id = ghost.id;
        w.store.profiles.push(ghost);
        let pool = w.pool();
        let (tx, _rx): (EventSink, _) = unbounded_channel();
        let err = tokio::time::timeout(STEP, pool.open_shell(&w.store, &ghost_id, SIZE, 10, tx))
            .await
            .expect("open_shell timed out")
            .err()
            .expect("an unreachable target must fail");
        let text = err.to_string();
        assert!(matches!(err, SshError::Connect(_)), "{err:?}");
        assert!(
            text.contains("bastion (127.0.0.1:2222)") && text.contains("no-such-host.invalid"),
            "{text}"
        );
        assert!(pool.connected_profiles().is_empty());
    }

    /// Blocks every host-key question until released; records whether the question was
    /// dropped (cancelled) before it was answered.
    #[derive(Default)]
    struct Gate {
        asked: tokio::sync::Notify,
        release: tokio::sync::Notify,
        dropped_unanswered: AtomicUsize,
        answered: AtomicUsize,
    }

    #[async_trait]
    impl Prompter for Gate {
        async fn confirm_host_key(&self, _host: &str, _port: u16, _fp: &str) -> bool {
            struct Pending<'a>(&'a AtomicUsize, bool);
            impl Drop for Pending<'_> {
                fn drop(&mut self) {
                    if !self.1 {
                        self.0.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
            let mut pending = Pending(&self.dropped_unanswered, false);
            let released = self.release.notified();
            self.asked.notify_one();
            released.await;
            pending.1 = true;
            self.answered.fetch_add(1, Ordering::SeqCst);
            true
        }
        async fn ask_secret(
            &self,
            name: &str,
            kind: SecretKind,
            _retry: bool,
        ) -> Option<SecretAnswer> {
            panic!("unexpected {kind:?} prompt for {name}");
        }
    }

    #[tokio::test]
    async fn aborted_dial_cancels_host_key_prompt() {
        if !enabled() {
            return;
        }
        let w = World::new();
        let gate = Arc::new(Gate::default());
        let pool = SshPool::new(w.fx.ctx(gate.clone()));
        let (tx, _rx): (EventSink, _) = unbounded_channel();
        // Open a tab and close it (drop the open) while the host-key question is showing.
        tokio::time::timeout(STEP, async {
            tokio::select! {
                r = pool.open_shell(&w.store, &w.bastion, SIZE, 10, tx) => {
                    panic!("open finished without an answer: {:?}", r.err())
                }
                () = gate.asked.notified() => {}
            }
        })
        .await
        .expect("the host-key question never came");

        // The question is withdrawn ...
        tokio::time::timeout(Duration::from_secs(2), async {
            while gate.dropped_unanswered.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the host-key prompt stayed open after the dial was aborted");
        // ... and a late "yes" goes nowhere: nothing is learned for the closed tab.
        gate.release.notify_waiters();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(gate.answered.load(Ordering::SeqCst), 0);
        assert!(
            w.fx.known_hosts_lines().is_empty(),
            "{:?}",
            w.fx.known_hosts_lines()
        );
        assert!(pool.connected_profiles().is_empty());
    }
}
