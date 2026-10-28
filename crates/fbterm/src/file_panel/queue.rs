//! Uploads, the transfer queue's `Retry` and dismiss, and the transfers closing would end.
//!
//! - **Uploads.** Only the toolbar's `Upload…` (the system's multi-file picker over the
//!   window) and files the OS drops on a window whose panel is open start one; nothing a
//!   remote program sends can. They go into the folder the panel shows. Files picked or
//!   dropped together are one [`Batch`]: decided one at a time, folders skipped with
//!   `Folders are not uploaded: {name}` in the queue. A name already in the folder (in its
//!   listing, or being uploaded) is asked about with the clash dialog: `Replace`, `Rename` (a
//!   name field prefilled with a free name) or `Cancel`, and `Apply to all` (Replace and
//!   Cancel) while other files of the batch wait. Everything else uploads with
//!   [`UploadMode::New`], which never replaces anything even when the listing was out of
//!   date. Once no upload into the shown folder runs, the folder is read again.
//! - **Drops** come one OS event per file; those of one burst are gathered (a single short
//!   wait after the first, no timer otherwise) into one batch. With the window's panel
//!   closed the batch types its paths instead (`crate::file_drop`).
//! - **Retry** does a failed row's [`Job`] again in its place. When the session is gone (the
//!   connection dropped) it is opened again first, asking over the window `Retry` was
//!   pressed in, and the row waits for it.
//! - **Dismiss** removes a row that no longer runs; finished and skipped rows also go when no
//!   panel shows their profile any more. Cancelled rows leave by themselves.
//! - **Closing.** Closing a tab, a pane or a window that holds a profile's last panes while
//!   that profile has transfers running, or quitting while any runs, asks first
//!   (`{n} transfers are still running.`, `Cancel transfers and close` / `Keep open`).
//!   Quitting waits (briefly, bounded) for the cancelled transfers to clean up.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use fbterm_config::ProfileId;
use fbterm_session::ssh::transfer::{
    DownloadMode, TransferId, UploadMode, free_name_avoiding, same_destination,
};
use iced::{Task, window};

use super::state::{AllChoice, Batch, Direction, Job, Listing, RowState, TransferRow};
use super::{DownloadRequest, FilePanelMsg, Started, UploadRequest, plain_name};
use crate::app::{App, Message};
use crate::layout::PaneId;
use crate::prompts::{Clash, Closing, Dialog};

/// How long after the first dropped file the others of the same drop are waited for.
const DROP_GATHER: Duration = Duration::from_millis(120);
/// The longest a quit waits for cancelled transfers to clean up.
const QUIT_WAIT: Duration = Duration::from_secs(5);

/// The clash dialog's answer for the file it asks about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClashAnswer {
    Replace {
        all: bool,
    },
    Cancel {
        all: bool,
    },
    /// Upload under this (checked, free) name instead.
    Rename(String),
}

/// The clash dialog's line.
pub(crate) fn clash_message(name: &str, folder: &str) -> String {
    format!("\"{name}\" already exists in {folder}.")
}

/// The close confirmation's line.
pub(crate) fn running_message(count: usize) -> String {
    if count == 1 {
        "1 transfer is still running.".into()
    } else {
        format!("{count} transfers are still running.")
    }
}

/// The close confirmation's second line, while an upload it would cancel writes over an
/// existing file in place.
pub(crate) const IN_PLACE_WARNING: &str = "Some files being replaced may be left incomplete.";

/// The first of `name`, `name (1).ext`, `name (2).ext`, … that `taken` does not claim.
pub(crate) fn remote_free_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_owned();
    }
    let as_path = std::path::Path::new(name);
    let (stem, ext) = match (as_path.file_stem(), as_path.extension()) {
        (Some(stem), Some(ext)) => (stem.to_string_lossy(), Some(ext.to_string_lossy())),
        _ => (name.into(), None),
    };
    (1u64..)
        .map(|n| match &ext {
            Some(ext) => format!("{stem} ({n}).{ext}"),
            None => format!("{stem} ({n})"),
        })
        .find(|candidate| !taken(candidate))
        .expect("some numbered name is free")
}

/// The queue note of a file skipped because the same name is being uploaded there.
pub(crate) fn already_uploading(name: &str) -> String {
    format!("Already uploading {name}")
}

