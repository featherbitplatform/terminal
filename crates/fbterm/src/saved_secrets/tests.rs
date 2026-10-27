//! Choosing and moving saved-secret storage against a temporary config folder, an in-memory
//! system storage and vaults in temporary folders (never the real keychain, DPAPI file or
//! `~`). Background work runs at once (`blocking_inline`).

use std::path::Path;
use std::sync::Arc;

use fbterm_config::profile::{AuthMethod, Profile, ProfileKind, SshProfile};
use fbterm_config::{Paths, ProfileId, ProfileStore, SecretStorage, Settings};
use fbterm_secrets::{
    MainPassword, MemoryStore, SecretBackend, SecretKind, SecretStore, Storage, Zeroizing,
};
use iced::window;
use tokio::sync::oneshot;

use super::{AUTO_SWITCHED, Kind, Purpose, VaultMsg};
use crate::app::{App, Delivered, Message};
use crate::editor::EditorMsg;
use crate::prompts::{Ask, PromptEvent, PromptMsg};
use crate::redact::Redacted;
use crate::restore_tests::paths_with;
use crate::settings_window::SettingsMsg;
use crate::tabs::WindowState;

const MAIN: &str = "main-pw-app-tests-41c2";
const SAVED: &str = "saved-pw-9e07";

fn ssh(name: &str) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        group: None,
        accent: None,
        accent_style: Default::default(),
        color_scheme: None,
        kind: ProfileKind::Ssh(SshProfile {
            host: format!("{name}.example.com"),
            port: 22,
            user: "ops".into(),
            auth: AuthMethod::Password,
            jump: Vec::new(),
            tunnels: Vec::new(),
        }),
    }
}

fn app_with(profiles: &[Profile]) -> App {
    let (mut app, _) = App::build(paths_with(profiles), true);
    app.blocking_inline = true;
    app
}

/// A backend using the vault at `path` (locked), over a system storage of its own.
fn vault_backend(path: &Path, system: Arc<MemoryStore>) -> Arc<SecretBackend> {
    Arc::new(SecretBackend::with_system(
        SecretStorage::Vault,
        Box::new(Shared(system)),
        path.to_path_buf(),
    ))
}

/// A vault at `path` holding `id`'s password.
fn make_vault(path: &Path, id: ProfileId) {
    let setup = vault_backend(path, Arc::new(MemoryStore::new()));
    setup.create_vault(MAIN).unwrap();
    setup.set(&id, SecretKind::Password, SAVED).unwrap();
}

/// A system store a test keeps looking at after handing it to a backend.
struct Shared(Arc<MemoryStore>);
impl SecretStore for Shared {
    fn get(
        &self,
        id: &ProfileId,
        kind: SecretKind,
    ) -> Result<Option<String>, fbterm_secrets::SecretError> {
        self.0.get(id, kind)
    }
    fn set(
        &self,
        id: &ProfileId,
        kind: SecretKind,
        value: &str,
    ) -> Result<(), fbterm_secrets::SecretError> {
        self.0.set(id, kind, value)
    }
    fn remove(&self, id: &ProfileId) -> Result<(), fbterm_secrets::SecretError> {
        self.0.remove(id)
    }
    fn remove_kind(
        &self,
        id: &ProfileId,
        kind: SecretKind,
    ) -> Result<(), fbterm_secrets::SecretError> {
        self.0.remove_kind(id, kind)
    }
}

/// A terminal window to ask in.
fn terminal_window(app: &mut App) -> window::Id {
    let wid = window::Id::unique();
    app.windows.insert(wid, WindowState::new());
    wid
}

/// A connect's `Unlock saved secrets` question, as its prompter sends it.
fn ask_unlock(app: &mut App, wid: window::Id) -> oneshot::Receiver<Option<Zeroizing<String>>> {
    let (reply, answer) = oneshot::channel();
    let event = PromptEvent::Ask {
        id: 0,
        window: Some(wid),
        tunnel: None,
        ask: Ask::UnlockVault { reply },
    };
    let _ = app.update(Message::Prompt(PromptMsg::Event(Delivered::new(event))));
    answer
}

fn vault(app: &mut App, m: VaultMsg) {
    let _ = app.update(Message::Vault(m));
}

