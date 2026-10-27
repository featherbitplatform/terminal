//! The app side of the profile editor window and the connection dialogs.

use std::path::{Path, PathBuf};

use fbterm_config::{AuthMethod, ProfileId, ProfileKind};
use iced::{Task, window};

use crate::app::{App, Delivered, Message, load_profiles};
use crate::chrome::Chrome;
use crate::editor::{EditorMsg, EditorState, SaveError, SecretResults, run_secret_ops};
use crate::file_panel::ClashAnswer;
use crate::prompts::{
    Ask, Clash, Dialog, PromptEvent, PromptId, PromptMsg, Rename, key_file_title,
};
use crate::saved_secrets::Kind;
use crate::tunnels::NO_WINDOW;

impl App {
    pub fn editor_window(&self) -> Option<window::Id> {
        self.editor.as_ref().map(|(id, _)| *id)
    }

    pub(crate) fn editor_msg(&mut self, msg: EditorMsg) -> Task<Message> {
        match msg {
            EditorMsg::Open => self.open_editor(),
            EditorMsg::Close => match self.editor_window() {
                Some(id) => self.close_window(id),
                None => Task::none(),
            },
            EditorMsg::Save => self.save_profiles(true),
            EditorMsg::SaveNoAsk => self.save_profiles(false),
            EditorMsg::EditSchemes => {
                let current = self
                    .editor
                    .as_ref()
                    .and_then(|(_, ed)| ed.selected_draft())
                    .and_then(|d| d.color_scheme.clone());
                self.open_schemes(current)
            }
            EditorMsg::Export => self.open_archive(crate::archive_window::ArchiveMode::Export),
            EditorMsg::Import => self.open_archive(crate::archive_window::ArchiveMode::Import),
            EditorMsg::Reload => {
                self.reload_profiles(true);
                Task::none()
            }
            EditorMsg::PickKey => {
                let start = directories::BaseDirs::new().map(|d| d.home_dir().join(".ssh"));
                Task::perform(
                    async move {
                        let mut dialog =
                            rfd::AsyncFileDialog::new().set_title("Choose a private key");
                        if let Some(dir) = start.filter(|d| d.is_dir()) {
                            dialog = dialog.set_directory(dir);
                        }
                        dialog.pick_file().await.map(|f| f.path().to_path_buf())
                    },
                    |path| Message::Editor(EditorMsg::KeyPicked(path)),
                )
            }
            other => {
                if let Some((_, ed)) = &mut self.editor {
                    ed.update(other);
                }
                Task::none()
            }
        }
    }

    pub(crate) fn open_editor(&mut self) -> Task<Message> {
        if let Some(id) = self.editor_window() {
            return window::gain_focus(id);
        }
        let (id, open) = window::open(Chrome::current().window_settings(window::Settings {
            size: iced::Size::new(920.0, 720.0),
            min_size: Some(iced::Size::new(640.0, 420.0)),
            exit_on_close_request: false,
            ..window::Settings::default()
        }));
        let state = EditorState::new(
            &self.profiles,
            self.secrets.clone(),
            self.profiles_error.clone(),
        );
        self.editor = Some((id, state));
        open.discard()
    }

