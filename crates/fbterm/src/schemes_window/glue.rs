//! The app side of the Color schemes window: opening it, and the actions that touch files and
//! open sessions (save, duplicate, delete, import, export).

use std::path::PathBuf;

use fbterm_config::schemes::{
    BUILTIN_IDS, Scheme, delete_user, is_plain_id, new_user_id, read_file, save_user, write_file,
};
use fbterm_config::{SettingChange, Settings};
use fbterm_term::TerminalOutput;
use iced::{Task, window};

use super::{DeleteAsk, ImportAsk, ImportChoice, SchemesMsg, SchemesState};
use crate::app::{App, Message, SettingsSource};

/// What the global setting falls back to when its scheme is deleted.
const FALLBACK: &str = "graphite";

impl App {
    pub fn schemes_window(&self) -> Option<window::Id> {
        self.schemes_win.as_ref().map(|(id, _)| *id)
    }

    /// Open the Color schemes window with `select` selected (else the global scheme), or bring
    /// it to the front and select `select` there.
    pub(crate) fn open_schemes(&mut self, select: Option<String>) -> Task<Message> {
        let select = select.unwrap_or_else(|| self.settings.color_scheme.clone());
        if let Some((id, state)) = &mut self.schemes_win {
            state.update(SchemesMsg::Select(select));
            return window::gain_focus(*id);
        }
        let (id, open) = crate::chrome::open_window(window::Settings {
            size: iced::Size::new(980.0, 780.0),
            min_size: Some(iced::Size::new(760.0, 520.0)),
            exit_on_close_request: false,
            ..window::Settings::default()
        });
        let state = SchemesState::new(
            &self.schemes,
            Some(&select),
            crate::settings_window::installed_monospace(),
        );
        self.schemes_win = Some((id, state));
        open.discard()
    }

    pub(crate) fn schemes_msg(&mut self, msg: SchemesMsg) -> Task<Message> {
        match msg {
            SchemesMsg::Open => self.open_schemes(None),
            SchemesMsg::Close => match self.schemes_window() {
                Some(id) => self.close_window(id),
                None => Task::none(),
            },
            SchemesMsg::Save => self.save_scheme(),
            SchemesMsg::Duplicate => self.duplicate_scheme(),
            SchemesMsg::Delete => {
                self.ask_delete_scheme();
                Task::none()
            }
            SchemesMsg::ConfirmDelete => self.delete_scheme(),
            SchemesMsg::Preview(TerminalOutput::Resized(size)) => {
                if let Some((_, state)) = &mut self.schemes_win {
                    state.resize_preview(size);
                }
                Task::none()
            }
            SchemesMsg::Import => self.pick_import(),
            SchemesMsg::Imported(path) => Self::read_import(path),
            SchemesMsg::ImportRead(result) => self.imported(result),
            SchemesMsg::ImportChoice(choice) => self.import_choice(choice),
            SchemesMsg::Export => self.pick_export(),
            SchemesMsg::Exported(path) => self.exported(path),
            SchemesMsg::Preview(_) => Task::none(),
            other => {
                if let Some((_, state)) = &mut self.schemes_win {
                    state.update(other);
                }
                Task::none()
            }
        }
    }

    /// Write the selected user scheme to `schemes/` and load the set again, which recolors the
    /// open panes that use it (and makes those whose scheme sets a font measure again). A
    /// scheme with no name is refused; low contrast never is.
    fn save_scheme(&mut self) -> Task<Message> {
        let dir = self.paths.schemes();
        let Some((_, state)) = &mut self.schemes_win else {
            return Task::none();
        };
        let Some(draft) = state.selected_draft().filter(|d| !d.builtin).cloned() else {
            return Task::none();
        };
        let name = draft.name.trim();
        if name.is_empty() {
            state.notice = None;
            state.error = Some("A scheme needs a name.".into());
            return Task::none();
        }
        let scheme = fbterm_config::schemes::Scheme {
            name: name.to_owned(),
            ..draft
        };
        match save_user(&dir, &scheme) {
            Ok(_) => {
                state.mark_saved(&scheme);
                state.renaming = false;
                state.error = None;
                state.notice = Some("Saved".into());
                tracing::info!(id = %scheme.id, "scheme saved");
                self.reload_schemes()
            }
            Err(e) => {
                tracing::warn!(error = %e, "scheme not saved");
                state.notice = None;
                state.error = Some(format!("Could not save: {e}"));
                Task::none()
            }
        }
    }

    /// Copy the selected scheme as a new user scheme, saved at once, and select it.
    fn duplicate_scheme(&mut self) -> Task<Message> {
        let dir = self.paths.schemes();
        let Some((_, state)) = &mut self.schemes_win else {
            return Task::none();
        };
        state.notice = None;
        let Some(copy) = state.duplicate() else {
            return Task::none();
        };
        match save_user(&dir, &copy) {
            Ok(_) => {
                state.mark_saved(&copy);
                state.error = None;
                self.reload_schemes()
            }
            Err(e) => {
                // The copy stays as an unsaved draft; Save tries again.
                tracing::warn!(error = %e, "scheme copy not saved");
                state.error = Some(format!("Could not save the copy: {e}"));
                Task::none()
            }
        }
    }