fn settings(app: &mut App, m: SettingsMsg) {
    let _ = app.update(Message::Settings(m));
}

fn type_password(app: &mut App, password: &str, twice: bool) {
    vault(app, VaultMsg::Password(Redacted(password.into())));
    if twice {
        vault(app, VaultMsg::Confirm(Redacted(password.into())));
    }
}

#[test]
fn locked_vault_asks_once_per_session() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secrets.fbvault");
    let db = ssh("db");
    make_vault(&path, db.id);
    let mut app = app_with(std::slice::from_ref(&db));
    let backend = vault_backend(&path, Arc::new(MemoryStore::new()));
    app.use_secret_backend(backend.clone());
    let wid = terminal_window(&mut app);

    // Two connects ask at once: one dialog.
    let mut first = ask_unlock(&mut app, wid);
    let d = app.vault.dialog.as_ref().expect("the unlock dialog");
    assert_eq!((d.window, d.kind), (wid, Kind::Unlock));
    let mut second = ask_unlock(&mut app, wid);
    assert!(
        first.try_recv().is_err() && second.try_recv().is_err(),
        "both wait"
    );

    // Cancelled: both connects go on as if nothing were saved...
    vault(&mut app, VaultMsg::Cancel);
    assert!(app.vault.dialog.is_none());
    assert_eq!(first.try_recv().unwrap(), None);
    assert_eq!(second.try_recv().unwrap(), None);
    // ...and later connects are not asked again this session.
    let mut later = ask_unlock(&mut app, wid);
    assert!(app.vault.dialog.is_none());
    assert_eq!(later.try_recv().unwrap(), None);

    // An explicit action (a full export) still asks; unlocking there serves every connect.
    let _ = app.update(Message::Shortcut(wid, crate::keys::Action::ExportProfiles));
    let _ = app.update(Message::Archive(
        crate::archive_window::ArchiveMsg::Password(Redacted("archive-pw".into())),
    ));
    let _ = app.update(Message::Archive(
        crate::archive_window::ArchiveMsg::Confirm(Redacted("archive-pw".into())),
    ));
    let _ = app.update(Message::Archive(crate::archive_window::ArchiveMsg::Export));
    let d = app.vault.dialog.as_ref().expect("the export asks");
    assert_eq!(d.kind, Kind::Unlock);
    assert_eq!(Some(d.window), app.archive_window());
    type_password(&mut app, "not it", false);
    vault(&mut app, VaultMsg::Submit);
    let d = app.vault.dialog.as_ref().expect("still asking");
    assert_eq!(d.problem.as_deref(), Some("Wrong main password."));
    assert!(d.password.is_empty(), "the wrong password is cleared");
    type_password(&mut app, MAIN, false);
    vault(&mut app, VaultMsg::Submit);
    assert!(app.vault.dialog.is_none());
    assert!(backend.vault_unlocked());
    // The export went on to its save dialog.
    assert!(app.archive_win.as_ref().unwrap().1.dialog.is_some());
    // Open now: a connect is answered at once, and the saved secret reads.
    let mut open = ask_unlock(&mut app, wid);
    assert!(open.try_recv().unwrap().is_some());
    assert_eq!(
        backend
            .get(&db.id, SecretKind::Password)
            .unwrap()
            .as_deref(),
        Some(SAVED)
    );
}

#[test]
fn unlock_for_a_connect_answers_with_the_main_password() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secrets.fbvault");
    let db = ssh("db");
    make_vault(&path, db.id);
    let mut app = app_with(std::slice::from_ref(&db));
    let backend = vault_backend(&path, Arc::new(MemoryStore::new()));
    app.use_secret_backend(backend.clone());
    let wid = terminal_window(&mut app);
    let mut answer = ask_unlock(&mut app, wid);
    type_password(&mut app, MAIN, false);
    vault(&mut app, VaultMsg::Submit);
    assert!(app.vault.dialog.is_none());
    assert_eq!(
        answer.try_recv().unwrap().as_deref().map(|s| s.as_str()),
        Some(MAIN)
    );
    assert!(backend.vault_unlocked());
    assert!(!app.vault.declined);
}

