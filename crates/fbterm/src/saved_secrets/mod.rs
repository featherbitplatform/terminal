//! Where saved secrets live, as the app sees it: the `Saved secrets` choice in Settings,
//! moving secrets when it changes, `Reset vault`, and the main password dialogs
//! (`Unlock saved secrets`, `Set a main password`) shown over whichever window needs them.
//!
//! The secrets themselves are behind [`SecretBackend`]. Every call that can reach the vault
//! (unlock, create, switch, reset) runs on the blocking pool while the dialog or the Settings
//! section shows a busy state; the UI thread never waits on scrypt.
//!
//! A connect that finds the vault locked asks through its prompter
//! ([`fbterm_session::ssh::Prompter::unlock_vault`]); the app shows one dialog however many
//! connects ask, and once the user cancels it, connects stop asking for the rest of the
//! session ([`VaultUi::declined`]). Explicit actions (Settings, an export, an import, an
//! editor save) still ask.

use fbterm_config::SecretStorage;
use fbterm_secrets::{MainPassword, SecretError, Storage, Zeroizing};
use iced::window;
use tokio::sync::oneshot;

use crate::app::Message;
use crate::redact::Redacted;

mod glue;
pub mod view;

/// Said once when fbterm moves to the vault on its own.
pub const AUTO_SWITCHED: &str =
    "The system keychain is not available, so saved secrets now use an encrypted vault.";
/// The reset confirmation.
pub const RESET_QUESTION: &str = "Delete all saved passwords and passphrases? Profiles are kept.";
pub const UNLOCK_TITLE: &str = "Unlock saved secrets";
pub const SET_TITLE: &str = "Set a main password";
pub const RESET_TITLE: &str = "Reset vault";
pub const MISMATCH: &str = "The passwords do not match.";
pub const EMPTY: &str = "Type a main password.";
/// A move asked for while `profiles.toml` does not load: the ids of the profiles are not
/// known, so their secrets cannot be found.
pub const PROFILES_UNREADABLE: &str =
    "Saved secrets cannot move while profiles.toml cannot be read. Fix it first.";

/// Where saved secrets are kept, as the notes under secret fields say it.
pub fn storage_note(storage: Storage) -> &'static str {
    match storage {
        Storage::Vault => "Saved in the encrypted vault",
        Storage::System if cfg!(windows) => "Saved for your Windows account",
        Storage::System => "Saved in the system keychain",
    }
}

/// The `Remember` checkbox of a password prompt.
pub fn remember_label(storage: Storage) -> &'static str {
    match storage {
        Storage::Vault => "Remember in the encrypted vault",
        Storage::System if cfg!(windows) => "Remember for your Windows account",
        Storage::System => "Remember in the system keychain",
    }
}

/// The place secrets moved to, for the Settings notice.
fn place(storage: Storage) -> &'static str {
    match storage {
        Storage::Vault => "the encrypted vault",
        Storage::System if cfg!(windows) => "storage for your Windows account",
        Storage::System => "the system keychain",
    }
}

/// The Settings notice after a move.
pub fn moved_text(to: Storage) -> String {
    format!("Saved secrets moved to {}.", place(to))
}

/// Said once a session when a secret the user asked to remember could not be saved.
pub fn not_saved_text(kind: fbterm_secrets::SecretKind, storage: Storage) -> String {
    let what = match kind {
        fbterm_secrets::SecretKind::Password => "password",
        fbterm_secrets::SecretKind::KeyPassphrase => "key passphrase",
    };
    format!(
        "Your {what} could not be saved in {}, so fbterm will ask for it next time.",
        place(storage)
    )
}

/// A request for a main password dialog while another one is open: finish that first.
pub fn dialog_open_text(open: Kind) -> &'static str {
    match open {
        Kind::Set => "Finish setting the main password first.",
        Kind::Reset => "Finish resetting the vault first.",
        Kind::Unlock => "Finish unlocking saved secrets first.",
    }
}

