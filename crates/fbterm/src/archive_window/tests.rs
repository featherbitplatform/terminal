//! The archive window against a temporary config folder, an in-memory secret store and
//! temporary key folders (never `~/.ssh`). File dialogs never open: their answers are sent as
//! the messages the dialogs would send. Background work runs at once (`blocking_inline`).

use std::path::{Path, PathBuf};

use fbterm_config::profile::{
    AuthMethod, Profile, ProfileKind, SshProfile, StartMode, TunnelDef, TunnelKind,
};
use fbterm_config::{ProfileId, ProfileStore};
use fbterm_secrets::SecretKind;
use fbterm_secrets::archive::{self, Archive, ArchiveKind, ArchivedKey, ArchivedSecret};
use zeroize::Zeroizing;

use super::{
    ArchiveMode, ArchiveMsg, ArchiveState, Busy, ExportSummary, ImportSummary, MISMATCH, NEW,
    NO_PASSWORD, NOTHING_SELECTED, RowChoice, default_file_name, exported_text, imported_text,
    preview_rows, write_key_files,
};
use crate::app::{App, Message};
use crate::archive_import::NewKeyFile;
use crate::keys::Action;
use crate::palette::PaletteItem;
use crate::redact::Redacted;
use crate::restore_tests::paths_with;

const PASSWORD: &str = "archive-pw-3f9a";
const SAVED: &str = "saved-pw-71bc";
const KEY_BYTES: &[u8] = b"-----BEGIN OPENSSH PRIVATE KEY-----\nsample-5e21\n";

fn ssh(name: &str, auth: AuthMethod, jump: Vec<ProfileId>) -> Profile {
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
            auth,
            jump,
            tunnels: Vec::new(),
        }),
    }
}

fn tunnel(name: &str) -> TunnelDef {
    TunnelDef {
        name: name.into(),
        kind: TunnelKind::Local {
            bind: "127.0.0.1".into(),
            port: 5432,
            host: "localhost".into(),
            host_port: 5432,
        },
        start: StartMode::OnDemand,
    }
}

fn app_with(profiles: &[Profile]) -> App {
    let (mut app, _) = App::build(paths_with(profiles), true);
    app.blocking_inline = true;
    app
}

fn send(app: &mut App, m: ArchiveMsg) {
    let _ = app.update(Message::Archive(m));
}

fn state(app: &App) -> &ArchiveState {
    &app.archive_win.as_ref().expect("the archive window").1
}

fn open(app: &mut App, action: Action) {
    let _ = app.update(Message::Shortcut(iced::window::Id::unique(), action));
}

fn temp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// Seal `archive` into a file in `dir`.
fn archive_file(dir: &Path, archive: &Archive) -> PathBuf {
    let path = dir.join("sample.fbprofiles");
    std::fs::write(&path, archive::seal(archive, PASSWORD).unwrap()).unwrap();
    path
}

/// Choose `path` in the open dialog, type the password and open it.
fn open_archive(app: &mut App, path: &Path, password: &str) {
    send(app, ArchiveMsg::ChooseFile);
    let seq = state(app).dialog.expect("the open dialog");
    send(app, ArchiveMsg::FileChosen(seq, Some(path.to_path_buf())));
    send(app, ArchiveMsg::ImportPassword(Redacted(password.into())));
    send(app, ArchiveMsg::Open);
}

fn key_folder(app: &mut App, dir: &Path) {
    send(app, ArchiveMsg::ChooseKeyFolder);
    let seq = state(app).dialog.expect("the folder dialog");
    send(
        app,
        ArchiveMsg::KeyFolderChosen(seq, Some(dir.to_path_buf())),
    );
}

fn key_path(store: &ProfileStore, name: &str) -> Option<PathBuf> {
    let p = store.profiles.iter().find(|p| p.name == name).unwrap();
    match &p.kind {
        ProfileKind::Ssh(SshProfile {
            auth: AuthMethod::Key { path, .. },
            ..
        }) => path.clone(),
        _ => panic!("{name} is not a key profile"),
    }
}