/// Why a name typed in the clash dialog cannot be used, if it cannot.
pub(crate) fn rename_problem(name: &str) -> Option<&'static str> {
    if name.trim().is_empty() {
        Some("Type a name.")
    } else if name.contains('/') {
        Some("A name cannot contain /.")
    } else if name.chars().any(char::is_control) {
        Some("A name cannot contain control characters.")
    } else if name == "." || name == ".." {
        Some("Choose another name.")
    } else {
        None
    }
}

impl App {
    /// Whether `wid`'s toolbar offers `Upload…`: its panel shows a loaded folder on an open
    /// session.
    pub(crate) fn can_upload(&self, wid: window::Id) -> bool {
        self.shown_profile(wid)
            .and_then(|p| self.file_panels.by_profile.get(&p))
            .is_some_and(|p| {
                p.sftp.is_some() && !p.folder.is_empty() && matches!(p.listing, Listing::Loaded(_))
            })
    }

    /// `Upload…`: the system's picker over `wid`, several files at once.
    pub(super) fn pick_uploads(&mut self, wid: window::Id) -> Task<Message> {
        if !self.can_upload(wid) || !self.file_panels.picking.insert(wid) {
            return Task::none();
        }
        self.file_panel_work
            .backend
            .pick_files(wid)
            .map(move |paths| Message::FilePanel(FilePanelMsg::Picked(wid, paths)))
    }

    /// The picker closed: upload what was chosen.
    pub(super) fn picked(&mut self, wid: window::Id, paths: Option<Vec<PathBuf>>) {
        self.file_panels.picking.remove(&wid);
        if let Some(paths) = paths {
            self.upload_files(wid, paths);
        }
    }

    /// The OS dropped `path` on `wid`. The files of one drop are gathered, then uploaded
    /// together when the panel there shows a server, or typed into the focused pane when
    /// the window's panel is closed (`crate::file_drop`). Ignored anywhere else.
    pub(super) fn dropped(&mut self, wid: window::Id, path: PathBuf) -> Task<Message> {
        if self.shown_profile(wid).is_none() && !self.drop_types_paths(wid) {
            return Task::none();
        }
        let gathered = self.file_panels.dropped.entry(wid).or_default();
        gathered.push(path);
        if gathered.len() > 1 {
            return Task::none();
        }
        Task::perform(async { tokio::time::sleep(DROP_GATHER).await }, move |()| {
            Message::FilePanel(FilePanelMsg::DropsSettled(wid))
        })
    }

    /// The files of one drop have all arrived: upload them, or type their paths.
    pub(super) fn drops_settled(&mut self, wid: window::Id) -> Task<Message> {
        let Some(paths) = self.file_panels.dropped.remove(&wid) else {
            return Task::none();
        };
        if self.drop_types_paths(wid) {
            return self.type_dropped_paths(wid, &paths);
        }
        self.upload_files(wid, paths);
        Task::none()
    }

    /// Upload `paths` into the folder `wid`'s panel shows, as one batch.
    pub(crate) fn upload_files(&mut self, wid: window::Id, paths: Vec<PathBuf>) {
        let Some(profile) = self.shown_profile(wid) else {
            return;
        };
        let Some(folder) = self
            .file_panels
            .by_profile
            .get(&profile)
            .map(|p| p.folder.clone())
            .filter(|f| !f.is_empty())
        else {
            return;
        };
        let mut items = VecDeque::new();
        for path in paths {
            let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            if path.is_dir() {
                let shown = plain_name(&name);
                self.add_row(
                    profile,
                    TransferRow::stopped(
                        shown.clone(),
                        Direction::Upload,
                        RowState::Skipped(format!("Folders are not uploaded: {shown}")),
                    ),
                );
                continue;
            }
            items.push_back((path, name));
        }
        if items.is_empty() {
            return;
        }
        self.file_panels
            .batches
            .entry(wid)
            .or_default()
            .push_back(Batch {
                profile,
                folder,
                items,
                asking: None,
                all: None,
            });
        self.advance_uploads(wid);
    }

