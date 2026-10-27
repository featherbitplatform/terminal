//! The panel's real backend ([`PoolBackend`]) end to end against the containers in
//! `dev/ssh-test/compose.yaml`: open a session, upload (new, refused clash, replace) and
//! download the file back.
//!
//! Skipped (returns early and passes) unless `FBTERM_TEST_SSH=1`:
//!
//! ```sh
//! docker compose -f dev/ssh-test/compose.yaml up -d && dev/ssh-test/wait-ready.sh
//! FBTERM_TEST_SSH=1 cargo test -p fbterm file_panel::live_tests
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fbterm_config::{AuthMethod, Profile, ProfileId, ProfileKind, ProfileStore, SshProfile};
use fbterm_secrets::{MemoryStore, SecretKind};
use fbterm_session::ssh::transfer::{DownloadMode, TransferEvent, TransferId, UploadMode};
use fbterm_session::ssh::{Prompter, SecretAnswer, SshContext, SshPool};
use iced::window;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use super::{
    Backend, DownloadRequest, OpenRequest, Opened, PanelEvent, PoolBackend, Remote, Started,
    UploadRequest,
};

/// Bound for every network await, so a broken server fails the test instead of hanging it.
const STEP: Duration = Duration::from_secs(20);

fn enabled() -> bool {
    std::env::var("FBTERM_TEST_SSH").as_deref() == Ok("1")
}

/// Trusts the test host and never expects a secret question.
struct TrustAny;

#[async_trait::async_trait]
impl Prompter for TrustAny {
    async fn unlock_vault(&self) -> Option<fbterm_secrets::Zeroizing<String>> {
        None
    }

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

fn profile() -> Profile {
    let key = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dev/ssh-test/keys/fb_test_ed25519");
    Profile {
        id: ProfileId::new(),
        name: "panel-live".into(),
        group: None,
        accent: None,
        accent_style: Default::default(),
        color_scheme: None,
        kind: ProfileKind::Ssh(SshProfile {
            host: "127.0.0.1".into(),
            port: 2222,
            user: "fb".into(),
            auth: AuthMethod::Key {
                path: Some(key),
                passphrase: false,
            },
            jump: vec![],
            tunnels: Vec::new(),
        }),
    }
}

/// The final event of transfer `id`.
async fn outcome(
    rx: &mut UnboundedReceiver<(TransferId, TransferEvent)>,
    id: TransferId,
) -> TransferEvent {
    loop {
        let (got, event) = tokio::time::timeout(STEP, rx.recv())
            .await
            .expect("the transfer reported in time")
            .expect("the transfer channel is open");
        if got == id && !matches!(event, TransferEvent::Progress { .. }) {
            return event;
        }
    }
}

/// Download `path` to `dest` through `backend` and return what arrived.
async fn fetch(
    backend: &PoolBackend,
    remote: &Arc<dyn Remote>,
    path: String,
    dest: PathBuf,
) -> String {
    let (tx, mut rx) = unbounded_channel();
    let Started { id, .. } = backend
        .download(
            DownloadRequest {
                remote: remote.clone(),
                path,
                dest: dest.clone(),
                mode: DownloadMode::FreeName,
            },
            tx,
        )
        .expect("the download starts");
    assert!(matches!(
        outcome(&mut rx, id).await,
        TransferEvent::Finished { .. }
    ));
    std::fs::read_to_string(&dest).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pool_backend_uploads_and_downloads() {
    if !enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let trust: Arc<dyn Prompter> = Arc::new(TrustAny);
    let pool = Arc::new(SshPool::new(SshContext {
        known_hosts: dir.path().join("known_hosts"),
        secrets: Arc::new(MemoryStore::new()),
        prompter: trust.clone(),
    }));
    let p = profile();
    let id = p.id;
    let store = ProfileStore { profiles: vec![p] };
    let backend = PoolBackend(pool);

    // Open the session as the panel does.
    let (sink, mut panel_rx) = unbounded_channel();
    backend.open(
        OpenRequest {
            profile: id,
            token: 1,
            store,
            prompter: trust,
            window: window::Id::unique(),
        },
        sink,
    );
    let event = tokio::time::timeout(STEP, panel_rx.recv())
        .await
        .expect("opened in time")
        .expect("an answer");
    let PanelEvent::Opened {
        result: Ok(Opened { remote, home }),
        ..
    } = event
    else {
        panic!("the session did not open");
    };
    let remote: Arc<dyn Remote> = remote;
    assert!(remote.is_alive());
    assert_eq!(home.as_deref(), Ok("/config"));

    let folder = "/tmp".to_owned();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("fbterm-panel-live-{nanos}.conf");
    let (tx, mut rx) = unbounded_channel();
    let upload = |local: PathBuf, mode| {
        let Started { id, .. } = backend
            .upload(
                UploadRequest {
                    remote: remote.clone(),
                    local,
                    folder: folder.clone(),
                    name: name.clone(),
                    mode,
                },
                tx.clone(),
            )
            .expect("the upload starts");
        id
    };
    let first = dir.path().join("first.conf");
    std::fs::write(&first, "first\n").unwrap();
    let second = dir.path().join("second.conf");
    std::fs::write(&second, "second version\n").unwrap();

    // A new file.
    let up = upload(first.clone(), UploadMode::New);
    assert_eq!(
        outcome(&mut rx, up).await,
        TransferEvent::Finished {
            note: None,
            local: None
        }
    );
    // The same name again without Replace: refused with the clash copy, nothing replaced.
    let up = upload(second.clone(), UploadMode::New);
    assert_eq!(
        outcome(&mut rx, up).await,
        TransferEvent::Failed(format!("\"{name}\" already exists in {folder}."))
    );
    // Nothing was replaced: the server still holds the first file.
    let kept = dir.path().join("kept.conf");
    assert_eq!(
        fetch(&backend, &remote, format!("{folder}/{name}"), kept).await,
        "first\n"
    );
    // Replace.
    let up = upload(second.clone(), UploadMode::Replace);
    assert!(matches!(
        outcome(&mut rx, up).await,
        TransferEvent::Finished { .. }
    ));

    // And back down.
    let dest = dir.path().join("back.conf");
    let Started { id: down, .. } = backend
        .download(
            DownloadRequest {
                remote: remote.clone(),
                path: format!("{folder}/{name}"),
                dest: dest.clone(),
                mode: DownloadMode::Replace,
            },
            tx.clone(),
        )
        .expect("the download starts");
    assert_eq!(
        outcome(&mut rx, down).await,
        TransferEvent::Finished {
            note: None,
            local: Some(dest.clone())
        }
    );
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "second version\n");

    // Leave nothing behind on the server.
    let sftp = remote.sftp().expect("a real session");
    tokio::time::timeout(STEP, sftp.remove(&format!("{folder}/{name}")))
        .await
        .expect("removed in time")
        .expect("removed");
}
