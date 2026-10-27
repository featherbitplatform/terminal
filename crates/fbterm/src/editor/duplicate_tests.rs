//! Duplicating a profile, against a temporary config folder and in-memory or temporary-folder
//! secret storage (never the real keychain). Background work runs at once (`blocking_inline`).

use std::path::Path;
use std::sync::Arc;

use fbterm_config::{
    AuthMethod, Profile, ProfileId, ProfileKind, ProfileStore, SecretStorage, SshProfile,
};
use fbterm_secrets::{MemoryStore, SecretBackend, SecretKind, SecretStore};
use iced::window;

use super::duplicate::{DIRTY, DUPLICATED, LOCKED, NO_PROFILE, READ_ONLY};
use super::{DraftKind, EditorMsg};
use crate::app::{App, Message};
use crate::keys::Action;
use crate::redact::Redacted;
use crate::restore_tests::paths_with;
use crate::saved_secrets::VaultMsg;
use crate::tabs::{Pane, Tab, WindowState};

const MAIN: &str = "main-pw-dup-tests-5a1e";
const PW: &str = "saved-pw-dup-77c3";
const PP: &str = "saved-pp-dup-19f0";

fn ssh(name: &str, auth: AuthMethod) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        group: Some("prod".into()),
        accent: None,
        accent_style: Default::default(),
        color_scheme: None,
        kind: ProfileKind::Ssh(SshProfile {
            host: format!("{name}.example.com"),
            port: 2200,
            user: "ops".into(),
            auth,
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

fn editor(app: &mut App, m: EditorMsg) {
    let _ = app.update(Message::Editor(m));
}

fn ed(app: &App) -> &super::EditorState {
    &app.editor.as_ref().expect("the editor is open").1
}

fn names(store: &ProfileStore) -> Vec<&str> {
    store.profiles.iter().map(|p| p.name.as_str()).collect()
}

fn on_disk(app: &App) -> ProfileStore {
    ProfileStore::load(&app.paths.profiles()).unwrap()
}

/// The copy of `original` in `store`: its id, with every field checked equal but id and name.
fn copy_of(store: &ProfileStore, original: &Profile, name: &str) -> ProfileId {
    let copy = store
        .profiles
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("{name} in {:?}", names(store)));
    assert_ne!(copy.id, original.id);
    assert_eq!(
        Profile {
            id: original.id,
            name: original.name.clone(),
            ..copy.clone()
        },
        *original
    );
    copy.id
}

#[test]
fn duplicate_saves_a_copy_after_the_original_with_its_secrets_and_selects_it() {
    let a = ssh("a", AuthMethod::Password);
    let b = ssh("b", AuthMethod::Agent);
    let mut app = app_with(&[a.clone(), b.clone()]);
    app.secrets.set(&a.id, SecretKind::Password, PW).unwrap();
    editor(&mut app, EditorMsg::Open);
    editor(&mut app, EditorMsg::Select(a.id));
    editor(&mut app, EditorMsg::Duplicate);

    let disk = on_disk(&app);
    assert_eq!(&names(&disk)[..3], ["a", "a copy", "b"]);
    let copy = copy_of(&disk, &a, "a copy");
    assert_eq!(app.profiles, disk, "the app uses the saved profiles");
    assert_eq!(
        app.profiles_seen,
        std::fs::read_to_string(app.paths.profiles()).ok(),
        "our own write is not a hand edit"
    );
    assert_eq!(
        app.secrets
            .get(&copy, SecretKind::Password)
            .unwrap()
            .as_deref(),
        Some(PW)
    );
    assert_eq!(
        app.secrets
            .get(&a.id, SecretKind::Password)
            .unwrap()
            .as_deref(),
        Some(PW),
        "the original keeps its password"
    );
    let ed = ed(&app);
    assert_eq!(ed.selected, Some(copy));
    assert_eq!(ed.selected_draft().unwrap().name, "a copy");
    let order: Vec<&str> = ed.drafts().iter().map(|d| d.name.as_str()).collect();
    assert_eq!(&order[..3], ["a", "a copy", "b"]);
    assert!(!ed.dirty);
    assert!(ed.saving.is_none());
    assert_eq!(ed.notice.as_deref(), Some(DUPLICATED));

    // Again: the next free name, right after the original.
    editor(&mut app, EditorMsg::Select(a.id));
    editor(&mut app, EditorMsg::Duplicate);
    assert_eq!(
        &names(&on_disk(&app))[..4],
        ["a", "a copy 2", "a copy", "b"]
    );
}

#[test]
fn duplicate_copies_a_key_passphrase_and_nothing_for_agent_profiles() {
    let key = ssh(
        "k",
        AuthMethod::Key {
            path: Some("/keys/id_ed25519".into()),
            passphrase: true,
        },
    );
    let agent = ssh("ag", AuthMethod::Agent);
    let mut app = app_with(&[key.clone(), agent.clone()]);
    app.secrets
        .set(&key.id, SecretKind::KeyPassphrase, PP)
        .unwrap();
    // A stale password of another auth method is not part of the profile's login.
    app.secrets.set(&key.id, SecretKind::Password, PW).unwrap();
    editor(&mut app, EditorMsg::Open);
    editor(&mut app, EditorMsg::Select(key.id));
    editor(&mut app, EditorMsg::Duplicate);
    let copy = copy_of(&on_disk(&app), &key, "k copy");
    assert_eq!(
        app.secrets
            .get(&copy, SecretKind::KeyPassphrase)
            .unwrap()
            .as_deref(),
        Some(PP)
    );
    assert_eq!(app.secrets.get(&copy, SecretKind::Password).unwrap(), None);

    editor(&mut app, EditorMsg::Select(agent.id));
    editor(&mut app, EditorMsg::Duplicate);
    let copy = copy_of(&on_disk(&app), &agent, "ag copy");
    for kind in SecretKind::ALL {
        assert_eq!(app.secrets.get(&copy, kind).unwrap(), None);
    }
    assert_eq!(ed(&app).notice.as_deref(), Some(DUPLICATED));
}

#[test]
fn duplicate_is_refused_with_unsaved_changes() {
    let a = ssh("a", AuthMethod::Agent);
    let mut app = app_with(std::slice::from_ref(&a));
    let before = std::fs::read_to_string(app.paths.profiles()).unwrap();
    editor(&mut app, EditorMsg::Open);
    editor(&mut app, EditorMsg::Select(a.id));
    editor(&mut app, EditorMsg::Host("changed.example.com".into()));
    editor(&mut app, EditorMsg::Duplicate);
    assert_eq!(
        std::fs::read_to_string(app.paths.profiles()).unwrap(),
        before
    );
    let ed = ed(&app);
    assert_eq!(ed.notice.as_deref(), Some(DIRTY));
    assert!(ed.dirty, "the edits are kept");
    assert_eq!(ed.drafts().len(), app.profiles.profiles.len());
}

#[test]
fn duplicate_is_refused_while_profiles_toml_is_unreadable() {
    let a = ssh("a", AuthMethod::Agent);
    let mut app = app_with(std::slice::from_ref(&a));
    let broken = "[[profiles]\nname = 1\n";
    std::fs::write(app.paths.profiles(), broken).unwrap();
    let _ = app.update(Message::ConfigChanged(crate::watch::Changed {
        profiles: true,
        settings: false,
        schemes: false,
    }));
    assert!(app.profiles_error.is_some());
    editor(&mut app, EditorMsg::Open);
    editor(&mut app, EditorMsg::Select(a.id));
    editor(&mut app, EditorMsg::Duplicate);
    assert_eq!(
        std::fs::read_to_string(app.paths.profiles()).unwrap(),
        broken
    );
    assert_eq!(ed(&app).notice.as_deref(), Some(READ_ONLY));
    assert_eq!(ed(&app).drafts().len(), app.profiles.profiles.len());
}

/// A backend using a vault at `path` that holds `id`'s password, locked.
fn locked_vault(path: &Path, id: ProfileId) -> Arc<SecretBackend> {
    let setup = SecretBackend::with_system(
        SecretStorage::Vault,
        Box::new(MemoryStore::new()),
        path.to_path_buf(),
    );
    setup.create_vault(MAIN).unwrap();
    setup.set(&id, SecretKind::Password, PW).unwrap();
    drop(setup);
    Arc::new(SecretBackend::with_system(
        SecretStorage::Vault,
        Box::new(MemoryStore::new()),
        path.to_path_buf(),
    ))
}

#[test]
fn a_locked_vault_is_unlocked_first_and_the_secrets_follow() {
    let a = ssh("a", AuthMethod::Password);
    let mut app = app_with(std::slice::from_ref(&a));
    let dir = tempfile::tempdir().unwrap();
    let backend = locked_vault(&dir.path().join("secrets.fbvault"), a.id);
    app.use_secret_backend(backend.clone());
    editor(&mut app, EditorMsg::Open);
    editor(&mut app, EditorMsg::Select(a.id));
    let before = std::fs::read_to_string(app.paths.profiles()).unwrap();
    editor(&mut app, EditorMsg::Duplicate);
    let d = app.vault.dialog.as_ref().expect("Unlock saved secrets");
    assert_eq!(Some(d.window), app.editor_window());
    assert_eq!(
        std::fs::read_to_string(app.paths.profiles()).unwrap(),
        before,
        "nothing is written before the answer"
    );

    let _ = app.update(Message::Vault(VaultMsg::Password(Redacted(MAIN.into()))));
    let _ = app.update(Message::Vault(VaultMsg::Submit));
    assert!(app.vault.dialog.is_none());
    let copy = copy_of(&on_disk(&app), &a, "a copy");
    assert_eq!(
        backend.get(&copy, SecretKind::Password).unwrap().as_deref(),
        Some(PW)
    );
    assert_eq!(ed(&app).selected, Some(copy));
    assert_eq!(ed(&app).notice.as_deref(), Some(DUPLICATED));
}

#[test]
fn cancelling_the_unlock_duplicates_without_secrets_and_says_so() {
    let a = ssh("a", AuthMethod::Password);
    let mut app = app_with(std::slice::from_ref(&a));
    let dir = tempfile::tempdir().unwrap();
    let backend = locked_vault(&dir.path().join("secrets.fbvault"), a.id);
    app.use_secret_backend(backend.clone());
    editor(&mut app, EditorMsg::Open);
    editor(&mut app, EditorMsg::Select(a.id));
    editor(&mut app, EditorMsg::Duplicate);
    assert!(app.vault.dialog.is_some());
    let _ = app.update(Message::Vault(VaultMsg::Cancel));
    assert!(app.vault.dialog.is_none());
    let copy = copy_of(&on_disk(&app), &a, "a copy");
    assert!(!backend.vault_unlocked());
    let ed = ed(&app);
    assert_eq!(ed.selected, Some(copy));
    assert_eq!(ed.notice.as_deref(), Some(LOCKED));
    assert!(!ed.dirty && ed.saving.is_none());
}

#[test]
fn the_action_in_the_editor_duplicates_its_selection() {
    let a = ssh("a", AuthMethod::Agent);
    let b = ssh("b", AuthMethod::Agent);
    let mut app = app_with(&[a.clone(), b.clone()]);
    editor(&mut app, EditorMsg::Open);
    editor(&mut app, EditorMsg::Select(b.id));
    let wid = app.editor_window().unwrap();
    let _ = app.update(Message::Shortcut(wid, Action::DuplicateProfile));
    let copy = copy_of(&on_disk(&app), &b, "b copy");
    assert_eq!(ed(&app).selected, Some(copy));
}

#[test]
fn the_action_from_a_terminal_window_duplicates_the_focused_panes_profile() {
    let a = ssh("a", AuthMethod::Agent);
    let b = ssh("b", AuthMethod::Agent);
    let mut app = app_with(&[a.clone(), b.clone()]);
    let wid = window::Id::unique();
    let mut w = WindowState::new();
    w.push(Tab::single(Pane::new(b.id, "b", None)));
    app.windows.insert(wid, w);
    assert!(app.editor.is_none());
    let _ = app.update(Message::Shortcut(wid, Action::DuplicateProfile));
    let copy = copy_of(&on_disk(&app), &b, "b copy");
    let ed = ed(&app);
    assert_eq!(ed.selected, Some(copy), "the editor opens on the copy");
    assert_eq!(ed.notice.as_deref(), Some(DUPLICATED));
}

#[test]
fn the_action_without_a_profile_to_copy_opens_the_editor_and_says_why() {
    let a = ssh("a", AuthMethod::Agent);
    let mut app = app_with(std::slice::from_ref(&a));
    let before = std::fs::read_to_string(app.paths.profiles()).unwrap();
    let wid = window::Id::unique();
    let mut w = WindowState::new();
    // A pane whose profile is no longer saved.
    w.push(Tab::single(Pane::new(ProfileId::new(), "gone", None)));
    app.windows.insert(wid, w);
    let _ = app.update(Message::Shortcut(wid, Action::DuplicateProfile));
    assert_eq!(
        std::fs::read_to_string(app.paths.profiles()).unwrap(),
        before
    );
    assert_eq!(ed(&app).notice.as_deref(), Some(NO_PROFILE));
}

#[test]
fn the_copy_draft_matches_the_original_draft() {
    let a = ssh(
        "a",
        AuthMethod::Key {
            path: Some("/keys/id".into()),
            passphrase: true,
        },
    );
    let mut app = app_with(std::slice::from_ref(&a));
    editor(&mut app, EditorMsg::Open);
    editor(&mut app, EditorMsg::Select(a.id));
    editor(&mut app, EditorMsg::Duplicate);
    let ed = ed(&app);
    let copy = ed.selected_draft().unwrap();
    let original = ed.draft(a.id).unwrap();
    let (DraftKind::Ssh(c), DraftKind::Ssh(o)) = (&copy.kind, &original.kind) else {
        panic!("ssh drafts");
    };
    assert_eq!(
        (
            &c.host,
            &c.port,
            &c.user,
            c.auth,
            &c.key_path,
            c.key_passphrase
        ),
        (
            &o.host,
            &o.port,
            &o.user,
            o.auth,
            &o.key_path,
            o.key_passphrase
        )
    );
    assert_eq!(copy.group, original.group);
}
