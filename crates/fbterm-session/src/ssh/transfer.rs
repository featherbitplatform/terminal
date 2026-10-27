//! Downloads and uploads over an [`Sftp`] session, for the file panel's transfer queue.
//!
//! Each transfer is a task that reports to a [`TransferSink`]: a first
//! [`TransferEvent::Progress`] with the total, further progress at most every
//! [`PROGRESS_INTERVAL`] (only when bytes moved; there is no timer otherwise), then exactly one
//! final event, preceded by a last progress event when the count changed since the previous
//! one. The channel then has nothing more for that transfer.
//!
//! Nothing is ever left half-written under a name the user knows:
//! - A download writes `.<name>.fbterm-part` next to its destination and renames it over the
//!   destination when complete; cancelling or failing removes the part file. It reads until
//!   the end of the remote file, so a file that changes size meanwhile is not an error (the
//!   size from `stat` is only shown as the total).
//! - An upload writes `.<name>.fbterm-part` in the target folder and renames it into place
//!   when complete. [`UploadMode::New`] never replaces anything (the rename refuses an
//!   existing name). [`UploadMode::Replace`] first gives the part file the existing file's
//!   owner and group (when the server allows it) and mode, then renames it over the target
//!   with `posix-rename@openssh.com`. Without that extension, when the folder cannot take
//!   the part file, or when the server refuses that rename as not permitted (the part file is
//!   then removed and the upload starts over from zero), it writes the existing file in place
//!   and finishes with the note [`REPLACED_IN_PLACE`]; a failed or cancelled in-place write
//!   fails with a reason starting with [`MAY_BE_INCOMPLETE`]. Replacing a symbolic link always
//!   writes the file it ends at in place, so the link stays a link. A new file gets the
//!   server's default permissions.
//! - A file already called `.<name>.fbterm-part` in the target folder is taken for a leftover
//!   of an earlier upload and removed.
//!
//! Cancelling never abandons a request that changes the server (creating or emptying a file,
//! renaming) or hands out a handle: such a request is awaited to its answer (bounded), and the
//! cancel is acted on after it, cleaning up what it did. A cancel that arrives once the final
//! rename has been sent is too late: the transfer reports [`TransferEvent::Finished`].
//!
//! Names never leave their folder: upload names with `/`, control characters, `.` or `..` are
//! refused, and local names come from [`safe_local_name`].

use std::future::Future;
use std::io::{ErrorKind, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::{FuturesOrdered, FuturesUnordered};
use russh_sftp::protocol::FileAttributes;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::sftp::{CLOSE_WAIT, EntryKind, Sftp, SftpError, join, kind_of_mode};
use super::{Cancel, HOP_TIMEOUT};

/// The note of a finished upload that wrote over the existing file instead of replacing it.
pub const REPLACED_IN_PLACE: &str = "Replaced in place";
/// Starts the reason of a failed or cancelled in-place replace.
pub const MAY_BE_INCOMPLETE: &str = "File may be incomplete";
/// The shortest time between two progress events of one transfer.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// What a transfer has outstanding at most: requests in flight times their data length. Kept
/// small enough that the last request of a full window still answers within the session's
/// request timeout on a slow link.
const IN_FLIGHT_BYTES: u64 = 512 * 1024;
/// Most requests one transfer keeps in flight.
const MAX_REQUESTS: u64 = 16;
/// The name a part file adds after the dotted name of the file it becomes.
const PART_SUFFIX: &str = ".fbterm-part";
/// How long a request that must not be abandoned may take (it has the session's own request
/// timeout too; this only bounds a server that never answers at all).
const SETTLE_WAIT: Duration = Duration::from_secs(HOP_TIMEOUT.as_secs() + 5);
/// The name used for a download whose remote name leaves nothing usable.
const FALLBACK_NAME: &str = "download";

/// Identifies one transfer for the lifetime of the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TransferId(pub u64);

impl TransferId {
    /// Allocate a fresh, process-unique id.
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// What a transfer reports, in order: progress, then one of the other three, once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferEvent {
    /// Bytes moved so far, of `total` (the size when the transfer started, for display).
    Progress { done: u64, total: Option<u64> },
    /// Done. `note` is [`REPLACED_IN_PLACE`] for an in-place replace; `local` is where a
    /// download was saved.
    Finished {
        note: Option<String>,
        local: Option<PathBuf>,
    },
    /// Stopped with a reason fit to show in the queue.
    Failed(String),
    /// Stopped by [`TransferHandle::cancel`]; nothing was left behind.
    Cancelled,
}

/// Where transfers send their events, tagged with their id.
pub type TransferSink = mpsc::UnboundedSender<(TransferId, TransferEvent)>;

/// What an upload may do when its name exists in the target folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadMode {
    /// Never replace: an existing name fails the upload.
    New,
    /// Replace the existing file (the user chose to).
    Replace,
}

