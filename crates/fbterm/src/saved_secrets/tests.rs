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

/// A system storage that counts every call, and is either there or not at all.
#[derive(Default)]
struct Probed {
    inner: MemoryStore,
    calls: std::sync::atomic::AtomicUsize,
    missing: bool,
    batches: std::sync::atomic::AtomicUsize,
    single: std::sync::atomic::AtomicUsize,
}

impl Probed {
    fn call(&self) -> Result<(), fbterm_secrets::SecretError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.missing {
            return Err(fbterm_secrets::SecretError::Unavailable(
                "no secret service".into(),
            ));
        }
        Ok(())
    }
    fn alone(&self) {
        self.single
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl SecretStore for Probed {
    fn get(
        &self,
        id: &ProfileId,
        kind: SecretKind,
    ) -> Result<Option<String>, fbterm_secrets::SecretError> {
        self.call()?;
        self.inner.get(id, kind)
    }
    fn set(
        &self,
        id: &ProfileId,
        kind: SecretKind,
        value: &str,
    ) -> Result<(), fbterm_secrets::SecretError> {
        self.call()?;
        self.alone();
        self.inner.set(id, kind, value)
    }
    fn remove(&self, id: &ProfileId) -> Result<(), fbterm_secrets::SecretError> {
        self.call()?;
        self.alone();
        self.inner.remove(id)
    }
    fn remove_kind(
        &self,
        id: &ProfileId,
        kind: SecretKind,
    ) -> Result<(), fbterm_secrets::SecretError> {
        self.call()?;
        self.alone();
        self.inner.remove_kind(id, kind)
    }
    fn apply_changes(
        &self,
        changes: &[fbterm_secrets::SecretChange],
    ) -> Result<(), fbterm_secrets::SecretError> {
        self.call()?;
        self.batches
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.apply_changes(changes)
    }
}

/// Lets a test keep looking at a store it handed to a backend.
struct SharedProbed(Arc<Probed>);
impl SecretStore for SharedProbed {
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
    fn apply_changes(
        &self,
        changes: &[fbterm_secrets::SecretChange],
    ) -> Result<(), fbterm_secrets::SecretError> {
        self.0.apply_changes(changes)
    }
}

/// An app over `system` (with the setting from the config folder), as `App::boot` builds
/// it but for the store.
fn app_over(paths: Paths, system: Arc<Probed>) -> App {
    let (mut app, _) = App::build_with(paths, move |settings, paths| {
        SecretBackend::with_system(
            settings.secret_storage,
            Box::new(SharedProbed(system)),
            paths.vault(),
        )
    });
    app.blocking_inline = true;
    app
}

/// Deliver the events the app's prompt channel holds, as its subscription would.
fn deliver_prompt_events(app: &mut App) {
    let mut rx = app.prompt_rx.0.lock().take().expect("the prompt channel");
    while let Ok(event) = rx.try_recv() {
        let _ = app.update(Message::Prompt(PromptMsg::Event(Delivered::new(event))));
    }
    *app.prompt_rx.0.lock() = Some(rx);
}

#[test]
fn building_the_app_touches_no_storage() {
    let paths = paths_with(&[ssh("db")]);
    let system = Arc::new(Probed::default());
    let mut app = app_over(paths.clone(), system.clone());
    let _ = app.update(Message::Settings(SettingsMsg::Open));
    let _ = app.update(Message::Editor(EditorMsg::Open));
    let _ = app.secrets_section();
    let _ = app.storage();
    assert_eq!(
        system.calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "nothing asks the system storage before a secret is needed"
    );
    assert!(!paths.vault().exists());
}

#[test]
fn auto_switch_is_found_on_first_use_said_once_and_written() {
    let paths = paths_with(&[]);
    let system = Arc::new(Probed {
        missing: true,
        ..Probed::default()
    });
    let mut app = app_over(paths.clone(), system.clone());
    let before = app.notices.len();
    assert_eq!(app.storage(), Storage::System, "not probed at launch");
    // A connect needs a saved password: the first data call finds no system storage.
    assert_eq!(
        app.secrets
            .get(&ProfileId::new(), SecretKind::Password)
            .unwrap(),
        None
    );
    assert_eq!(app.storage(), Storage::Vault);
    deliver_prompt_events(&mut app);
    assert_eq!(app.notices.len(), before + 1);
    assert!(app.visible_notices().contains(&AUTO_SWITCHED.to_owned()));
    assert_eq!(app.settings.secret_storage, SecretStorage::Vault);
    let loaded = Settings::load(&paths.settings()).unwrap();
    assert_eq!(loaded.secret_storage, SecretStorage::Vault);
    // Said once: more data calls report nothing new.
    let _ = app.secrets.get(&ProfileId::new(), SecretKind::Password);
    deliver_prompt_events(&mut app);
    assert_eq!(app.notices.len(), before + 1);
    // The next launch starts in the vault and checks nothing (it is not Auto any more).
    let (next, _) = App::build(paths.clone(), true);
    assert_eq!(next.settings.secret_storage, SecretStorage::Vault);
    assert_eq!(next.storage(), Storage::Vault);
    assert!(!next.visible_notices().contains(&AUTO_SWITCHED.to_owned()));
}

/// A connect's `Set a main password` question, as its prompter sends it.
fn ask_set(app: &mut App, wid: window::Id) -> oneshot::Receiver<Option<Zeroizing<String>>> {
    let (reply, answer) = oneshot::channel();
    let event = PromptEvent::Ask {
        id: 0,
        window: Some(wid),
        tunnel: None,
        ask: Ask::SetVaultPassword { reply },
    };
    let _ = app.update(Message::Prompt(PromptMsg::Event(Delivered::new(event))));
    answer
}

#[test]
fn a_connect_remember_with_no_vault_sets_a_main_password() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secrets.fbvault");
    let mut app = app_with(&[]);
    let backend = vault_backend(&path, Arc::new(MemoryStore::new()));
    app.use_secret_backend(backend.clone());
    let wid = terminal_window(&mut app);

    let mut answer = ask_set(&mut app, wid);
    let d = app.vault.dialog.as_ref().expect("Set a main password");
    assert_eq!((d.window, d.kind), (wid, Kind::Set));
    type_password(&mut app, MAIN, true);
    vault(&mut app, VaultMsg::Submit);
    assert!(app.vault.dialog.is_none());
    assert_eq!(
        answer.try_recv().unwrap().as_deref().map(|s| s.as_str()),
        Some(MAIN)
    );
    assert!(path.exists() && backend.vault_unlocked());
}

