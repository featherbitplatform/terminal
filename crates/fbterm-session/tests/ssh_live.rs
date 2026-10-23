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