/// A running (or finished) transfer. Dropping it does not cancel the transfer.
#[derive(Clone)]
pub struct TransferHandle {
    id: TransferId,
    cancel: Arc<Cancel>,
}

impl TransferHandle {
    fn new() -> Self {
        Self {
            id: TransferId::next(),
            cancel: Arc::default(),
        }
    }

    pub fn id(&self) -> TransferId {
        self.id
    }

    /// Stop the transfer and clean up after it. Idempotent; does nothing once it finished.
    pub fn cancel(&self) {
        self.cancel.raise();
    }
}

/// Download the remote file `remote` (a link is followed) to the local path `dest`, replacing
/// `dest` when complete. Must be called within a tokio runtime.
pub fn download(
    sftp: Arc<Sftp>,
    remote: String,
    dest: PathBuf,
    events: TransferSink,
) -> TransferHandle {
    let handle = TransferHandle::new();
    let mut run = Run::new(handle.id, events, handle.cancel.clone());
    tokio::spawn(async move {
        let outcome = run_download(&mut run, &sftp, &remote, &dest).await;
        run.end(outcome);
    });
    handle
}

/// Upload the local file `local` as `name` into the remote `folder`. Must be called within a
/// tokio runtime.
pub fn upload(
    sftp: Arc<Sftp>,
    local: PathBuf,
    folder: String,
    name: String,
    mode: UploadMode,
    events: TransferSink,
) -> TransferHandle {
    let handle = TransferHandle::new();
    let mut run = Run::new(handle.id, events, handle.cancel.clone());
    tokio::spawn(async move {
        let outcome = run_upload(&mut run, &sftp, &local, &folder, &name, mode).await;
        run.end(outcome);
    });
    handle
}

/// The first of `name`, `name (1).ext`, `name (2).ext`, … that is free in `dir`. The name is
/// made safe first ([`safe_local_name`]), so the result is always directly inside `dir`.
pub fn free_name(dir: &Path, name: &str) -> PathBuf {
    let name = safe_local_name(name);
    let taken = |p: &Path| p.symlink_metadata().is_ok();
    let first = dir.join(&name);
    if !taken(&first) {
        return first;
    }
    let as_path = Path::new(&name);
    let (stem, ext) = match (as_path.file_stem(), as_path.extension()) {
        (Some(stem), Some(ext)) => (stem.to_string_lossy(), Some(ext.to_string_lossy())),
        _ => (name.as_str().into(), None),
    };
    (1u64..)
        .map(|n| match &ext {
            Some(ext) => dir.join(format!("{stem} ({n}).{ext}")),
            None => dir.join(format!("{stem} ({n})")),
        })
        .find(|p| !taken(p))
        .expect("some numbered name is free")
}

/// `remote` (a remote file name) as a local file name: path separators and control
/// characters become `_` (on Windows also the characters it forbids, trailing dots and
/// spaces go, and device names get a `_` in front); empty, `.` and `..` become `download`.
pub fn safe_local_name(remote: &str) -> String {
    let forbidden = |c: char| {
        c == '/' || c == '\\' || c.is_control() || (cfg!(windows) && "<>:\"|?*".contains(c))
    };
    let mut name: String = remote
        .chars()
        .map(|c| if forbidden(c) { '_' } else { c })
        .collect();
    if cfg!(windows) {
        name.truncate(name.trim_end_matches(['.', ' ']).len());
        let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
        let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && stem.as_bytes()[3].is_ascii_digit());
        if device {
            name.insert(0, '_');
        }
    }
    if name.is_empty() || name == "." || name == ".." {
        FALLBACK_NAME.to_owned()
    } else {
        name
    }
}