#[test]
fn actions_open_the_window() {
    let mut app = app_with(&[ssh("db-host", AuthMethod::Agent, Vec::new())]);
    assert!(app.archive_window().is_none());
    open(&mut app, Action::ExportProfiles);
    let id = app.archive_window().expect("opened");
    assert_eq!(state(&app).mode, ArchiveMode::Export);
    assert_eq!(app.title(id), "Export profiles \u{b7} fbterm");

    // The other action switches the same window to its mode.
    open(&mut app, Action::ImportProfiles);
    assert_eq!(app.archive_window(), Some(id));
    assert_eq!(state(&app).mode, ArchiveMode::Import);
    assert_eq!(app.title(id), "Import profiles \u{b7} fbterm");

    // The editor's footer buttons open it too.
    send(&mut app, ArchiveMsg::Close);
    let _ = app.update(Message::WindowClosed(id));
    assert!(app.archive_window().is_none());
    let _ = app.update(Message::Editor(crate::editor::EditorMsg::Export));
    assert_eq!(state(&app).mode, ArchiveMode::Export);
    let _ = app.update(Message::Editor(crate::editor::EditorMsg::Import));
    assert_eq!(state(&app).mode, ArchiveMode::Import);

    // The palette lists both actions.
    let labels: Vec<String> = app
        .palette_items()
        .iter()
        .filter_map(|i| match i {
            PaletteItem::Action {
                action: Action::ExportProfiles | Action::ImportProfiles,
                label,
            } => Some(label.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        ["Export profiles\u{2026}", "Import profiles\u{2026}"]
    );
}

#[test]
fn export_requires_matching_password() {
    let db = ssh("db-host", AuthMethod::Agent, Vec::new());
    let mut app = app_with(std::slice::from_ref(&db));
    open(&mut app, Action::ExportProfiles);

    send(&mut app, ArchiveMsg::Export);
    assert_eq!(state(&app).export.problem.as_deref(), Some(NO_PASSWORD));
    assert_eq!(state(&app).dialog, None, "no save dialog");

    send(&mut app, ArchiveMsg::Password(Redacted(PASSWORD.into())));
    send(
        &mut app,
        ArchiveMsg::Confirm(Redacted("something else".into())),
    );
    send(&mut app, ArchiveMsg::Export);
    assert_eq!(state(&app).export.problem.as_deref(), Some(MISMATCH));
    assert_eq!(state(&app).dialog, None);

    // Nothing checked.
    send(&mut app, ArchiveMsg::Confirm(Redacted(PASSWORD.into())));
    let all: Vec<ProfileId> = app.profiles.profiles.iter().map(|p| p.id).collect();
    for id in &all {
        send(&mut app, ArchiveMsg::Toggle(*id, false));
    }
    send(&mut app, ArchiveMsg::Export);
    assert_eq!(
        state(&app).export.problem.as_deref(),
        Some(NOTHING_SELECTED)
    );
    assert_eq!(state(&app).dialog, None);

    send(&mut app, ArchiveMsg::Toggle(db.id, true));
    send(&mut app, ArchiveMsg::Export);
    assert_eq!(state(&app).export.problem, None);
    assert!(state(&app).dialog.is_some(), "the save dialog is open");
    // One dialog at a time.
    let seq = state(&app).dialog;
    send(&mut app, ArchiveMsg::Export);
    assert_eq!(state(&app).dialog, seq);
    // Cancelled: nothing happens, the form stays.
    send(&mut app, ArchiveMsg::SaveChosen(seq.unwrap(), None));
    assert_eq!(state(&app).dialog, None);
    assert_eq!(state(&app).busy, None);
    assert_eq!(state(&app).export.password.as_str(), PASSWORD);
}

#[test]
fn export_lists_jump_hosts_and_warnings() {
    let bastion = ssh("bastion", AuthMethod::Agent, Vec::new());
    let db = ssh("db-host", AuthMethod::Agent, vec![bastion.id]);
    let mut app = app_with(&[db.clone(), bastion.clone()]);
    open(&mut app, Action::ExportProfiles);
    send(&mut app, ArchiveMsg::Toggle(bastion.id, false));
    let rows: Vec<_> = super::export_groups(&app.profiles, &state(&app).export.unchecked)
        .into_iter()
        .flat_map(|(_, rows)| rows)
        .filter(|r| r.name == "bastion" || r.name == "db-host")
        .map(|r| (r.name, r.checked, r.via_jump))
        .collect();
    assert_eq!(
        rows,
        [
            ("db-host".to_string(), true, false),
            ("bastion".to_string(), false, true)
        ]
    );

    // A shared archive leaves the local shell out, and says so.
    send(&mut app, ArchiveMsg::Kind(ArchiveKind::Shared));
    let warnings = state(&app).export.preview.clone().unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].starts_with("Local profiles are not part of a shared archive, so "),
        "{warnings:?}"
    );
}