    /// Whether `name` is taken in `profile`'s remote `folder`: listed there, or being
    /// uploaded there.
    fn remote_taken(&self, profile: ProfileId, folder: &str, name: &str) -> bool {
        let Some(panel) = self.file_panels.by_profile.get(&profile) else {
            return false;
        };
        let listed = panel.folder == folder
            && matches!(&panel.listing, Listing::Loaded(entries) if entries.iter().any(|e| e.name == name));
        listed || self.uploading_same(profile, folder, name)
    }

    /// Whether a running upload into `profile`'s `folder` writes `name` (ignoring case, as
    /// [`same_destination`] does): a second upload there would share its part file.
    fn uploading_same(&self, profile: ProfileId, folder: &str, name: &str) -> bool {
        self.uploading_same_except(profile, folder, name, None)
    }

    /// [`App::uploading_same`], not counting the row `except` (a row waiting to start again
    /// is running already, and must not block itself).
    fn uploading_same_except(
        &self,
        profile: ProfileId,
        folder: &str,
        name: &str,
        except: Option<TransferId>,
    ) -> bool {
        let name = name.to_lowercase();
        self.file_panels.by_profile.get(&profile).is_some_and(|p| {
            p.transfers
                .iter()
                .filter(|r| Some(r.id) != except)
                .filter_map(|r| r.uploading_into(folder))
                .any(|n| n.to_lowercase() == name)
        })
    }

    /// Whether `profile`'s listing of `folder` is on its way: clashes are checked once it is
    /// there.
    fn listing_pending(&self, profile: ProfileId, folder: &str) -> bool {
        self.file_panels.by_profile.get(&profile).is_some_and(|p| {
            p.folder == folder && matches!(p.listing, Listing::Opening | Listing::Loading)
        })
    }

    /// Go on with every window's uploads (a listing they wait for may have arrived).
    pub(super) fn advance_all_uploads(&mut self) {
        let windows: Vec<window::Id> = self.file_panels.batches.keys().copied().collect();
        for wid in windows {
            self.advance_uploads(wid);
        }
    }

    /// Decide the files of `wid`'s batches in order until one needs the clash dialog (or
    /// the folder's listing, still on its way).
    fn advance_uploads(&mut self, wid: window::Id) {
        loop {
            let Some(queue) = self.file_panels.batches.get_mut(&wid) else {
                return;
            };
            let Some(batch) = queue.front() else {
                self.file_panels.batches.remove(&wid);
                return;
            };
            if batch.asking.is_some() {
                return;
            }
            let (profile, folder, all) = (batch.profile, batch.folder.clone(), batch.all);
            if self.listing_pending(profile, &folder) {
                return;
            }
            let Some(queue) = self.file_panels.batches.get_mut(&wid) else {
                return;
            };
            let Some(batch) = queue.front_mut() else {
                return;
            };
            let Some((local, name)) = batch.items.pop_front() else {
                queue.pop_front();
                continue;
            };
            let more = !batch.items.is_empty();
            let upload = |mode| Job::Upload {
                local: local.clone(),
                folder: folder.clone(),
                name: name.clone(),
                mode,
            };
            // A second upload of a name being uploaded would share its part file: never.
            if self.uploading_same(profile, &folder, &name) {
                let shown = plain_name(&name);
                self.add_row(
                    profile,
                    TransferRow::stopped(
                        shown.clone(),
                        Direction::Upload,
                        RowState::Skipped(already_uploading(&shown)),
                    ),
                );
                continue;
            }
            if !self.remote_taken(profile, &folder, &name) {
                self.start_job(profile, upload(UploadMode::New));
                continue;
            }
            match all {
                Some(AllChoice::Replace) => self.start_job(profile, upload(UploadMode::Replace)),
                Some(AllChoice::Cancel) => {}
                None => {
                    if let Some(batch) = self
                        .file_panels
                        .batches
                        .get_mut(&wid)
                        .and_then(VecDeque::front_mut)
                    {
                        batch.asking = Some((local.clone(), name.clone()));
                    }
                    self.prompts.push(
                        wid,
                        None,
                        Dialog::Clash(Clash {
                            name,
                            folder,
                            offer_all: more,
                            apply_all: false,
                            rename: None,
                        }),
                    );
                    return;
                }
            }
        }
    }