/// `name` for messages: control characters shown as `?`.
fn shown(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// The part file's name for a file called `name`.
fn part_name(name: &str) -> String {
    format!(".{name}{PART_SUFFIX}")
}

/// How many requests of `chunk` bytes one transfer keeps in flight.
fn window(chunk: u32) -> usize {
    let n = (IN_FLIGHT_BYTES / u64::from(chunk.max(1))).clamp(2, MAX_REQUESTS);
    usize::try_from(n).unwrap_or(2)
}

/// Why a transfer stopped early.
#[derive(Debug)]
enum Stop {
    Cancelled,
    Failed(String),
}

impl From<SftpError> for Stop {
    fn from(e: SftpError) -> Self {
        Stop::Failed(e.to_string())
    }
}

fn local_error(what: &str, path: &Path, e: &std::io::Error) -> Stop {
    Stop::Failed(format!("Could not {what} {}: {e}.", path.display()))
}

/// Progress reporting, coalesced to one event per [`PROGRESS_INTERVAL`].
struct Progress {
    id: TransferId,
    sink: TransferSink,
    total: Option<u64>,
    done: u64,
    /// The count of the last event sent, and when it went.
    sent: Option<(u64, Instant)>,
}

impl Progress {
    fn new(id: TransferId, sink: TransferSink) -> Self {
        Self {
            id,
            sink,
            total: None,
            done: 0,
            sent: None,
        }
    }

    fn send(&self, event: TransferEvent) {
        let _ = self.sink.send((self.id, event));
    }

    /// Report the start: nothing done of `total`.
    fn start(&mut self, total: Option<u64>, now: Instant) {
        self.total = total;
        self.done = 0;
        self.emit(now);
    }

    /// Start over from zero (the same total), reported at once.
    fn restart(&mut self, now: Instant) {
        self.done = 0;
        self.emit(now);
    }

    /// `done` bytes have moved; reported now unless an event went out less than an interval
    /// ago (then [`Progress::due`] says when).
    fn set(&mut self, done: u64, now: Instant) {
        self.done = done;
        match self.sent {
            Some((_, at)) if now < at + PROGRESS_INTERVAL => {}
            _ => self.emit(now),
        }
    }

    /// When the held-back count should go out, if one is held back.
    fn due(&self) -> Option<Instant> {
        match self.sent {
            Some((sent, at)) if sent != self.done => Some(at + PROGRESS_INTERVAL),
            _ => None,
        }
    }

    /// Send the held-back count, if any.
    fn flush(&mut self, now: Instant) {
        if self.sent.is_some_and(|(sent, _)| sent != self.done) {
            self.emit(now);
        }
    }

    fn emit(&mut self, now: Instant) {
        self.send(TransferEvent::Progress {
            done: self.done,
            total: self.total,
        });
        self.sent = Some((self.done, now));
    }
}

/// One transfer's task state: its cancel signal and its progress.
struct Run {
    cancel: Arc<Cancel>,
    progress: Progress,
}

impl Run {
    fn new(id: TransferId, sink: TransferSink, cancel: Arc<Cancel>) -> Self {
        Self {
            cancel,
            progress: Progress::new(id, sink),
        }
    }

    /// Await `fut` unless the transfer is cancelled first; sends held-back progress when it
    /// is due meanwhile (a one-off wake-up, only while a count is held back).
    async fn step<T>(&mut self, fut: impl Future<Output = T>) -> Result<T, Stop> {
        tokio::pin!(fut);
        loop {
            let due = self.progress.due();
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => return Err(Stop::Cancelled),
                value = &mut fut => return Ok(value),
                () = sleep_until(due) => self.progress.flush(Instant::now()),
            }
        }
    }

    /// Await `fut` to its end even when the transfer is cancelled meanwhile: for requests that
    /// change the server or hand out a handle, whose effect must be known before cleaning up.
    /// The caller checks for a cancel afterwards. Bounded by [`SETTLE_WAIT`].
    async fn settle<T>(&mut self, fut: impl Future<Output = T>) -> Result<T, Stop> {
        let fut = tokio::time::timeout(SETTLE_WAIT, fut);
        tokio::pin!(fut);
        loop {
            let due = self.progress.due();
            tokio::select! {
                biased;
                value = &mut fut => {
                    return value.map_err(|_| {
                        Stop::Failed("The server did not answer in time.".into())
                    });
                }
                () = sleep_until(due) => self.progress.flush(Instant::now()),
            }
        }
    }

    /// Send the final event: a last progress event first when the count moved, on success.
    fn end(mut self, outcome: Result<TransferEvent, Stop>) {
        let event = match outcome {
            Ok(event) => {
                self.progress.flush(Instant::now());
                event
            }
            Err(Stop::Cancelled) => TransferEvent::Cancelled,
            Err(Stop::Failed(reason)) => TransferEvent::Failed(reason),
        };
        self.progress.send(event);
    }
}