/// The label of a `Saved secrets` choice.
pub fn choice_label(choice: SecretStorage) -> &'static str {
    match choice {
        SecretStorage::Auto => "Automatic",
        // On Windows the system storage is a DPAPI file for the user's account, not a
        // keychain (owner decision on DPAPI; label approved by the controller).
        SecretStorage::Keychain if cfg!(windows) => "Windows account",
        SecretStorage::Keychain => "System keychain",
        SecretStorage::Vault => "Encrypted vault",
    }
}

/// What the main password dialogs and the storage jobs report.
#[derive(Debug, Clone)]
pub enum VaultMsg {
    Password(Redacted),
    Confirm(Redacted),
    /// Enter, or the dialog's main button.
    Submit,
    /// Escape, or `Cancel`.
    Cancel,
    /// Background job `seq` finished.
    Done(u64, Outcome),
    /// The `FBTERM_DEMO_VAULT` debug aid's vault is made.
    DemoReady,
}

/// How a background storage job ended (a plain sentence on failure, never a secret).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    WrongPassword,
    Failed(String),
}

impl From<Result<(), SecretError>> for Outcome {
    fn from(r: Result<(), SecretError>) -> Self {
        match r {
            Ok(()) => Self::Ok,
            Err(SecretError::WrongMainPassword) => Self::WrongPassword,
            Err(e) => Self::Failed(e.to_string()),
        }
    }
}

/// Which question a dialog asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `Unlock saved secrets`: the main password of the existing vault.
    Unlock,
    /// `Set a main password`, typed twice: a new vault.
    Set,
    /// `Reset vault`: the confirmation.
    Reset,
}

impl From<MainPassword> for Kind {
    fn from(m: MainPassword) -> Self {
        match m {
            MainPassword::Unlock => Self::Unlock,
            MainPassword::Set => Self::Set,
        }
    }
}

/// What happens once a dialog succeeds.
pub enum Purpose {
    /// Connects are waiting for the vault to open (see [`VaultUi::waiters`]).
    Connect,
    /// Open (or make) the vault, then send `then`; on cancel send `on_cancel`, if any.
    Then {
        then: Box<Message>,
        on_cancel: Option<Box<Message>>,
    },
    /// Move saved secrets for this choice of `secret_storage`.
    Switch(SecretStorage),
    /// Delete the vault.
    Reset,
}

/// A main password dialog over one window.
pub struct VaultDialog {
    pub window: window::Id,
    pub kind: Kind,
    pub purpose: Purpose,
    pub password: Zeroizing<String>,
    pub confirm: Zeroizing<String>,
    pub problem: Option<String>,
    /// The job running for it: buttons wait meanwhile.
    pub busy: Option<u64>,
    pub input: iced::widget::Id,
    pub confirm_input: iced::widget::Id,
}

impl VaultDialog {
    pub fn new(window: window::Id, kind: Kind, purpose: Purpose) -> Self {
        Self {
            window,
            kind,
            purpose,
            password: Zeroizing::default(),
            confirm: Zeroizing::default(),
            problem: None,
            busy: None,
            input: iced::widget::Id::unique(),
            confirm_input: iced::widget::Id::unique(),
        }
    }

    /// Why the typed passwords cannot be used yet, if they cannot.
    pub fn check(&self) -> Option<&'static str> {
        match self.kind {
            Kind::Reset => None,
            _ if self.password.is_empty() => Some(EMPTY),
            Kind::Set if *self.password != *self.confirm => Some(MISMATCH),
            _ => None,
        }
    }

    /// Whether the main button acts now.
    pub fn can_submit(&self) -> bool {
        self.busy.is_none() && self.check().is_none()
    }

    /// The job's busy line.
    pub fn busy_text(&self) -> &'static str {
        match (&self.purpose, self.kind) {
            (Purpose::Switch(_), _) => "Moving saved secrets\u{2026}",
            (_, Kind::Reset) => "Deleting saved secrets\u{2026}",
            (_, Kind::Set) => "Making the vault\u{2026}",
            (_, Kind::Unlock) => "Unlocking\u{2026}",
        }
    }
}