    /// Ask before deleting the selected user scheme, saying how many profiles use it.
    fn ask_delete_scheme(&mut self) {
        let Some((_, state)) = &mut self.schemes_win else {
            return;
        };
        let Some(d) = state.selected_draft().filter(|d| !d.builtin) else {
            return;
        };
        let profiles = self
            .profiles
            .profiles
            .iter()
            .filter(|p| p.color_scheme.as_deref() == Some(d.id.as_str()))
            .count();
        state.confirm = Some(DeleteAsk {
            id: d.id.clone(),
            name: d.name.clone(),
            profiles,
        });
    }

    /// Delete the scheme the question was about. References go first, the file last, and each
    /// step stops the delete when it fails, so what is on disk always agrees with itself: the
    /// profiles that used it follow the global scheme (saved to `profiles.toml`), then a global
    /// setting that named it goes back to Graphite (saved to `settings.toml`), then the file is
    /// removed. A failure leaves the scheme in place, at worst no longer used. Open panes
    /// recolor once at the end.
    fn delete_scheme(&mut self) -> Task<Message> {
        let refused = self.profiles_error.is_some();
        let Some((_, state)) = &mut self.schemes_win else {
            return Task::none();
        };
        let Some(ask) = state.confirm.take() else {
            return Task::none();
        };
        state.notice = None;
        state.error = None;
        if refused {
            // The profiles in memory are a stand-in: which of the real ones use it is unknown.
            state.error =
                Some("profiles.toml failed to load: fix it before deleting a scheme".into());
            return Task::none();
        }
        let mut tasks = Vec::new();
        let result = self
            .forget_scheme_in_profiles(&ask.id)
            .map_err(|e| format!("Could not update profiles.toml: {e}"))
            .and_then(|()| {
                self.forget_scheme_in_settings(&ask.id, &mut tasks)
                    .map_err(|e| format!("Could not update settings.toml: {e}"))
            })
            .and_then(|()| {
                delete_user(&self.paths.schemes(), &ask.id)
                    .map_err(|e| format!("Could not delete: {e}"))
            });
        let Some((_, state)) = &mut self.schemes_win else {
            return Task::batch(tasks);
        };
        match result {
            Ok(()) => {
                tracing::info!(id = %ask.id, "scheme deleted");
                state.remove(&ask.id, &self.settings.color_scheme);
                state.notice = Some(format!("Deleted \"{}\"", ask.name));
            }
            Err(e) => {
                tracing::warn!(error = %e, "scheme not deleted");
                state.error = Some(e);
            }
        }
        tasks.push(self.reload_schemes());
        Task::batch(tasks)
    }

    /// Profiles that use `id` follow the global scheme from now on, saved to `profiles.toml`
    /// like an editor save. Only once that is written do the profile editor's drafts follow.
    fn forget_scheme_in_profiles(&mut self, id: &str) -> Result<(), fbterm_config::ConfigError> {
        let uses = |p: &fbterm_config::Profile| p.color_scheme.as_deref() == Some(id);
        if self.profiles.profiles.iter().any(uses) {
            let mut store = self.profiles.clone();
            for p in store.profiles.iter_mut().filter(|p| uses(p)) {
                p.color_scheme = None;
            }
            let path = self.paths.profiles();
            store.save(&path)?;
            self.profiles = store;
            // Our own write is not an edit to pick up again.
            self.profiles_seen = std::fs::read_to_string(&path).ok();
            self.profiles_changed();
        }
        if let Some((_, ed)) = &mut self.editor {
            ed.forget_scheme(id);
        }
        Ok(())
    }

    /// A global setting that names `id` goes back to Graphite: written to `settings.toml`
    /// first, applied only once written.
    fn forget_scheme_in_settings(
        &mut self,
        id: &str,
        tasks: &mut Vec<Task<Message>>,
    ) -> Result<(), fbterm_config::ConfigError> {
        if self.settings.color_scheme != id {
            return Ok(());
        }
        let change = SettingChange::ColorScheme(FALLBACK.into());
        let path = self.paths.settings();
        Settings::write_field(&path, &change)?;
        self.settings_seen = std::fs::read_to_string(&path).ok();
        let mut next = self.settings.clone();
        change.apply(&mut next);
        tasks.push(self.apply_settings(next, SettingsSource::Window));
        if let Some((_, s)) = &mut self.settings_win {
            s.values.color_scheme = FALLBACK.into();
        }
        Ok(())
    }

    /// Open the Open dialog for a scheme file, parented to the window.
    fn pick_import(&mut self) -> Task<Message> {
        let Some(id) = self.schemes_window() else {
            return Task::none();
        };
        window::run(id, |w| {
            rfd::AsyncFileDialog::new()
                .set_title("Import color scheme")
                .add_filter("Color scheme (.toml)", &["toml"])
                .set_parent(w)
        })
        .then(|d| {
            Task::perform(
                async move { d.pick_file().await.map(|f| f.path().to_path_buf()) },
                |path| Message::Schemes(SchemesMsg::Imported(path)),
            )
        })
    }

