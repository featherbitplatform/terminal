//! The app side of saved-secret storage: the main password dialogs, connects waiting to
//! unlock, the Settings choice and `Reset vault`, and the Auto check at startup.

use fbterm_config::{ProfileId, SecretStorage, SettingChange, Settings};
use fbterm_secrets::{MainPassword, SecretError, Storage, Zeroizing};
use iced::{Task, window};
use tokio::sync::oneshot;

use super::{
    AUTO_SWITCHED, Asked, DirectSwitch, Kind, Outcome, Purpose, SectionInfo, SectionNotice,
    VaultDialog, VaultMsg, moved_text,
};
use crate::app::{App, Message};

impl App {
    /// The storage saved secrets use now.
    pub(crate) fn storage(&self) -> Storage {
        self.backend.active()
    }

    /// What the Settings window's `Saved secrets` section shows.
    pub(crate) fn secrets_section(&self) -> SectionInfo {
        SectionInfo {
            choice: self.settings.secret_storage,
            vault_exists: self.backend.vault_exists(),
            busy: self.vault.section_busy(),
            notice: self.vault.notice.clone(),
            active: self.storage(),
        }
    }

    /// Auto found no system storage on the first secret needed and moved to the vault:
    /// write `"vault"` and say why, once.
    pub(crate) fn storage_auto_switched(&mut self) {
        if self.backend.take_auto_switched() {
            self.auto_switched();
        }
    }

    pub(crate) fn vault_msg(&mut self, msg: VaultMsg) -> Task<Message> {
        match msg {
            VaultMsg::Password(typed) => {
                if let Some(d) = self.vault.dialog.as_mut().filter(|d| d.busy.is_none()) {
                    d.password = Zeroizing::new(typed.0);
                    d.problem = None;
                }
                Task::none()
            }
            VaultMsg::Confirm(typed) => {
                if let Some(d) = self.vault.dialog.as_mut().filter(|d| d.busy.is_none()) {
                    d.confirm = Zeroizing::new(typed.0);
                    d.problem = None;
                }
                Task::none()
            }
            VaultMsg::Submit => self.vault_submit(),
            VaultMsg::Cancel => self.vault_cancel(),
            VaultMsg::Done(seq, outcome) => self.vault_done(seq, outcome),
            VaultMsg::DemoReady => {
                self.vault.demo_pending = false;
                Task::none()
            }
        }
    }

    /// The Auto check found no system storage: the vault from now on, said once, and
    /// `"vault"` written so the next launch starts there.
    fn auto_switched(&mut self) {
        self.write_secret_storage(SecretStorage::Vault);
        self.notices.push(AUTO_SWITCHED.into());
        self.notices_dismissed = false;
    }

    /// Set `secret_storage` in memory, in the Settings window and in `settings.toml`.
    fn write_secret_storage(&mut self, choice: SecretStorage) {
        self.settings.secret_storage = choice;
        if let Some((_, state)) = &mut self.settings_win {
            state.values.secret_storage = choice;
        }
        let path = self.paths.settings();
        match Settings::write_field(&path, &SettingChange::SecretStorage(choice)) {
            Ok(()) => self.settings_seen = std::fs::read_to_string(&path).ok(),
            Err(e) => {
                tracing::warn!(error = %e, "secret_storage not written");
                self.vault.notice = Some(SectionNotice {
                    error: true,
                    text: format!("settings.toml could not be written: {}", e.message),
                });
            }
        }
    }

    /// The ids of every profile: what a move or an export asks the storage about.
    fn profile_ids(&self) -> Vec<ProfileId> {
        self.profiles.profiles.iter().map(|p| p.id).collect()
    }

    /// Show a main password dialog over `window`. Only one shows at a time: while another
    /// is open, a request with a cancel path takes it at once, others are dropped.
    pub(crate) fn open_vault_dialog(
        &mut self,
        window: window::Id,
        kind: Kind,
        purpose: Purpose,
    ) -> Task<Message> {
        if self.vault.dialog.is_some() {
            if let Purpose::Then {
                on_cancel: Some(m), ..
            } = purpose
            {
                return self.update(*m);
            }
            return Task::none();
        }
        let dialog = VaultDialog::new(window, kind, purpose);
        let focus = dialog.input.clone();
        self.vault.dialog = Some(dialog);
        Task::batch([
            window::gain_focus(window),
            iced::widget::operation::focus(focus),
        ])
    }

    /// Ask for what the vault in use needs before `then` can save or read secrets; `None`
    /// when nothing is needed (the caller goes ahead itself).
    pub(crate) fn ask_main_password(
        &mut self,
        window: window::Id,
        need: MainPassword,
        then: Message,
        on_cancel: Option<Message>,
    ) -> Task<Message> {
        self.open_vault_dialog(
            window,
            need.into(),
            Purpose::Then {
                then: Box::new(then),
                on_cancel: on_cancel.map(Box::new),
            },
        )
    }