    /// Validate, write `profiles.toml` atomically and use the new profiles everywhere, then
    /// make the secret changes on the blocking pool (Save waits meanwhile). With `ask`, a
    /// vault that must be unlocked or made first is asked for before anything is written.
    fn save_profiles(&mut self, ask: bool) -> Task<Message> {
        let path = self.paths.profiles();
        let refused = self.profiles_error.is_some();
        let need = self.backend.needs_main_password();
        let dialog_open = self.vault.dialog_open();
        let Some((wid, ed)) = &mut self.editor else {
            return Task::none();
        };
        let wid = *wid;
        if ed.saving.is_some() {
            return Task::none();
        }
        if refused {
            ed.notice =
                Some("profiles.toml failed to load: fix it and reload before saving".into());
            return Task::none();
        }
        if ask
            && let Some(need) = need
            && ed.needs_vault(&self.profiles, need)
        {
            // Another main password dialog is open (a connect's): finish it first, nothing
            // is written meanwhile and the drafts stay.
            if let Some(text) = dialog_open {
                ed.notice = Some(text.into());
                return Task::none();
            }
            return self.ask_main_password(
                wid,
                need,
                Message::Editor(EditorMsg::SaveNoAsk),
                Some(Message::Editor(EditorMsg::SaveNoAsk)),
            );
        }
        let ops = match ed.write(&mut self.profiles, &path) {
            Ok(ops) => ops,
            Err(SaveError::Invalid(errors)) => {
                tracing::info!(problems = errors.len(), "profiles not saved: invalid");
                ed.notice = None;
                return Task::none();
            }
            Err(SaveError::Write(e)) => {
                ed.notice = Some(format!("Could not save: {e}"));
                return Task::none();
            }
            Err(SaveError::Secrets(_)) => unreachable!("write makes no secret changes"),
        };
        let nothing_else = ops.is_empty();
        if nothing_else {
            let _ = ed.finish(Vec::new());
            ed.notice = Some("Saved".into());
        }
        // Our own write is not an edit to pick up again.
        self.profiles_seen = std::fs::read_to_string(&path).ok();
        self.profiles_changed();
        if nothing_else {
            return Task::none();
        }
        self.editor_seq += 1;
        let seq = self.editor_seq;
        let Some((_, ed)) = &mut self.editor else {
            return Task::none();
        };
        ed.saving = Some(seq);
        ed.notice = Some("Saving\u{2026}".into());
        let secrets = ed.secret_store();
        self.run_blocking(
            move || Delivered::new(Ok(run_secret_ops(secrets.as_ref(), ops))),
            |e| Delivered::new(Err(e)),
            move |results| Message::EditorSecrets(seq, results),
        )
    }

    /// The secret changes of editor save `seq` ran.
    pub(crate) fn editor_secrets_done(
        &mut self,
        seq: u64,
        results: Delivered<Result<SecretResults, String>>,
    ) {
        let Some((_, ed)) = self
            .editor
            .as_mut()
            .filter(|(_, ed)| ed.saving == Some(seq))
        else {
            return;
        };
        ed.saving = None;
        ed.notice = Some(match results.take() {
            Some(Ok(results)) => match ed.finish(results) {
                Ok(()) => "Saved".into(),
                Err(SaveError::Secrets(failures)) => {
                    format!("Profiles saved, but {}", failures.join("; "))
                }
                Err(_) => "Saved".into(),
            },
            Some(Err(e)) => {
                ed.dirty = true;
                format!("Profiles saved, but the saved secrets could not be changed: {e}")
            }
            None => return,
        });
    }

    /// `profiles.toml` changed on disk (hand edit, or a tool). Changes that are only our own
    /// write (or a touch) leave the same text and are ignored.
    pub(crate) fn profiles_file_changed(&mut self) {
        let text = std::fs::read_to_string(self.paths.profiles()).ok();
        if text == self.profiles_seen {
            return;
        }
        self.reload_profiles(false);
    }