    /// The clash dialog of `wid` was answered (and taken off the queue).
    pub(crate) fn clash_answer(&mut self, wid: window::Id, answer: ClashAnswer) -> Task<Message> {
        let Some(batch) = self
            .file_panels
            .batches
            .get_mut(&wid)
            .and_then(VecDeque::front_mut)
        else {
            return Task::none();
        };
        let Some((local, name)) = batch.asking.take() else {
            return Task::none();
        };
        let (profile, folder) = (batch.profile, batch.folder.clone());
        let job = |name: String, mode| Job::Upload {
            local: local.clone(),
            folder: folder.clone(),
            name,
            mode,
        };
        match answer {
            ClashAnswer::Replace { all } => {
                if all {
                    batch.all = Some(AllChoice::Replace);
                }
                self.start_job(profile, job(name, UploadMode::Replace));
            }
            ClashAnswer::Cancel { all } => {
                if all {
                    batch.all = Some(AllChoice::Cancel);
                }
            }
            ClashAnswer::Rename(new) => self.start_job(profile, job(new, UploadMode::New)),
        }
        self.advance_uploads(wid);
        Task::none()
    }

    /// The name `Rename` offers for `name` in the folder `wid`'s clash dialog is about.
    pub(crate) fn clash_free_name(&self, wid: window::Id, name: &str) -> String {
        match self.file_panels.batches.get(&wid).and_then(VecDeque::front) {
            Some(batch) => {
                remote_free_name(name, |n| self.remote_taken(batch.profile, &batch.folder, n))
            }
            None => name.to_owned(),
        }
    }

    /// `Rename` (or Enter) in the clash dialog's name field: a usable, free name uploads; a
    /// name that is taken too is asked about again; anything else says why not.
    pub(crate) fn clash_rename_submit(&mut self, wid: window::Id) -> Task<Message> {
        let Some(Dialog::Clash(Clash {
            rename: Some(rename),
            ..
        })) = self.prompts.front(wid)
        else {
            return Task::none();
        };
        let typed = rename.value.clone();
        // A name being uploaded there cannot be replaced either: it would share a part file.
        let in_flight = self
            .file_panels
            .batches
            .get(&wid)
            .and_then(VecDeque::front)
            .is_some_and(|b| self.uploading_same(b.profile, &b.folder, &typed));
        let problem = rename_problem(&typed)
            .map(str::to_owned)
            .or_else(|| in_flight.then(|| already_uploading(&plain_name(&typed))));
        if let Some(problem) = problem {
            if let Some(Dialog::Clash(Clash {
                rename: Some(rename),
                ..
            })) = self.prompts.front_mut(wid)
            {
                rename.problem = Some(problem);
            }
            return self.focus_prompt(wid);
        }
        let taken = self
            .file_panels
            .batches
            .get(&wid)
            .and_then(VecDeque::front)
            .is_some_and(|b| self.remote_taken(b.profile, &b.folder, &typed));
        if taken {
            if let Some(batch) = self
                .file_panels
                .batches
                .get_mut(&wid)
                .and_then(VecDeque::front_mut)
                && let Some(asking) = &mut batch.asking
            {
                asking.1.clone_from(&typed);
            }
            if let Some(Dialog::Clash(clash)) = self.prompts.front_mut(wid) {
                clash.name = typed;
                clash.rename = None;
                clash.apply_all = false;
            }
            return Task::none();
        }
        self.prompts.pop(wid);
        let task = self.clash_answer(wid, ClashAnswer::Rename(typed));
        Task::batch([task, self.focus_prompt(wid)])
    }

    /// Start `job` on `profile`'s session, with a row in its queue.
    pub(super) fn start_job(&mut self, profile: ProfileId, job: Job) {
        let row = self.launch_job(profile, job, None);
        self.add_row(profile, row);
    }