    /// Run a storage job on the blocking pool; its outcome comes back as `Done(seq, ..)`.
    fn vault_job(
        &mut self,
        seq: u64,
        job: impl FnOnce() -> Result<(), SecretError> + Send + 'static,
    ) -> Task<Message> {
        self.run_blocking(
            move || Outcome::from(job()),
            Outcome::Failed,
            move |o| Message::Vault(VaultMsg::Done(seq, o)),
        )
    }

    fn vault_submit(&mut self) -> Task<Message> {
        let ids = self.profile_ids();
        let Some(d) = &mut self.vault.dialog else {
            return Task::none();
        };
        if d.busy.is_some() {
            return Task::none();
        }
        if let Some(problem) = d.check() {
            d.problem = Some(problem.into());
            return Task::none();
        }
        let seq = self.vault.next_seq();
        let Some(d) = &mut self.vault.dialog else {
            return Task::none();
        };
        d.busy = Some(seq);
        d.problem = None;
        let backend = self.backend.clone();
        let password = d.password.clone();
        match (&d.purpose, d.kind) {
            (Purpose::Reset, _) => self.vault_job(seq, move || backend.reset_vault()),
            (Purpose::Switch(choice), _) => {
                let target = Storage::for_setting(*choice);
                self.vault_job(seq, move || {
                    backend.switch_to(target, Some(password.as_str()), &ids)
                })
            }
            (_, Kind::Set) => self.vault_job(seq, move || {
                fbterm_secrets::SecretStore::create_vault(backend.as_ref(), &password)
            }),
            (_, _) => self.vault_job(seq, move || {
                fbterm_secrets::SecretStore::unlock_vault(backend.as_ref(), &password)
            }),
        }
    }

    fn vault_cancel(&mut self) -> Task<Message> {
        if self.vault.dialog.as_ref().is_none_or(|d| d.busy.is_some()) {
            return Task::none();
        }
        let Some(d) = self.vault.dialog.take() else {
            return Task::none();
        };
        self.close_vault_dialog(d, false)
    }

    /// A dialog went: answer the connects waiting on it, then do what its purpose says on
    /// success (`done`) or on cancel.
    fn close_vault_dialog(&mut self, d: VaultDialog, done: bool) -> Task<Message> {
        let unlocked = self.backend.vault_unlocked();
        if matches!(d.purpose, Purpose::Connect) && !done {
            // Cancelled for a connect: connects stop asking this session.
            match d.kind {
                Kind::Set => self.vault.set_declined = true,
                _ => self.vault.declined = true,
            }
        }
        let typed = matches!(d.kind, Kind::Unlock | Kind::Set);
        let password = (unlocked && typed).then_some(&d.password);
        let fallback = unlocked.then(Zeroizing::default);
        self.vault.answer_waiters(password.or(fallback.as_ref()));
        match d.purpose {
            Purpose::Then { then, .. } if done => self.update(*then),
            Purpose::Then {
                on_cancel: Some(m), ..
            } if !done => self.update(*m),
            _ => Task::none(),
        }
    }

    fn vault_done(&mut self, seq: u64, outcome: Outcome) -> Task<Message> {
        if let Some(direct) = self.vault.direct.filter(|d| d.seq == seq) {
            self.vault.direct = None;
            self.switch_finished(direct.choice, &outcome);
            return Task::none();
        }
        let Some(d) = self.vault.dialog.as_mut().filter(|d| d.busy == Some(seq)) else {
            return Task::none();
        };
        d.busy = None;
        match &outcome {
            Outcome::WrongPassword => {
                d.problem = Some(SecretError::WrongMainPassword.to_string());
                d.password = Zeroizing::default();
                d.confirm = Zeroizing::default();
                return iced::widget::operation::focus(d.input.clone());
            }
            Outcome::Failed(m) if matches!(d.purpose, Purpose::Connect | Purpose::Then { .. }) => {
                d.problem = Some(m.clone());
                return Task::none();
            }
            _ => {}
        }
        let Some(d) = self.vault.dialog.take() else {
            return Task::none();
        };
        match d.purpose {
            Purpose::Switch(choice) => {
                self.switch_finished(choice, &outcome);
                self.close_vault_dialog(d, outcome == Outcome::Ok)
            }
            Purpose::Reset => {
                self.vault.notice = Some(match &outcome {
                    Outcome::Ok => SectionNotice {
                        error: false,
                        text: "The vault was reset. Profiles are kept.".into(),
                    },
                    Outcome::WrongPassword | Outcome::Failed(_) => SectionNotice {
                        error: true,
                        text: fail_text(&outcome),
                    },
                });
                self.close_vault_dialog(d, outcome == Outcome::Ok)
            }
            Purpose::Connect | Purpose::Then { .. } => self.close_vault_dialog(d, true),
        }
    }

    /// A move ended: on success the setting follows; either way the section says so.
    fn switch_finished(&mut self, choice: SecretStorage, outcome: &Outcome) {
        match outcome {
            Outcome::Ok => {
                self.vault.notice = Some(SectionNotice {
                    error: false,
                    text: moved_text(self.storage()),
                });
                self.write_secret_storage(choice);
            }
            _ => {
                self.vault.notice = Some(SectionNotice {
                    error: true,
                    text: format!("Nothing was moved. {}", fail_text(outcome)),
                });
            }
        }
    }