#[test]
fn export_writes_openable_archive() {
    let db = ssh("db-host", AuthMethod::Password, Vec::new());
    let mut app = app_with(std::slice::from_ref(&db));
    app.secrets
        .set(&db.id, SecretKind::Password, SAVED)
        .unwrap();
    let dir = temp();
    let path = dir.path().join(default_file_name(
        chrono::NaiveDate::from_ymd_opt(2026, 10, 27).unwrap(),
    ));

    open(&mut app, Action::ExportProfiles);
    send(&mut app, ArchiveMsg::Password(Redacted(PASSWORD.into())));
    send(&mut app, ArchiveMsg::Confirm(Redacted(PASSWORD.into())));
    send(&mut app, ArchiveMsg::Export);
    let seq = state(&app).dialog.unwrap();
    send(&mut app, ArchiveMsg::SaveChosen(seq, Some(path.clone())));

    let opened = archive::open(&std::fs::read(&path).unwrap(), PASSWORD).unwrap();
    assert_eq!(opened.kind, ArchiveKind::Full);
    assert!(opened.profiles.iter().any(|p| p.id == db.id));
    assert_eq!(opened.secrets.len(), 1);
    assert_eq!(opened.secrets[0].value.as_str(), SAVED);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let s = state(&app);
    assert_eq!(s.busy, None);
    let notice = s.export.notice.as_ref().unwrap();
    assert!(!notice.error);
    assert_eq!(
        notice.text,
        format!(
            "Exported {} profiles and 1 saved secret to fbterm-profiles-2026-10-27.fbprofiles.",
            opened.profiles.len()
        )
    );
    // The passwords are gone once used.
    assert!(s.export.password.is_empty());
    assert!(s.export.confirm.is_empty());
}

#[test]
fn background_results_for_another_job_are_ignored() {
    let db = ssh("db-host", AuthMethod::Agent, Vec::new());
    let mut app = app_with(std::slice::from_ref(&db));
    app.blocking_inline = false;
    open(&mut app, Action::ExportProfiles);
    send(&mut app, ArchiveMsg::Password(Redacted(PASSWORD.into())));
    send(&mut app, ArchiveMsg::Confirm(Redacted(PASSWORD.into())));
    send(&mut app, ArchiveMsg::Export);
    let seq = state(&app).dialog.unwrap();
    let dir = temp();
    send(
        &mut app,
        ArchiveMsg::SaveChosen(seq, Some(dir.path().join("a.fbprofiles"))),
    );
    let (job, busy) = state(&app).busy.unwrap();
    assert_eq!(busy, Busy::Sealing);
    // While it runs the form cannot start another.
    send(&mut app, ArchiveMsg::Export);
    assert_eq!(state(&app).dialog, None);

    let summary = ExportSummary {
        path: dir.path().join("a.fbprofiles"),
        profiles: 1,
        secrets: 0,
        keys: 0,
        schemes: 0,
        warnings: Vec::new(),
    };
    // Another job's result changes nothing.
    send(
        &mut app,
        ArchiveMsg::Exported(job + 100, Ok(summary.clone())),
    );
    assert_eq!(state(&app).busy, Some((job, Busy::Sealing)));
    assert_eq!(state(&app).export.notice, None);

    // Closed meanwhile: the result is dropped, and a new window does not take it.
    let id = app.archive_window().unwrap();
    send(&mut app, ArchiveMsg::Close);
    let _ = app.update(Message::WindowClosed(id));
    open(&mut app, Action::ExportProfiles);
    send(&mut app, ArchiveMsg::Exported(job, Ok(summary)));
    assert_eq!(state(&app).export.notice, None);
    assert_eq!(state(&app).busy, None);
}

/// A full archive: `db-host` (a password profile with its saved password, matching the local
/// one by name), `app-box` (new, a key profile with its key file, jumping through `bastion`),
/// `bastion` (new), `app-box` with two tunnels.
fn sample_archive() -> Archive {
    // Another id than the local db-host: it matches by name.
    let db = ssh("db-host", AuthMethod::Password, Vec::new());
    let bastion = ssh("bastion", AuthMethod::Agent, Vec::new());
    let mut app_box = ssh(
        "app-box",
        AuthMethod::Key {
            path: Some("/home/someone/.ssh/id_sample".into()),
            passphrase: false,
        },
        vec![bastion.id],
    );
    if let ProfileKind::Ssh(s) = &mut app_box.kind {
        s.tunnels = vec![tunnel("pg"), tunnel("web")];
    }
    let mut a = Archive::new(ArchiveKind::Full, "2026-10-27T10:00:00Z");
    a.secrets = vec![ArchivedSecret {
        profile: db.id,
        kind: SecretKind::Password,
        value: Zeroizing::new(SAVED.into()),
    }];
    a.keys = vec![ArchivedKey {
        file_name: "id_sample".into(),
        bytes: Zeroizing::new(KEY_BYTES.to_vec()),
        profiles: vec![app_box.id],
    }];
    a.profiles = vec![db, app_box, bastion];
    a
}