    /// Re-read `profiles.toml`. A file that does not parse changes nothing in memory: the
    /// previous profiles stay, the error is shown, and saving is refused until it loads. A file
    /// that parses replaces the profiles; open tabs keep their sessions (by profile id) and
    /// validation problems go to the banner. `explicit`: the editor's Reload button, which
    /// discards unsaved edits; a reload triggered by a file change keeps them.
    fn reload_profiles(&mut self, explicit: bool) {
        let path = self.paths.profiles();
        self.profiles_seen = std::fs::read_to_string(&path).ok();
        let mut found = Vec::new();
        let (store, error) = load_profiles(&self.paths, &mut found);
        let prefix = path.display().to_string();
        self.notices.retain(|n| !n.starts_with(&prefix));
        if !found.is_empty() {
            self.notices_dismissed = false;
        }
        self.notices.extend(found);
        match error {
            Some(e) => {
                tracing::warn!(error = %e, "profiles.toml not applied");
                if let Some((_, ed)) = &mut self.editor {
                    ed.read_only = Some(e.clone());
                    ed.notice = Some("Still not readable".into());
                }
                self.profiles_error = Some(e);
            }
            None => {
                self.profiles_error = None;
                self.profiles = store;
                if let Some((_, ed)) = &mut self.editor {
                    if explicit || !ed.dirty || ed.read_only.is_some() {
                        *ed = EditorState::new(&self.profiles, self.secrets.clone(), None);
                        ed.notice = Some("Reloaded".into());
                    } else {
                        ed.notice = Some(
                            "profiles.toml changed on disk: Reload to discard your edits and see it"
                                .into(),
                        );
                    }
                }
                tracing::info!(
                    profiles = self.profiles.profiles.len(),
                    "profiles.toml reloaded"
                );
                self.profiles_changed();
            }
        }
    }

    /// The profile store changed: open palettes list the new names and tunnels, panes take new
    /// names, accents and color schemes (they keep their sessions; they refer to profiles by id),
    /// and the panes whose scheme changed are recolored.
    pub(crate) fn profiles_changed(&mut self) {
        let before = self.pane_colors();
        for w in self.windows.values_mut() {
            for pane in w.tabs.iter_mut().flat_map(|t| &mut t.panes) {
                if let Some(p) = self.profiles.get(pane.profile) {
                    if pane.title == pane.name {
                        pane.title = p.name.clone();
                    }
                    pane.name = p.name.clone();
                    pane.accent = p.accent;
                    pane.accent_style = p.accent_style;
                    pane.scheme.clone_from(&p.color_scheme);
                }
            }
        }
        self.schemes_moved(&before);
        self.tunnels_profiles_changed();
        self.refresh_palettes();
        self.refresh_export_preview();
    }

