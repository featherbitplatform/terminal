//! Duplicating a profile: the editor's `Duplicate` button and the `Duplicate profile` action.
//!
//! The copy is the saved profile (never an unsaved draft: with unsaved changes it is refused)
//! with a new id and the first free name of `{name} copy`, `{name} copy 2`, ... It goes right
//! after the original in `profiles.toml`, written like an editor save, and opens selected in
//! the editor with its Name field focused. The saved secrets its login uses (password, key
//! passphrase) are copied too, in one batch on the blocking pool; a locked vault is asked for
//! first, and when that is cancelled the copy is made without them and the notice says so.

use fbterm_config::{AuthMethod, Profile, ProfileId, ProfileKind};
use fbterm_secrets::{MainPassword, SecretError, SecretKind, SecretStore, Zeroizing};
use iced::{Task, window};

use super::{EditorMsg, JobKind, SecretJob};
use crate::app::{App, Message};

pub const DUPLICATED: &str = "Duplicated";
pub const DUPLICATING: &str = "Duplicating\u{2026}";
pub const DIRTY: &str = "Unsaved changes: save or discard them before duplicating.";
pub const SAVING: &str = "Still saving: wait for it to finish before duplicating.";
pub const READ_ONLY: &str = "profiles.toml failed to load: fix it and reload before duplicating";
pub const LOCKED: &str = "Duplicated without its saved secrets: they are locked.";
pub const GONE: &str = "The profile no longer exists.";
pub const NO_PROFILE: &str = "Select a profile, then choose Duplicate.";

/// The kinds of saved secret `p`'s login uses (and a copy of it takes along).
pub fn secret_kinds(p: &Profile) -> Vec<SecretKind> {
    match &p.kind {
        ProfileKind::Ssh(s) => match s.auth {
            AuthMethod::Password => vec![SecretKind::Password],
            AuthMethod::Key {
                passphrase: true, ..
            } => vec![SecretKind::KeyPassphrase],
            AuthMethod::Key { .. } | AuthMethod::Agent => Vec::new(),
        },
        ProfileKind::Local(_) => Vec::new(),
    }
}

/// Give profile `to` the saved secrets of `kinds` that `from` has, in one batch (one vault
/// write). Returns how many were copied. May reach the vault: never on the UI thread.
pub fn copy_secrets(
    secrets: &dyn SecretStore,
    from: ProfileId,
    to: ProfileId,
    kinds: &[SecretKind],
) -> Result<usize, SecretError> {
    let mut entries = Vec::new();
    for &kind in kinds {
        if let Some(value) = secrets.get(&from, kind)? {
            entries.push((to, kind, Zeroizing::new(value)));
        }
    }
    if !entries.is_empty() {
        secrets.set_many(&entries)?;
    }
    Ok(entries.len())
}

impl App {
    /// The `Duplicate profile` action. In the profile editor (or with no terminal window) it
    /// copies the selected profile; from a terminal window only the focused pane's profile,
    /// opening (or raising) the editor on the copy. A pane whose profile is not saved copies
    /// nothing: the editor says why, and its selection is never copied instead.
    pub(crate) fn duplicate_action(&mut self, from: Option<window::Id>) -> Task<Message> {
        let in_editor = from.is_some() && from == self.editor_window();
        let terminal = self.focused_window().and_then(|w| self.windows.get(&w));
        let target = match terminal {
            Some(w) if !in_editor => w
                .active_tab()
                .map(|t| t.focused_pane().profile)
                .filter(|id| self.profiles.get(*id).is_some()),
            _ => self.editor.as_ref().and_then(|(_, ed)| ed.selected),
        };
        let open = self.open_editor();
        let Some(id) = target else {
            if let Some((_, ed)) = &mut self.editor {
                ed.notice = Some(NO_PROFILE.into());
            }
            return open;
        };
        open.chain(self.duplicate_profile(id, true))
    }