#[test]
fn cancelling_a_connect_set_password_asks_no_more_this_session() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secrets.fbvault");
    let mut app = app_with(&[]);
    app.use_secret_backend(vault_backend(&path, Arc::new(MemoryStore::new())));
    let wid = terminal_window(&mut app);
    let mut answer = ask_set(&mut app, wid);
    vault(&mut app, VaultMsg::Cancel);
    assert_eq!(answer.try_recv().unwrap(), None);
    let mut again = ask_set(&mut app, wid);
    assert!(app.vault.dialog.is_none());
    assert_eq!(again.try_recv().unwrap(), None);
    assert!(!path.exists());
}

#[test]
fn an_editor_save_writes_the_storage_once() {
    let db = ssh("db");
    let web = ssh("web");
    let gone = ssh("gone");
    let paths = paths_with(&[db.clone(), web.clone(), gone.clone()]);
    let system = Arc::new(Probed::default());
    let mut app = app_over(paths, system.clone());
    let _ = app.update(Message::Editor(EditorMsg::Open));
    for (id, pw) in [(db.id, "db-pw"), (web.id, "web-pw")] {
        let _ = app.update(Message::Editor(EditorMsg::Select(id)));
        let _ = app.update(Message::Editor(EditorMsg::Secret(Redacted(pw.into()))));
    }
    let _ = app.update(Message::Editor(EditorMsg::Select(gone.id)));
    let _ = app.update(Message::Editor(EditorMsg::Delete));
    let _ = app.update(Message::Editor(EditorMsg::Save));
    use std::sync::atomic::Ordering::SeqCst;
    assert_eq!(system.batches.load(SeqCst), 1, "one batch for the save");
    assert_eq!(system.single.load(SeqCst), 0, "no change made alone");
    assert_eq!(
        system
            .get(&web.id, SecretKind::Password)
            .unwrap()
            .as_deref(),
        Some("web-pw")
    );
    let ed = &app.editor.as_ref().unwrap().1;
    assert_eq!(ed.notice.as_deref(), Some("Saved"));
    assert!(!ed.dirty);
}