    pub(crate) fn prompt_msg(&mut self, msg: PromptMsg) -> Task<Message> {
        match msg {
            PromptMsg::Event(delivered) => match delivered.take() {
                Some(PromptEvent::Ask {
                    window,
                    ask: Ask::UnlockVault { reply },
                    ..
                }) => self.connect_vault_question(window, reply, Kind::Unlock),
                Some(PromptEvent::Ask {
                    window,
                    ask: Ask::SetVaultPassword { reply },
                    ..
                }) => self.connect_vault_question(window, reply, Kind::Set),
                Some(PromptEvent::StorageAutoSwitched) => {
                    self.storage_auto_switched();
                    Task::none()
                }
                Some(PromptEvent::Ask {
                    id,
                    window,
                    tunnel,
                    ask,
                }) => {
                    let target = window
                        .filter(|w| self.windows.contains_key(w))
                        .or_else(|| self.focused_window());
                    match (target, tunnel) {
                        (Some(w), tunnel) => {
                            if !self.prompts.ask(w, id, ask) {
                                return Task::none();
                            }
                            // A tunnel switched on from the Tunnels window asks over the
                            // terminal window used last, which may be behind it: bring that
                            // one forward.
                            let raise = if tunnel.is_some() && self.focused != Some(w) {
                                window::gain_focus(w)
                            } else {
                                Task::none()
                            };
                            Task::batch([raise, self.focus_prompt(w)])
                        }
                        // No terminal window to ask in: the tunnel cannot go on (dropping
                        // `ask` answers "no").
                        (None, Some(key)) => {
                            let key = self.tunnels.resolve(&key);
                            if self.tunnels.running(&key) {
                                self.tunnel_failed(&key, NO_WINDOW.into());
                            }
                            Task::none()
                        }
                        // Dropping `ask` answers "no".
                        (None, None) => Task::none(),
                    }
                }
                Some(PromptEvent::Withdraw(id)) => {
                    self.connect_unlock_withdrawn();
                    match self.prompts.withdraw(id) {
                        Some(w) => self.focus_prompt(w),
                        None => Task::none(),
                    }
                }
                Some(PromptEvent::KeyFileAccepted { profile, path }) => {
                    self.remember_key_file(profile, path);
                    Task::none()
                }
                Some(PromptEvent::PassphraseRemembered { profile }) => {
                    self.remember_passphrase_flag(profile);
                    Task::none()
                }
                Some(PromptEvent::SecretNotSaved { kind }) => {
                    self.secret_not_saved(kind);
                    Task::none()
                }
                None => Task::none(),
            },
            PromptMsg::Trust(w) => {
                self.prompts.trust(w);
                self.focus_prompt(w)
            }
            PromptMsg::Cancel(w) => {
                // In the name field, `Back` (or Escape) returns to the choice.
                if let Some(Dialog::Clash(clash)) = self.prompts.front_mut(w)
                    && clash.rename.is_some()
                {
                    clash.rename = None;
                    return Task::none();
                }
                // The clash dialog's `Cancel` (or Escape) skips that file and goes on.
                if let Some(Dialog::Clash(clash)) = self.prompts.front(w) {
                    let all = clash.apply_all && clash.offer_all;
                    self.prompts.pop(w);
                    let task = self.clash_answer(w, ClashAnswer::Cancel { all });
                    return Task::batch([task, self.focus_prompt(w)]);
                }
                self.prompts.cancel(w);
                self.focus_prompt(w)
            }
            PromptMsg::Submit(w) => {
                // Enter in the key file dialog is `Choose…`.
                if let Some(Dialog::KeyFile { .. }) = self.prompts.front(w) {
                    return self.prompt_msg(PromptMsg::ChooseKey(w));
                }
                if let Some(Dialog::Clash(clash)) = self.prompts.front(w) {
                    if clash.rename.is_some() {
                        return self.clash_rename_submit(w);
                    }
                    return Task::none();
                }
                self.prompts.submit(w);
                self.focus_prompt(w)
            }
            PromptMsg::ClashReplace(w) => {
                let Some(Dialog::Clash(clash)) = self.prompts.front(w) else {
                    return Task::none();
                };
                let all = clash.apply_all && clash.offer_all;
                self.prompts.pop(w);
                let task = self.clash_answer(w, ClashAnswer::Replace { all });
                Task::batch([task, self.focus_prompt(w)])
            }
            PromptMsg::ClashRename(w) => {
                let Some(Dialog::Clash(clash)) = self.prompts.front(w) else {
                    return Task::none();
                };
                // One new name is for one file: not with `Apply to all`.
                if clash.rename.is_some() || (clash.offer_all && clash.apply_all) {
                    return Task::none();
                }
                let value = self.clash_free_name(w, &clash.name.clone());
                if let Some(Dialog::Clash(clash)) = self.prompts.front_mut(w) {
                    clash.rename = Some(Rename {
                        value,
                        problem: None,
                        input: iced::widget::Id::unique(),
                    });
                }
                match self.prompts.front(w).and_then(Dialog::input) {
                    Some(id) => Task::batch([
                        iced::widget::operation::focus(id.clone()),
                        iced::widget::operation::select_all(id),
                    ]),
                    None => Task::none(),
                }
            }
            PromptMsg::ClashApplyAll(w, on) => {
                if let Some(Dialog::Clash(clash)) = self.prompts.front_mut(w) {
                    clash.apply_all = on;
                }
                Task::none()
            }
            PromptMsg::ClashName(w, typed) => {
                if let Some(Dialog::Clash(Clash {
                    rename: Some(r), ..
                })) = self.prompts.front_mut(w)
                {
                    r.value = typed;
                    r.problem = None;
                }
                Task::none()
            }
            PromptMsg::StopTransfers(w) => {
                let Some(Dialog::Transfers { then, .. }) = self.prompts.front(w) else {
                    return Task::none();
                };
                let then = *then;
                self.prompts.pop(w);
                let task = self.stop_transfers_and_close(Some(w), then);
                Task::batch([task, self.focus_prompt(w)])
            }
            PromptMsg::CloseTab(w) => {
                let tab = self.prompts.confirm_close(w);
                let index = tab.and_then(|id| self.windows.get(&w)?.find(id));
                // Transfers that closing would end are asked about too.
                let close = match index {
                    Some(i) => self.close_tab_if_no_transfers(w, i),
                    None => Task::none(),
                };
                Task::batch([close, self.focus_prompt(w)])
            }
            PromptMsg::Input(w, v) => {
                if let Some(Dialog::Secret { value, .. }) = self.prompts.front_mut(w) {
                    *value = v.0;
                }
                Task::none()
            }
            PromptMsg::Remember(w, b) => {
                if let Some(Dialog::Secret { remember, .. }) = self.prompts.front_mut(w) {
                    *remember = b;
                }
                Task::none()
            }
            PromptMsg::Copy(s) => iced::clipboard::write(s),
            PromptMsg::ChooseKey(w) => {
                let profile = match self.prompts.front(w) {
                    Some(Dialog::KeyFile { profile, .. }) => profile.clone(),
                    _ => return Task::none(),
                };
                match self.prompts.choose_key(w) {
                    Some(id) => key_file_picker(w, id, key_file_title(&profile)),
                    None => Task::none(),
                }
            }
            PromptMsg::KeyChosen(w, id, path) => {
                self.prompts.key_chosen(w, id, path);
                self.focus_prompt(w)
            }
        }
    }

