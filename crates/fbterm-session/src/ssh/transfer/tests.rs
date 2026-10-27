//! Unit tests for transfers, against an in-memory SFTP server.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time::Instant;

use super::*;

/// Bound for waiting on one event, so a broken transfer fails the test instead of hanging it.
const STEP: Duration = Duration::from_secs(10);

type Events = UnboundedReceiver<(TransferId, TransferEvent)>;

/// The next event of transfer `id`.
async fn next(rx: &mut Events, id: TransferId) -> Option<TransferEvent> {
    let got = tokio::time::timeout(STEP, rx.recv())
        .await
        .expect("no transfer event in time");
    got.map(|(from, event)| {
        assert_eq!(from, id);
        event
    })
}

/// Every event of transfer `id` up to its end (the channel closes when the task ends).
async fn all(rx: &mut Events, id: TransferId) -> Vec<TransferEvent> {
    let mut events = Vec::new();
    while let Some(event) = next(rx, id).await {
        events.push(event);
    }
    events
}

fn last(events: &[TransferEvent]) -> &TransferEvent {
    events.last().expect("no events")
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 % 251) as u8).collect()
}

#[test]
fn free_name_counts_up() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    assert_eq!(free_name(d, "report.txt"), d.join("report.txt"));
    std::fs::write(d.join("report.txt"), "x").unwrap();
    assert_eq!(free_name(d, "report.txt"), d.join("report (1).txt"));
    std::fs::write(d.join("report (1).txt"), "x").unwrap();
    assert_eq!(free_name(d, "report.txt"), d.join("report (2).txt"));
    // No extension, and a dot file (all name, no extension).
    std::fs::write(d.join("notes"), "x").unwrap();
    assert_eq!(free_name(d, "notes"), d.join("notes (1)"));
    std::fs::write(d.join(".profile"), "x").unwrap();
    assert_eq!(free_name(d, ".profile"), d.join(".profile (1)"));
    // A folder takes the name too.
    std::fs::create_dir(d.join("logs")).unwrap();
    assert_eq!(free_name(d, "logs"), d.join("logs (1)"));
    // The name never leaves the folder.
    let escaped = free_name(d, "../evil");
    assert_eq!(escaped.parent(), Some(d));
    assert_eq!(free_name(d, ".."), d.join("download"));
}

#[test]
fn free_name_skips_reserved_names() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // A name another download is about to write counts as taken, though nothing is there yet.
    let reserved = [d.join("report.txt"), d.join("report (1).txt")];
    let taken = |p: &Path| reserved.iter().any(|r| r == p);
    assert_eq!(
        free_name_avoiding(d, "report.txt", taken),
        d.join("report (2).txt")
    );
    std::fs::write(d.join("report (2).txt"), "x").unwrap();
    assert_eq!(
        free_name_avoiding(d, "report.txt", taken),
        d.join("report (3).txt")
    );
    assert_eq!(
        free_name_avoiding(d, "other.txt", taken),
        d.join("other.txt")
    );
}

#[test]
fn destinations_clash_ignoring_case() {
    let d = Path::new("/home/user/Downloads");
    assert!(same_destination(&d.join("App.log"), &d.join("app.log")));
    assert!(same_destination(&d.join("README"), &d.join("readme")));
    assert!(!same_destination(
        &d.join("app.log"),
        &d.join("app (1).log")
    ));
    assert!(!same_destination(
        &d.join("app.log"),
        &Path::new("/tmp").join("app.log")
    ));
    // Free names skip a reserved name in any case.
    let dir = tempfile::tempdir().unwrap();
    let reserved = [dir.path().join("README")];
    assert_eq!(
        free_name_avoiding(dir.path(), "readme", |p| reserved
            .iter()
            .any(|r| same_destination(r, p))),
        dir.path().join("readme (1)")
    );
}

#[test]
fn download_name_is_sanitized() {
    assert_eq!(safe_local_name("plain.txt"), "plain.txt");
    assert_eq!(safe_local_name(".hidden"), ".hidden");
    assert_eq!(safe_local_name("a/b"), "a_b");
    assert_eq!(safe_local_name("a\\b"), "a_b");
    assert_eq!(safe_local_name("../../etc/passwd"), ".._.._etc_passwd");
    assert_eq!(safe_local_name("\u{1b}[31mred\n"), "_[31mred_");
    assert_eq!(safe_local_name("nul\0byte"), "nul_byte");
    // Characters that reorder or hide what is shown never reach a local name: Finder would
    // show this one as `invoiceexe.pdf`.
    assert_eq!(safe_local_name("invoice\u{202E}fdp.exe"), "invoice_fdp.exe");
    assert_eq!(
        safe_local_name("a\u{2066}b\u{2028}c\u{200B}d\u{2060}e\u{FEFF}f\u{200E}g"),
        "a_b_c_d_e_f_g"
    );
    // Joiners stay: names in many scripts need them.
    assert_eq!(safe_local_name("a\u{200D}b"), "a\u{200D}b");
    for bad in ["", ".", ".."] {
        assert_eq!(safe_local_name(bad), "download", "{bad:?}");
    }
    // Unicode stays as it is.
    assert_eq!(
        safe_local_name("caf\u{e9} \u{65e5}.md"),
        "caf\u{e9} \u{65e5}.md"
    );
    #[cfg(windows)]
    {
        assert_eq!(safe_local_name("a:b?.txt"), "a_b_.txt");
        assert_eq!(safe_local_name("CON.txt"), "_CON.txt");
        assert_eq!(safe_local_name("trailing. "), "trailing");
    }
}

