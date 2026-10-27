//! The app side of the archive window: opening it, the file dialogs, background work and the
//! writes of an import.

use std::path::{Path, PathBuf};

use fbterm_config::schemes;
use fbterm_secrets::archive::ArchiveKind;
use fbterm_secrets::{MainPassword, MemoryStore, SecretEntry};
use iced::{Task, window};
use zeroize::Zeroizing;

use super::{
    ArchiveMode, ArchiveMsg, ArchiveState, Busy, EDITOR_DIRTY, EDITOR_SAVING, EXTENSION, ExportJob,
    FILTER_LABEL, ImportSummary, NO_FILE, NO_IMPORT_PASSWORD, NOTHING_TO_IMPORT, Notice, Opened,
    SCHEMES_STALE, anything_to_import, default_file_name, default_key_dir, ensure_key_dir,
    exported_text, imported_text, run_export, run_open, set_row_choice, shown_dir, write_key_files,
    written_before,
};
use crate::app::{App, Delivered, Message};
use crate::archive_import::{self, Choice};
use crate::editor::EditorState;

/// No key folder to put key files in (no home folder and none chosen).
const NO_KEY_DIR: &str = "Choose a folder for the key files.";

impl App {
    pub fn archive_window(&self) -> Option<window::Id> {
        self.archive_win.as_ref().map(|(id, _)| *id)
    }

    /// Open the archive window in `mode`, or bring it to the front in that mode.
    pub(crate) fn open_archive(&mut self, mode: ArchiveMode) -> Task<Message> {
        if let Some((id, state)) = &mut self.archive_win {
            state.mode = mode;
            return window::gain_focus(*id);
        }
        let (id, open) = crate::chrome::open_window(window::Settings {
            size: iced::Size::new(720.0, 700.0),
            min_size: Some(iced::Size::new(560.0, 420.0)),
            exit_on_close_request: false,
            ..window::Settings::default()
        });
        self.archive_win = Some((id, ArchiveState::new(mode)));
        self.refresh_export_preview();
        open.discard()
    }