    /// Open the Save dialog for the selected scheme, parented to the window.
    fn pick_export(&mut self) -> Task<Message> {
        let Some((id, state)) = &mut self.schemes_win else {
            return Task::none();
        };
        let Some(d) = state.selected_draft() else {
            return Task::none();
        };
        let name = export_file_name(&d.name);
        let id = *id;
        window::run(id, move |w| {
            rfd::AsyncFileDialog::new()
                .set_title("Export color scheme")
                .set_file_name(name)
                .add_filter("Color scheme (.toml)", &["toml"])
                .set_parent(w)
        })
        .then(|d| {
            Task::perform(
                async move { d.save_file().await.map(|f| f.path().to_path_buf()) },
                |path| Message::Schemes(SchemesMsg::Exported(path)),
            )
        })
    }

    /// Write the selected scheme (a built-in too) to the chosen path.
    fn exported(&mut self, path: Option<PathBuf>) -> Task<Message> {
        let Some(path) = path else {
            return Task::none();
        };
        let Some((_, state)) = &mut self.schemes_win else {
            return Task::none();
        };
        let Some(scheme) = state.selected_draft().cloned() else {
            return Task::none();
        };
        match write_file(&path, &scheme) {
            Ok(()) => {
                tracing::info!(id = %scheme.id, "scheme exported");
                state.error = None;
                state.notice = Some("Exported".into());
            }
            Err(e) => {
                tracing::warn!(error = %e, "scheme not exported");
                state.notice = None;
                state.error = Some(format!("Could not export: {e}"));
            }
        }
        Task::none()
    }

    /// Read the chosen file on the blocking pool.
    fn read_import(path: Option<PathBuf>) -> Task<Message> {
        let Some(path) = path else {
            return Task::none();
        };
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || read_file(&path).map_err(|e| e.to_string()))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
            },
            |r| Message::Schemes(SchemesMsg::ImportRead(r)),
        )
    }

    /// The file was read. One that does not read shows its error (with file and line). A
    /// built-in or unusable id gets a fresh one; an id that is already a user scheme asks.
    fn imported(&mut self, result: Result<Scheme, String>) -> Task<Message> {
        let dir = self.paths.schemes();
        let Some((_, state)) = &mut self.schemes_win else {
            return Task::none();
        };
        state.notice = None;
        state.import = None;
        let mut scheme = match result {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "scheme not imported");
                state.error = Some(format!("Could not import: {e}"));
                return Task::none();
            }
        };
        state.error = None;
        if BUILTIN_IDS.contains(&scheme.id.as_str()) || !is_plain_id(&scheme.id) {
            scheme.id = new_user_id();
        } else if let Some(existing) = self
            .schemes
            .get(&scheme.id)
            .map(|s| s.name.clone())
            .or_else(|| {
                // A file that did not load (so is not in the set) would be overwritten too.
                dir.join(format!("{}.toml", scheme.id))
                    .exists()
                    .then(|| scheme.id.clone())
            })
        {
            state.import = Some(ImportAsk { scheme, existing });
            return Task::none();
        }
        self.install_imported(scheme)
    }

    fn import_choice(&mut self, choice: ImportChoice) -> Task<Message> {
        let Some((_, state)) = &mut self.schemes_win else {
            return Task::none();
        };
        let Some(ask) = state.import.take() else {
            return Task::none();
        };
        let mut scheme = ask.scheme;
        match choice {
            ImportChoice::Cancel => return Task::none(),
            ImportChoice::Replace => {}
            ImportChoice::KeepBoth => {
                scheme.id = new_user_id();
                scheme.name = format!("{} (imported)", scheme.name.trim());
            }
        }
        self.install_imported(scheme)
    }

    /// Copy `scheme` into `schemes/`, load the set again (which recolors the panes that use a
    /// replaced scheme) and select it.
    fn install_imported(&mut self, scheme: Scheme) -> Task<Message> {
        let dir = self.paths.schemes();
        let Some((_, state)) = &mut self.schemes_win else {
            return Task::none();
        };
        if let Err(e) = save_user(&dir, &scheme) {
            tracing::warn!(error = %e, "imported scheme not saved");
            state.error = Some(format!("Could not import: {e}"));
            return Task::none();
        }
        tracing::info!(id = %scheme.id, "scheme imported");
        state.notice = Some(format!("Imported \"{}\"", scheme.name.trim()));
        // The file is the truth now: an open draft of the same id (with unsaved edits)
        // takes the imported version instead of keeping the stale edits.
        state.mark_saved(&scheme);
        let task = self.reload_schemes();
        if let Some((_, state)) = &mut self.schemes_win
            && state.draft(&scheme.id).is_some()
        {
            state.select(scheme.id);
        }
        task
    }
}

/// The suggested file name of an export: the scheme's name as plain characters.
fn export_file_name(name: &str) -> String {
    let stem: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{}.toml", if stem.is_empty() { "scheme" } else { &stem })
}