    /// A connection logged in with the key file the user chose for key profile `id`, which
    /// named none: write it into the profile, so later connections use it without asking.
    /// Only an empty path is filled. When `profiles.toml` cannot be written, the path is kept
    /// in memory for this session only (see [`Self::change_key_auth`]).
    pub(crate) fn remember_key_file(&mut self, id: ProfileId, path: PathBuf) {
        let changed = self.change_key_auth(id, "chosen key file", |slot, _| {
            if slot.is_some() {
                return false;
            }
            *slot = Some(path.clone());
            true
        });
        if changed && let Some((_, ed)) = &mut self.editor {
            ed.key_file_remembered(id, &path);
        }
    }

    /// A connection logged in with key profile `id` and saved the passphrase the user typed
    /// for its key: mark the key as having a passphrase (when it was not), so a full export
    /// carries it and the editor removes it when the key stops needing one. Written like a
    /// remembered key file.
    pub(crate) fn remember_passphrase_flag(&mut self, id: ProfileId) {
        let changed = self.change_key_auth(id, "key passphrase flag", |_, flag| {
            if *flag {
                return false;
            }
            *flag = true;
            true
        });
        if changed && let Some((_, ed)) = &mut self.editor {
            ed.passphrase_remembered(id);
        }
    }

    /// Change key profile `id`'s key settings with `change` (its key path and passphrase
    /// flag; it returns whether it changed anything) and write `profiles.toml`. When the file
    /// cannot be written (it failed to load, it was edited on disk since we last read or
    /// wrote it, or the write fails), the change is kept in memory for this session only.
    /// Returns whether anything changed.
    fn change_key_auth(
        &mut self,
        id: ProfileId,
        what: &str,
        change: impl FnOnce(&mut Option<PathBuf>, &mut bool) -> bool,
    ) -> bool {
        let Some(profile) = self.profiles.profiles.iter_mut().find(|p| p.id == id) else {
            return false;
        };
        let ProfileKind::Ssh(ssh) = &mut profile.kind else {
            return false;
        };
        let AuthMethod::Key { path, passphrase } = &mut ssh.auth else {
            return false;
        };
        if !change(path, passphrase) {
            return false;
        }
        let file = self.paths.profiles();
        if self.profiles_error.is_some() {
            tracing::info!(what, "profiles.toml is not loaded: kept for this session");
        } else if std::fs::read_to_string(&file).ok() != self.profiles_seen {
            // Edited on disk and not reloaded yet: writing would overwrite that edit.
            tracing::info!(what, "profiles.toml changed on disk: kept for this session");
        } else {
            match self.profiles.save(&file) {
                // Our own write is not an edit to pick up again.
                Ok(()) => self.profiles_seen = std::fs::read_to_string(&file).ok(),
                Err(e) => {
                    tracing::warn!(error = %e, what, "kept for this session only");
                }
            }
        }
        self.profiles_changed();
        true
    }