#[test]
fn progress_is_throttled() {
    let (tx, mut rx) = unbounded_channel();
    let id = TransferId(7);
    let t0 = Instant::now();
    let ms = |n: u64| t0 + Duration::from_millis(n);
    let mut p = Progress::new(id, tx);
    p.start(Some(100), t0);
    // Updates inside the interval are held back.
    p.set(10, ms(50));
    p.set(20, ms(200));
    assert_eq!(p.due(), Some(ms(250)));
    p.set(30, ms(260));
    p.set(40, ms(300));
    // The held update goes out when it is due; then nothing is.
    p.flush(ms(520));
    assert_eq!(p.due(), None);
    p.flush(ms(900));
    let got: Vec<TransferEvent> =
        std::iter::from_fn(|| rx.try_recv().ok().map(|(_, e)| e)).collect();
    let progress = |done| TransferEvent::Progress {
        done,
        total: Some(100),
    };
    assert_eq!(got, [progress(0), progress(30), progress(40)]);
}

#[tokio::test]
async fn download_reads_to_the_end() {
    // The server answers short reads and reports a size that is out of date: the download
    // still gets every byte, and the stat size is only shown.
    let content = pattern(300_000);
    let server = fake::Server::new(true);
    server.put("/r/data.bin", &content, 0o644);
    server.set_short_reads(7_000);
    server.set_stat_size(10);
    let sftp = Arc::new(fake::session(server.clone()).await);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("data.bin");
    let (tx, mut rx) = unbounded_channel();
    let handle = download(
        sftp,
        "/r/data.bin".into(),
        dest.clone(),
        DownloadMode::Replace,
        tx,
    );
    let events = all(&mut rx, handle.id()).await;
    assert_eq!(
        last(&events),
        &TransferEvent::Finished {
            note: None,
            local: Some(dest.clone()),
        }
    );
    assert_eq!(
        events.first(),
        Some(&TransferEvent::Progress {
            done: 0,
            total: Some(10),
        })
    );
    assert_eq!(std::fs::read(&dest).unwrap(), content);
    assert_eq!(files_in(dir.path()), ["data.bin"]);
    assert_eq!(server.open_handles(), 0);
}

#[tokio::test]
async fn download_refuses_folders_and_special_files() {
    let server = fake::Server::new(true);
    server.put_dir("/r/sub");
    server.put_special("/r/pipe");
    let sftp = Arc::new(fake::session(server).await);
    let dir = tempfile::tempdir().unwrap();
    for (remote, says) in [("/r/sub", "folder"), ("/r/pipe", "not a regular file")] {
        let (tx, mut rx) = unbounded_channel();
        let handle = download(
            sftp.clone(),
            remote.into(),
            dir.path().join("x"),
            DownloadMode::Replace,
            tx,
        );
        let events = all(&mut rx, handle.id()).await;
        match last(&events) {
            TransferEvent::Failed(m) => assert!(m.contains(says), "{m}"),
            other => panic!("{other:?}"),
        }
    }
    assert!(files_in(dir.path()).is_empty());
}

#[tokio::test]
async fn cancel_is_idempotent_and_removes_the_part_file() {
    let server = fake::Server::new(true);
    server.put("/r/big", &pattern(2_000_000), 0o644);
    server.set_read_delay(Duration::from_millis(30));
    let sftp = Arc::new(fake::session(server.clone()).await);
    let dir = tempfile::tempdir().unwrap();
    let (tx, mut rx) = unbounded_channel();
    let handle = download(
        sftp,
        "/r/big".into(),
        dir.path().join("big"),
        DownloadMode::FreeName,
        tx,
    );
    assert!(matches!(
        next(&mut rx, handle.id()).await,
        Some(TransferEvent::Progress { .. })
    ));
    handle.cancel();
    handle.cancel();
    let events = all(&mut rx, handle.id()).await;
    assert_eq!(last(&events), &TransferEvent::Cancelled);
    assert_eq!(
        events
            .iter()
            .filter(|e| **e == TransferEvent::Cancelled)
            .count(),
        1
    );
    assert!(
        files_in(dir.path()).is_empty(),
        "{:?}",
        files_in(dir.path())
    );
    handle.cancel();
    assert_eq!(server.open_handles(), 0);
}