    /// The Settings choice changed: move saved secrets to it (asking for the main password
    /// when the vault needs one), or just write it when the storage stays the same.
    pub(crate) fn choose_secret_storage(&mut self, choice: SecretStorage) -> Task<Message> {
        let Some((wid, state)) = &self.settings_win else {
            return Task::none();
        };
        let wid = *wid;
        if state.read_only.is_some()
            || self.vault.section_busy()
            || self.vault.dialog.is_some()
            || choice == self.settings.secret_storage
        {
            return Task::none();
        }
        if self.profiles_error.is_some() {
            self.vault.notice = Some(SectionNotice {
                error: true,
                text: super::PROFILES_UNREADABLE.into(),
            });
            return Task::none();
        }
        self.vault.notice = None;
        let target = Storage::for_setting(choice);
        if target == self.storage() {
            self.write_secret_storage(choice);
            return Task::none();
        }
        let need = match target {
            // Into the vault: make it, or open the one there.
            Storage::Vault if !self.backend.vault_unlocked() => {
                Some(if self.backend.vault_exists() {
                    Kind::Unlock
                } else {
                    Kind::Set
                })
            }
            // Out of a locked vault: open it to read.
            Storage::System if !self.backend.vault_unlocked() && self.backend.vault_exists() => {
                Some(Kind::Unlock)
            }
            _ => None,
        };
        if let Some(kind) = need {
            return self.open_vault_dialog(wid, kind, Purpose::Switch(choice));
        }
        let seq = self.vault.next_seq();
        self.vault.direct = Some(DirectSwitch { seq, choice });
        let backend = self.backend.clone();
        let ids = self.profile_ids();
        self.vault_job(seq, move || backend.switch_to(target, None, &ids))
    }

    /// `Reset vault` in Settings: confirm first.
    pub(crate) fn ask_reset_vault(&mut self) -> Task<Message> {
        let Some((wid, _)) = &self.settings_win else {
            return Task::none();
        };
        let wid = *wid;
        if self.vault.section_busy() || !self.backend.vault_exists() {
            return Task::none();
        }
        self.vault.notice = None;
        self.open_vault_dialog(wid, Kind::Reset, Purpose::Reset)
    }

    /// A connect found the vault locked (`Kind::Unlock`) or missing when remembering a
    /// secret (`Kind::Set`) and asks the user (over `window`, else the focused terminal
    /// window).
    pub(crate) fn connect_vault_question(
        &mut self,
        window: Option<window::Id>,
        reply: oneshot::Sender<Option<Zeroizing<String>>>,
        kind: Kind,
    ) -> Task<Message> {
        let need = self.backend.needs_main_password();
        let needed = match kind {
            Kind::Set => need == Some(MainPassword::Set),
            _ => need == Some(MainPassword::Unlock),
        };
        match self.vault.ask(reply, kind, needed) {
            Asked::Answered | Asked::Joined => Task::none(),
            Asked::Show => {
                let target = window
                    .filter(|w| self.windows.contains_key(w))
                    .or_else(|| self.focused_window());
                match target {
                    Some(w) => self.open_vault_dialog(w, kind, Purpose::Connect),
                    None => {
                        self.vault.answer_waiters(None);
                        Task::none()
                    }
                }
            }
        }
    }

    /// A connect stopped waiting: a connect's dialog nobody waits for any more goes (without
    /// counting as a cancel).
    pub(crate) fn connect_unlock_withdrawn(&mut self) {
        let orphan = self
            .vault
            .dialog
            .as_ref()
            .is_some_and(|d| matches!(d.purpose, Purpose::Connect) && d.busy.is_none())
            && !self.vault.has_waiters();
        if orphan {
            self.vault.dialog = None;
        }
    }

    /// `window` closed: its dialog goes as if cancelled, unless a job runs for it (its
    /// result still applies, without the dialog).
    pub(crate) fn vault_window_closed(&mut self, window: window::Id) {
        let ours = self
            .vault
            .dialog
            .as_ref()
            .is_some_and(|d| d.window == window && d.busy.is_none());
        if ours && let Some(d) = self.vault.dialog.take() {
            // Not a decline: the window's own connects went with it, and a follow-up message
            // belongs to the closed window.
            let unlocked = self.backend.vault_unlocked().then(Zeroizing::default);
            self.vault.answer_waiters(unlocked.as_ref());
            drop(d);
        }
    }

    #[cfg(test)]
    pub(crate) fn use_secret_backend(
        &mut self,
        backend: std::sync::Arc<fbterm_secrets::SecretBackend>,
    ) {
        self.secrets = backend.clone();
        self.backend = backend;
    }
}

/// A failed job's sentence.
fn fail_text(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Ok => String::new(),
        Outcome::WrongPassword => SecretError::WrongMainPassword.to_string(),
        Outcome::Failed(m) => m.clone(),
    }
}