#[test]
fn closing_the_window_drops_its_dialog_without_declining() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secrets.fbvault");
    make_vault(&path, ProfileId::new());
    let mut app = app_with(&[]);
    app.use_secret_backend(vault_backend(&path, Arc::new(MemoryStore::new())));
    let wid = terminal_window(&mut app);
    let mut answer = ask_unlock(&mut app, wid);
    type_password(&mut app, "typed", false);
    let _ = app.update(Message::WindowClosed(wid));
    assert!(app.vault.dialog.is_none());
    assert_eq!(answer.try_recv().unwrap(), None);
    assert!(!app.vault.declined);
}

fn settings_text(paths: &Paths) -> String {
    std::fs::read_to_string(paths.settings()).unwrap_or_default()
}

#[test]
fn settings_section_switches_storage() {
    let db = ssh("db");
    let web = ssh("web");
    let mut app = app_with(&[db.clone(), web.clone()]);
    let paths = app.paths.clone();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secrets.fbvault");
    let system = Arc::new(MemoryStore::new());
    let backend = Arc::new(SecretBackend::with_system(
        SecretStorage::Auto,
        Box::new(Shared(system.clone())),
        path.clone(),
    ));
    app.use_secret_backend(backend.clone());
    backend.set(&db.id, SecretKind::Password, SAVED).unwrap();
    backend
        .set(&web.id, SecretKind::KeyPassphrase, "pp-web")
        .unwrap();
    settings(&mut app, SettingsMsg::Open);
    let section = app.secrets_section();
    assert_eq!(section.choice, SecretStorage::Auto);
    assert!(!section.vault_exists && !section.busy);

    // Encrypted vault: no vault yet, so a main password is set (typed twice).
    settings(&mut app, SettingsMsg::SecretStorage(SecretStorage::Vault));
    let d = app.vault.dialog.as_ref().expect("Set a main password");
    assert_eq!(d.kind, Kind::Set);
    assert!(matches!(d.purpose, Purpose::Switch(SecretStorage::Vault)));
    assert_eq!(Some(d.window), app.settings_window());
    vault(&mut app, VaultMsg::Password(Redacted(MAIN.into())));
    vault(
        &mut app,
        VaultMsg::Confirm(Redacted("something else".into())),
    );
    vault(&mut app, VaultMsg::Submit);
    assert_eq!(
        app.vault.dialog.as_ref().unwrap().problem.as_deref(),
        Some(super::MISMATCH)
    );
    assert!(!path.exists(), "nothing made before the passwords match");
    vault(&mut app, VaultMsg::Confirm(Redacted(MAIN.into())));
    vault(&mut app, VaultMsg::Submit);
    assert!(app.vault.dialog.is_none());
    assert_eq!(backend.active(), Storage::Vault);
    assert!(path.exists());
    assert!(
        system.entries(&[db.id, web.id]).unwrap().is_empty(),
        "moved"
    );
    assert_eq!(
        backend
            .get(&db.id, SecretKind::Password)
            .unwrap()
            .as_deref(),
        Some(SAVED)
    );
    assert_eq!(app.settings.secret_storage, SecretStorage::Vault);
    assert!(settings_text(&paths).contains("\nsecret_storage = \"vault\"\n"));
    let section = app.secrets_section();
    assert_eq!(section.choice, SecretStorage::Vault);
    assert!(section.vault_exists);
    assert_eq!(
        section.notice.map(|n| n.text),
        Some(super::moved_text(Storage::Vault))
    );
    // Our own write is not taken for a hand edit.
    assert_eq!(app.settings_seen, Some(settings_text(&paths)));

    // Back to the system keychain: the vault is open, so no question.
    settings(
        &mut app,
        SettingsMsg::SecretStorage(SecretStorage::Keychain),
    );
    assert!(app.vault.dialog.is_none());
    assert_eq!(backend.active(), Storage::System);
    assert_eq!(
        system
            .get(&web.id, SecretKind::KeyPassphrase)
            .unwrap()
            .as_deref(),
        Some("pp-web")
    );
    assert_eq!(app.settings.secret_storage, SecretStorage::Keychain);
    assert!(settings_text(&paths).contains("\nsecret_storage = \"keychain\"\n"));

    // Automatic uses the same storage here: only the setting changes.
    settings(&mut app, SettingsMsg::SecretStorage(SecretStorage::Auto));
    assert!(app.vault.dialog.is_none());
    assert_eq!(app.settings.secret_storage, SecretStorage::Auto);
    assert_eq!(backend.active(), Storage::System);
}