    /// The window's title for its mode.
    pub(crate) fn archive_title(&self) -> &'static str {
        match self.archive_win.as_ref().map(|(_, s)| s.mode) {
            Some(ArchiveMode::Import) => "Import profiles \u{b7} fbterm",
            _ => "Export profiles \u{b7} fbterm",
        }
    }

    /// A number for a dialog or background job, never reused (not even by a new window).
    fn next_job(&mut self) -> u64 {
        self.archive_seq += 1;
        self.archive_seq
    }

    /// Work out again what the export would leave out, for the warnings list: the archive
    /// built now without saved secrets (nothing is read from the secret store).
    pub(crate) fn refresh_export_preview(&mut self) {
        let Some((_, state)) = &mut self.archive_win else {
            return;
        };
        let selected = state.selected(&self.profiles);
        let form = &mut state.export;
        form.preview = if selected.is_empty() {
            Ok(Vec::new())
        } else {
            crate::archive_build::build(
                &self.profiles,
                &selected,
                form.kind,
                form.include_keys && form.kind == ArchiveKind::Full,
                &MemoryStore::new(),
                &self.schemes,
            )
            .map(|(_, warnings)| warnings)
        };
    }

    /// Run `job` on the blocking pool and deliver its result as `done(result)` (at once when
    /// `blocking_inline`, for tests). `fallback` stands in for the result if the job panics.
    fn blocking<T: Send + 'static>(
        &mut self,
        job: impl FnOnce() -> T + Send + 'static,
        fallback: fn(String) -> T,
        done: impl FnOnce(T) -> ArchiveMsg + Send + 'static,
    ) -> Task<Message> {
        if self.blocking_inline {
            let msg = done(job());
            return self.archive_msg(msg);
        }
        Task::perform(
            async move {
                tokio::task::spawn_blocking(job)
                    .await
                    .unwrap_or_else(|e| fallback(e.to_string()))
            },
            move |result| Message::Archive(done(result)),
        )
    }

    pub(crate) fn archive_msg(&mut self, msg: ArchiveMsg) -> Task<Message> {
        let Some((wid, state)) = &mut self.archive_win else {
            // Answers and results for a window that has closed are dropped.
            return Task::none();
        };
        let wid = *wid;
        match msg {
            ArchiveMsg::Close => return self.close_window(wid),
            ArchiveMsg::Mode(mode) => state.mode = mode,

            ArchiveMsg::Toggle(id, on) => {
                if on {
                    state.export.unchecked.remove(&id);
                } else {
                    state.export.unchecked.insert(id);
                }
                state.export.problem = None;
                self.refresh_export_preview();
            }
            ArchiveMsg::Kind(kind) => {
                state.export.kind = kind;
                state.export.problem = None;
                self.refresh_export_preview();
            }
            ArchiveMsg::IncludeKeys(on) => {
                state.export.include_keys = on;
                self.refresh_export_preview();
            }
            ArchiveMsg::Password(typed) => {
                state.export.password = Zeroizing::new(typed.0);
                state.export.problem = None;
            }
            ArchiveMsg::Confirm(typed) => {
                state.export.confirm = Zeroizing::new(typed.0);
                state.export.problem = None;
            }
            ArchiveMsg::Export => {
                if state.busy.is_some() || state.dialog.is_some() {
                    return Task::none();
                }
                if let Some(problem) = state.export_problem(&self.profiles) {
                    state.export.problem = Some(problem);
                    return Task::none();
                }
                // A full archive reads saved secrets: open a locked vault first (this asks
                // even after a connect's unlock was cancelled: the user asked for it).
                if state.export.kind == ArchiveKind::Full
                    && self.backend.needs_main_password() == Some(MainPassword::Unlock)
                {
                    if let Some(text) = self.vault.dialog_open() {
                        state.export.problem = Some(text.into());
                        return Task::none();
                    }
                    return self.ask_main_password(
                        wid,
                        MainPassword::Unlock,
                        Message::Archive(ArchiveMsg::Export),
                        None,
                    );
                }
                let seq = self.next_job();
                if let Some((_, state)) = &mut self.archive_win {
                    state.dialog = Some(seq);
                    state.export.notice = None;
                }
                let name = default_file_name(chrono::Local::now().date_naive());
                return save_dialog(wid, name, seq);
            }
            ArchiveMsg::SaveChosen(seq, path) => {
                if state.dialog != Some(seq) {
                    return Task::none();
                }
                state.dialog = None;
                let Some(path) = path else {
                    return Task::none();
                };
                // The form may have changed while the dialog was open.
                if let Some(problem) = state.export_problem(&self.profiles) {
                    state.export.problem = Some(problem);
                    return Task::none();
                }
                let job = ExportJob {
                    store: self.profiles.clone(),
                    selected: state.selected(&self.profiles),
                    kind: state.export.kind,
                    include_keys: state.export.include_keys,
                    secrets: self.secrets.clone(),
                    schemes: self.schemes.clone(),
                    password: std::mem::take(&mut state.export.password),
                    path,
                };
                state.export.confirm = Zeroizing::default();
                let seq = self.next_job();
                if let Some((_, state)) = &mut self.archive_win {
                    state.busy = Some((seq, Busy::Sealing));
                }
                return self.blocking(
                    move || run_export(job),
                    |e| Err(format!("Could not export: {e}")),
                    move |r| ArchiveMsg::Exported(seq, r),
                );
            }
            ArchiveMsg::Exported(seq, result) => {
                if state.busy.map(|(s, _)| s) != Some(seq) {
                    return Task::none();
                }
                state.busy = None;
                state.export.notice = Some(match result {
                    Ok(summary) => {
                        tracing::info!(profiles = summary.profiles, "exported profiles");
                        let text = exported_text(&summary);
                        state.export.preview = Ok(summary.warnings);
                        Notice::done(text)
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "profile export failed");
                        Notice::error(e)
                    }
                });
            }

            ArchiveMsg::ChooseFile => {
                if state.busy.is_some() || state.dialog.is_some() {
                    return Task::none();
                }
                let seq = self.next_job();
                if let Some((_, state)) = &mut self.archive_win {
                    state.dialog = Some(seq);
                }
                return open_dialog(wid, seq);
            }
            ArchiveMsg::FileChosen(seq, path) => {
                if state.dialog != Some(seq) {
                    return Task::none();
                }
                state.dialog = None;
                if let Some(path) = path {
                    let form = &mut state.import;
                    form.file = Some(path);
                    form.opened = None;
                    form.problem = None;
                    form.notice = None;
                    return iced::widget::operation::focus(state.import_password_input.clone());
                }
            }
            ArchiveMsg::ImportPassword(typed) => {
                state.import.password = Zeroizing::new(typed.0);
                state.import.problem = None;
            }
            ArchiveMsg::Open => {
                if state.busy.is_some() {
                    return Task::none();
                }
                let form = &mut state.import;
                let Some(path) = form.file.clone() else {
                    form.problem = Some(NO_FILE.into());
                    return Task::none();
                };
                if form.password.is_empty() {
                    form.problem = Some(NO_IMPORT_PASSWORD.into());
                    return Task::none();
                }
                let password = std::mem::take(&mut form.password);
                form.opened = None;
                form.problem = None;
                form.notice = None;
                let seq = self.next_job();
                if let Some((_, state)) = &mut self.archive_win {
                    state.busy = Some((seq, Busy::Opening));
                }
                return self.blocking(
                    move || Delivered::new(run_open(&path, &password)),
                    |e| Delivered::new(Err(format!("Could not open the archive: {e}"))),
                    move |d| ArchiveMsg::Opened(seq, d),
                );
            }
            ArchiveMsg::Opened(seq, delivered) => {
                if state.busy.map(|(s, _)| s) != Some(seq) {
                    return Task::none();
                }
                state.busy = None;
                match delivered.take() {
                    Some(Ok(archive)) => {
                        match archive_import::plan(&self.profiles, &archive, &self.schemes) {
                            Ok(plan) => state.import.opened = Some(Opened { archive, plan }),
                            Err(e) => state.import.problem = Some(e),
                        }
                    }
                    Some(Err(e)) => state.import.problem = Some(e),
                    None => {}
                }
            }
            ArchiveMsg::Choice(i, choice) => {
                if let Some(opened) = &mut state.import.opened {
                    set_row_choice(&mut opened.plan, i, choice);
                    state.import.problem = None;
                }
            }
            ArchiveMsg::ChooseKeyFolder => {
                if state.busy.is_some() || state.dialog.is_some() {
                    return Task::none();
                }
                let start = state.import.key_dir.clone().or_else(default_key_dir);
                let seq = self.next_job();
                if let Some((_, state)) = &mut self.archive_win {
                    state.dialog = Some(seq);
                }
                return folder_dialog(wid, seq, start);
            }
            ArchiveMsg::KeyFolderChosen(seq, path) => {
                if state.dialog != Some(seq) {
                    return Task::none();
                }
                state.dialog = None;
                if let Some(path) = path {
                    state.import.key_dir = Some(path);
                    state.import.problem = None;
                }
            }
            ArchiveMsg::Import => return self.import_opened(),
            ArchiveMsg::Probed(seq) => {
                if state.busy.map(|(s, _)| s) != Some(seq) {
                    return Task::none();
                }
                state.busy = None;
                // A move to the vault is written and said now, before the import goes on.
                self.storage_auto_switched();
                return self.import_opened();
            }
            ArchiveMsg::SecretsStored(seq, result) => {
                if state.busy.map(|(s, _)| s) != Some(seq) {
                    return Task::none();
                }
                state.busy = None;
                let Some((mut summary, schemes_written)) = state.import.storing.take() else {
                    return Task::none();
                };
                match result {
                    Ok(n) => summary.secrets = n,
                    Err(e) => {
                        tracing::warn!(error = %e, "imported secrets were not stored");
                        summary
                            .secret_failures
                            .push(format!("The saved secrets could not be stored ({e})."));
                    }
                }
                return self.import_done(&summary, schemes_written);
            }
        }
        Task::none()
    }

    /// `Import`: apply the preview, then write key files, color schemes and `profiles.toml`,
    /// in that order, then store the saved secrets in one batch on the blocking pool. A vault
    /// that must be unlocked or made first is asked for before anything is written.
    fn import_opened(&mut self) -> Task<Message> {
        let editor = self
            .editor
            .as_ref()
            .map(|(_, ed)| (ed.dirty, ed.saving.is_some()));
        let probe = self.backend.probe_pending();
        let Some((wid, state)) = &mut self.archive_win else {
            return Task::none();
        };
        let wid = *wid;
        if state.busy.is_some() {
            return Task::none();
        }
        // Automatic storage has not checked the system storage yet: check now, off the UI
        // thread, so the main password question below is the one the secrets will meet (a
        // move to the vault found while storing them would come after profiles.toml).
        if probe
            && state
                .import
                .opened
                .as_ref()
                .is_some_and(|o| o.plan.has_secrets)
        {
            let seq = self.next_job();
            if let Some((_, state)) = &mut self.archive_win {
                state.busy = Some((seq, Busy::Checking));
            }
            let backend = self.backend.clone();
            return self.blocking(
                move || backend.probe(),
                |_| (),
                move |()| ArchiveMsg::Probed(seq),
            );
        }
        let need = self.backend.needs_main_password();
        let Some((_, state)) = &mut self.archive_win else {
            return Task::none();
        };
        let Some(opened) = state.import.opened.take() else {
            return Task::none();
        };
        let key_dir = state.import.key_dir.clone().or_else(default_key_dir);
        let result = self.write_import(&opened, key_dir.as_deref(), editor, need);
        let Some((_, state)) = &mut self.archive_win else {
            return Task::none();
        };
        match result {
            Ok(Written::Ask(need)) => {
                state.import.opened = Some(opened);
                if let Some(text) = self.vault.dialog_open() {
                    state.import.problem = Some(text.into());
                    return Task::none();
                }
                self.ask_main_password(wid, need, Message::Archive(ArchiveMsg::Import), None)
            }
            Ok(Written::Files {
                summary,
                schemes_written,
                secrets,
            }) => {
                state.import.problem = None;
                // `opened` (the decrypted archive) is dropped here, wiping its secrets.
                drop(opened);
                if secrets.is_empty() {
                    return self.import_done(&summary, schemes_written);
                }
                let seq = self.next_job();
                let Some((_, state)) = &mut self.archive_win else {
                    return Task::none();
                };
                state.busy = Some((seq, Busy::Storing));
                state.import.storing = Some((summary, schemes_written));
                let store = self.secrets.clone();
                self.blocking(
                    move || {
                        let n = secrets.len();
                        store
                            .set_many(&secrets)
                            .map(|()| n)
                            .map_err(|e| e.to_string())
                    },
                    Err,
                    move |r| ArchiveMsg::SecretsStored(seq, r),
                )
            }
            Err(e) => {
                state.import.problem = Some(e);
                state.import.opened = Some(opened);
                Task::none()
            }
        }
    }

    /// An import is complete: say what it wrote.
    fn import_done(&mut self, summary: &ImportSummary, schemes_written: bool) -> Task<Message> {
        tracing::info!(
            added = summary.added,
            replaced = summary.replaced,
            "imported profiles"
        );
        if let Some((_, state)) = &mut self.archive_win {
            state.import.notice = Some(Notice {
                error: !summary.secret_failures.is_empty(),
                text: imported_text(summary),
            });
        }
        if schemes_written {
            return self.reload_schemes();
        }
        Task::none()
    }

    /// The writes of an import but its saved secrets, which it returns to store. Nothing is
    /// written when the vault must be unlocked or made first (`need`).
    fn write_import(
        &mut self,
        opened: &Opened,
        key_dir: Option<&Path>,
        editor: Option<(bool, bool)>,
        need: Option<MainPassword>,
    ) -> Result<Written, String> {
        let plan = &opened.plan;
        if plan.has_keys && key_dir.is_none() {
            return Err(NO_KEY_DIR.into());
        }
        let key_dir = key_dir.map_or_else(PathBuf::new, Path::to_path_buf);
        // Refuses read-only profiles and a preview made against other profiles.
        let applied = archive_import::apply(
            plan,
            &opened.archive,
            &self.profiles,
            &key_dir,
            self.profiles_error.is_some(),
        )?;
        // The profile editor (unsaved changes, saving) would overwrite or lose the import.
        match editor {
            Some((true, _)) => return Err(EDITOR_DIRTY.into()),
            Some((_, true)) => return Err(EDITOR_SAVING.into()),
            _ => {}
        }
        if !anything_to_import(plan) {
            return Err(NOTHING_TO_IMPORT.into());
        }
        // A scheme saved since the preview under an id the import would write.
        let scheme_dir = self.paths.schemes();
        if applied.scheme_writes.iter().any(|s| {
            self.schemes.get(&s.id).is_some() || scheme_dir.join(format!("{}.toml", s.id)).exists()
        }) {
            return Err(SCHEMES_STALE.into());
        }
        if !applied.secret_ops.is_empty()
            && let Some(need) = need
        {
            return Ok(Written::Ask(need));
        }

        let mut summary = ImportSummary::default();
        for e in &plan.entries {
            if e.choice == Choice::Skip || e.error.is_some() {
                continue;
            }
            if e.matched.is_some() && e.choice == Choice::Overwrite {
                summary.replaced += 1;
            } else {
                summary.added += 1;
            }
        }

        // 1. Key files: never replacing anything.
        if !applied.key_writes.is_empty() {
            ensure_key_dir(&key_dir).map_err(|e| {
                format!(
                    "Could not make the key folder {} ({e}), so nothing was imported.",
                    shown_dir(&key_dir)
                )
            })?;
            summary.keys = write_key_files(&applied.key_writes)?;
        }

        // 2. Color schemes, through the scheme file rules.
        for s in &applied.scheme_writes {
            if let Err(e) = schemes::save_user(&scheme_dir, s) {
                return Err(match written_before(summary.keys, summary.schemes) {
                    Some(done) => format!(
                        "Wrote {done}, but the color scheme {} could not be saved ({}), so no profiles were imported.",
                        s.name, e.message
                    ),
                    None => format!(
                        "The color scheme {} could not be saved ({}), so nothing was imported.",
                        s.name, e.message
                    ),
                });
            }
            summary.schemes += 1;
        }

        // 3. profiles.toml, as the editor saves it.
        let file = self.paths.profiles();
        if let Err(e) = applied.store.save(&file) {
            return Err(match written_before(summary.keys, summary.schemes) {
                Some(done) => format!(
                    "Wrote {done}, but profiles.toml could not be saved ({}), so no profiles were imported.",
                    e.message
                ),
                None => format!(
                    "profiles.toml could not be saved ({}), so nothing was imported.",
                    e.message
                ),
            });
        }
        // Our own write is not an edit to pick up again.
        self.profiles_seen = std::fs::read_to_string(&file).ok();
        self.profiles = applied.store;
        if let Some((_, ed)) = &mut self.editor {
            *ed = EditorState::new(&self.profiles, self.secrets.clone(), None);
            ed.notice = Some("Imported".into());
        }
        self.profiles_changed();

        // 4. Saved secrets (only ever set): the caller stores them in one batch.
        let secrets: Vec<SecretEntry> = applied
            .secret_ops
            .into_iter()
            .map(|op| (op.profile, op.kind, op.value))
            .collect();
        let schemes_written = summary.schemes > 0;
        Ok(Written::Files {
            summary,
            schemes_written,
            secrets,
        })
    }
}