#[test]
fn import_preview_lists_entries_and_clashes() {
    let db = ssh("db-host", AuthMethod::Password, Vec::new());
    let mut app = app_with(std::slice::from_ref(&db));
    let dir = temp();
    let file = archive_file(dir.path(), &sample_archive());
    open(&mut app, Action::ImportProfiles);

    // A wrong password says so and opens nothing.
    open_archive(&mut app, &file, "not it");
    assert!(state(&app).import.opened.is_none());
    assert_eq!(
        state(&app).import.problem.as_deref(),
        Some("Wrong password, or the file is not an fbterm archive.")
    );
    assert!(state(&app).import.password.is_empty(), "cleared once used");

    send(
        &mut app,
        ArchiveMsg::ImportPassword(Redacted(PASSWORD.into())),
    );
    send(&mut app, ArchiveMsg::Open);
    assert_eq!(state(&app).import.problem, None);
    assert!(state(&app).import.password.is_empty());
    let opened = state(&app).import.opened.as_ref().expect("a preview");
    let rows = preview_rows(&opened.plan, &app.profiles);
    let summary: Vec<_> = rows
        .iter()
        .map(|r| {
            (
                r.name.as_str(),
                r.host.as_str(),
                r.jumps.as_str(),
                r.tunnels,
                r.matched.as_str(),
                r.choice,
                r.choices.clone(),
            )
        })
        .collect();
    use RowChoice::*;
    assert_eq!(
        summary,
        [
            (
                "db-host",
                "ops@db-host.example.com",
                "",
                0,
                "Matches db-host",
                Skip,
                vec![Skip, Overwrite, KeepBoth]
            ),
            (
                "app-box",
                "ops@app-box.example.com",
                "bastion",
                2,
                NEW,
                Import,
                vec![Import, Skip]
            ),
            (
                "bastion",
                "ops@bastion.example.com",
                "",
                0,
                NEW,
                Import,
                vec![Import, Skip]
            ),
        ]
    );
    // app-box's key file is in the archive and takes the place of its path: no path shown.
    assert!(rows.iter().all(|r| r.key_path.is_none()));
    assert_eq!(
        super::archive_summary(&opened.plan, ArchiveKind::Full),
        "Full archive. Includes saved secrets and private key files."
    );

    // Skipping the new hop blocks the profile that jumps through it, by name.
    send(&mut app, ArchiveMsg::Choice(2, Skip));
    let opened = state(&app).import.opened.as_ref().unwrap();
    let rows = preview_rows(&opened.plan, &app.profiles);
    assert_eq!(
        rows[1].error.as_deref(),
        Some("It jumps through bastion, which is not imported.")
    );
    assert_eq!(rows[2].choice, Skip);
}

#[test]
fn import_applies_choices_and_writes_keys_0600() {
    let db = ssh("db-host", AuthMethod::Password, Vec::new());
    let mut app = app_with(std::slice::from_ref(&db));
    let paths = app.paths.clone();
    let dir = temp();
    let file = archive_file(dir.path(), &sample_archive());
    // A key folder that does not exist yet: made at import time, private.
    let keys = dir.path().join("keys");

    open(&mut app, Action::ImportProfiles);
    open_archive(&mut app, &file, PASSWORD);
    key_folder(&mut app, &keys);
    assert!(!keys.exists(), "nothing is written before Import");
    // Overwrite the local db-host with the archived one (and its saved password).
    send(&mut app, ArchiveMsg::Choice(0, RowChoice::Overwrite));
    send(&mut app, ArchiveMsg::Import);

    let s = state(&app);
    assert_eq!(s.import.problem, None);
    assert_eq!(
        s.import.notice.as_ref().map(|n| n.text.as_str()),
        Some("Imported 3 profiles (1 replaced), 1 key file and 1 saved secret.")
    );
    assert!(
        s.import.opened.is_none(),
        "the archive is dropped once imported"
    );

    // The key file: private, the archived bytes, used by app-box.
    let key = keys.join("id_sample");
    assert_eq!(std::fs::read(&key).unwrap(), KEY_BYTES);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&key), 0o600);
        assert_eq!(mode(&keys), 0o700);
    }
    assert_eq!(key_path(&app.profiles, "app-box"), Some(key.clone()));

    // profiles.toml holds the import; our own write is not taken for a hand edit.
    let on_disk = ProfileStore::load(&paths.profiles()).unwrap();
    assert_eq!(on_disk, app.profiles);
    assert_eq!(
        app.profiles_seen,
        std::fs::read_to_string(paths.profiles()).ok()
    );
    let names: Vec<&str> = on_disk.profiles.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"app-box") && names.contains(&"bastion"));
    assert_eq!(
        names.iter().filter(|n| **n == "db-host").count(),
        1,
        "replaced, not added"
    );
    // The saved password went to the local db-host (Overwrite keeps its id).
    assert_eq!(
        app.secrets
            .get(&db.id, SecretKind::Password)
            .unwrap()
            .as_deref(),
        Some(SAVED)
    );
}