#[test]
fn switching_is_refused_while_profiles_toml_is_unreadable() {
    let db = ssh("db");
    let mut app = app_with(std::slice::from_ref(&db));
    let paths = app.paths.clone();
    let dir = tempfile::tempdir().unwrap();
    let system = Arc::new(MemoryStore::new());
    let backend = Arc::new(SecretBackend::with_system(
        SecretStorage::Keychain,
        Box::new(Shared(system.clone())),
        dir.path().join("secrets.fbvault"),
    ));
    app.use_secret_backend(backend.clone());
    backend.set(&db.id, SecretKind::Password, SAVED).unwrap();
    std::fs::write(paths.profiles(), "[[profiles]\nname = 1\n").unwrap();
    let _ = app.update(Message::ConfigChanged(crate::watch::Changed {
        profiles: true,
        settings: false,
        schemes: false,
    }));
    assert!(app.profiles_error.is_some());
    settings(&mut app, SettingsMsg::Open);
    settings(&mut app, SettingsMsg::SecretStorage(SecretStorage::Vault));
    assert!(app.vault.dialog.is_none(), "nothing asked");
    assert_eq!(backend.active(), Storage::System);
    assert_eq!(
        app.secrets_section().notice.map(|n| (n.error, n.text)),
        Some((true, super::PROFILES_UNREADABLE.to_owned()))
    );
    assert_eq!(
        system.get(&db.id, SecretKind::Password).unwrap().as_deref(),
        Some(SAVED)
    );
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
    let keychain = super::choice_label(SecretStorage::Keychain);
    if cfg!(windows) {
        assert_eq!(keychain, "Windows account");
    } else {
        assert_eq!(keychain, "System keychain");
    }
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

#[test]
fn an_explicit_keychain_choice_is_never_switched_over() {
    // Automatic, nothing checked yet, no system storage on this machine: the user picks the
    // system keychain on purpose. The first secret needed must not move to the vault.
    let paths = paths_with(&[]);
    let system = Arc::new(Probed {
        missing: true,
        ..Probed::default()
    });
    let mut app = app_over(paths.clone(), system);
    settings(&mut app, SettingsMsg::Open);
    settings(
        &mut app,
        SettingsMsg::SecretStorage(SecretStorage::Keychain),
    );
    assert_eq!(app.settings.secret_storage, SecretStorage::Keychain);
    assert!(
        app.secrets
            .get(&ProfileId::new(), SecretKind::Password)
            .is_err()
    );
    deliver_prompt_events(&mut app);
    assert_eq!(app.storage(), Storage::System);
    assert_eq!(app.settings.secret_storage, SecretStorage::Keychain);
    assert!(!app.visible_notices().contains(&AUTO_SWITCHED.to_owned()));
    assert_eq!(
        Settings::load(&paths.settings()).unwrap().secret_storage,
        SecretStorage::Keychain
    );
}

fn not_saved(app: &mut App, kind: SecretKind) {
    let event = PromptEvent::SecretNotSaved { kind };
    let _ = app.update(Message::Prompt(PromptMsg::Event(Delivered::new(event))));
}

#[test]
fn a_failed_remember_is_said_once_a_session() {
    let mut app = app_with(&[ssh("db")]);
    let before = app.notices.len();
    not_saved(&mut app, SecretKind::Password);
    let said = super::not_saved_text(SecretKind::Password, app.storage());
    let expected = if cfg!(windows) {
        "Your password could not be saved for your Windows account, so fbterm will ask for it \
         next time."
    } else {
        "Your password could not be saved in the system keychain, so fbterm will ask for it \
         next time."
    };
    assert_eq!(app.storage(), Storage::System);
    assert_eq!(said, expected);
    assert!(app.visible_notices().contains(&said));
    not_saved(&mut app, SecretKind::Password);
    not_saved(&mut app, SecretKind::KeyPassphrase);
    assert_eq!(app.notices.len(), before + 1, "once a session");
    assert_eq!(
        super::not_saved_text(SecretKind::KeyPassphrase, Storage::Vault),
        "Your key passphrase could not be saved in the encrypted vault, so fbterm will ask \
         for it next time."
    );
}

#[test]
fn requests_while_a_connects_unlock_dialog_is_open_say_so() {
    let db = ssh("db");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secrets.fbvault");
    make_vault(&path, db.id);
    let mut app = app_with(std::slice::from_ref(&db));
    let paths = app.paths.clone();
    let backend = vault_backend(&path, Arc::new(MemoryStore::new()));
    app.use_secret_backend(backend.clone());
    app.settings.secret_storage = SecretStorage::Vault;
    let wid = terminal_window(&mut app);
    let mut waiting = ask_unlock(&mut app, wid);
    assert!(app.vault.dialog.is_some());
    const BUSY: &str = "Finish unlocking saved secrets first.";

    // Settings: a storage choice and Reset vault say why nothing happens.
    settings(&mut app, SettingsMsg::Open);
    settings(
        &mut app,
        SettingsMsg::SecretStorage(SecretStorage::Keychain),
    );
    assert_eq!(
        app.secrets_section().notice.map(|n| n.text).as_deref(),
        Some(BUSY)
    );
    assert_eq!(app.settings.secret_storage, SecretStorage::Vault);
    app.vault.notice = None;
    settings(&mut app, SettingsMsg::ResetVault);
    assert_eq!(
        app.secrets_section().notice.map(|n| n.text).as_deref(),
        Some(BUSY)
    );
    assert!(path.exists());
    let d = app.vault.dialog.as_ref().unwrap();
    assert!(matches!(d.purpose, Purpose::Connect), "still the connect's");

    // The editor: a save that needs the vault writes nothing and keeps the drafts.
    let profiles_before = std::fs::read_to_string(paths.profiles()).unwrap();
    let _ = app.update(Message::Editor(EditorMsg::Open));
    let _ = app.update(Message::Editor(EditorMsg::Select(db.id)));
    let _ = app.update(Message::Editor(EditorMsg::Name("db-renamed".into())));
    let _ = app.update(Message::Editor(EditorMsg::Secret(Redacted(
        "new-pw".into(),
    ))));
    let _ = app.update(Message::Editor(EditorMsg::Save));
    let ed = &app.editor.as_ref().unwrap().1;
    assert_eq!(ed.notice.as_deref(), Some(BUSY));
    assert!(ed.dirty && app.editor_job.is_none());
    assert_eq!(
        std::fs::read_to_string(paths.profiles()).unwrap(),
        profiles_before
    );
    assert!(matches!(
        app.vault.dialog.as_ref().unwrap().purpose,
        Purpose::Connect
    ));

    // Unlocked: the connect goes on, and the save works.
    type_password(&mut app, MAIN, false);
    vault(&mut app, VaultMsg::Submit);
    assert!(waiting.try_recv().unwrap().is_some());
    let _ = app.update(Message::Editor(EditorMsg::Save));
    assert_eq!(
        backend
            .get(&db.id, SecretKind::Password)
            .unwrap()
            .as_deref(),
        Some("new-pw")
    );
}

#[test]
fn storage_choice_and_reset_wait_for_an_editor_save_even_after_it_closed() {
    let db = ssh("db");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secrets.fbvault");
    make_vault(&path, db.id);
    let mut app = app_with(std::slice::from_ref(&db));
    let backend = vault_backend(&path, Arc::new(MemoryStore::new()));
    app.use_secret_backend(backend.clone());
    app.settings.secret_storage = SecretStorage::Vault;
    backend.unlock_vault(MAIN).unwrap();
    // The confirmation of a reset is open before the editor's save starts.
    settings(&mut app, SettingsMsg::Open);
    settings(&mut app, SettingsMsg::ResetVault);
    assert!(app.vault.dialog.is_some());

    // An editor save whose secrets are still being written, from an editor closed since.
    app.blocking_inline = false;
    let _ = app.update(Message::Editor(EditorMsg::Open));
    let _ = app.update(Message::Editor(EditorMsg::Select(db.id)));
    let _ = app.update(Message::Editor(EditorMsg::Secret(Redacted(
        "new-pw".into(),
    ))));
    let _ = app.update(Message::Editor(EditorMsg::Save));
    let job = app
        .editor_job
        .expect("the save's secrets are being written");
    let _ = app.update(Message::WindowClosed(app.editor_window().unwrap()));
    assert!(app.secrets_section().busy, "the section shows it busy");
    assert!(app.secrets_section().editor_saving, "as the editor's save");

    // The open confirmation does not reset under the write.
    vault(&mut app, VaultMsg::Submit);
    let d = app.vault.dialog.as_ref().expect("still asking");
    assert_eq!(d.problem.as_deref(), Some(super::EDITOR_SAVING));
    assert!(d.busy.is_none());
    assert!(path.exists());
    vault(&mut app, VaultMsg::Cancel);

    // Neither Reset vault nor a storage choice starts.
    settings(&mut app, SettingsMsg::ResetVault);
    assert!(app.vault.dialog.is_none());
    settings(
        &mut app,
        SettingsMsg::SecretStorage(SecretStorage::Keychain),
    );
    assert!(app.vault.dialog.is_none() && app.vault.direct.is_none());
    assert_eq!(app.settings.secret_storage, SecretStorage::Vault);
    assert_eq!(backend.active(), Storage::Vault);
    assert!(path.exists());

    // Once the write ends, both work again.
    let _ = app.update(Message::EditorSecrets(
        job.seq,
        Delivered::new(Ok(Vec::new())),
    ));
    assert!(app.editor_job.is_none());
    assert!(!app.secrets_section().busy);
    settings(&mut app, SettingsMsg::ResetVault);
    assert_eq!(app.vault.dialog.as_ref().map(|d| d.kind), Some(Kind::Reset));
}