#[tokio::test]
async fn cancel_after_finish_does_nothing_and_drop_does_not_cancel() {
    let server = fake::Server::new(true);
    server.put("/r/small", b"hello", 0o644);
    server.set_read_delay(Duration::from_millis(30));
    let sftp = Arc::new(fake::session(server).await);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("small");

    // Dropped at once: the download still completes.
    let (tx, mut rx) = unbounded_channel();
    let id = download(
        sftp.clone(),
        "/r/small".into(),
        dest.clone(),
        DownloadMode::FreeName,
        tx,
    )
    .id();
    let events = all(&mut rx, id).await;
    assert!(matches!(last(&events), TransferEvent::Finished { .. }));

    let (tx, mut rx) = unbounded_channel();
    let handle = download(
        sftp,
        "/r/small".into(),
        dir.path().join("again"),
        DownloadMode::Replace,
        tx,
    );
    let events = all(&mut rx, handle.id()).await;
    assert!(matches!(last(&events), TransferEvent::Finished { .. }));
    handle.cancel();
    assert_eq!(std::fs::read(dest).unwrap(), b"hello");
    assert_eq!(std::fs::read(dir.path().join("again")).unwrap(), b"hello");
}

async fn run_upload(
    server: &fake::Server,
    local: &Path,
    name: &str,
    mode: UploadMode,
) -> Vec<TransferEvent> {
    let sftp = Arc::new(fake::session(server.clone()).await);
    let (tx, mut rx) = unbounded_channel();
    let handle = upload(sftp, local.into(), "/up".into(), name.into(), mode, tx);
    all(&mut rx, handle.id()).await
}

fn local_file(dir: &Path, content: &[u8]) -> std::path::PathBuf {
    let path = dir.join("local.bin");
    std::fs::write(&path, content).unwrap();
    path
}

#[tokio::test]
async fn upload_new_creates_without_replacing() {
    let server = fake::Server::new(true);
    server.put_dir("/up");
    let dir = tempfile::tempdir().unwrap();
    let content = pattern(100_000);
    let local = local_file(dir.path(), &content);

    let events = run_upload(&server, &local, "fresh.bin", UploadMode::New).await;
    assert_eq!(
        last(&events),
        &TransferEvent::Finished {
            note: None,
            local: None,
        }
    );
    assert_eq!(server.get("/up/fresh.bin"), Some(content.clone()));
    // The server chose the permissions; nothing was set.
    assert_eq!(server.mode("/up/fresh.bin"), Some(fake::DEFAULT_MODE));
    assert_eq!(server.names("/up"), ["fresh.bin"]);

    // A clash is never replaced by a New upload.
    server.put("/up/taken.txt", b"keep", 0o600);
    let events = run_upload(&server, &local, "taken.txt", UploadMode::New).await;
    assert_eq!(
        last(&events),
        &TransferEvent::Failed("\"taken.txt\" already exists in /up.".into())
    );
    assert_eq!(server.get("/up/taken.txt"), Some(b"keep".to_vec()));
    assert_eq!(server.names("/up"), ["fresh.bin", "taken.txt"]);
}

#[tokio::test]
async fn upload_rejects_unsafe_names() {
    let server = fake::Server::new(true);
    server.put_dir("/up");
    let dir = tempfile::tempdir().unwrap();
    let local = local_file(dir.path(), b"data");
    for name in ["", ".", "..", "a/b", "../x", "line\nbreak", "nul\0"] {
        let events = run_upload(&server, &local, name, UploadMode::New).await;
        assert!(
            matches!(last(&events), TransferEvent::Failed(_)),
            "{name:?}: {events:?}"
        );
    }
    assert!(server.names("/up").is_empty());
    assert_eq!(server.all_paths(), ["/up"]);
}

#[tokio::test]
async fn upload_skips_folders() {
    let server = fake::Server::new(true);
    server.put_dir("/up");
    let dir = tempfile::tempdir().unwrap();
    let events = run_upload(&server, dir.path(), "photos", UploadMode::New).await;
    assert_eq!(
        last(&events),
        &TransferEvent::Failed("Folders are not uploaded: photos".into())
    );
}

#[tokio::test]
async fn replace_keeps_mode_through_a_part_file() {
    let server = fake::Server::new(true);
    server.put_dir("/up");
    server.put("/up/run.sh", b"old", 0o750);
    let dir = tempfile::tempdir().unwrap();
    let local = local_file(dir.path(), b"#!/bin/sh\necho new\n");
    let events = run_upload(&server, &local, "run.sh", UploadMode::Replace).await;
    assert_eq!(
        last(&events),
        &TransferEvent::Finished {
            note: None,
            local: None,
        }
    );
    assert!(!events.contains(&TransferEvent::InPlace), "{events:?}");
    assert_eq!(
        server.get("/up/run.sh"),
        Some(b"#!/bin/sh\necho new\n".to_vec())
    );
    assert_eq!(server.mode("/up/run.sh"), Some(0o750));
    assert_eq!(server.names("/up"), ["run.sh"]);
    assert!(server.posix_renames() >= 1);
}

