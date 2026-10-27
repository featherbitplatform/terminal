//! The app side of the profile editor window and the connection dialogs.

use std::path::{Path, PathBuf};

use fbterm_config::{AuthMethod, ProfileId, ProfileKind};
use iced::{Task, window};

use crate::app::{App, Message, load_profiles};
use crate::chrome::Chrome;
use crate::editor::{EditorMsg, EditorState, SaveError};
use crate::file_panel::ClashAnswer;
use crate::prompts::{Clash, Dialog, PromptEvent, PromptId, PromptMsg, Rename, key_file_title};
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
            EditorMsg::Save => {
                self.save_profiles();
                Task::none()
            }
            EditorMsg::EditSchemes => {
                let current = self
                    .editor
                    .as_ref()
                    .and_then(|(_, ed)| ed.selected_draft())
                    .and_then(|d| d.color_scheme.clone());
                self.open_schemes(current)
            }
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

    /// Validate, write `profiles.toml` atomically, then change the keychain and use the new
    /// profiles everywhere.
    fn save_profiles(&mut self) {
        let path = self.paths.profiles();
        let refused = self.profiles_error.is_some();
        let Some((_, ed)) = &mut self.editor else {
            return;
        };
        if refused {
            ed.notice =
                Some("profiles.toml failed to load: fix it and reload before saving".into());
            return;
        }
        let saved = match ed.save(&mut self.profiles, &path) {
            Ok(()) => {
                ed.notice = Some("Saved".into());
                true
            }
            Err(SaveError::Invalid(errors)) => {
                tracing::info!(problems = errors.len(), "profiles not saved: invalid");
                ed.notice = None;
                false
            }
            Err(SaveError::Write(e)) => {
                ed.notice = Some(format!("Could not save: {e}"));
                false
            }
            // The file was written; only some keychain changes failed.
            Err(SaveError::Secrets(failures)) => {
                ed.notice = Some(format!("Profiles saved, but {}", failures.join("; ")));
                true
            }
        };
        if saved {
            // Our own write is not an edit to pick up again.
            self.profiles_seen = std::fs::read_to_string(&path).ok();
            self.profiles_changed();
        }
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
    }

    pub(crate) fn prompt_msg(&mut self, msg: PromptMsg) -> Task<Message> {
        match msg {
            PromptMsg::Event(delivered) => match delivered.take() {
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
                Some(PromptEvent::Withdraw(id)) => match self.prompts.withdraw(id) {
                    Some(w) => self.focus_prompt(w),
                    None => Task::none(),
                },
                Some(PromptEvent::KeyFileAccepted { profile, path }) => {
                    self.remember_key_file(profile, path);
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
    /// Only an empty path is filled. When `profiles.toml` cannot be written (it failed to load,
    /// or the write fails), the path is kept in memory for this session only.
    pub(crate) fn remember_key_file(&mut self, id: ProfileId, path: PathBuf) {
        let Some(profile) = self.profiles.profiles.iter_mut().find(|p| p.id == id) else {
            return;
        };
        let ProfileKind::Ssh(ssh) = &mut profile.kind else {
            return;
        };
        let AuthMethod::Key { path: slot, .. } = &mut ssh.auth else {
            return;
        };
        if slot.is_some() {
            return;
        }
        *slot = Some(path.clone());
        if self.profiles_error.is_none() {
            let file = self.paths.profiles();
            match self.profiles.save(&file) {
                // Our own write is not an edit to pick up again.
                Ok(()) => self.profiles_seen = std::fs::read_to_string(&file).ok(),
                Err(e) => {
                    tracing::warn!(error = %e, "chosen key file kept for this session only");
                }
            }
        } else {
            tracing::info!("profiles.toml is not loaded: chosen key file kept for this session");
        }
        if let Some((_, ed)) = &mut self.editor {
            ed.key_file_remembered(id, &path);
        }
        self.profiles_changed();
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
    fn key_file_picker_starts_in_dot_ssh_or_home() {
        let home = std::env::temp_dir().join(format!("fbterm-home-{}", ProfileId::new()));
        std::fs::create_dir_all(&home).unwrap();
        assert_eq!(key_file_start(Some(&home)), Some(home.clone()));
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        assert_eq!(key_file_start(Some(&home)), Some(home.join(".ssh")));
        assert_eq!(key_file_start(None), None);
    }
}