    /// Start `job` on `profile`'s session: its row, running, or failed with why it could not
    /// start (and `Retry`). `again` is the row a `Retry` starts again, if it is one.
    ///
    /// Every upload starts here, so this is where two uploads of one name into one folder are
    /// kept apart (they would share a part file, and one could rename the other's half-written
    /// file into place): an upload whose name another row is uploading there does not start.
    /// A new row is skipped with `Already uploading {name}`; a row started again fails with
    /// that reason and keeps its `Retry`.
    fn launch_job(&self, profile: ProfileId, job: Job, again: Option<TransferId>) -> TransferRow {
        let (name, direction) = match &job {
            Job::Download { name, .. } => (plain_name(name), Direction::Download),
            Job::Upload { name, .. } => (plain_name(name), Direction::Upload),
        };
        if let Job::Upload {
            folder,
            name: remote,
            ..
        } = &job
            && self.uploading_same_except(profile, folder, remote, again)
        {
            let note = already_uploading(&name);
            if again.is_none() {
                return TransferRow::stopped(name, direction, RowState::Skipped(note));
            }
            let mut row = TransferRow::stopped(name, direction, RowState::Failed(note));
            row.job = Some(job);
            return row;
        }
        let session = self
            .file_panels
            .by_profile
            .get(&profile)
            .and_then(|p| p.sftp.clone());
        let started = match session {
            None => Err("The SFTP session is not open.".to_owned()),
            Some(remote) => {
                let sink = self.file_panel_work.transfer_sink.clone();
                match &job {
                    Job::Download {
                        remote: path,
                        dest,
                        chosen,
                        ..
                    } => self.file_panel_work.backend.download(
                        DownloadRequest {
                            remote,
                            path: path.clone(),
                            dest: dest.clone(),
                            // Only a destination the user chose (and agreed to replace in the
                            // save dialog) is replaced; `Download` never replaces anything.
                            mode: if *chosen {
                                DownloadMode::Replace
                            } else {
                                DownloadMode::FreeName
                            },
                        },
                        sink,
                    ),
                    Job::Upload {
                        local,
                        folder,
                        name,
                        mode,
                    } => self.file_panel_work.backend.upload(
                        UploadRequest {
                            remote,
                            local: local.clone(),
                            folder: folder.clone(),
                            name: name.clone(),
                            mode: *mode,
                        },
                        sink,
                    ),
                }
            }
        };
        let mut row = match (started, &job) {
            (Ok(Started { id, cancel }), Job::Download { dest, .. }) => {
                TransferRow::download(id, name, dest.clone(), cancel)
            }
            (Ok(Started { id, cancel }), Job::Upload { .. }) => {
                TransferRow::upload(id, name, cancel)
            }
            (Err(reason), _) => TransferRow::stopped(name, direction, RowState::Failed(reason)),
        };
        row.job = Some(job);
        row
    }

    /// The profile whose queue holds row `id`, and the row's place in it.
    fn find_row(
        &self,
        id: fbterm_session::ssh::transfer::TransferId,
    ) -> Option<(ProfileId, usize)> {
        self.file_panels
            .by_profile
            .iter()
            .find_map(|(p, panel)| Some((*p, panel.transfers.iter().position(|r| r.id == id)?)))
    }

    /// A row's `Retry` in `wid`: its job again, in its place; a session that is gone is
    /// opened again first (asking over `wid`) and the row waits for it.
    pub(super) fn retry_transfer(
        &mut self,
        wid: window::Id,
        id: fbterm_session::ssh::transfer::TransferId,
    ) {
        let Some((profile, index)) = self.find_row(id) else {
            return;
        };
        let row = &self.file_panels.by_profile[&profile].transfers[index];
        if !row.retryable() {
            return;
        }
        let Some(mut job) = row.job.clone() else {
            return;
        };
        match &mut job {
            // A name being uploaded there now: not a second upload of it, and the row keeps
            // its `Retry` for later.
            Job::Upload { folder, name, .. } if self.uploading_same(profile, folder, name) => {
                let note = already_uploading(&plain_name(name));
                if let Some(panel) = self.file_panels.by_profile.get_mut(&profile) {
                    panel.transfers[index].state = RowState::Failed(note);
                }
                return;
            }
            // A name now in the folder is asked about before anything is replaced.
            Job::Upload {
                local,
                folder,
                name,
                mode: UploadMode::New,
            } if self.remote_taken(profile, folder, name) => {
                let batch = Batch {
                    profile,
                    folder: folder.clone(),
                    items: VecDeque::from([(local.clone(), name.clone())]),
                    asking: None,
                    all: None,
                };
                if let Some(panel) = self.file_panels.by_profile.get_mut(&profile) {
                    panel.transfers.remove(index);
                }
                self.file_panels
                    .batches
                    .entry(wid)
                    .or_default()
                    .push_back(batch);
                self.advance_uploads(wid);
                return;
            }
            Job::Download {
                name, dest, chosen, ..
            } => {
                let writing = self.writing();
                if *chosen {
                    if writing.iter().any(|w| same_destination(w, dest)) {
                        let shown = dest
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        if let Some(panel) = self.file_panels.by_profile.get_mut(&profile) {
                            panel.transfers[index].state = RowState::Failed(format!(
                                "Already downloading to {}",
                                plain_name(&shown)
                            ));
                        }
                        return;
                    }
                } else if let Some(folder) = dest.parent() {
                    // Never over a file that appeared since: a free name again.
                    *dest = free_name_avoiding(folder, name, |p| {
                        writing.iter().any(|w| same_destination(w, p))
                    });
                }
            }
            Job::Upload { .. } => {}
        }
        let alive = self
            .file_panels
            .by_profile
            .get(&profile)
            .and_then(|p| p.sftp.as_ref())
            .is_some_and(|s| s.is_alive());
        if alive {
            let row = self.launch_job(profile, job, Some(id));
            if let Some(panel) = self.file_panels.by_profile.get_mut(&profile) {
                panel.transfers[index] = row;
            }
            return;
        }
        let Some(panel) = self.file_panels.by_profile.get_mut(&profile) else {
            return;
        };
        let row = &mut panel.transfers[index];
        if let Job::Download { dest, .. } = &job {
            row.local = Some(dest.clone());
        }
        row.state = RowState::Running;
        row.done = 0;
        row.total = None;
        row.cancel = None;
        row.job = Some(job);
        panel.waiting.push(id);
        if panel.listing != Listing::Opening {
            self.reopen(profile, wid);
        }
    }