#[tokio::test]
async fn replace_without_the_rename_extension_writes_in_place() {
    let server = fake::Server::new(false);
    server.put_dir("/up");
    server.put("/up/conf", b"a much longer old content", 0o640);
    let dir = tempfile::tempdir().unwrap();
    let local = local_file(dir.path(), b"new");
    let events = run_upload(&server, &local, "conf", UploadMode::Replace).await;
    assert_eq!(
        last(&events),
        &TransferEvent::Finished {
            note: Some(REPLACED_IN_PLACE.into()),
            local: None,
        }
    );
    // Said once, before the file is touched: a stop from then on may leave it incomplete.
    assert_eq!(
        events
            .iter()
            .filter(|e| **e == TransferEvent::InPlace)
            .count(),
        1,
        "{events:?}"
    );
    assert_eq!(server.get("/up/conf"), Some(b"new".to_vec()));
    assert_eq!(server.mode("/up/conf"), Some(0o640));
    assert_eq!(server.names("/up"), ["conf"]);
}

#[tokio::test]
async fn replace_in_a_read_only_folder_writes_in_place() {
    let server = fake::Server::new(true);
    server.put_dir("/up");
    server.put("/up/conf", b"old", 0o644);
    server.deny_create_in("/up");
    let dir = tempfile::tempdir().unwrap();
    let local = local_file(dir.path(), b"new content");
    let events = run_upload(&server, &local, "conf", UploadMode::Replace).await;
    assert_eq!(
        last(&events),
        &TransferEvent::Finished {
            note: Some(REPLACED_IN_PLACE.into()),
            local: None,
        }
    );
    assert_eq!(server.get("/up/conf"), Some(b"new content".to_vec()));
}

