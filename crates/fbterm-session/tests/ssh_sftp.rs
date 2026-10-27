//! Live SFTP tests (home folder, listings, kinds and errors) against the containers in
//! `dev/ssh-test/compose.yaml`.
//!
//! Skipped (they return early and pass) unless `FBTERM_TEST_SSH=1`:
//!
//! ```sh
//! docker compose -f dev/ssh-test/compose.yaml up -d && dev/ssh-test/wait-ready.sh
//! FBTERM_TEST_SSH=1 cargo test -p fbterm-session --test ssh_sftp
//! ```
//!
//! Fixtures (files, links, a pipe) are made by each test in a fresh folder through a shell on
//! the same profile, so tests never depend on each other or on the container's history.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fbterm_config::{AuthMethod, Profile, ProfileId, ProfileKind, ProfileStore, SshProfile};
use fbterm_secrets::{MemoryStore, SecretKind};
use fbterm_session::ssh::sftp::{Entry, EntryKind, Sftp, SftpError};
use fbterm_session::ssh::{Prompter, SecretAnswer, SshContext, SshPool};
use fbterm_session::{EventSink, SessionHandle};
use fbterm_term::GridSize;
use feathercritty::index::{Column, Line};
use tokio::sync::mpsc::unbounded_channel;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 2222;
const USER: &str = "fb";
/// The test image's home folder for `fb`.
const HOME: &str = "/config";
/// Bound for every network await, so a broken server fails a test instead of hanging it.
const STEP: Duration = Duration::from_secs(20);

const SIZE: GridSize = GridSize {
    cols: 200,
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
                path: keys_dir().join("fb_test_ed25519"),
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
    target2: ProfileId,
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
        let (b, t2) = (bastion.id, target2.id);
        Self {
            _dir: dir,
            pool,
            store: ProfileStore {
                profiles: vec![bastion, target, target2],
            },
            bastion: b,
            target2: t2,
            trust,
        }
    }

    async fn sftp(&self, profile: ProfileId) -> Sftp {
        tokio::time::timeout(
            STEP,
            Sftp::open(&self.pool, &self.store, &profile, self.trust.clone()),
        )
        .await
        .expect("Sftp::open timed out")
        .expect("Sftp::open")
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

async fn list(sftp: &Sftp, path: &str) -> Result<Vec<Entry>, SftpError> {
    tokio::time::timeout(STEP, sftp.list(path))
        .await
        .expect("list timed out")
}

fn entry<'a>(entries: &'a [Entry], name: &str) -> &'a Entry {
    entries
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("no {name} in {entries:?}"))
}

#[tokio::test]
async fn home_is_reported() {
    if !enabled() {
        return;
    }
    let w = World::new();
    let sftp = w.sftp(w.bastion).await;
    assert_eq!(sftp.home().await.unwrap(), HOME);
    // The test image runs OpenSSH, which offers the atomic rename.
    assert!(sftp.supports_posix_rename());
}

#[tokio::test]
async fn list_home_through_jump() {
    if !enabled() {
        return;
    }
    let w = World::new();
    // `target2` is two hops away (bastion, then target); a marker made there by its own shell
    // shows the listing comes from that server and not from a hop.
    let shell = w.shell(w.target2).await;
    let marker = shell_value(
        &shell,
        "M",
        "R=$(basename \"$(mktemp ~/sftp-mark-XXXXXX)\")",
    )
    .await;
    assert!(marker.starts_with("sftp-mark-"), "{marker:?}");

    let sftp = w.sftp(w.target2).await;
    let home = sftp.home().await.unwrap();
    assert_eq!(home, HOME);
    let entries = list(&sftp, &home).await.unwrap();
    assert_eq!(entry(&entries, &marker).kind, EntryKind::File);
    assert!(
        entries.iter().all(|e| e.name != "." && e.name != ".."),
        "{entries:?}"
    );
    shell.close();
}

#[tokio::test]
async fn list_shows_links_and_kinds() {
    if !enabled() {
        return;
    }
    let w = World::new();
    let shell = w.shell(w.bastion).await;
    let dir = shell_value(
        &shell,
        "D",
        "R=$(mktemp -d ~/sftp-kinds-XXXXXX) && cd \"$R\" && printf hello > file.txt \
         && mkdir Sub && ln -s file.txt to-file && ln -s Sub to-dir && ln -s nowhere broken \
         && mkfifo pipe && touch -d '2001-02-03 04:05:06 UTC' file.txt && cd",
    )
    .await;
    assert!(dir.starts_with(HOME), "{dir:?}");

    let sftp = w.sftp(w.bastion).await;
    let entries = list(&sftp, &dir).await.unwrap();

    // Folders (and links to folders) first, then the rest, each by name.
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        ["Sub", "to-dir", "broken", "file.txt", "pipe", "to-file"]
    );

    let file = entry(&entries, "file.txt");
    assert_eq!(file.kind, EntryKind::File);
    assert_eq!(file.size, 5);
    assert_eq!(
        file.modified,
        Some(std::time::UNIX_EPOCH + Duration::from_secs(981_173_106))
    );
    assert_eq!(file.mode.map(|m| m & 0o7777), Some(0o644));

    assert_eq!(entry(&entries, "Sub").kind, EntryKind::Dir);
    assert_eq!(entry(&entries, "pipe").kind, EntryKind::Other);
    assert_eq!(
        entry(&entries, "to-file").kind,
        EntryKind::Link {
            target: "file.txt".into(),
            to: Some(Box::new(EntryKind::File)),
        }
    );
    assert_eq!(
        entry(&entries, "to-dir").kind,
        EntryKind::Link {
            target: "Sub".into(),
            to: Some(Box::new(EntryKind::Dir)),
        }
    );
    assert_eq!(
        entry(&entries, "broken").kind,
        EntryKind::Link {
            target: "nowhere".into(),
            to: None,
        }
    );

    // `stat` follows a link, `lstat` does not.
    let to_file = format!("{dir}/to-file");
    let followed = sftp.stat(&to_file).await.unwrap();
    assert_eq!(followed.name, "to-file");
    assert_eq!(followed.kind, EntryKind::File);
    assert_eq!(followed.size, 5);
    let link = sftp.lstat(&to_file).await.unwrap();
    assert!(matches!(link.kind, EntryKind::Link { .. }), "{link:?}");
    assert!(matches!(
        sftp.stat(&format!("{dir}/broken")).await,
        Err(SftpError::NotFound(_))
    ));
    shell.close();
}

#[tokio::test]
async fn missing_folder_is_not_found() {
    if !enabled() {
        return;
    }
    let w = World::new();
    let sftp = w.sftp(w.bastion).await;
    let err = list(&sftp, "/no/such/folder").await.unwrap_err();
    assert!(matches!(err, SftpError::NotFound(_)), "{err:?}");
    let message = err.to_string();
    assert!(message.contains("/no/such/folder"), "{message}");
}

#[tokio::test]
async fn denied_folder_is_denied() {
    if !enabled() {
        return;
    }
    let w = World::new();
    let sftp = w.sftp(w.bastion).await;
    let err = list(&sftp, "/root").await.unwrap_err();
    assert!(matches!(err, SftpError::Denied(_)), "{err:?}");
    let message = err.to_string();
    assert!(message.contains("/root"), "{message}");
    // The session stays usable after an error.
    assert_eq!(sftp.home().await.unwrap(), HOME);
}