    /// `profile`'s session opened (`Ok`) or could not (`Err`, why): the rows waiting for it
    /// start, or fail with the reason.
    pub(super) fn run_waiting(&mut self, profile: ProfileId, opened: Result<(), &str>) {
        let Some(panel) = self.file_panels.by_profile.get_mut(&profile) else {
            return;
        };
        let waiting = std::mem::take(&mut panel.waiting);
        for id in waiting {
            let Some(index) = self.file_panels.by_profile[&profile]
                .transfers
                .iter()
                .position(|r| r.id == id)
            else {
                continue;
            };
            let job = self.file_panels.by_profile[&profile].transfers[index]
                .job
                .clone();
            let row = match (opened, job) {
                (Ok(()), Some(job)) => self.launch_job(profile, job, Some(id)),
                (Err(reason), job) => {
                    let row = &self.file_panels.by_profile[&profile].transfers[index];
                    let mut failed = TransferRow::stopped(
                        row.name.clone(),
                        row.direction,
                        RowState::Failed(format!("SFTP is not available: {reason}")),
                    );
                    failed.job = job;
                    failed
                }
                (Ok(()), None) => continue,
            };
            if let Some(panel) = self.file_panels.by_profile.get_mut(&profile) {
                panel.transfers[index] = row;
            }
        }
        // Rows that failed no longer run: a close confirmation counts them no more.
        self.refresh_transfer_questions();
    }

    /// A row's dismiss button: rows that no longer run leave the queue.
    pub(super) fn dismiss(&mut self, id: fbterm_session::ssh::transfer::TransferId) {
        if let Some((profile, index)) = self.find_row(id)
            && let Some(panel) = self.file_panels.by_profile.get_mut(&profile)
            && !panel.transfers[index].is_running()
        {
            panel.transfers.remove(index);
        }
    }

    /// No panel shows `profile` any more: its finished and skipped rows go.
    pub(super) fn forget_finished(&mut self, profile: ProfileId) {
        let shown = self
            .file_panels
            .open
            .iter()
            .any(|&w| self.shown_profile(w) == Some(profile));
        if shown {
            return;
        }
        if let Some(panel) = self.file_panels.by_profile.get_mut(&profile) {
            panel
                .transfers
                .retain(|r| !matches!(r.state, RowState::Finished { .. } | RowState::Skipped(_)));
        }
    }

    /// An upload into `folder` of `profile` ended: once none runs there any more, the folder
    /// is read again when the panel shows it, so the new names are listed (and clash).
    pub(super) fn uploads_ended(&mut self, profile: ProfileId, folder: &str) {
        let Some(panel) = self.file_panels.by_profile.get(&profile) else {
            return;
        };
        let running = panel
            .transfers
            .iter()
            .any(|r| r.uploading_into(folder).is_some());
        if !running && panel.folder == folder && matches!(panel.listing, Listing::Loaded(_)) {
            self.read_folder(profile);
        }
    }