async fn sleep_until(due: Option<Instant>) {
    match due {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Close the remote `handle` without caring how that went (bounded).
async fn close_quietly(sftp: &Sftp, handle: &str, path: &str) {
    let _ = tokio::time::timeout(CLOSE_WAIT, sftp.close(handle, path)).await;
}

/// Remove the remote `path` without caring how that went (bounded).
async fn remove_quietly(sftp: &Sftp, path: &str) {
    let _ = tokio::time::timeout(CLOSE_WAIT, sftp.remove(path)).await;
}

// ---- Downloads ----

async fn run_download(
    run: &mut Run,
    sftp: &Sftp,
    remote: &str,
    dest: &Path,
) -> Result<TransferEvent, Stop> {
    let name = shown(remote.rsplit('/').next().unwrap_or(remote));
    let entry = match run.step(sftp.stat(remote)).await? {
        Ok(entry) => entry,
        Err(SftpError::NotFound(m)) => {
            let link = run.step(sftp.lstat(remote)).await?;
            return Err(Stop::Failed(match link {
                Ok(e) if matches!(e.kind, EntryKind::Link { .. }) => {
                    format!("{name} is a broken link and cannot be downloaded.")
                }
                _ => m,
            }));
        }
        Err(e) => return Err(e.into()),
    };
    match entry.kind {
        EntryKind::File => {}
        EntryKind::Dir => {
            return Err(Stop::Failed(format!(
                "{name} is a folder; only files can be downloaded."
            )));
        }
        _ => {
            return Err(Stop::Failed(format!(
                "{name} is not a regular file and cannot be downloaded."
            )));
        }
    }
    let Some(file_name) = dest.file_name() else {
        return Err(Stop::Failed(format!(
            "{} is not a file name to save to.",
            dest.display()
        )));
    };
    let part = dest.with_file_name(part_name(&file_name.to_string_lossy()));
    run.progress.start(Some(entry.size), Instant::now());

    let handle = run.settle(sftp.open_read(remote)).await??;
    if run.cancel.is_cancelled() {
        close_quietly(sftp, &handle, remote).await;
        return Err(Stop::Cancelled);
    }
    let mut file = match create_local_part(&part).await {
        Ok(file) => file,
        Err(stop) => {
            close_quietly(sftp, &handle, remote).await;
            return Err(stop);
        }
    };
    let copied = match fetch(run, sftp, &handle, remote, &mut file, &part).await {
        Ok(()) => file
            .sync_all()
            .await
            .map_err(|e| local_error("write", &part, &e)),
        Err(stop) => Err(stop),
    };
    drop(file);
    close_quietly(sftp, &handle, remote).await;
    let saved = match copied {
        Ok(()) if run.cancel.is_cancelled() => Err(Stop::Cancelled),
        Ok(()) => tokio::fs::rename(&part, dest)
            .await
            .map_err(|e| local_error("save", dest, &e)),
        Err(stop) => Err(stop),
    };
    if let Err(stop) = saved {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(stop);
    }
    Ok(TransferEvent::Finished {
        note: None,
        local: Some(dest.to_owned()),
    })
}

/// Create the local part file; one left over from an earlier run is replaced.
async fn create_local_part(part: &Path) -> Result<tokio::fs::File, Stop> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let create = || options.open(part);
    match create().await {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            let _ = tokio::fs::remove_file(part).await;
            create().await.map_err(|e| local_error("create", part, &e))
        }
        Err(e) => Err(local_error("create", part, &e)),
    }
}