    /// Focus whatever input `w` now shows on top: the dialog's field, else the palette's.
    pub(crate) fn focus_prompt(&self, w: window::Id) -> Task<Message> {
        match self.prompts.front(w) {
            Some(d) => d
                .input()
                .map_or_else(Task::none, iced::widget::operation::focus),
            None => match self.palettes.get(&w) {
                Some(p) => iced::widget::operation::focus(p.input.clone()),
                None => Task::none(),
            },
        }
    }

    /// Explain a refused, changed host key over the window of the pane that dialled.
    pub(crate) fn host_key_changed(
        &mut self,
        pane: crate::layout::PaneId,
        host: String,
        port: u16,
    ) -> Task<Message> {
        let Some(w) = self
            .windows
            .iter()
            .find_map(|(id, w)| w.locate(pane).map(|_| *id))
        else {
            return Task::none();
        };
        // Panes sharing one refused dial explain it once.
        if !self.prompts.has_changed(w, &host, port) {
            self.prompts.push(w, None, Dialog::Changed { host, port });
        }
        Task::none()
    }
}

/// Where the key file picker opens: `~/.ssh`, else the home folder.
fn key_file_start(home: Option<&Path>) -> Option<PathBuf> {
    let home = home?;
    let ssh = home.join(".ssh");
    Some(if ssh.is_dir() {
        ssh
    } else {
        home.to_path_buf()
    })
}