    /// Duplicate saved profile `id` (see the module). With `ask`, a locked vault is asked for
    /// before anything is written when the profile has secrets to copy.
    pub(crate) fn duplicate_profile(&mut self, id: ProfileId, ask: bool) -> Task<Message> {
        let path = self.paths.profiles();
        let refused = self.profiles_error.is_some();
        let need = self.backend.needs_main_password();
        let dialog_open = self.vault.dialog_open();
        let busy = self.editor_job.is_some();
        let Some((wid, ed)) = &mut self.editor else {
            return Task::none();
        };
        let wid = *wid;
        let refusal = if busy {
            Some(SAVING)
        } else if refused || ed.read_only.is_some() {
            Some(READ_ONLY)
        } else if ed.dirty {
            Some(DIRTY)
        } else {
            None
        };
        if let Some(text) = refusal {
            ed.notice = Some(text.into());
            return Task::none();
        }
        let Some(original) = self.profiles.get(id) else {
            // Deleted or reloaded away meanwhile (while the main password was asked).
            ed.notice = Some(GONE.into());
            return Task::none();
        };
        let kinds = secret_kinds(original);
        let locked = !kinds.is_empty() && need == Some(MainPassword::Unlock);
        if locked && ask {
            // Another main password dialog is open (a connect's): finish it first.
            if let Some(text) = dialog_open {
                ed.notice = Some(text.into());
                return Task::none();
            }
            let then = Message::Editor(EditorMsg::DuplicateNoAsk(id));
            return self.ask_main_password(wid, MainPassword::Unlock, then.clone(), Some(then));
        }

        let mut store = self.profiles.clone();
        let Some(copy) = store.duplicate(id) else {
            return Task::none();
        };
        if let Err(e) = store.save(&path) {
            ed.notice = Some(format!("Could not save: {e}"));
            return Task::none();
        }
        self.profiles = store;
        // Our own write is not an edit to pick up again.
        self.profiles_seen = std::fs::read_to_string(&path).ok();
        tracing::info!("profile duplicated");
        let copied = self.profiles.get(copy).cloned();
        if let (Some((_, ed)), Some(profile)) = (&mut self.editor, &copied) {
            ed.insert_copy(id, profile);
        }
        self.profiles_changed();
        let Some((_, ed)) = &mut self.editor else {
            return Task::none();
        };
        let focus = Task::batch([
            window::gain_focus(wid),
            iced::widget::operation::focus(ed.name_input.clone()),
        ]);
        // No vault yet (nothing saved in it) or nothing the login uses: nothing to copy.
        if kinds.is_empty() || need == Some(MainPassword::Set) {
            ed.notice = Some(DUPLICATED.into());
            return focus;
        }
        if locked {
            ed.notice = Some(LOCKED.into());
            return focus;
        }
        self.editor_seq += 1;
        let seq = self.editor_seq;
        // Save waits for the copy, so a quick delete of it cannot race the secrets.
        self.editor_job = Some(SecretJob {
            seq,
            kind: JobKind::Duplicate,
            editor: ed.instance(),
        });
        ed.notice = Some(DUPLICATING.into());
        let secrets = ed.secret_store();
        let job = self.run_blocking(
            move || copy_secrets(secrets.as_ref(), id, copy, &kinds).map_err(|e| e.to_string()),
            Err,
            move |result| Message::EditorDuplicated(seq, result),
        );
        Task::batch([focus, job])
    }

    /// The saved secrets of duplicate `seq` were copied (or not).
    pub(crate) fn editor_duplicated(
        &mut self,
        seq: u64,
        result: Result<usize, String>,
    ) -> Task<Message> {
        let Some(job) = self.take_editor_job(seq, JobKind::Duplicate) else {
            return Task::none();
        };
        let failure = result.err().map(|e| {
            tracing::warn!(error = %e, "a duplicate's saved secrets were not copied");
            format!("Duplicated, but its saved secrets could not be copied: {e}")
        });
        match self
            .editor
            .as_mut()
            .filter(|(_, ed)| ed.instance() == job.editor)
        {
            Some((_, ed)) => ed.notice = Some(failure.unwrap_or_else(|| DUPLICATED.into())),
            None => self.editor_job_orphaned(failure),
        }
        self.editor_job_ended()
    }
}