/// Copy the open remote `handle` into `file` until the end of the remote file, with a window
/// of reads in flight. A short read is completed before the next one is taken.
async fn fetch(
    run: &mut Run,
    sftp: &Sftp,
    handle: &str,
    remote: &str,
    file: &mut tokio::fs::File,
    part: &Path,
) -> Result<(), Stop> {
    let chunk = sftp.read_len();
    let window = window(chunk);
    let read_at = |offset: u64, len: u32| async move {
        (offset, sftp.read(handle, remote, offset, len).await)
    };
    let mut pending = FuturesOrdered::new();
    let mut next = 0u64;
    let mut done = 0u64;
    loop {
        while pending.len() < window {
            pending.push_back(read_at(next, chunk));
            next += u64::from(chunk);
        }
        let Some((offset, read)) = run.step(pending.next()).await? else {
            break;
        };
        let Some(mut data) = read?.filter(|d| !d.is_empty()) else {
            break;
        };
        // Never trust the server with more than was asked for.
        data.truncate(chunk as usize);
        file.write_all(&data)
            .await
            .map_err(|e| local_error("write", part, &e))?;
        done += data.len() as u64;
        // A short answer: read the rest of this request's range before going on.
        let end = offset + u64::from(chunk);
        let mut ended = false;
        while done < end {
            let want = u32::try_from(end - done).unwrap_or(chunk);
            match run.step(sftp.read(handle, remote, done, want)).await?? {
                Some(mut more) if !more.is_empty() => {
                    more.truncate(want as usize);
                    file.write_all(&more)
                        .await
                        .map_err(|e| local_error("write", part, &e))?;
                    done += more.len() as u64;
                }
                _ => {
                    ended = true;
                    break;
                }
            }
        }
        run.progress.set(done, Instant::now());
        if ended {
            break;
        }
    }
    run.progress.set(done, Instant::now());
    Ok(())
}

// ---- Uploads ----

async fn run_upload(
    run: &mut Run,
    sftp: &Sftp,
    local: &Path,
    folder: &str,
    name: &str,
    mode: UploadMode,
) -> Result<TransferEvent, Stop> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.chars().any(char::is_control)
    {
        return Err(Stop::Failed(format!(
            "\"{}\" cannot be used as a file name on the server.",
            shown(name)
        )));
    }
    let meta = tokio::fs::metadata(local)
        .await
        .map_err(|e| local_error("read", local, &e))?;
    if meta.is_dir() {
        return Err(Stop::Failed(format!("Folders are not uploaded: {name}")));
    }
    if !meta.is_file() {
        return Err(Stop::Failed(format!(
            "{name} is not a regular file and cannot be uploaded."
        )));
    }
    let mut file = tokio::fs::File::open(local)
        .await
        .map_err(|e| local_error("read", local, &e))?;
    run.progress.start(Some(meta.len()), Instant::now());

    let target = join(folder, name);
    let part = join(folder, &part_name(name));
    let existing = match run.step(sftp.lstat_attrs(&target)).await? {
        Ok(attrs) => Some(attrs),
        Err(SftpError::NotFound(_)) => None,
        Err(e) => return Err(e.into()),
    };
    let up = Upload {
        sftp,
        local,
        folder,
        name,
        target: &target,
        part: &part,
    };
    let Some(existing) = existing else {
        let handle = up.create_part(run).await??;
        return up.through_part(run, &mut file, handle, None).await;
    };
    if mode == UploadMode::New {
        return Err(Stop::Failed(format!(
            "\"{name}\" already exists in {folder}."
        )));
    }
    match kind_of_mode(existing.permissions) {
        EntryKind::File => {}
        EntryKind::Dir => {
            return Err(Stop::Failed(format!(
                "\"{name}\" is a folder in {folder} and cannot be replaced."
            )));
        }
        EntryKind::Link { .. } => {
            // Renaming over the link would turn it into a file: write what it ends at in
            // place, so the link stays a link to the same file.
            let end = run.step(sftp.stat_attrs(&target)).await??;
            if kind_of_mode(end.permissions) != EntryKind::File {
                return Err(Stop::Failed(format!(
                    "{name} is not a regular file and cannot be replaced."
                )));
            }
            return up.in_place(run, &mut file).await;
        }
        EntryKind::Other => {
            return Err(Stop::Failed(format!(
                "{name} is not a regular file and cannot be replaced."
            )));
        }
    }
    if !sftp.supports_posix_rename() {
        return up.in_place(run, &mut file).await;
    }
    match up.create_part(run).await? {
        Ok(handle) => {
            up.through_part(run, &mut file, handle, Some(existing))
                .await
        }
        // The folder cannot take a new file: write the existing one instead.
        Err(SftpError::Denied(_)) => up.in_place(run, &mut file).await,
        Err(e) => Err(e.into()),
    }
}