/// The system's file picker for key file question `id`, over window `w` (a sheet on macOS),
/// opening in `~/.ssh`. Its answer comes back as [`PromptMsg::KeyChosen`].
fn key_file_picker(w: window::Id, id: PromptId, title: String) -> Task<Message> {
    let home = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf());
    let start = key_file_start(home.as_deref());
    window::run(w, move |handle| {
        let dialog = rfd::AsyncFileDialog::new().set_title(title);
        match start {
            Some(dir) => dialog.set_directory(dir),
            None => dialog,
        }
        .set_parent(handle)
    })
    .then(move |dialog| {
        Task::perform(
            async move { dialog.pick_file().await.map(|f| f.path().to_path_buf()) },
            move |path| Message::Prompt(PromptMsg::KeyChosen(w, id, path)),
        )
    })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use fbterm_config::{AuthMethod, Profile, ProfileId, ProfileKind, ProfileStore, SshProfile};

    use super::key_file_start;
    use crate::app::{App, Delivered, Message};
    use crate::editor::{DraftKind, EditorMsg};
    use crate::prompts::{PromptEvent, PromptMsg};
    use crate::restore_tests::paths_with;

    fn keyless(name: &str) -> Profile {
        Profile {
            id: ProfileId::new(),
            name: name.into(),
            group: None,
            accent: None,
            accent_style: Default::default(),
            color_scheme: None,
            kind: ProfileKind::Ssh(SshProfile {
                host: "127.0.0.1".into(),
                port: 1,
                user: "user".into(),
                auth: AuthMethod::Key {
                    path: None,
                    passphrase: false,
                },
                jump: vec![],
                tunnels: vec![],
            }),
        }
    }

    fn key_path(store: &ProfileStore, id: ProfileId) -> Option<PathBuf> {
        match &store.get(id).unwrap().kind {
            ProfileKind::Ssh(SshProfile {
                auth: AuthMethod::Key { path, .. },
                ..
            }) => path.clone(),
            _ => panic!("key profile"),
        }
    }

    fn accepted(app: &mut App, profile: ProfileId, path: &Path) {
        let _ = app.update(Message::Prompt(PromptMsg::Event(Delivered::new(
            PromptEvent::KeyFileAccepted {
                profile,
                path: path.to_path_buf(),
            },
        ))));
    }

    #[test]
    fn accepted_key_file_is_remembered_in_profiles_toml() {
        let p = keyless("db");
        let paths = paths_with(std::slice::from_ref(&p));
        let (mut app, _) = App::build(paths.clone(), true);
        let _ = app.update(Message::Editor(EditorMsg::Open));
        let chosen = PathBuf::from("/keys/id_ed25519");
        accepted(&mut app, p.id, &chosen);

        assert_eq!(key_path(&app.profiles, p.id), Some(chosen.clone()));
        let on_disk = ProfileStore::load(&paths.profiles()).unwrap();
        assert_eq!(key_path(&on_disk, p.id), Some(chosen.clone()));
        // The editor shows it, with nothing left to save.
        let ed = &app.editor.as_ref().unwrap().1;
        let DraftKind::Ssh(d) = &ed.draft(p.id).unwrap().kind else {
            panic!("ssh")
        };
        assert_eq!(d.key_path, "/keys/id_ed25519");
        assert!(!ed.dirty);
        // Our own write is not picked up as a hand edit.
        assert_eq!(
            app.profiles_seen,
            std::fs::read_to_string(paths.profiles()).ok()
        );

        // A path the profile has meanwhile is never replaced.
        accepted(&mut app, p.id, Path::new("/keys/other"));
        assert_eq!(key_path(&app.profiles, p.id), Some(chosen));
    }

    #[test]
    fn read_only_profiles_keep_the_key_file_for_this_session_only() {
        let p = keyless("db");
        let paths = paths_with(std::slice::from_ref(&p));
        let (mut app, _) = App::build(paths.clone(), true);
        // profiles.toml no longer loads: the app works on the last good profiles and never
        // writes the file.
        let broken = "[[profiles]\nname = 1\n";
        std::fs::write(paths.profiles(), broken).unwrap();
        let _ = app.update(Message::ConfigChanged(crate::watch::Changed {
            profiles: true,
            settings: false,
            schemes: false,
        }));
        assert!(app.profiles_error.is_some());
        let chosen = PathBuf::from("/keys/id_ed25519");
        accepted(&mut app, p.id, &chosen);
        assert_eq!(key_path(&app.profiles, p.id), Some(chosen));
        assert_eq!(std::fs::read_to_string(paths.profiles()).unwrap(), broken);
    }

    #[cfg(unix)]
    #[test]
    fn unwritable_profiles_keep_the_key_file_for_this_session_only() {
        use std::os::unix::fs::PermissionsExt;
        let p = keyless("db");
        let paths = paths_with(std::slice::from_ref(&p));
        let (mut app, _) = App::build(paths.clone(), true);
        let before = std::fs::read_to_string(paths.profiles()).unwrap();
        let dir = paths.config_dir.clone();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let chosen = PathBuf::from("/keys/id_ed25519");
        accepted(&mut app, p.id, &chosen);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(key_path(&app.profiles, p.id), Some(chosen));
        assert_eq!(std::fs::read_to_string(paths.profiles()).unwrap(), before);
    }

    #[test]
    fn a_hand_edit_racing_the_key_file_is_kept() {
        let p = keyless("db");
        let paths = paths_with(std::slice::from_ref(&p));
        let (mut app, _) = App::build(paths.clone(), true);
        // profiles.toml was edited by hand and the watcher has not reported it yet.
        let edited = format!(
            "# edited by hand\n{}",
            std::fs::read_to_string(paths.profiles()).unwrap()
        );
        std::fs::write(paths.profiles(), &edited).unwrap();
        let chosen = PathBuf::from("/keys/id_ed25519");
        accepted(&mut app, p.id, &chosen);
        // The path is used for this session; the hand edit is not overwritten.
        assert_eq!(key_path(&app.profiles, p.id), Some(chosen));
        assert_eq!(std::fs::read_to_string(paths.profiles()).unwrap(), edited);
    }

    fn passphrase_flag(store: &ProfileStore, id: ProfileId) -> bool {
        match &store.get(id).unwrap().kind {
            ProfileKind::Ssh(SshProfile {
                auth: AuthMethod::Key { passphrase, .. },
                ..
            }) => *passphrase,
            _ => panic!("key profile"),
        }
    }

    fn passphrase_remembered(app: &mut App, profile: ProfileId) {
        let _ = app.update(Message::Prompt(PromptMsg::Event(Delivered::new(
            PromptEvent::PassphraseRemembered { profile },
        ))));
    }

    #[test]
    fn a_remembered_passphrase_marks_the_key_as_having_one() {
        let mut p = keyless("db");
        if let ProfileKind::Ssh(ssh) = &mut p.kind {
            ssh.auth = AuthMethod::Key {
                path: Some(PathBuf::from("/keys/id_ed25519")),
                passphrase: false,
            };
        }
        let paths = paths_with(std::slice::from_ref(&p));
        let (mut app, _) = App::build(paths.clone(), true);
        let _ = app.update(Message::Editor(EditorMsg::Open));
        passphrase_remembered(&mut app, p.id);
        assert!(passphrase_flag(&app.profiles, p.id));
        let on_disk = ProfileStore::load(&paths.profiles()).unwrap();
        assert!(passphrase_flag(&on_disk, p.id));
        assert_eq!(
            key_path(&on_disk, p.id),
            Some(PathBuf::from("/keys/id_ed25519"))
        );
        // The editor shows it, with nothing left to save, and a later save keeps it.
        let ed = &app.editor.as_ref().unwrap().1;
        let DraftKind::Ssh(d) = &ed.draft(p.id).unwrap().kind else {
            panic!("ssh")
        };
        assert!(d.key_passphrase);
        assert!(!ed.dirty);
        assert_eq!(
            app.profiles_seen,
            std::fs::read_to_string(paths.profiles()).ok()
        );
    }

    #[test]
    fn read_only_profiles_keep_the_passphrase_flag_for_this_session_only() {
        let p = keyless("db");
        let paths = paths_with(std::slice::from_ref(&p));
        let (mut app, _) = App::build(paths.clone(), true);
        // A hand edit not picked up yet is never overwritten.
        let edited = format!(
            "# edited by hand\n{}",
            std::fs::read_to_string(paths.profiles()).unwrap()
        );
        std::fs::write(paths.profiles(), &edited).unwrap();
        passphrase_remembered(&mut app, p.id);
        assert!(passphrase_flag(&app.profiles, p.id));
        assert_eq!(std::fs::read_to_string(paths.profiles()).unwrap(), edited);
    }

    #[test]
    fn enter_in_the_key_file_dialog_chooses() {
        use crate::prompts::Dialog;
        let p = keyless("db");
        let paths = paths_with(std::slice::from_ref(&p));
        let (mut app, _) = App::build(paths, true);
        let w = iced::window::Id::unique();
        app.prompts.push(
            w,
            Some(7),
            Dialog::KeyFile {
                profile: "db".into(),
                choosing: false,
                reply: None,
            },
        );
        let _ = app.update(Message::Prompt(PromptMsg::Submit(w)));
        assert!(
            matches!(
                app.prompts.front(w),
                Some(Dialog::KeyFile { choosing: true, .. })
            ),
            "Enter opens the picker, like Choose"
        );
    }

    #[test]
    fn key_file_picker_starts_in_dot_ssh_or_home() {
        let home = std::env::temp_dir().join(format!("fbterm-home-{}", ProfileId::new()));
        std::fs::create_dir_all(&home).unwrap();
        assert_eq!(key_file_start(Some(&home)), Some(home.clone()));
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        assert_eq!(key_file_start(Some(&home)), Some(home.join(".ssh")));
        assert_eq!(key_file_start(None), None);
    }
}