#[test]
fn key_files_never_replace_a_file() {
    let dir = temp();
    let taken = dir.path().join("id_taken");
    std::fs::write(&taken, b"mine").unwrap();
    let files = vec![
        NewKeyFile {
            path: dir.path().join("id_first"),
            bytes: Zeroizing::new(KEY_BYTES.to_vec()),
        },
        NewKeyFile {
            path: taken.clone(),
            bytes: Zeroizing::new(KEY_BYTES.to_vec()),
        },
    ];
    let err = write_key_files(&files).unwrap_err();
    assert_eq!(
        err,
        "A file named id_taken appeared in the key folder meanwhile, so nothing was imported. Open the archive again to import."
    );
    assert_eq!(std::fs::read(&taken).unwrap(), b"mine", "never replaced");
    assert!(
        !dir.path().join("id_first").exists(),
        "what this import wrote is taken back"
    );

    // A free name is written, private.
    let n = write_key_files(&files[..1]).unwrap();
    assert_eq!(n, 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("id_first"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn import_refused_when_profiles_read_only() {
    let db = ssh("db-host", AuthMethod::Password, Vec::new());
    let mut app = app_with(std::slice::from_ref(&db));
    let paths = app.paths.clone();
    let dir = temp();
    let file = archive_file(dir.path(), &sample_archive());
    let broken = "[[profiles]\nname = 1\n";
    std::fs::write(paths.profiles(), broken).unwrap();
    let _ = app.update(Message::ConfigChanged(crate::watch::Changed {
        profiles: true,
        settings: false,
        schemes: false,
    }));
    assert!(app.profiles_error.is_some());

    open(&mut app, Action::ImportProfiles);
    open_archive(&mut app, &file, PASSWORD);
    let keys = dir.path().join("keys");
    key_folder(&mut app, &keys);
    send(&mut app, ArchiveMsg::Import);
    assert_eq!(
        state(&app).import.problem.as_deref(),
        Some("Profiles are read-only until profiles.toml is fixed, so nothing was imported.")
    );
    assert_eq!(std::fs::read_to_string(paths.profiles()).unwrap(), broken);
    assert!(!keys.exists());
    assert_eq!(app.secrets.get(&db.id, SecretKind::Password).unwrap(), None);
}

#[test]
fn import_refuses_a_stale_preview() {
    let db = ssh("db-host", AuthMethod::Password, Vec::new());
    let mut app = app_with(std::slice::from_ref(&db));
    let paths = app.paths.clone();
    let dir = temp();
    let file = archive_file(dir.path(), &sample_archive());
    open(&mut app, Action::ImportProfiles);
    open_archive(&mut app, &file, PASSWORD);
    key_folder(&mut app, &dir.path().join("keys"));

    // profiles.toml changes on disk after the archive was opened.
    let mut changed = app.profiles.clone();
    changed
        .profiles
        .push(ssh("added-meanwhile", AuthMethod::Agent, Vec::new()));
    changed.save(&paths.profiles()).unwrap();
    let _ = app.update(Message::ConfigChanged(crate::watch::Changed {
        profiles: true,
        settings: false,
        schemes: false,
    }));
    let before = std::fs::read_to_string(paths.profiles()).unwrap();
    send(&mut app, ArchiveMsg::Import);
    assert_eq!(
        state(&app).import.problem.as_deref(),
        Some("The profiles changed since the archive was opened. Open it again to import.")
    );
    assert_eq!(std::fs::read_to_string(paths.profiles()).unwrap(), before);
}

#[test]
fn closing_clears_the_passwords() {
    let mut app = app_with(&[]);
    open(&mut app, Action::ExportProfiles);
    send(&mut app, ArchiveMsg::Password(Redacted(PASSWORD.into())));
    let id = app.archive_window().unwrap();
    let _ = app.update(Message::WindowClosed(id));
    assert!(app.archive_win.is_none());
    open(&mut app, Action::ExportProfiles);
    assert!(state(&app).export.password.is_empty());
}

#[test]
fn passwords_never_show_in_debug_output() {
    let mut app = app_with(&[]);
    open(&mut app, Action::ExportProfiles);
    send(&mut app, ArchiveMsg::Password(Redacted(PASSWORD.into())));
    send(&mut app, ArchiveMsg::Confirm(Redacted(PASSWORD.into())));
    send(
        &mut app,
        ArchiveMsg::ImportPassword(Redacted(PASSWORD.into())),
    );
    let shown = format!("{:?}", state(&app));
    assert!(!shown.contains(PASSWORD), "{shown}");
    let shown = format!("{:?}", ArchiveMsg::Password(Redacted(PASSWORD.into())));
    assert!(!shown.contains(PASSWORD), "{shown}");
}

#[test]
fn copy() {
    assert_eq!(
        default_file_name(chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()),
        "fbterm-profiles-2026-01-05.fbprofiles"
    );
    let s = |added, replaced, keys, schemes, secrets| ImportSummary {
        added,
        replaced,
        keys,
        schemes,
        secrets,
        secret_failures: Vec::new(),
    };
    assert_eq!(imported_text(&s(1, 0, 0, 0, 0)), "Imported 1 profile.");
    assert_eq!(
        imported_text(&s(2, 1, 2, 1, 2)),
        "Imported 3 profiles (1 replaced), 2 key files, 1 color scheme and 2 saved secrets."
    );
    let e = ExportSummary {
        path: PathBuf::from("/x/a.fbprofiles"),
        profiles: 1,
        secrets: 0,
        keys: 0,
        schemes: 0,
        warnings: Vec::new(),
    };
    assert_eq!(exported_text(&e), "Exported 1 profile to a.fbprofiles.");
}

/// Opens `sample_archive` (in `dir`) in the import window, its key folder in `dir/keys`, and
/// chooses Overwrite for the local db-host.
fn ready_import(app: &mut App, dir: &Path, archive: &Archive) -> PathBuf {
    let file = archive_file(dir, archive);
    open(app, Action::ImportProfiles);
    open_archive(app, &file, PASSWORD);
    let keys = dir.join("keys");
    key_folder(app, &keys);
    send(app, ArchiveMsg::Choice(0, RowChoice::Overwrite));
    keys
}

#[test]
fn import_refused_while_the_editor_has_unsaved_changes() {
    let db = ssh("db-host", AuthMethod::Password, Vec::new());
    let mut app = app_with(std::slice::from_ref(&db));
    let paths = app.paths.clone();
    let dir = temp();
    let keys = ready_import(&mut app, dir.path(), &sample_archive());
    let _ = app.update(Message::Editor(crate::editor::EditorMsg::Open));
    let _ = app.update(Message::Editor(crate::editor::EditorMsg::Select(db.id)));
    let _ = app.update(Message::Editor(crate::editor::EditorMsg::Name(
        "db-edited".into(),
    )));
    assert!(app.editor.as_ref().unwrap().1.dirty);
    let before = std::fs::read_to_string(paths.profiles()).unwrap();

    send(&mut app, ArchiveMsg::Import);
    assert_eq!(
        state(&app).import.problem.as_deref(),
        Some(super::EDITOR_DIRTY)
    );
    assert!(state(&app).import.opened.is_some(), "kept to import later");
    assert_eq!(std::fs::read_to_string(paths.profiles()).unwrap(), before);
    assert!(!keys.exists());
    assert_eq!(app.secrets.get(&db.id, SecretKind::Password).unwrap(), None);
    assert_eq!(
        app.editor
            .as_ref()
            .unwrap()
            .1
            .selected_draft()
            .unwrap()
            .name,
        "db-edited",
        "the edits are untouched"
    );
}

#[test]
fn import_refused_when_color_schemes_changed_since_opening() {
    let db = ssh("db-host", AuthMethod::Password, Vec::new());
    let mut app = app_with(std::slice::from_ref(&db));
    let paths = app.paths.clone();
    let dir = temp();
    let mut scheme = fbterm_config::schemes::builtins().remove(2);
    scheme.id = format!("user-{}", ProfileId::new());
    scheme.name = "Dusk".into();
    scheme.builtin = false;
    let mut archive = sample_archive();
    archive.profiles[0].color_scheme = Some(scheme.id.clone());
    archive.schemes = vec![scheme.clone()];
    let keys = ready_import(&mut app, dir.path(), &archive);
    let before = std::fs::read_to_string(paths.profiles()).unwrap();

    // A scheme file with that id is saved after the preview was made.
    let mut mine = scheme.clone();
    mine.name = "Mine".into();
    fbterm_config::schemes::save_user(&paths.schemes(), &mine).unwrap();
    send(&mut app, ArchiveMsg::Import);
    assert_eq!(
        state(&app).import.problem.as_deref(),
        Some(super::SCHEMES_STALE)
    );
    assert_eq!(std::fs::read_to_string(paths.profiles()).unwrap(), before);
    assert!(!keys.exists());
    let on_disk =
        std::fs::read_to_string(paths.schemes().join(format!("{}.toml", scheme.id))).unwrap();
    assert!(on_disk.contains("Mine"), "the local scheme is untouched");
    assert_eq!(app.secrets.get(&db.id, SecretKind::Password).unwrap(), None);
}

/// A system store that counts single and batch saves.
#[derive(Default)]
struct Counting {
    inner: fbterm_secrets::MemoryStore,
    sets: std::sync::atomic::AtomicUsize,
    batches: std::sync::atomic::AtomicUsize,
}

impl fbterm_secrets::SecretStore for Counting {
    fn get(
        &self,
        id: &ProfileId,
        kind: SecretKind,
    ) -> Result<Option<String>, fbterm_secrets::SecretError> {
        self.inner.get(id, kind)
    }
    fn set(
        &self,
        id: &ProfileId,
        kind: SecretKind,
        value: &str,
    ) -> Result<(), fbterm_secrets::SecretError> {
        self.sets.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.set(id, kind, value)
    }
    fn remove(&self, id: &ProfileId) -> Result<(), fbterm_secrets::SecretError> {
        self.inner.remove(id)
    }
    fn remove_kind(
        &self,
        id: &ProfileId,
        kind: SecretKind,
    ) -> Result<(), fbterm_secrets::SecretError> {
        self.inner.remove_kind(id, kind)
    }
    fn set_many(
        &self,
        items: &[fbterm_secrets::SecretEntry],
    ) -> Result<(), fbterm_secrets::SecretError> {
        self.batches
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.set_many(items)
    }
}

/// Lets a test keep looking at a store it handed to a backend.
struct Shared(std::sync::Arc<Counting>);

impl fbterm_secrets::SecretStore for Shared {
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
    fn set_many(
        &self,
        items: &[fbterm_secrets::SecretEntry],
    ) -> Result<(), fbterm_secrets::SecretError> {
        self.0.set_many(items)
    }
}

#[test]
fn imported_secrets_are_stored_in_one_batch() {
    use fbterm_secrets::SecretStore;
    let db = ssh("db-host", AuthMethod::Password, Vec::new());
    let web = ssh("web", AuthMethod::Password, Vec::new());
    let mut app = app_with(std::slice::from_ref(&db));
    let dir = temp();
    let counting = std::sync::Arc::new(Counting::default());
    app.use_secret_backend(std::sync::Arc::new(
        fbterm_secrets::SecretBackend::with_system(
            fbterm_config::SecretStorage::Keychain,
            Box::new(Shared(counting.clone())),
            dir.path().join("secrets.fbvault"),
        ),
    ));
    let mut archive = sample_archive();
    archive.secrets.push(ArchivedSecret {
        profile: web.id,
        kind: SecretKind::Password,
        value: Zeroizing::new("web-pw".into()),
    });
    archive.profiles.push(web.clone());
    ready_import(&mut app, dir.path(), &archive);
    send(&mut app, ArchiveMsg::Import);
    assert_eq!(
        state(&app).import.notice.as_ref().map(|n| n.text.as_str()),
        Some("Imported 4 profiles (1 replaced), 1 key file and 2 saved secrets.")
    );
    assert_eq!(state(&app).busy, None);
    use std::sync::atomic::Ordering::SeqCst;
    assert_eq!(counting.batches.load(SeqCst), 1, "one batch");
    assert_eq!(counting.sets.load(SeqCst), 0, "no single saves");
    assert_eq!(
        counting
            .get(&web.id, SecretKind::Password)
            .unwrap()
            .as_deref(),
        Some("web-pw")
    );
}

#[test]
fn import_into_a_locked_vault_unlocks_first() {
    use fbterm_secrets::SecretStore;
    let db = ssh("db-host", AuthMethod::Password, Vec::new());
    let mut app = app_with(std::slice::from_ref(&db));
    let paths = app.paths.clone();
    let dir = temp();
    let vault = dir.path().join("secrets.fbvault");
    let make = fbterm_secrets::SecretBackend::with_system(
        fbterm_config::SecretStorage::Vault,
        Box::new(fbterm_secrets::MemoryStore::new()),
        vault.clone(),
    );
    make.create_vault("main-pw-import").unwrap();
    drop(make);
    let backend = std::sync::Arc::new(fbterm_secrets::SecretBackend::with_system(
        fbterm_config::SecretStorage::Vault,
        Box::new(fbterm_secrets::MemoryStore::new()),
        vault.clone(),
    ));
    app.use_secret_backend(backend.clone());
    let keys = ready_import(&mut app, dir.path(), &sample_archive());
    let before = std::fs::read_to_string(paths.profiles()).unwrap();

    send(&mut app, ArchiveMsg::Import);
    let d = app.vault.dialog.as_ref().expect("unlock first");
    assert_eq!(d.kind, crate::saved_secrets::Kind::Unlock);
    assert_eq!(Some(d.window), app.archive_window());
    assert_eq!(std::fs::read_to_string(paths.profiles()).unwrap(), before);
    assert!(!keys.exists(), "nothing written before the vault opens");
    assert!(state(&app).import.opened.is_some());

    let _ = app.update(Message::Vault(crate::saved_secrets::VaultMsg::Password(
        Redacted("main-pw-import".into()),
    )));
    let _ = app.update(Message::Vault(crate::saved_secrets::VaultMsg::Submit));
    assert!(app.vault.dialog.is_none());
    assert_eq!(
        state(&app).import.notice.as_ref().map(|n| n.text.as_str()),
        Some("Imported 3 profiles (1 replaced), 1 key file and 1 saved secret.")
    );
    // In the vault file: a fresh store opened with the main password reads it.
    let fresh = fbterm_secrets::VaultStore::new();
    fresh.unlock(&vault, "main-pw-import").unwrap();
    assert_eq!(
        fresh.get(&db.id, SecretKind::Password).unwrap().as_deref(),
        Some(SAVED)
    );
}

/// A system storage that is not there at all (no Secret Service).
struct NoService;
impl fbterm_secrets::SecretStore for NoService {
    fn get(
        &self,
        _: &ProfileId,
        _: SecretKind,
    ) -> Result<Option<String>, fbterm_secrets::SecretError> {
        Err(fbterm_secrets::SecretError::Unavailable(
            "no service".into(),
        ))
    }
    fn set(
        &self,
        _: &ProfileId,
        _: SecretKind,
        _: &str,
    ) -> Result<(), fbterm_secrets::SecretError> {
        Err(fbterm_secrets::SecretError::Unavailable(
            "no service".into(),
        ))
    }
    fn remove(&self, _: &ProfileId) -> Result<(), fbterm_secrets::SecretError> {
        Err(fbterm_secrets::SecretError::Unavailable(
            "no service".into(),
        ))
    }
    fn remove_kind(&self, _: &ProfileId, _: SecretKind) -> Result<(), fbterm_secrets::SecretError> {
        Err(fbterm_secrets::SecretError::Unavailable(
            "no service".into(),
        ))
    }
}

#[test]
fn an_import_checks_automatic_storage_before_writing() {
    // Automatic, nothing checked yet, and no system storage: the import is the first thing
    // to need secrets. It must find out first (the vault, with no main password yet) and
    // ask for one before writing anything, not fail its secrets after profiles.toml.
    use fbterm_secrets::SecretStore;
    let db = ssh("db-host", AuthMethod::Password, Vec::new());
    let mut app = app_with(std::slice::from_ref(&db));
    let paths = app.paths.clone();
    let dir = temp();
    let vault = dir.path().join("secrets.fbvault");
    let backend = std::sync::Arc::new(
        fbterm_secrets::SecretBackend::with_system(
            fbterm_config::SecretStorage::Auto,
            Box::new(NoService),
            vault.clone(),
        )
        .with_probe_retry_delay(std::time::Duration::ZERO),
    );
    app.use_secret_backend(backend.clone());
    let keys = ready_import(&mut app, dir.path(), &sample_archive());
    let before = std::fs::read_to_string(paths.profiles()).unwrap();

    send(&mut app, ArchiveMsg::Import);
    assert_eq!(backend.active(), fbterm_secrets::Storage::Vault);
    assert_eq!(
        app.settings.secret_storage,
        fbterm_config::SecretStorage::Vault
    );
    let d = app.vault.dialog.as_ref().expect("a main password first");
    assert_eq!(d.kind, crate::saved_secrets::Kind::Set);
    assert_eq!(std::fs::read_to_string(paths.profiles()).unwrap(), before);
    assert!(!keys.exists(), "nothing written before the vault is made");

    for m in [
        crate::saved_secrets::VaultMsg::Password(Redacted("main-pw-auto".into())),
        crate::saved_secrets::VaultMsg::Confirm(Redacted("main-pw-auto".into())),
        crate::saved_secrets::VaultMsg::Submit,
    ] {
        let _ = app.update(Message::Vault(m));
    }
    assert_eq!(
        state(&app).import.notice.as_ref().map(|n| n.text.as_str()),
        Some("Imported 3 profiles (1 replaced), 1 key file and 1 saved secret.")
    );
    assert_eq!(
        backend
            .get(&db.id, SecretKind::Password)
            .unwrap()
            .as_deref(),
        Some(SAVED)
    );
}

#[test]
fn import_refused_while_the_editor_is_saving() {
    let db = ssh("db-host", AuthMethod::Password, Vec::new());
    let mut app = app_with(std::slice::from_ref(&db));
    let paths = app.paths.clone();
    let dir = temp();
    let keys = ready_import(&mut app, dir.path(), &sample_archive());
    let _ = app.update(Message::Editor(crate::editor::EditorMsg::Open));
    // An editor save whose secret changes are still running.
    let job = crate::editor::SecretJob {
        seq: 7,
        kind: crate::editor::JobKind::Save,
        editor: app.editor.as_ref().unwrap().1.instance(),
    };
    app.editor_job = Some(job);
    let before = std::fs::read_to_string(paths.profiles()).unwrap();

    send(&mut app, ArchiveMsg::Import);
    assert_eq!(
        state(&app).import.problem.as_deref(),
        Some(super::EDITOR_SAVING)
    );
    assert!(state(&app).import.opened.is_some(), "kept to import later");
    assert_eq!(std::fs::read_to_string(paths.profiles()).unwrap(), before);
    assert!(!keys.exists());
    assert_eq!(app.editor_job, Some(job));

    // Still refused once the editor that started the save is closed.
    let wid = app.editor_window().unwrap();
    let _ = app.update(Message::WindowClosed(wid));
    assert!(app.editor.is_none());
    send(&mut app, ArchiveMsg::Import);
    assert_eq!(
        state(&app).import.problem.as_deref(),
        Some(super::EDITOR_SAVING)
    );
    assert_eq!(std::fs::read_to_string(paths.profiles()).unwrap(), before);
    assert!(!keys.exists());
}