/// One upload's paths.
struct Upload<'a> {
    sftp: &'a Sftp,
    local: &'a Path,
    folder: &'a str,
    name: &'a str,
    target: &'a str,
    part: &'a str,
}

/// How filling the part file ended.
enum PartEnd {
    /// Renamed to the target: done.
    Renamed,
    /// Complete and closed, but the server would not rename it over the target.
    RenameDenied,
}

impl Upload<'_> {
    /// The clash message for this upload's name.
    fn clash(&self) -> Stop {
        Stop::Failed(format!(
            "\"{}\" already exists in {}.",
            self.name, self.folder
        ))
    }

    /// Create the part file `.<name>.fbterm-part`. Anything already there under that name is
    /// taken for a leftover of an earlier upload and removed first. The inner error is the
    /// server's answer, for the caller to judge. A cancel during the create is acted on once
    /// the server answered: a part file it made is closed and removed.
    async fn create_part(&self, run: &mut Run) -> Result<Result<String, SftpError>, Stop> {
        if let Err(e @ SftpError::Unavailable(_)) = run.step(self.sftp.remove(self.part)).await? {
            return Err(e.into());
        }
        let created = run.settle(self.sftp.create_new(self.part)).await?;
        if run.cancel.is_cancelled() {
            if let Ok(handle) = &created {
                close_quietly(self.sftp, handle, self.part).await;
                remove_quietly(self.sftp, self.part).await;
            }
            return Err(Stop::Cancelled);
        }
        Ok(created)
    }

    /// Fill the open part file, give it `keep`'s owner and mode, and rename it to the target
    /// (over it when `keep` is set, never over anything otherwise). Any failure removes the
    /// part file.
    async fn through_part(
        &self,
        run: &mut Run,
        file: &mut tokio::fs::File,
        handle: String,
        keep: Option<FileAttributes>,
    ) -> Result<TransferEvent, Stop> {
        let mut open = Some(handle);
        match self.fill_part(run, file, &mut open, keep.as_ref()).await {
            Ok(PartEnd::Renamed) => Ok(TransferEvent::Finished {
                note: None,
                local: None,
            }),
            Ok(PartEnd::RenameDenied) => {
                // The folder will not let this entry be replaced: start over in place.
                remove_quietly(self.sftp, self.part).await;
                if run.cancel.is_cancelled() {
                    return Err(Stop::Cancelled);
                }
                file.seek(SeekFrom::Start(0))
                    .await
                    .map_err(|e| local_error("read", self.local, &e))?;
                run.progress.restart(Instant::now());
                self.in_place(run, file).await
            }
            Err(stop) => {
                if let Some(handle) = open {
                    close_quietly(self.sftp, &handle, self.part).await;
                }
                remove_quietly(self.sftp, self.part).await;
                Err(stop)
            }
        }
    }

    async fn fill_part(
        &self,
        run: &mut Run,
        file: &mut tokio::fs::File,
        open: &mut Option<String>,
        keep: Option<&FileAttributes>,
    ) -> Result<PartEnd, Stop> {
        let sftp = self.sftp;
        let handle = open.clone().unwrap_or_default();
        if let Some(keep) = keep {
            // Owner first (changing it may clear set-id bits), then the mode.
            if let (Some(uid), Some(gid)) = (keep.uid, keep.gid) {
                let owner = FileAttributes {
                    uid: Some(uid),
                    gid: Some(gid),
                    ..FileAttributes::empty()
                };
                // Only allowed when the server lets this user give files that owner.
                if let Err(e @ SftpError::Unavailable(_)) =
                    run.step(sftp.fsetstat(&handle, self.part, owner)).await?
                {
                    return Err(e.into());
                }
            }
            if let Some(mode) = keep.permissions {
                let mode = FileAttributes {
                    permissions: Some(mode & 0o7777),
                    ..FileAttributes::empty()
                };
                run.step(sftp.fsetstat(&handle, self.part, mode)).await??;
            }
        }
        send(run, sftp, &handle, self.part, file, self.local).await?;
        *open = None;
        run.step(sftp.close(&handle, self.part)).await??;
        if run.cancel.is_cancelled() {
            return Err(Stop::Cancelled);
        }
        // From here on a cancel is too late: the rename is awaited to its answer.
        if keep.is_some() {
            match run
                .settle(sftp.posix_rename(self.part, self.target))
                .await?
            {
                Ok(()) => Ok(PartEnd::Renamed),
                Err(SftpError::Denied(_)) => Ok(PartEnd::RenameDenied),
                Err(e) => Err(e.into()),
            }
        } else {
            match run.settle(sftp.rename(self.part, self.target)).await? {
                Ok(()) => Ok(PartEnd::Renamed),
                Err(e @ SftpError::Unavailable(_)) => Err(e.into()),
                // Lost a race: something took the name since it was checked.
                Err(e) => match run.settle(sftp.lstat_attrs(self.target)).await? {
                    Ok(_) => Err(self.clash()),
                    Err(_) => Err(e.into()),
                },
            }
        }
    }

    /// Write the existing target in place (it keeps its inode, owner and mode). Once the
    /// open (which empties it) has been sent, any stop says the file may be incomplete.
    async fn in_place(
        &self,
        run: &mut Run,
        file: &mut tokio::fs::File,
    ) -> Result<TransferEvent, Stop> {
        let sftp = self.sftp;
        if run.cancel.is_cancelled() {
            return Err(Stop::Cancelled);
        }
        let incomplete = |reason: String| Stop::Failed(format!("{MAY_BE_INCOMPLETE}. {reason}"));
        let handle = match run.settle(sftp.open_truncate(self.target)).await {
            Ok(Ok(handle)) => handle,
            // The server may or may not have emptied the file before the session went.
            Ok(Err(e @ SftpError::Unavailable(_))) => return Err(incomplete(e.to_string())),
            // Refused: the file was not touched.
            Ok(Err(e)) => return Err(e.into()),
            Err(Stop::Failed(reason)) => return Err(incomplete(reason)),
            Err(Stop::Cancelled) => return Err(Stop::Failed(format!("{MAY_BE_INCOMPLETE}."))),
        };
        if run.cancel.is_cancelled() {
            close_quietly(sftp, &handle, self.target).await;
            return Err(Stop::Failed(format!("{MAY_BE_INCOMPLETE}.")));
        }
        let written = match send(run, sftp, &handle, self.target, file, self.local).await {
            Ok(()) => run
                .step(sftp.close(&handle, self.target))
                .await
                .and_then(|closed| closed.map_err(Stop::from)),
            Err(stop) => {
                close_quietly(sftp, &handle, self.target).await;
                Err(stop)
            }
        };
        match written {
            Ok(()) => Ok(TransferEvent::Finished {
                note: Some(REPLACED_IN_PLACE.into()),
                local: None,
            }),
            Err(Stop::Cancelled) => Err(Stop::Failed(format!("{MAY_BE_INCOMPLETE}."))),
            Err(Stop::Failed(reason)) => Err(incomplete(reason)),
        }
    }
}