#[test]
fn switch_from_a_locked_vault_asks_to_unlock_and_reports_failures() {
    let db = ssh("db");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secrets.fbvault");
    make_vault(&path, db.id);
    let mut app = app_with(std::slice::from_ref(&db));
    let system = Arc::new(MemoryStore::new());
    let backend = vault_backend(&path, system.clone());
    app.use_secret_backend(backend.clone());
    app.settings.secret_storage = SecretStorage::Vault;
    settings(&mut app, SettingsMsg::Open);
    settings(
        &mut app,
        SettingsMsg::SecretStorage(SecretStorage::Keychain),
    );
    let d = app.vault.dialog.as_ref().expect("unlock first");
    assert_eq!(d.kind, Kind::Unlock);
    // Cancel: nothing moves.
    vault(&mut app, VaultMsg::Cancel);
    assert_eq!(backend.active(), Storage::Vault);
    assert!(!app.vault.declined, "a Settings cancel is not a connect's");
    settings(
        &mut app,
        SettingsMsg::SecretStorage(SecretStorage::Keychain),
    );
    type_password(&mut app, MAIN, false);
    vault(&mut app, VaultMsg::Submit);
    assert_eq!(backend.active(), Storage::System);
    assert_eq!(
        system.get(&db.id, SecretKind::Password).unwrap().as_deref(),
        Some(SAVED)
    );
}

#[test]
fn reset_vault_keeps_profiles() {
    let db = ssh("db");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secrets.fbvault");
    make_vault(&path, db.id);
    let mut app = app_with(std::slice::from_ref(&db));
    let paths = app.paths.clone();
    let profiles_before = std::fs::read_to_string(paths.profiles()).unwrap();
    let backend = vault_backend(&path, Arc::new(MemoryStore::new()));
    app.use_secret_backend(backend.clone());
    app.settings.secret_storage = SecretStorage::Vault;
    settings(&mut app, SettingsMsg::Open);
    assert!(app.secrets_section().vault_exists);

    // A forgotten main password: reset works on the locked vault, after a confirmation.
    settings(&mut app, SettingsMsg::ResetVault);
    let d = app.vault.dialog.as_ref().expect("the confirmation");
    assert_eq!(d.kind, Kind::Reset);
    assert!(d.can_submit(), "nothing to type");
    vault(&mut app, VaultMsg::Cancel);
    assert!(path.exists(), "cancel keeps the vault");
    settings(&mut app, SettingsMsg::ResetVault);
    vault(&mut app, VaultMsg::Submit);
    assert!(app.vault.dialog.is_none());
    assert!(!path.exists());
    assert!(!app.secrets_section().vault_exists);
    assert_eq!(
        std::fs::read_to_string(paths.profiles()).unwrap(),
        profiles_before,
        "profiles are kept"
    );
    assert!(app.profiles.get(db.id).is_some());
    assert_eq!(backend.get(&db.id, SecretKind::Password).unwrap(), None);

    // Still the vault: the next save of a secret asks for a new main password first.
    assert_eq!(backend.needs_main_password(), Some(MainPassword::Set));
    let _ = app.update(Message::Editor(EditorMsg::Open));
    let _ = app.update(Message::Editor(EditorMsg::Select(db.id)));
    let _ = app.update(Message::Editor(EditorMsg::Secret(Redacted(
        "new-pw".into(),
    ))));
    let _ = app.update(Message::Editor(EditorMsg::Save));
    let d = app.vault.dialog.as_ref().expect("Set a main password");
    assert_eq!(d.kind, Kind::Set);
    assert_eq!(Some(d.window), app.editor_window());
    type_password(&mut app, "new main", true);
    vault(&mut app, VaultMsg::Submit);
    assert!(app.vault.dialog.is_none());
    assert_eq!(
        backend
            .get(&db.id, SecretKind::Password)
            .unwrap()
            .as_deref(),
        Some("new-pw")
    );
    let ed = &app.editor.as_ref().unwrap().1;
    assert_eq!(ed.notice.as_deref(), Some("Saved"));
    assert!(!ed.dirty);
}