#[tokio::test]
async fn failed_in_place_replace_says_the_file_may_be_incomplete() {
    let server = fake::Server::new(false);
    server.put_dir("/up");
    server.put("/up/conf", b"old", 0o644);
    server.fail_writes();
    let dir = tempfile::tempdir().unwrap();
    let local = local_file(dir.path(), &pattern(50_000));
    let events = run_upload(&server, &local, "conf", UploadMode::Replace).await;
    match last(&events) {
        TransferEvent::Failed(m) => assert!(m.starts_with(MAY_BE_INCOMPLETE), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn failed_upload_removes_its_part_file() {
    let server = fake::Server::new(true);
    server.put_dir("/up");
    server.put("/up/conf", b"keep", 0o644);
    server.fail_writes();
    let dir = tempfile::tempdir().unwrap();
    let local = local_file(dir.path(), &pattern(50_000));
    let events = run_upload(&server, &local, "conf", UploadMode::Replace).await;
    assert!(
        matches!(last(&events), TransferEvent::Failed(_)),
        "{events:?}"
    );
    assert_eq!(server.get("/up/conf"), Some(b"keep".to_vec()));
    assert_eq!(server.names("/up"), ["conf"]);
    assert_eq!(server.open_handles(), 0);
}

/// Start an upload of `content` as `/up/{name}` and cancel it `after` its first event.
async fn cancelled_upload(
    server: &fake::Server,
    content: &[u8],
    name: &str,
    after: Duration,
) -> Vec<TransferEvent> {
    let dir = tempfile::tempdir().unwrap();
    let local = local_file(dir.path(), content);
    let sftp = Arc::new(fake::session(server.clone()).await);
    let (tx, mut rx) = unbounded_channel();
    let handle = upload(
        sftp,
        local,
        "/up".into(),
        name.into(),
        UploadMode::Replace,
        tx,
    );
    let first = next(&mut rx, handle.id()).await;
    assert!(
        matches!(first, Some(TransferEvent::Progress { done: 0, .. })),
        "{first:?}"
    );
    tokio::time::sleep(after).await;
    handle.cancel();
    all(&mut rx, handle.id()).await
}

#[tokio::test]
async fn cancel_during_in_place_open_says_the_file_may_be_incomplete() {
    // The open (which empties the target) is answered late; the cancel lands meanwhile.
    let server = fake::Server::new(false);
    server.put_dir("/up");
    server.put("/up/conf", b"old content", 0o644);
    server.set_open_delay(Duration::from_millis(300));
    let events = cancelled_upload(&server, b"new", "conf", Duration::from_millis(50)).await;
    assert_eq!(
        last(&events),
        &TransferEvent::Failed(format!("{MAY_BE_INCOMPLETE}."))
    );
    assert_eq!(server.open_handles(), 0);
    assert_eq!(server.names("/up"), ["conf"]);
}

#[tokio::test]
async fn cancel_during_part_create_removes_the_part_file() {
    let server = fake::Server::new(true);
    server.put_dir("/up");
    server.put("/up/conf", b"keep", 0o644);
    server.set_open_delay(Duration::from_millis(300));
    let events = cancelled_upload(&server, b"new", "conf", Duration::from_millis(50)).await;
    // Whatever the server still had in hand has landed by now.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(last(&events), &TransferEvent::Cancelled);
    assert_eq!(server.names("/up"), ["conf"]);
    assert_eq!(server.get("/up/conf"), Some(b"keep".to_vec()));
    assert_eq!(server.open_handles(), 0);
}

#[tokio::test]
async fn cancel_after_the_rename_reports_finished() {
    // The rename is done at once but answered late; the cancel lands while it is answered.
    let server = fake::Server::new(true);
    server.put_dir("/up");
    server.put("/up/conf", b"old", 0o640);
    server.set_rename_delay(Duration::from_millis(400));
    let events = cancelled_upload(&server, b"new", "conf", Duration::from_millis(150)).await;
    assert_eq!(
        last(&events),
        &TransferEvent::Finished {
            note: None,
            local: None,
        }
    );
    assert_eq!(server.get("/up/conf"), Some(b"new".to_vec()));
    assert_eq!(server.names("/up"), ["conf"]);
}

#[tokio::test]
async fn refused_rename_falls_back_to_in_place() {
    let server = fake::Server::new(true);
    server.put_dir("/up");
    server.put("/up/conf", b"old", 0o640);
    server.deny_posix_rename();
    let dir = tempfile::tempdir().unwrap();
    let content = pattern(80_000);
    let local = local_file(dir.path(), &content);
    let events = run_upload(&server, &local, "conf", UploadMode::Replace).await;
    assert_eq!(
        last(&events),
        &TransferEvent::Finished {
            note: Some(REPLACED_IN_PLACE.into()),
            local: None,
        }
    );
    // The row restarts from zero for the second pass.
    let restarts = events
        .iter()
        .filter(|e| matches!(e, TransferEvent::Progress { done: 0, .. }))
        .count();
    assert_eq!(restarts, 2, "{events:?}");
    assert_eq!(server.get("/up/conf"), Some(content));
    assert_eq!(server.mode("/up/conf"), Some(0o640));
    assert_eq!(server.names("/up"), ["conf"]);
    assert_eq!(server.open_handles(), 0);
}

#[tokio::test]
async fn download_into_a_free_name_never_replaces_a_file_that_appeared() {
    let content = pattern(400_000);
    let server = fake::Server::new(true);
    server.put("/r/report.pdf", &content, 0o644);
    server.set_read_delay(Duration::from_millis(20));
    let sftp = Arc::new(fake::session(server).await);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("report.pdf");
    let (tx, mut rx) = unbounded_channel();
    let handle = download(
        sftp,
        "/r/report.pdf".into(),
        dest.clone(),
        DownloadMode::FreeName,
        tx,
    );
    assert!(matches!(
        next(&mut rx, handle.id()).await,
        Some(TransferEvent::Progress { .. })
    ));
    // Something else saves under the same name while the download runs.
    std::fs::write(&dest, "theirs").unwrap();
    let events = all(&mut rx, handle.id()).await;
    let saved = dir.path().join("report (1).pdf");
    assert_eq!(
        last(&events),
        &TransferEvent::Finished {
            note: None,
            local: Some(saved.clone()),
        }
    );
    assert_eq!(std::fs::read(&dest).unwrap(), b"theirs");
    assert_eq!(std::fs::read(&saved).unwrap(), content);
    assert_eq!(files_in(dir.path()), ["report (1).pdf", "report.pdf"]);
}

#[test]
fn saving_without_replacing_takes_the_next_free_name() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    for (name, data) in [
        ("notes.txt", "one"),
        ("notes (1).txt", "two"),
        (".notes.txt.fbterm-part", "mine"),
    ] {
        std::fs::write(d.join(name), data).unwrap();
    }
    let saved =
        save_without_replacing(&d.join(".notes.txt.fbterm-part"), &d.join("notes.txt")).unwrap();
    assert_eq!(saved, d.join("notes (2).txt"));
    assert_eq!(std::fs::read(&saved).unwrap(), b"mine");
    assert_eq!(std::fs::read(d.join("notes.txt")).unwrap(), b"one");
    assert_eq!(
        files_in(d),
        ["notes (1).txt", "notes (2).txt", "notes.txt"],
        "the part file is gone"
    );
}

#[tokio::test]
async fn download_as_replaces_the_chosen_file() {
    let server = fake::Server::new(true);
    server.put("/r/report.pdf", b"new", 0o644);
    let sftp = Arc::new(fake::session(server).await);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("chosen.pdf");
    std::fs::write(&dest, "old").unwrap();
    let (tx, mut rx) = unbounded_channel();
    let handle = download(
        sftp,
        "/r/report.pdf".into(),
        dest.clone(),
        DownloadMode::Replace,
        tx,
    );
    let events = all(&mut rx, handle.id()).await;
    assert_eq!(
        last(&events),
        &TransferEvent::Finished {
            note: None,
            local: Some(dest.clone()),
        }
    );
    assert_eq!(std::fs::read(&dest).unwrap(), b"new");
    assert_eq!(files_in(dir.path()), ["chosen.pdf"]);
}

#[tokio::test]
async fn overlong_reads_are_cut_to_what_was_asked() {
    let content = pattern(200_000);
    let server = fake::Server::new(true);
    server.put("/r/data.bin", &content, 0o644);
    server.set_extra_read_bytes(100);
    let sftp = Arc::new(fake::session(server).await);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("data.bin");
    let (tx, mut rx) = unbounded_channel();
    let handle = download(
        sftp,
        "/r/data.bin".into(),
        dest.clone(),
        DownloadMode::FreeName,
        tx,
    );
    let events = all(&mut rx, handle.id()).await;
    assert!(
        matches!(last(&events), TransferEvent::Finished { .. }),
        "{events:?}"
    );
    assert_eq!(std::fs::read(&dest).unwrap(), content);
}

#[tokio::test]
async fn new_upload_losing_the_rename_race_gets_the_clash_copy() {
    let server = fake::Server::new(true);
    server.put_dir("/up");
    server.clash_on_rename();
    let dir = tempfile::tempdir().unwrap();
    let local = local_file(dir.path(), b"mine");
    let events = run_upload(&server, &local, "late.txt", UploadMode::New).await;
    assert_eq!(
        last(&events),
        &TransferEvent::Failed("\"late.txt\" already exists in /up.".into())
    );
    assert_eq!(server.get("/up/late.txt"), Some(b"theirs".to_vec()));
    assert_eq!(server.names("/up"), ["late.txt"]);
}

/// An in-memory SFTP server with just enough of a file system for transfers.
mod fake {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Arc;
    use std::time::Duration;

    use parking_lot::Mutex;
    use russh_sftp::protocol::{
        Attrs, Data, FileAttributes, Handle, OpenFlags, Packet, Status, StatusCode, Version,
    };
    use russh_sftp::server::{Handler, StatusReply};

    use super::super::super::sftp::Sftp;

    /// What a created file gets when the client sets nothing (umask 022).
    pub const DEFAULT_MODE: u32 = 0o644;

    enum Node {
        File { data: Vec<u8>, mode: u32 },
        Dir,
        Special,
    }

    #[derive(Default)]
    struct Fs {
        nodes: BTreeMap<String, Node>,
        handles: HashMap<String, String>,
        next_handle: u32,
        short_reads: Option<usize>,
        stat_size: Option<u64>,
        read_delay: Option<Duration>,
        deny_create_in: Option<String>,
        fail_writes: bool,
        posix_renames: usize,
        open_delay: Option<Duration>,
        rename_delay: Option<Duration>,
        deny_posix_rename: bool,
        extra_read_bytes: usize,
        clash_on_rename: bool,
    }

    #[derive(Clone)]
    pub struct Server {
        fs: Arc<Mutex<Fs>>,
        posix_rename: bool,
    }

    fn ok(id: u32) -> Status {
        Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".into(),
            language_tag: "en-US".into(),
        }
    }

    fn parent(path: &str) -> &str {
        path.rsplit_once('/').map_or("", |(p, _)| p)
    }

    impl Server {
        pub fn new(posix_rename: bool) -> Self {
            Self {
                fs: Arc::default(),
                posix_rename,
            }
        }

        pub fn put(&self, path: &str, data: &[u8], mode: u32) {
            self.fs.lock().nodes.insert(
                path.into(),
                Node::File {
                    data: data.to_vec(),
                    mode,
                },
            );
        }

        pub fn put_dir(&self, path: &str) {
            self.fs.lock().nodes.insert(path.into(), Node::Dir);
        }

        pub fn put_special(&self, path: &str) {
            self.fs.lock().nodes.insert(path.into(), Node::Special);
        }

        pub fn get(&self, path: &str) -> Option<Vec<u8>> {
            match self.fs.lock().nodes.get(path) {
                Some(Node::File { data, .. }) => Some(data.clone()),
                _ => None,
            }
        }

        pub fn mode(&self, path: &str) -> Option<u32> {
            match self.fs.lock().nodes.get(path) {
                Some(Node::File { mode, .. }) => Some(*mode),
                _ => None,
            }
        }

        /// The names directly inside `dir`.
        pub fn names(&self, dir: &str) -> Vec<String> {
            self.fs
                .lock()
                .nodes
                .keys()
                .filter(|p| parent(p) == dir)
                .map(|p| p[dir.len() + 1..].to_owned())
                .collect()
        }

        pub fn all_paths(&self) -> Vec<String> {
            self.fs.lock().nodes.keys().cloned().collect()
        }

        pub fn open_handles(&self) -> usize {
            self.fs.lock().handles.len()
        }

        pub fn posix_renames(&self) -> usize {
            self.fs.lock().posix_renames
        }

        pub fn set_short_reads(&self, at_most: usize) {
            self.fs.lock().short_reads = Some(at_most);
        }

        pub fn set_stat_size(&self, size: u64) {
            self.fs.lock().stat_size = Some(size);
        }

        pub fn set_read_delay(&self, delay: Duration) {
            self.fs.lock().read_delay = Some(delay);
        }

        pub fn deny_create_in(&self, dir: &str) {
            self.fs.lock().deny_create_in = Some(dir.into());
        }

        pub fn fail_writes(&self) {
            self.fs.lock().fail_writes = true;
        }

        /// Every open (and create) answers only after `delay`.
        pub fn set_open_delay(&self, delay: Duration) {
            self.fs.lock().open_delay = Some(delay);
        }

        /// Every rename answers only after `delay` (done at once, answered late).
        pub fn set_rename_delay(&self, delay: Duration) {
            self.fs.lock().rename_delay = Some(delay);
        }

        /// `posix-rename@openssh.com` is offered but refused with permission denied.
        pub fn deny_posix_rename(&self) {
            self.fs.lock().deny_posix_rename = true;
        }

        /// Reads answer `extra` bytes more than asked for (a broken server).
        pub fn set_extra_read_bytes(&self, extra: usize) {
            self.fs.lock().extra_read_bytes = extra;
        }

        /// Another writer creates the target just before a plain rename, which then fails.
        pub fn clash_on_rename(&self) {
            self.fs.lock().clash_on_rename = true;
        }

        fn attrs(&self, path: &str) -> Result<FileAttributes, StatusReply> {
            let fs = self.fs.lock();
            let (mode, size) = match fs.nodes.get(path) {
                Some(Node::File { data, mode }) => {
                    (0o100000 | mode, fs.stat_size.unwrap_or(data.len() as u64))
                }
                Some(Node::Dir) => (0o040755, 4096),
                Some(Node::Special) => (0o010644, 0),
                None => return Err(StatusCode::NoSuchFile.into()),
            };
            Ok(FileAttributes {
                size: Some(size),
                uid: Some(1000),
                gid: Some(1000),
                permissions: Some(mode),
                ..FileAttributes::default()
            })
        }

        fn path_of(&self, handle: &str) -> Result<String, StatusReply> {
            self.fs
                .lock()
                .handles
                .get(handle)
                .cloned()
                .ok_or_else(|| StatusCode::Failure.into())
        }
    }

    fn read_string(data: &mut &[u8]) -> Option<String> {
        let (len, rest) = data.split_first_chunk::<4>()?;
        let len = u32::from_be_bytes(*len) as usize;
        let (s, rest) = (rest.get(..len)?, rest.get(len..)?);
        *data = rest;
        String::from_utf8(s.to_vec()).ok()
    }

    impl Handler for Server {
        type Error = StatusReply;

        fn unimplemented(&self) -> Self::Error {
            StatusCode::OpUnsupported.into()
        }

        async fn init(
            &mut self,
            _version: u32,
            _extensions: HashMap<String, String>,
        ) -> Result<Version, Self::Error> {
            let mut version = Version::new();
            if self.posix_rename {
                version
                    .extensions
                    .insert("posix-rename@openssh.com".into(), "1".into());
            }
            Ok(version)
        }

        async fn open(
            &mut self,
            id: u32,
            filename: String,
            pflags: OpenFlags,
            attrs: FileAttributes,
        ) -> Result<Handle, Self::Error> {
            let delay = self.fs.lock().open_delay;
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            let mut fs = self.fs.lock();
            let exists = fs.nodes.contains_key(&filename);
            if pflags.contains(OpenFlags::CREATE) && !exists {
                if fs.deny_create_in.as_deref() == Some(parent(&filename)) {
                    return Err(StatusCode::PermissionDenied.into());
                }
                let mode = attrs.permissions.map_or(DEFAULT_MODE, |m| m & 0o7777);
                fs.nodes.insert(
                    filename.clone(),
                    Node::File {
                        data: Vec::new(),
                        mode,
                    },
                );
            } else if pflags.contains(OpenFlags::CREATE | OpenFlags::EXCLUDE) {
                return Err(StatusCode::Failure.into());
            }
            match fs.nodes.get_mut(&filename) {
                Some(Node::File { data, .. }) => {
                    if pflags.contains(OpenFlags::TRUNCATE) {
                        data.clear();
                    }
                }
                Some(_) => return Err(StatusCode::Failure.into()),
                None => return Err(StatusCode::NoSuchFile.into()),
            }
            fs.next_handle += 1;
            let handle = format!("h{}", fs.next_handle);
            fs.handles.insert(handle.clone(), filename);
            Ok(Handle { id, handle })
        }

        async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
            match self.fs.lock().handles.remove(&handle) {
                Some(_) => Ok(ok(id)),
                None => Err(StatusCode::Failure.into()),
            }
        }

        async fn read(
            &mut self,
            id: u32,
            handle: String,
            offset: u64,
            len: u32,
        ) -> Result<Data, Self::Error> {
            let delay = self.fs.lock().read_delay;
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            let path = self.path_of(&handle)?;
            let fs = self.fs.lock();
            let Some(Node::File { data, .. }) = fs.nodes.get(&path) else {
                return Err(StatusCode::Failure.into());
            };
            let start = usize::try_from(offset).unwrap_or(usize::MAX);
            if start >= data.len() {
                return Err(StatusCode::Eof.into());
            }
            let mut end = data.len().min(start + len as usize + fs.extra_read_bytes);
            if let Some(cap) = fs.short_reads {
                end = end.min(start + cap);
            }
            Ok(Data {
                id,
                data: data[start..end].to_vec(),
            })
        }

        async fn write(
            &mut self,
            id: u32,
            handle: String,
            offset: u64,
            bytes: Vec<u8>,
        ) -> Result<Status, Self::Error> {
            let path = self.path_of(&handle)?;
            let mut fs = self.fs.lock();
            if fs.fail_writes {
                return Err(StatusCode::Failure.into());
            }
            let Some(Node::File { data, .. }) = fs.nodes.get_mut(&path) else {
                return Err(StatusCode::Failure.into());
            };
            let start = offset as usize;
            if data.len() < start + bytes.len() {
                data.resize(start + bytes.len(), 0);
            }
            data[start..start + bytes.len()].copy_from_slice(&bytes);
            Ok(ok(id))
        }

        async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
            Ok(Attrs {
                id,
                attrs: self.attrs(&path)?,
            })
        }

        async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
            Ok(Attrs {
                id,
                attrs: self.attrs(&path)?,
            })
        }

        async fn fsetstat(
            &mut self,
            id: u32,
            handle: String,
            attrs: FileAttributes,
        ) -> Result<Status, Self::Error> {
            let path = self.path_of(&handle)?;
            if let Some(Node::File { mode, .. }) = self.fs.lock().nodes.get_mut(&path)
                && let Some(p) = attrs.permissions
            {
                *mode = p & 0o7777;
            }
            Ok(ok(id))
        }

        async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
            match self.fs.lock().nodes.remove(&filename) {
                Some(_) => Ok(ok(id)),
                None => Err(StatusCode::NoSuchFile.into()),
            }
        }

        async fn rename(
            &mut self,
            id: u32,
            oldpath: String,
            newpath: String,
        ) -> Result<Status, Self::Error> {
            let delay = {
                let mut fs = self.fs.lock();
                if fs.clash_on_rename {
                    fs.nodes.insert(
                        newpath.clone(),
                        Node::File {
                            data: b"theirs".to_vec(),
                            mode: DEFAULT_MODE,
                        },
                    );
                }
                if fs.nodes.contains_key(&newpath) {
                    return Err(StatusCode::Failure.into());
                }
                let node = fs
                    .nodes
                    .remove(&oldpath)
                    .ok_or(StatusReply::from(StatusCode::NoSuchFile))?;
                fs.nodes.insert(newpath, node);
                fs.rename_delay
            };
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            Ok(ok(id))
        }

        async fn extended(
            &mut self,
            id: u32,
            request: String,
            data: Vec<u8>,
        ) -> Result<Packet, Self::Error> {
            if request != "posix-rename@openssh.com" || !self.posix_rename {
                return Err(StatusCode::OpUnsupported.into());
            }
            let mut rest = data.as_slice();
            let (Some(from), Some(to)) = (read_string(&mut rest), read_string(&mut rest)) else {
                return Err(StatusCode::BadMessage.into());
            };
            let delay = {
                let mut fs = self.fs.lock();
                if fs.deny_posix_rename {
                    return Err(StatusCode::PermissionDenied.into());
                }
                let node = fs
                    .nodes
                    .remove(&from)
                    .ok_or(StatusReply::from(StatusCode::NoSuchFile))?;
                fs.nodes.insert(to, node);
                fs.posix_renames += 1;
                fs.rename_delay
            };
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            Ok(Packet::Status(ok(id)))
        }
    }

    /// A session talking to `server` over an in-memory pipe.
    pub async fn session(server: Server) -> Sftp {
        let (client, remote) = tokio::io::duplex(1024 * 1024);
        russh_sftp::server::run(remote, server).await;
        Sftp::start(client, None).await.expect("fake sftp session")
    }
}