/// Fill `buf` from `file` as far as it goes; fewer bytes only at the end of the file.
async fn read_full(file: &mut tokio::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]).await? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// Write all of the local `file` (to its end, whatever its size now) to the open remote
/// `handle`, with a window of writes in flight.
async fn send(
    run: &mut Run,
    sftp: &Sftp,
    handle: &str,
    remote: &str,
    file: &mut tokio::fs::File,
    local: &Path,
) -> Result<(), Stop> {
    let chunk = sftp.write_len();
    let window = window(chunk);
    let mut pending = FuturesUnordered::new();
    let mut offset = 0u64;
    let mut acked = 0u64;
    let mut local_done = false;
    loop {
        while !local_done && pending.len() < window {
            let mut buf = vec![0; chunk as usize];
            let n = read_full(file, &mut buf)
                .await
                .map_err(|e| local_error("read", local, &e))?;
            if n < buf.len() {
                local_done = true;
            }
            if n == 0 {
                break;
            }
            buf.truncate(n);
            let at = offset;
            offset += n as u64;
            pending
                .push(async move { sftp.write(handle, remote, at, buf).await.map(|()| n as u64) });
        }
        let Some(written) = run.step(pending.next()).await? else {
            break;
        };
        acked += written?;
        run.progress.set(acked, Instant::now());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