    // ---- Closing while transfers run ----

    /// Running transfers of `profiles`.
    fn running_of(&self, profiles: &[ProfileId]) -> usize {
        self.running_rows(Some(profiles)).count()
    }

    /// The running rows of `profiles` (every profile with `None`).
    fn running_rows<'a>(
        &'a self,
        profiles: Option<&'a [ProfileId]>,
    ) -> impl Iterator<Item = &'a TransferRow> + 'a {
        self.file_panels
            .by_profile
            .iter()
            .filter(move |(p, _)| profiles.is_none_or(|ps| ps.contains(p)))
            .flat_map(|(_, panel)| panel.transfers.iter())
            .filter(|r| r.is_running())
    }

    /// What a close confirmation about `profiles` (every profile with `None`) says: how many
    /// transfers run, and whether one of them writes over an existing file in place.
    fn running_question(&self, profiles: Option<&[ProfileId]>) -> (usize, bool) {
        self.running_rows(profiles)
            .fold((0, false), |(n, in_place), r| {
                (n + 1, in_place || r.in_place)
            })
    }

    /// macOS: the app menu's Quit keeps `Cmd+Q` only while no transfer runs; while some do,
    /// the key goes to the app's Quit, which asks first. Runs after every message (transfers
    /// start and end only through messages), touching the menu only when that flips.
    pub(crate) fn sync_quit_key(&mut self) {
        let free = !crate::chrome::quit_key_wanted(self.running_total());
        if free != self.quit_key_free && crate::chrome::set_quit_key(!free) {
            self.quit_key_free = free;
            tracing::info!(free, "Cmd+Q taken from the app menu while transfers run");
        }
    }

    fn running_total(&self) -> usize {
        self.running_rows(None).count()
    }

    /// The profiles with transfers running that closing `panes` would leave without any
    /// pane anywhere (their queue could no longer be seen).
    fn ended_by(&self, panes: &[PaneId]) -> Vec<ProfileId> {
        let all: Vec<(PaneId, ProfileId)> = self
            .windows
            .values()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| &t.panes)
            .map(|p| (p.id, p.profile))
            .collect();
        let mut ended: Vec<ProfileId> = Vec::new();
        for &(id, profile) in &all {
            if !panes.contains(&id) || ended.contains(&profile) {
                continue;
            }
            let kept = all
                .iter()
                .any(|(other, p)| *p == profile && !panes.contains(other));
            if !kept && self.running_of(&[profile]) > 0 {
                ended.push(profile);
            }
        }
        ended
    }

    /// The panes of `wid` (all its tabs).
    fn window_panes(&self, wid: window::Id) -> Vec<PaneId> {
        self.windows
            .get(&wid)
            .map(|w| w.tabs.iter().flat_map(|t| &t.panes).map(|p| p.id).collect())
            .unwrap_or_default()
    }

    /// Whether closing `panes` (as `then`) must ask first; asks over `wid` when it must.
    pub(crate) fn ask_before_closing(
        &mut self,
        wid: window::Id,
        panes: &[PaneId],
        then: Closing,
    ) -> bool {
        let (count, in_place) = self.running_question(Some(&self.ended_by(panes)));
        if count == 0 {
            return false;
        }
        if !self.prompts.has_transfers(wid, then) {
            self.prompts.push(
                wid,
                None,
                Dialog::Transfers {
                    count,
                    in_place,
                    then,
                },
            );
        }
        true
    }

    /// What a close confirmation for `then` over `wid` says now ([`App::running_question`]).
    fn running_for(&self, wid: window::Id, then: Closing) -> (usize, bool) {
        let panes = match then {
            Closing::Quit => return self.running_question(None),
            Closing::Window => self.window_panes(wid),
            Closing::Pane(pane) => vec![pane],
            Closing::Tab(tab) => self
                .windows
                .get(&wid)
                .and_then(|w| w.tabs.iter().find(|t| t.id == tab))
                .map(|t| t.panes.iter().map(|p| p.id).collect())
                .unwrap_or_default(),
        };
        self.running_question(Some(&self.ended_by(&panes)))
    }

    /// A transfer ended or began writing in place: the close confirmations say how many still
    /// run (and whether one writes in place), and go once none does (nothing is closed by
    /// itself: the user asks again).
    pub(super) fn refresh_transfer_questions(&mut self) {
        for (wid, then) in self.prompts.transfer_questions() {
            let (count, in_place) = self.running_for(wid, then);
            self.prompts.set_transfers(wid, then, count, in_place);
        }
    }

    /// Whether closing `wid` must ask first; asks when it must.
    pub(crate) fn ask_before_closing_window(&mut self, wid: window::Id) -> bool {
        let panes = self.window_panes(wid);
        self.ask_before_closing(wid, &panes, Closing::Window)
    }

    /// Quit, asking first while transfers run.
    pub(crate) fn quit_asking(&mut self) -> Task<Message> {
        if self.quitting {
            return Task::none();
        }
        let (count, in_place) = self.running_question(None);
        if count == 0 {
            return self.quit();
        }
        if self.prompts.asks_quit() || self.file_panels.quit_when_stopped {
            return Task::none();
        }
        match self.focused_window() {
            Some(wid) => {
                self.prompts.push(
                    wid,
                    None,
                    Dialog::Transfers {
                        count,
                        in_place,
                        then: Closing::Quit,
                    },
                );
                // The question may be in a window behind others or minimised (Quit pressed in
                // another window, or with the app in the background): bring it forward.
                window::gain_focus(wid)
            }
            // Nowhere to ask: what runs is cancelled, as the user is leaving.
            None => self.stop_transfers_and_close(None, Closing::Quit),
        }
    }

    /// Cancel the running transfers of `profiles` (every profile with `None`).
    fn cancel_transfers(&mut self, profiles: Option<&[ProfileId]>) {
        for (profile, panel) in &mut self.file_panels.by_profile {
            if profiles.is_some_and(|ps| !ps.contains(profile)) {
                continue;
            }
            // A row waiting for its session has nothing to stop: it just goes.
            let waiting = std::mem::take(&mut panel.waiting);
            panel.transfers.retain(|r| !waiting.contains(&r.id));
            for row in panel.transfers.iter().filter(|r| r.is_running()) {
                if let Some(cancel) = &row.cancel {
                    cancel();
                }
            }
        }
    }

    /// `Cancel transfers and close`: the transfers closing ends are cancelled, then it
    /// closes (a quit once they have cleaned up, or after a bounded wait).
    pub(crate) fn stop_transfers_and_close(
        &mut self,
        wid: Option<window::Id>,
        then: Closing,
    ) -> Task<Message> {
        match (then, wid) {
            (Closing::Quit, _) => {
                self.cancel_transfers(None);
                if self.running_total() == 0 {
                    return self.quit();
                }
                self.file_panels.quit_when_stopped = true;
                Task::perform(async { tokio::time::sleep(QUIT_WAIT).await }, |()| {
                    Message::FilePanel(FilePanelMsg::QuitWaited)
                })
            }
            (Closing::Tab(tab), Some(wid)) => {
                let Some(index) = self.windows.get(&wid).and_then(|w| w.find(tab)) else {
                    return Task::none();
                };
                let panes: Vec<PaneId> = self.windows[&wid].tabs[index]
                    .panes
                    .iter()
                    .map(|p| p.id)
                    .collect();
                let ended = self.ended_by(&panes);
                self.cancel_transfers(Some(&ended));
                self.close_tab(wid, index)
            }
            (Closing::Pane(pane), Some(wid)) => {
                let ended = self.ended_by(&[pane]);
                self.cancel_transfers(Some(&ended));
                self.close_pane(wid, pane)
            }
            (Closing::Window, Some(wid)) => {
                let ended = self.ended_by(&self.window_panes(wid));
                self.cancel_transfers(Some(&ended));
                self.close_window(wid)
            }
            (_, None) => Task::none(),
        }
    }

    /// A quit is waiting for cancelled transfers: quit once none runs.
    pub(super) fn quit_if_stopped(&mut self, waited: bool) -> Task<Message> {
        if !self.file_panels.quit_when_stopped {
            return Task::none();
        }
        if waited || self.running_total() == 0 {
            self.file_panels.quit_when_stopped = false;
            return self.quit();
        }
        Task::none()
    }
}
