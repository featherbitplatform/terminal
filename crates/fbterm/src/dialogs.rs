//! The app side of the profile editor window and the connection dialogs.

use iced::{Task, window};

use crate::app::{App, Message, load_profiles};
use crate::editor::{EditorMsg, EditorState, SaveError};
use crate::palette;
use crate::prompts::{Dialog, PromptEvent, PromptMsg};

impl App {
    pub fn editor_window(&self) -> Option<window::Id> {
        self.editor.as_ref().map(|(id, _)| *id)
    }

    pub(crate) fn editor_msg(&mut self, msg: EditorMsg) -> Task<Message> {
        match msg {
            EditorMsg::Open => self.open_editor(),
            EditorMsg::Close => match self.editor_window() {
                Some(id) => window::close(id),
                None => Task::none(),
            },
            EditorMsg::Save => {
                self.save_profiles();
                Task::none()
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
        let (id, open) = window::open(window::Settings {
            size: iced::Size::new(920.0, 720.0),
            min_size: Some(iced::Size::new(640.0, 420.0)),
            ..window::Settings::default()
        });
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

    /// The profile store changed: open palettes list the new names, tabs take new names and
    /// accents (they keep their sessions; they refer to profiles by id).
    fn profiles_changed(&mut self) {
        let items = palette::items(&self.profiles);
        for state in self.palettes.values_mut() {
            state.set_items(items.clone(), &self.recency);
        }
        for w in self.windows.values_mut() {
            for tab in &mut w.tabs {
                if let Some(p) = self.profiles.get(tab.profile) {
                    if tab.title == tab.name {
                        tab.title = p.name.clone();
                    }
                    tab.name = p.name.clone();
                    tab.accent = p.accent;
                }
            }
        }
    }

    pub(crate) fn prompt_msg(&mut self, msg: PromptMsg) -> Task<Message> {
        match msg {
            PromptMsg::Event(delivered) => match delivered.take() {
                Some(PromptEvent::Ask { id, window, ask }) => {
                    let target = window
                        .filter(|w| self.windows.contains_key(w))
                        .or_else(|| self.focused_window());
                    // No window to ask in: dropping `ask` answers "no".
                    match target {
                        Some(w) if self.prompts.ask(w, id, ask) => self.focus_prompt(w),
                        _ => Task::none(),
                    }
                }
                Some(PromptEvent::Withdraw(id)) => match self.prompts.withdraw(id) {
                    Some(w) => self.focus_prompt(w),
                    None => Task::none(),
                },
                None => Task::none(),
            },
            PromptMsg::Trust(w) => {
                self.prompts.trust(w);
                self.focus_prompt(w)
            }
            PromptMsg::Cancel(w) => {
                self.prompts.cancel(w);
                self.focus_prompt(w)
            }
            PromptMsg::Submit(w) => {
                self.prompts.submit(w);
                self.focus_prompt(w)
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
        }
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

    /// Explain a refused, changed host key over the window of the tab that dialled.
    pub(crate) fn host_key_changed(
        &mut self,
        tab: crate::tabs::TabId,
        host: String,
        port: u16,
    ) -> Task<Message> {
        let Some(w) = self
            .windows
            .iter()
            .find_map(|(id, w)| w.find(tab).map(|_| *id))
        else {
            return Task::none();
        };
        // Tabs sharing one refused dial explain it once.
        if !self.prompts.has_changed(w, &host, port) {
            self.prompts.push(w, None, Dialog::Changed { host, port });
        }
        Task::none()
    }
}