#[test]
fn cancelling_the_main_password_still_saves_the_profiles() {
    let db = ssh("db");
    let mut app = app_with(std::slice::from_ref(&db));
    let paths = app.paths.clone();
    let dir = tempfile::tempdir().unwrap();
    let backend = vault_backend(
        &dir.path().join("secrets.fbvault"),
        Arc::new(MemoryStore::new()),
    );
    app.use_secret_backend(backend.clone());
    let _ = app.update(Message::Editor(EditorMsg::Open));
    let _ = app.update(Message::Editor(EditorMsg::Select(db.id)));
    let _ = app.update(Message::Editor(EditorMsg::Name("db-renamed".into())));
    let _ = app.update(Message::Editor(EditorMsg::Secret(Redacted(
        "new-pw".into(),
    ))));
    let _ = app.update(Message::Editor(EditorMsg::Save));
    assert!(app.vault.dialog.is_some());
    vault(&mut app, VaultMsg::Cancel);
    let on_disk = ProfileStore::load(&paths.profiles()).unwrap();
    assert!(on_disk.profiles.iter().any(|p| p.name == "db-renamed"));
    let ed = &app.editor.as_ref().unwrap().1;
    let notice = ed.notice.clone().unwrap();
    assert!(
        notice.starts_with("Profiles saved, but could not store the password of db-renamed"),
        "{notice}"
    );
    assert!(ed.dirty, "the typed password is kept to save again");
    assert_eq!(backend.get(&db.id, SecretKind::Password).unwrap(), None);
}

#[test]
fn auto_switch_is_said_once_and_written() {
    let mut app = app_with(&[]);
    let paths = app.paths.clone();
    let before = app.notices.len();
    vault(&mut app, VaultMsg::AutoDetected(false));
    assert_eq!(app.notices.len(), before);
    vault(&mut app, VaultMsg::AutoDetected(true));
    assert_eq!(app.notices.len(), before + 1);
    assert!(app.visible_notices().contains(&AUTO_SWITCHED.to_owned()));
    assert_eq!(app.settings.secret_storage, SecretStorage::Vault);
    let loaded = Settings::load(&paths.settings()).unwrap();
    assert_eq!(loaded.secret_storage, SecretStorage::Vault);
    // The next launch starts in the vault and checks nothing (it is not Auto any more).
    let (next, _) = App::build(paths.clone(), true);
    assert_eq!(next.settings.secret_storage, SecretStorage::Vault);
    assert_eq!(next.storage(), Storage::Vault);
    assert!(!next.visible_notices().contains(&AUTO_SWITCHED.to_owned()));
}

#[test]
fn storage_copy_names_the_storage_in_use() {
    assert_eq!(
        super::storage_note(Storage::Vault),
        "Saved in the encrypted vault"
    );
    let system = super::storage_note(Storage::System);
    if cfg!(windows) {
        assert_eq!(system, "Saved for your Windows account");
    } else {
        assert_eq!(system, "Saved in the system keychain");
    }
    assert_eq!(
        super::choice_label(SecretStorage::Keychain),
        "System keychain"
    );
    assert_eq!(super::choice_label(SecretStorage::Auto), "Automatic");
    assert_eq!(super::choice_label(SecretStorage::Vault), "Encrypted vault");
}

#[test]
fn main_passwords_never_show_in_debug_output() {
    let m = Message::Vault(VaultMsg::Password(Redacted(MAIN.into())));
    let shown = format!("{m:?}");
    assert!(!shown.contains(MAIN), "{shown}");
}

#[test]
fn a_hand_edit_of_secret_storage_waits_for_the_next_launch() {
    let mut app = app_with(&[]);
    let paths = app.paths.clone();
    std::fs::write(
        paths.settings(),
        "secret_storage = \"vault\"\ntheme = \"light\"\n",
    )
    .unwrap();
    let _ = app.update(Message::ConfigChanged(crate::watch::Changed {
        profiles: false,
        settings: true,
        schemes: false,
    }));
    assert_eq!(
        app.settings.theme,
        fbterm_config::ThemeMode::Light,
        "reloaded"
    );
    assert_eq!(app.settings.secret_storage, SecretStorage::Auto);
    assert_eq!(app.storage(), Storage::System, "nothing moved");
}