/// A switch running without a dialog (the vault already open, or nothing to open).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectSwitch {
    pub seq: u64,
    pub choice: SecretStorage,
}

/// The Settings section's line under the choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionNotice {
    pub error: bool,
    pub text: String,
}

/// What the `Saved secrets` section shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionInfo {
    pub choice: SecretStorage,
    pub vault_exists: bool,
    /// A move or reset is under way: the choices wait.
    pub busy: bool,
    pub notice: Option<SectionNotice>,
    /// The storage in use, for the hint.
    pub active: Storage,
}

/// The app's side of saved-secret storage.
#[derive(Default)]
pub struct VaultUi {
    pub dialog: Option<VaultDialog>,
    /// The user cancelled `Unlock saved secrets` for a connect: later connects stop asking
    /// this session (they ask for their secret as if none were saved).
    pub declined: bool,
    /// The user cancelled a connect's `Set a main password`: later remembered secrets are
    /// not saved this session without asking again.
    pub set_declined: bool,
    /// Connects waiting for the vault to open.
    waiters: Vec<oneshot::Sender<Option<Zeroizing<String>>>>,
    /// A switch running without a dialog.
    pub direct: Option<DirectSwitch>,
    /// The `Saved secrets` section's line.
    pub notice: Option<SectionNotice>,
    /// The `FBTERM_DEMO_VAULT` debug aid is making its vault: a screenshot waits.
    pub demo_pending: bool,
    /// A failed save of a remembered secret was already reported this session.
    pub not_saved_said: bool,
    seq: u64,
}

/// What a connect's unlock question leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    /// Answered at once (the vault is open, or the user already declined).
    Answered,
    /// Waits for the dialog already showing.
    Joined,
    /// Waits for a new dialog the app must show.
    Show,
}

impl VaultUi {
    pub fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// A connect asks to unlock (`Kind::Unlock`) or to set a main password (`Kind::Set`).
    /// `needed`: whether the vault still needs that.
    pub fn ask(
        &mut self,
        reply: oneshot::Sender<Option<Zeroizing<String>>>,
        kind: Kind,
        needed: bool,
    ) -> Asked {
        if !needed {
            // Opened or made meanwhile (by another connect, or Settings): any answer does.
            let _ = reply.send(Some(Zeroizing::default()));
            return Asked::Answered;
        }
        let declined = match kind {
            Kind::Set => self.set_declined,
            _ => self.declined,
        };
        if declined {
            let _ = reply.send(None);
            return Asked::Answered;
        }
        self.waiters.push(reply);
        if self.dialog.is_some() {
            Asked::Joined
        } else {
            Asked::Show
        }
    }

    /// Answer every waiting connect: with `password` when the vault opened, else `None`.
    pub fn answer_waiters(&mut self, password: Option<&Zeroizing<String>>) {
        for reply in self.waiters.drain(..) {
            let _ = reply.send(password.cloned());
        }
    }

    /// Whether connects are waiting.
    pub fn has_waiters(&self) -> bool {
        self.waiters.iter().any(|w| !w.is_closed())
    }

    /// What to say instead of opening a second main password dialog, while one is open.
    pub fn dialog_open(&self) -> Option<&'static str> {
        self.dialog.as_ref().map(|d| dialog_open_text(d.kind))
    }

    /// Whether a move or reset runs.
    pub fn section_busy(&self) -> bool {
        self.direct.is_some()
            || self.dialog.as_ref().is_some_and(|d| {
                d.busy.is_some() && matches!(d.purpose, Purpose::Switch(_) | Purpose::Reset)
            })
    }
}

#[cfg(test)]
mod tests;