/// What [`App::write_import`] did.
enum Written {
    /// Nothing yet: the vault needs its main password (or a new one) first.
    Ask(MainPassword),
    /// Key files, schemes and profiles are written; `secrets` are still to store.
    Files {
        summary: ImportSummary,
        schemes_written: bool,
        secrets: Vec<SecretEntry>,
    },
}

/// The system's save dialog over the archive window, offering `name`.
fn save_dialog(wid: window::Id, name: String, seq: u64) -> Task<Message> {
    window::run(wid, move |w| {
        rfd::AsyncFileDialog::new()
            .set_title("Export profiles")
            .set_file_name(name)
            .add_filter(FILTER_LABEL, &[EXTENSION])
            .set_parent(w)
    })
    .then(move |dialog| {
        Task::perform(
            async move { dialog.save_file().await.map(|f| f.path().to_path_buf()) },
            move |path| Message::Archive(ArchiveMsg::SaveChosen(seq, path)),
        )
    })
}

/// The system's open dialog over the archive window, for `.fbprofiles` files.
fn open_dialog(wid: window::Id, seq: u64) -> Task<Message> {
    window::run(wid, move |w| {
        rfd::AsyncFileDialog::new()
            .set_title("Import profiles")
            .add_filter(FILTER_LABEL, &[EXTENSION])
            .set_parent(w)
    })
    .then(move |dialog| {
        Task::perform(
            async move { dialog.pick_file().await.map(|f| f.path().to_path_buf()) },
            move |path| Message::Archive(ArchiveMsg::FileChosen(seq, path)),
        )
    })
}

/// The system's folder picker over the archive window, for the key files.
fn folder_dialog(wid: window::Id, seq: u64, start: Option<PathBuf>) -> Task<Message> {
    window::run(wid, move |w| {
        let dialog = rfd::AsyncFileDialog::new().set_title("Folder for key files");
        match start.filter(|d| d.is_dir()) {
            Some(dir) => dialog.set_directory(dir),
            None => dialog,
        }
        .set_parent(w)
    })
    .then(move |dialog| {
        Task::perform(
            async move { dialog.pick_folder().await.map(|f| f.path().to_path_buf()) },
            move |path| Message::Archive(ArchiveMsg::KeyFolderChosen(seq, path)),
        )
    })
}
