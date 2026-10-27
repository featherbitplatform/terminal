//! Live SFTP tests (home folder, listings, kinds, errors and transfers) against the containers in
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
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fbterm_config::{AuthMethod, Profile, ProfileId, ProfileKind, ProfileStore, SshProfile};
use fbterm_secrets::{MemoryStore, SecretKind};
use fbterm_session::ssh::sftp::{Entry, EntryKind, Sftp, SftpError};
use fbterm_session::ssh::transfer::{
    DownloadMode, REPLACED_IN_PLACE, TransferEvent, TransferId, UploadMode, download, upload,
};
use fbterm_session::ssh::{Prompter, SecretAnswer, SshContext, SshPool};
use fbterm_session::{EventSink, SessionHandle};
use fbterm_term::GridSize;
use feathercritty::index::{Column, Line};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

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
    key_profile_at(name, host, PORT, jump)
}

fn key_profile_at(name: &str, host: &str, port: u16, jump: Vec<ProfileId>) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        group: None,
        accent: None,
        accent_style: Default::default(),
        color_scheme: None,
        kind: ProfileKind::Ssh(SshProfile {
            host: host.into(),
            port,
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
    async fn unlock_vault(&self) -> Option<fbterm_secrets::Zeroizing<String>> {
        None
    }

    async fn set_vault_password(&self) -> Option<fbterm_secrets::Zeroizing<String>> {
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

    /// Add a profile for the bastion reached through `proxy`.
    fn add_via_proxy(&mut self, proxy: &Proxy) -> ProfileId {
        let profile = key_profile_at("proxied", HOST, proxy.port, vec![]);
        let id = profile.id;
        self.store.profiles.push(profile);
        id
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

/// Remove a fixture (made under the home folder) through the test's shell, before the
/// assertions run so a failing test leaves nothing behind either.
async fn remove(handle: &SessionHandle, path: &str) {
    assert!(path.starts_with(&format!("{HOME}/sftp-")), "{path}");
    let gone = shell_value(
        handle,
        "RM",
        &format!(
            "chmod -R u+w -- '{path}'; rm -rf -- '{path}' && R=$(test -e '{path}' || echo gone)"
        ),
    )
    .await;
    assert_eq!(gone, "gone", "fixture {path} was not removed");
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
    let home = sftp.home().await;
    let entries = list(&sftp, HOME).await;
    remove(&shell, &format!("{HOME}/{marker}")).await;
    assert_eq!(home.unwrap(), HOME);
    let entries = entries.unwrap();
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
    let entries = list(&sftp, &dir).await;
    let to_file = format!("{dir}/to-file");
    let followed = sftp.stat(&to_file).await;
    let link = sftp.lstat(&to_file).await;
    let broken = sftp.stat(&format!("{dir}/broken")).await;
    remove(&shell, &dir).await;
    let entries = entries.unwrap();

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
    let followed = followed.unwrap();
    assert_eq!(followed.name, "to-file");
    assert_eq!(followed.kind, EntryKind::File);
    assert_eq!(followed.size, 5);
    let link = link.unwrap();
    assert!(matches!(link.kind, EntryKind::Link { .. }), "{link:?}");
    assert!(matches!(broken, Err(SftpError::NotFound(_))), "{broken:?}");
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

/// Bound for one transfer event: a transfer of a few tens of MB through the containers.
const TRANSFER_STEP: Duration = Duration::from_secs(60);

type Events = UnboundedReceiver<(TransferId, TransferEvent)>;

/// The next event of transfer `id`, `None` once the transfer task has ended.
async fn next_event(rx: &mut Events, id: TransferId) -> Option<TransferEvent> {
    let got = tokio::time::timeout(TRANSFER_STEP, rx.recv())
        .await
        .expect("no transfer event in time");
    got.map(|(from, event)| {
        assert_eq!(from, id);
        event
    })
}

/// Every event of transfer `id` until its task ends, with the time each arrived.
async fn timed_events(rx: &mut Events, id: TransferId) -> Vec<(Instant, TransferEvent)> {
    let mut events = Vec::new();
    while let Some(event) = next_event(rx, id).await {
        events.push((Instant::now(), event));
    }
    events
}

/// Wait until transfer `id` reports bytes moved (a progress event with `done > 0`).
async fn until_moving(rx: &mut Events, id: TransferId) {
    loop {
        match next_event(rx, id).await {
            Some(TransferEvent::Progress { done, .. }) if done > 0 => return,
            Some(TransferEvent::Progress { .. }) => {}
            other => panic!("the transfer ended before moving any data: {other:?}"),
        }
    }
}

async fn events(rx: &mut Events, id: TransferId) -> Vec<TransferEvent> {
    timed_events(rx, id)
        .await
        .into_iter()
        .map(|(_, e)| e)
        .collect()
}

fn last(events: &[TransferEvent]) -> &TransferEvent {
    events.last().expect("no transfer events")
}

fn local_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A fresh fixture folder under the home folder, made by `setup` (run inside it).
async fn fixture(shell: &SessionHandle, tag: &str, setup: &str) -> String {
    let dir = shell_value(
        shell,
        tag,
        &format!("R=$(mktemp -d ~/sftp-{tag}-XXXXXX) && (cd \"$R\" && {setup})"),
    )
    .await;
    assert!(dir.starts_with(&format!("{HOME}/sftp-")), "{dir:?}");
    dir
}

/// What the shell prints for `command` (one line, as `$R`).
async fn shell_line(shell: &SessionHandle, tag: &str, command: &str) -> String {
    shell_value(shell, tag, &format!("R=$({command})")).await
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 % 251) as u8).collect()
}

#[tokio::test]
async fn download_round_trip() {
    if !enabled() {
        return;
    }
    let w = World::new();
    // Through two jump hosts.
    let shell = w.shell(w.target2).await;
    let dir = fixture(
        &shell,
        "dl",
        "head -c 3000000 /dev/urandom > data.bin && ln -s data.bin link && ln -s nowhere broken \
         && mkfifo pipe && mkdir Sub",
    )
    .await;
    let remote_sum = shell_line(&shell, "SUM", &format!("cksum < '{dir}/data.bin'")).await;

    let sftp = Arc::new(w.sftp(w.target2).await);
    let local = tempfile::tempdir().unwrap();
    let mut results = Vec::new();
    for name in ["data.bin", "link", "broken", "pipe", "Sub"] {
        let (tx, mut rx) = unbounded_channel();
        let dest = local.path().join(format!("got-{name}"));
        let handle = download(
            sftp.clone(),
            format!("{dir}/{name}"),
            dest,
            DownloadMode::FreeName,
            tx,
        );
        results.push(events(&mut rx, handle.id()).await);
    }
    // Checksums of what arrived, made by the same shell tool on the server side for comparison.
    let got = std::fs::read(local.path().join("got-data.bin")).unwrap();
    let via_link = std::fs::read(local.path().join("got-link")).unwrap();
    remove(&shell, &dir).await;
    shell.close();

    assert_eq!(got.len(), 3_000_000);
    assert_eq!(cksum(&got), remote_sum);
    assert_eq!(via_link, got, "a link to a file downloads its target");
    let finished = |events: &[TransferEvent], name: &str| {
        assert_eq!(
            last(events),
            &TransferEvent::Finished {
                note: None,
                local: Some(local.path().join(format!("got-{name}"))),
            }
        );
        // Progress starts at zero of the stat size and ends at the full size.
        assert_eq!(
            events.first(),
            Some(&TransferEvent::Progress {
                done: 0,
                total: Some(3_000_000),
            })
        );
        assert_eq!(
            events[events.len() - 2],
            TransferEvent::Progress {
                done: 3_000_000,
                total: Some(3_000_000),
            }
        );
    };
    finished(&results[0], "data.bin");
    finished(&results[1], "link");
    for (events, says) in [
        (&results[2], "broken link"),
        (&results[3], "not a regular file"),
        (&results[4], "folder"),
    ] {
        match last(events) {
            TransferEvent::Failed(m) => assert!(m.contains(says), "{m}"),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(local_names(local.path()), ["got-data.bin", "got-link"]);
}

/// POSIX `cksum` of `data` as the shell prints it (`<crc> <length>`).
fn cksum(data: &[u8]) -> String {
    let mut crc: u32 = 0;
    let mut feed = |byte: u8| {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    };
    for &b in data {
        feed(b);
    }
    let mut len = data.len();
    while len > 0 {
        feed((len & 0xff) as u8);
        len >>= 8;
    }
    format!("{} {}", !crc, data.len())
}

#[tokio::test]
async fn cancel_download_leaves_nothing() {
    if !enabled() {
        return;
    }
    let w = World::new();
    let shell = w.shell(w.bastion).await;
    let dir = fixture(&shell, "cancel", "head -c 200000000 /dev/zero > big").await;

    let sftp = Arc::new(w.sftp(w.bastion).await);
    let local = tempfile::tempdir().unwrap();
    let (tx, mut rx) = unbounded_channel();
    let handle = download(
        sftp,
        format!("{dir}/big"),
        local.path().join("big"),
        DownloadMode::FreeName,
        tx,
    );
    // Cancel mid-transfer, once data has reached the part file.
    until_moving(&mut rx, handle.id()).await;
    let part_before = local_names(local.path());
    handle.cancel();
    handle.cancel();
    let rest = events(&mut rx, handle.id()).await;
    handle.cancel();
    remove(&shell, &dir).await;
    shell.close();

    assert_eq!(part_before, [".big.fbterm-part"]);
    assert_eq!(last(&rest), &TransferEvent::Cancelled);
    assert_eq!(
        rest.iter()
            .filter(|e| **e == TransferEvent::Cancelled)
            .count(),
        1
    );
    assert!(
        local_names(local.path()).is_empty(),
        "{:?}",
        local_names(local.path())
    );
}

#[tokio::test]
async fn upload_new_file() {
    if !enabled() {
        return;
    }
    let w = World::new();
    let shell = w.shell(w.target2).await;
    let dir = fixture(&shell, "up", "printf keep > taken.txt").await;
    let local = tempfile::tempdir().unwrap();
    let content = pattern(1_500_000);
    let path = local.path().join("local.bin");
    std::fs::write(&path, &content).unwrap();

    let sftp = Arc::new(w.sftp(w.target2).await);
    let (tx, mut rx) = unbounded_channel();
    let handle = upload(
        sftp.clone(),
        path.clone(),
        dir.clone(),
        "fresh.bin".into(),
        UploadMode::New,
        tx,
    );
    let fresh = events(&mut rx, handle.id()).await;
    let (tx, mut rx) = unbounded_channel();
    let handle = upload(
        sftp.clone(),
        path.clone(),
        dir.clone(),
        "taken.txt".into(),
        UploadMode::New,
        tx,
    );
    let clash = events(&mut rx, handle.id()).await;
    let sum = shell_line(&shell, "SUM", &format!("cksum < '{dir}/fresh.bin'")).await;
    let mode = shell_line(&shell, "MODE", &format!("stat -c %a '{dir}/fresh.bin'")).await;
    let umask = shell_line(&shell, "UMASK", "umask").await;
    let kept = shell_line(&shell, "KEPT", &format!("cat '{dir}/taken.txt'")).await;
    let names = list(&sftp, &dir).await;
    remove(&shell, &dir).await;
    shell.close();

    assert_eq!(
        last(&fresh),
        &TransferEvent::Finished {
            note: None,
            local: None,
        }
    );
    assert_eq!(sum, cksum(&content));
    // The server's default permissions: 0666 less its umask.
    let umask = u32::from_str_radix(umask.trim(), 8).unwrap();
    assert_eq!(mode, format!("{:o}", 0o666 & !umask));
    let short = dir.rsplit('/').next().unwrap();
    assert_eq!(
        last(&clash),
        &TransferEvent::Failed(format!("\"taken.txt\" already exists in {dir}."))
    );
    assert_eq!(kept, "keep");
    let names: Vec<String> = names.unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(
        names,
        ["fresh.bin", "taken.txt"],
        "no part file left in {short}"
    );
}

#[tokio::test]
async fn replace_keeps_mode() {
    if !enabled() {
        return;
    }
    let w = World::new();
    let shell = w.shell(w.bastion).await;
    let dir = fixture(&shell, "mode", "printf old > run.sh && chmod 750 run.sh").await;
    let local = tempfile::tempdir().unwrap();
    let path = local.path().join("run.sh");
    std::fs::write(&path, b"replaced-content").unwrap();

    let sftp = Arc::new(w.sftp(w.bastion).await);
    let (tx, mut rx) = unbounded_channel();
    let handle = upload(
        sftp.clone(),
        path,
        dir.clone(),
        "run.sh".into(),
        UploadMode::Replace,
        tx,
    );
    let got = events(&mut rx, handle.id()).await;
    let mode = shell_line(&shell, "MODE", &format!("stat -c %a '{dir}/run.sh'")).await;
    let text = shell_line(&shell, "TEXT", &format!("cat '{dir}/run.sh'")).await;
    let names = list(&sftp, &dir).await;
    remove(&shell, &dir).await;
    shell.close();

    assert_eq!(
        last(&got),
        &TransferEvent::Finished {
            note: None,
            local: None,
        }
    );
    assert_eq!(mode, "750");
    assert_eq!(text, "replaced-content");
    let names: Vec<String> = names.unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["run.sh"]);
}

#[tokio::test]
async fn replace_without_folder_write_writes_in_place() {
    if !enabled() {
        return;
    }
    let w = World::new();
    let shell = w.shell(w.bastion).await;
    let dir = fixture(
        &shell,
        "ro",
        "mkdir locked && printf old-old-old > locked/conf && chmod 600 locked/conf \
         && chmod 555 locked",
    )
    .await;
    let folder = format!("{dir}/locked");
    let local = tempfile::tempdir().unwrap();
    let path = local.path().join("conf");
    std::fs::write(&path, b"new").unwrap();

    let sftp = Arc::new(w.sftp(w.bastion).await);
    let (tx, mut rx) = unbounded_channel();
    let handle = upload(
        sftp.clone(),
        path,
        folder.clone(),
        "conf".into(),
        UploadMode::Replace,
        tx,
    );
    let got = events(&mut rx, handle.id()).await;
    let text = shell_line(&shell, "TEXT", &format!("cat '{folder}/conf'")).await;
    let mode = shell_line(&shell, "MODE", &format!("stat -c %a '{folder}/conf'")).await;
    let names = list(&sftp, &folder).await;
    remove(&shell, &dir).await;
    shell.close();

    assert_eq!(
        last(&got),
        &TransferEvent::Finished {
            note: Some(REPLACED_IN_PLACE.into()),
            local: None,
        }
    );
    assert!(got.contains(&TransferEvent::InPlace), "{got:?}");
    assert_eq!(text, "new");
    assert_eq!(mode, "600");
    let names: Vec<String> = names.unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["conf"]);
}

#[tokio::test]
async fn replace_onto_a_link_keeps_the_link() {
    if !enabled() {
        return;
    }
    let w = World::new();
    let shell = w.shell(w.bastion).await;
    let dir = fixture(&shell, "link", "printf old > real && ln -s real link").await;
    let local = tempfile::tempdir().unwrap();
    let path = local.path().join("link");
    std::fs::write(&path, b"through-the-link").unwrap();

    let sftp = Arc::new(w.sftp(w.bastion).await);
    let (tx, mut rx) = unbounded_channel();
    let handle = upload(
        sftp,
        path,
        dir.clone(),
        "link".into(),
        UploadMode::Replace,
        tx,
    );
    let got = events(&mut rx, handle.id()).await;
    let target = shell_line(&shell, "LINK", &format!("readlink '{dir}/link'")).await;
    let text = shell_line(&shell, "TEXT", &format!("cat '{dir}/real'")).await;
    remove(&shell, &dir).await;
    shell.close();

    assert_eq!(
        last(&got),
        &TransferEvent::Finished {
            note: Some(REPLACED_IN_PLACE.into()),
            local: None,
        }
    );
    assert_eq!(target, "real", "the link is still a link to the same file");
    assert_eq!(text, "through-the-link");
}

#[tokio::test]
async fn connection_drop_fails_upload_and_keeps_target() {
    if !enabled() {
        return;
    }
    let proxy = Proxy::new().await;
    let mut w = World::new();
    let proxied = w.add_via_proxy(&proxy);
    // The fixture's shell is on a direct connection, which the cut does not touch.
    let shell = w.shell(w.bastion).await;
    let dir = fixture(&shell, "drop", "printf keep > target.bin").await;
    let local = tempfile::tempdir().unwrap();
    let path = local.path().join("big.bin");
    std::fs::write(&path, vec![0x5a_u8; 100_000_000]).unwrap();

    let sftp = Arc::new(w.sftp(proxied).await);
    let (tx, mut rx) = unbounded_channel();
    let handle = upload(
        sftp.clone(),
        path,
        dir.clone(),
        "target.bin".into(),
        UploadMode::Replace,
        tx,
    );
    // Cut mid-write, once data has reached the server.
    until_moving(&mut rx, handle.id()).await;
    proxy.cut();
    let started = Instant::now();
    let rest = events(&mut rx, handle.id()).await;
    let took = started.elapsed();
    let kept = shell_line(&shell, "KEPT", &format!("cat '{dir}/target.bin'")).await;
    remove(&shell, &dir).await;
    shell.close();

    assert_eq!(
        last(&rest),
        &TransferEvent::Failed("The SFTP session has ended.".into())
    );
    assert!(took < Duration::from_secs(20), "the failure took {took:?}");
    assert_eq!(kept, "keep");
}

#[tokio::test]
async fn progress_is_coalesced() {
    if !enabled() {
        return;
    }
    let w = World::new();
    let shell = w.shell(w.target2).await;
    let dir = fixture(&shell, "prog", "head -c 100000000 /dev/zero > big").await;

    let sftp = Arc::new(w.sftp(w.target2).await);
    let local = tempfile::tempdir().unwrap();
    let (tx, mut rx) = unbounded_channel();
    let started = Instant::now();
    let handle = download(
        sftp,
        format!("{dir}/big"),
        local.path().join("big"),
        DownloadMode::FreeName,
        tx,
    );
    let got = timed_events(&mut rx, handle.id()).await;
    let took = started.elapsed();
    remove(&shell, &dir).await;
    shell.close();

    let (end_at, end) = got.last().expect("events");
    assert!(matches!(end, TransferEvent::Finished { .. }), "{end:?}");
    let progress: Vec<(Instant, u64)> = got
        .iter()
        .filter_map(|(at, e)| match e {
            TransferEvent::Progress { done, .. } => Some((*at, *done)),
            _ => None,
        })
        .collect();
    // The last progress event carries the whole file; it may follow the one before it
    // closely (the final event), every other pair is at least the interval apart (less a
    // little for delivery jitter).
    assert_eq!(progress.last().map(|p| p.1), Some(100_000_000));
    // 100 MB through two hops takes long enough for several events (else this proves nothing).
    assert!(progress.len() >= 3, "{progress:?}");
    for pair in progress[..progress.len() - 1].windows(2) {
        let gap = pair[1].0 - pair[0].0;
        assert!(gap >= Duration::from_millis(200), "{gap:?} in {progress:?}");
    }
    // At most four a second, plus the first and the final one.
    let allowed = (took.as_millis() / 250) as usize + 2;
    assert!(progress.len() <= allowed, "{} > {allowed}", progress.len());
    assert!(*end_at >= progress.last().unwrap().0);
    // Byte counts only grow.
    assert!(progress.windows(2).all(|p| p[0].1 <= p[1].1));
}

/// A TCP proxy to the bastion's sshd that a test can cut: every live connection through it
/// drops, as when the network goes away.
struct Proxy {
    port: u16,
    accept: tokio::task::JoinHandle<()>,
    pipes: Arc<parking_lot::Mutex<Vec<tokio::task::AbortHandle>>>,
}

impl Proxy {
    async fn new() -> Self {
        let listener = TcpListener::bind((HOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let pipes: Arc<parking_lot::Mutex<Vec<tokio::task::AbortHandle>>> = Arc::default();
        let tracked = pipes.clone();
        let accept = tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let task = tokio::spawn(async move {
                    if let Ok(mut server) = TcpStream::connect((HOST, PORT)).await {
                        let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                    }
                });
                tracked.lock().push(task.abort_handle());
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
            pipe.abort();
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.accept.abort();
        self.cut();
    }
}
