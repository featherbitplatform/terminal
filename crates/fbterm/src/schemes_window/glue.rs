//! The app side of the Color schemes window: opening it, and the actions that touch files and
//! open sessions (save, duplicate, delete).

use fbterm_config::schemes::{delete_user, save_user};
use fbterm_config::{SettingChange, Settings};
use fbterm_term::TerminalOutput;
use iced::{Task, window};

use super::{DeleteAsk, SchemesMsg, SchemesState};
use crate::app::{App, Message, SettingsSource};
use crate::chrome::Chrome;

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
        let (id, open) = window::open(Chrome::current().window_settings(window::Settings {
            size: iced::Size::new(980.0, 780.0),
            min_size: Some(iced::Size::new(760.0, 520.0)),
            exit_on_close_request: false,
            ..window::Settings::default()
        }));
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
            // Task 6: import and export.
            SchemesMsg::Import
            | SchemesMsg::Imported(_)
            | SchemesMsg::ImportChoice(_)
            | SchemesMsg::Export
            | SchemesMsg::Exported(_)
            | SchemesMsg::Preview(_) => Task::none(),
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

    /// Delete the scheme the question was about: its file, then the profiles that used it
    /// follow the global scheme (saved to `profiles.toml`), and a global setting that named it
    /// goes back to Graphite (saved to `settings.toml`). Open panes recolor once at the end.
    fn delete_scheme(&mut self) -> Task<Message> {
        let dir = self.paths.schemes();
        let refused = self.profiles_error.is_some();
        let Some((_, state)) = &mut self.schemes_win else {
            return Task::none();
        };
        let Some(ask) = state.confirm.take() else {
            return Task::none();
        };
        state.notice = None;
        if refused {
            // The profiles in memory are a stand-in: which of the real ones use it is unknown.
            state.error =
                Some("profiles.toml failed to load: fix it before deleting a scheme".into());
            return Task::none();
        }
        if let Err(e) = delete_user(&dir, &ask.id) {
            tracing::warn!(error = %e, "scheme not deleted");
            state.error = Some(format!("Could not delete: {e}"));
            return Task::none();
        }
        tracing::info!(id = %ask.id, "scheme deleted");
        state.error = None;
        let next = if self.settings.color_scheme == ask.id {
            FALLBACK.to_owned()
        } else {
            self.settings.color_scheme.clone()
        };
        state.remove(&ask.id, &next);
        state.notice = Some(format!("Deleted \"{}\"", ask.name));
        let mut errors = Vec::new();
        if let Err(e) = self.forget_scheme_in_profiles(&ask.id) {
            errors.push(format!("Could not update profiles.toml: {e}"));
        }
        let mut tasks = Vec::new();
        if self.settings.color_scheme == ask.id {
            let change = SettingChange::ColorScheme(FALLBACK.into());
            let path = self.paths.settings();
            match Settings::write_field(&path, &change) {
                Ok(()) => self.settings_seen = std::fs::read_to_string(&path).ok(),
                Err(e) => errors.push(format!("Could not update settings.toml: {e}")),
            }
            let mut next = self.settings.clone();
            change.apply(&mut next);
            tasks.push(self.apply_settings(next, SettingsSource::Window));
            if let Some((_, s)) = &mut self.settings_win {
                s.values.color_scheme = FALLBACK.into();
            }
        }
        if !errors.is_empty()
            && let Some((_, state)) = &mut self.schemes_win
        {
            state.notice = None;
            state.error = Some(errors.join("; "));
        }
        tasks.push(self.reload_schemes());
        Task::batch(tasks)
    }

    /// Profiles that use `id` follow the global scheme from now on: saved to `profiles.toml`
    /// like an editor save, and drafts in the profile editor follow.
    fn forget_scheme_in_profiles(&mut self, id: &str) -> Result<(), fbterm_config::ConfigError> {
        if let Some((_, ed)) = &mut self.editor {
            ed.forget_scheme(id);
        }
        let uses = |p: &fbterm_config::Profile| p.color_scheme.as_deref() == Some(id);
        if !self.profiles.profiles.iter().any(uses) {
            return Ok(());
        }
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
        Ok(())
    }
}
