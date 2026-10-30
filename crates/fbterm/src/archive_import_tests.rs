//! Tests for `archive_import`: key folders are temporary folders, never `~/.ssh`, and nothing
//! here touches a secret store.

use std::path::PathBuf;

use fbterm_config::profile::{
    AuthMethod, LocalProfile, ProfileKind, SshProfile, StartMode, TunnelDef, TunnelKind,
};
use fbterm_config::schemes::{self, Scheme, SchemeSet};
use fbterm_config::{Profile, ProfileId, ProfileStore};
use fbterm_secrets::SecretKind;
use fbterm_secrets::archive::{Archive, ArchiveKind, ArchivedKey, ArchivedSecret, MAX_KEY_BYTES};
use zeroize::Zeroizing;

use crate::archive_import::{Choice, SchemeOutcome, apply, plan, set_choice};

const KEY_BYTES: &[u8] = b"-----BEGIN OPENSSH PRIVATE KEY-----\nimported-4d1f\n";
const OTHER_KEY: &[u8] = b"-----BEGIN OPENSSH PRIVATE KEY-----\nlocal-other-7a2c\n";
const PASSWORD: &str = "pw-archived-51c3";
const PASSPHRASE: &str = "pp-archived-0e9b";

fn ssh(name: &str, auth: AuthMethod, jump: Vec<ProfileId>) -> Profile {
    Profile {
        icon: None,
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

fn local(name: &str) -> Profile {
    Profile {
        icon: None,
        id: ProfileId::new(),
        name: name.into(),
        group: None,
        accent: None,
        accent_style: Default::default(),
        color_scheme: None,
        kind: ProfileKind::Local(LocalProfile {
            program: "/bin/sh".into(),
            args: Vec::new(),
            cwd: None,
            env: Default::default(),
        }),
    }
}

fn key(path: Option<&str>, passphrase: bool) -> AuthMethod {
    AuthMethod::Key {
        path: path.map(PathBuf::from),
        passphrase,
    }
}

fn store(profiles: &[&Profile]) -> ProfileStore {
    ProfileStore {
        profiles: profiles.iter().map(|p| (*p).clone()).collect(),
    }
}

fn archive(kind: ArchiveKind, profiles: &[&Profile]) -> Archive {
    let mut a = Archive::new(kind, "2026-10-27T10:00:00Z");
    a.profiles = profiles.iter().map(|p| (*p).clone()).collect();
    a
}

fn secret(profile: ProfileId, kind: SecretKind, value: &str) -> ArchivedSecret {
    ArchivedSecret {
        profile,
        kind,
        value: Zeroizing::new(value.into()),
    }
}

fn archived_key(name: &str, bytes: &[u8], profiles: &[ProfileId]) -> ArchivedKey {
    ArchivedKey {
        file_name: name.into(),
        bytes: Zeroizing::new(bytes.to_vec()),
        profiles: profiles.to_vec(),
    }
}

fn no_schemes() -> SchemeSet {
    let dir = tempfile::tempdir().unwrap();
    SchemeSet::load(dir.path()).0
}

fn user_scheme(id: &str, name: &str) -> Scheme {
    let mut s = schemes::builtins().remove(2);
    s.id = id.into();
    s.name = name.into();
    s.builtin = false;
    s
}

fn schemes_with(list: &[&Scheme]) -> (tempfile::TempDir, SchemeSet) {
    let dir = tempfile::tempdir().unwrap();
    for s in list {
        schemes::save_user(dir.path(), s).unwrap();
    }
    let set = SchemeSet::load(dir.path()).0;
    (dir, set)
}

fn by_name<'a>(store: &'a ProfileStore, name: &str) -> &'a Profile {
    store
        .profiles
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("no profile named {name}"))
}

fn jumps(p: &Profile) -> &[ProfileId] {
    match &p.kind {
        ProfileKind::Ssh(s) => &s.jump,
        ProfileKind::Local(_) => &[],
    }
}

fn auth(p: &Profile) -> &AuthMethod {
    match &p.kind {
        ProfileKind::Ssh(s) => &s.auth,
        ProfileKind::Local(_) => panic!("not ssh"),
    }
}

fn host(p: &Profile) -> &str {
    match &p.kind {
        ProfileKind::Ssh(s) => &s.host,
        ProfileKind::Local(_) => panic!("not ssh"),
    }
}

fn key_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn matches_by_id_then_name() {
    let same_id = ssh("db", AuthMethod::Agent, Vec::new());
    let same_name = ssh("web", AuthMethod::Agent, Vec::new());
    let unrelated = ssh("cache", AuthMethod::Agent, Vec::new());
    let local_store = store(&[&same_id, &same_name, &unrelated]);

    // Same id under another name: matched by id.
    let mut renamed = same_id.clone();
    renamed.name = "database".into();
    // Same name, new id: matched by name.
    let web = ssh("web", AuthMethod::Agent, Vec::new());
    // Id of one local profile, name of another: the id wins.
    let mut both = unrelated.clone();
    both.name = "web".into();
    // Nothing in common.
    let fresh = ssh("fresh", AuthMethod::Agent, Vec::new());

    let a = archive(ArchiveKind::Full, &[&renamed, &web, &both, &fresh]);
    let p = plan(&local_store, &a, &no_schemes()).unwrap();
    let matched: Vec<_> = p.entries.iter().map(|e| e.matched).collect();
    assert_eq!(
        matched,
        vec![
            Some(same_id.id),
            Some(same_name.id),
            Some(unrelated.id),
            None
        ]
    );
}

#[test]
fn default_choice_is_skip() {
    let db = ssh("db", AuthMethod::Agent, Vec::new());
    let local_store = store(&[&db]);
    let mut changed = db.clone();
    if let ProfileKind::Ssh(s) = &mut changed.kind {
        s.host = "changed.example.com".into();
    }
    let fresh = ssh("fresh", AuthMethod::Agent, Vec::new());
    let a = archive(ArchiveKind::Full, &[&changed, &fresh]);
    let p = plan(&local_store, &a, &no_schemes()).unwrap();
    assert_eq!(p.entries[0].choice, Choice::Skip);
    assert!(p.entries.iter().all(|e| e.error.is_none()));

    // Applying the defaults keeps the clashing local profile and adds the new one as it is.
    let dir = key_dir();
    let applied = apply(&p, &a, &local_store, dir.path(), false).unwrap();
    assert_eq!(applied.store.profiles.len(), 2);
    assert_eq!(applied.store.profiles[0], db);
    assert_eq!(applied.store.profiles[1], fresh);
    assert!(applied.secret_ops.is_empty());
    assert!(applied.key_writes.is_empty());
}

#[test]
fn keep_both_renames_and_rewires() {
    let bastion = ssh("bastion", AuthMethod::Agent, Vec::new());
    let local_store = store(&[&bastion]);
    let mut archived_bastion = bastion.clone();
    if let ProfileKind::Ssh(s) = &mut archived_bastion.kind {
        s.host = "new-bastion.example.com".into();
    }
    let db = ssh("db", AuthMethod::Password, vec![bastion.id]);
    let mut a = archive(ArchiveKind::Full, &[&archived_bastion, &db]);
    a.secrets = vec![secret(db.id, SecretKind::Password, PASSWORD)];

    let mut p = plan(&local_store, &a, &no_schemes()).unwrap();
    set_choice(&mut p, 0, Choice::KeepBoth);
    assert!(p.entries.iter().all(|e| e.error.is_none()));
    let dir = key_dir();
    let applied = apply(&p, &a, &local_store, dir.path(), false).unwrap();

    // The local profile is untouched; the copy has a new id and the imported name.
    assert_eq!(by_name(&applied.store, "bastion"), &bastion);
    let copy = by_name(&applied.store, "bastion (imported)");
    assert_ne!(copy.id, bastion.id);
    assert_eq!(host(copy), "new-bastion.example.com");
    // References inside the archive follow the copy.
    let new_db = by_name(&applied.store, "db");
    assert_eq!(jumps(new_db), &[copy.id]);
    assert!(applied.store.validate().is_empty());
    // Secrets follow the profile they belong to.
    assert_eq!(applied.secret_ops.len(), 1);
    assert_eq!(applied.secret_ops[0].profile, new_db.id);
    assert_eq!(applied.secret_ops[0].kind, SecretKind::Password);
    assert_eq!(applied.secret_ops[0].value.as_str(), PASSWORD);

    // A second Keep both of the same profile picks the next free name.
    let again = plan(&applied.store, &a, &no_schemes()).unwrap();
    let mut again = again;
    set_choice(&mut again, 0, Choice::KeepBoth);
    let twice = apply(&again, &a, &applied.store, dir.path(), false).unwrap();
    assert!(
        twice
            .store
            .profiles
            .iter()
            .any(|p| p.name == "bastion (imported 2)")
    );
}

#[test]
fn skip_points_references_at_local() {
    let bastion = ssh("bastion", AuthMethod::Agent, Vec::new());
    let local_store = store(&[&bastion]);
    // The archive's bastion has another id but the same name.
    let archived_bastion = ssh("bastion", AuthMethod::Agent, Vec::new());
    let db = ssh("db", AuthMethod::Agent, vec![archived_bastion.id]);
    let a = archive(ArchiveKind::Full, &[&archived_bastion, &db]);

    let p = plan(&local_store, &a, &no_schemes()).unwrap();
    assert_eq!(p.entries[0].matched, Some(bastion.id));
    assert_eq!(p.entries[0].choice, Choice::Skip);
    assert!(p.entries.iter().all(|e| e.error.is_none()));
    let dir = key_dir();
    let applied = apply(&p, &a, &local_store, dir.path(), false).unwrap();
    assert_eq!(applied.store.profiles.len(), 2);
    assert_eq!(jumps(by_name(&applied.store, "db")), &[bastion.id]);
    assert!(applied.store.validate().is_empty());

    // Overwrite points them at the same local profile, now replaced.
    let mut p = p;
    set_choice(&mut p, 0, Choice::Overwrite);
    let applied = apply(&p, &a, &local_store, dir.path(), false).unwrap();
    assert_eq!(applied.store.profiles.len(), 2);
    assert_eq!(applied.store.profiles[0].id, bastion.id);
    assert_eq!(jumps(by_name(&applied.store, "db")), &[bastion.id]);
}

#[test]
fn invalid_reference_blocks_entry() {
    // Locally "hop" is a local shell; the archive's "hop" is an SSH host others jump through.
    let local_hop = local("hop");
    let local_store = store(&[&local_hop]);
    let hop = ssh("hop", AuthMethod::Agent, Vec::new());
    let db = ssh("db", AuthMethod::Agent, vec![hop.id]);
    let web = ssh("web", AuthMethod::Agent, vec![db.id]);
    let a = archive(ArchiveKind::Full, &[&hop, &db, &web]);

    // Skip points db at the local shell: invalid, and web through db with it.
    let mut p = plan(&local_store, &a, &no_schemes()).unwrap();
    assert_eq!(p.entries[0].matched, Some(local_hop.id));
    assert!(p.entries[0].error.is_none());
    assert_eq!(
        p.entries[1].error.as_deref(),
        Some("It jumps through a profile that is not an SSH profile.")
    );
    assert!(p.entries[2].error.is_some());
    let dir = key_dir();
    let applied = apply(&p, &a, &local_store, dir.path(), false).unwrap();
    assert_eq!(applied.store, local_store);

    // Keep both resolves it: references follow the SSH copy.
    set_choice(&mut p, 0, Choice::KeepBoth);
    assert!(p.entries.iter().all(|e| e.error.is_none()), "{p:?}");
    let applied = apply(&p, &a, &local_store, dir.path(), false).unwrap();
    assert_eq!(applied.store.profiles.len(), 4);
    assert!(applied.store.validate().is_empty());

    // An archive whose jumps loop is blocked as well.
    let mut x = ssh("x", AuthMethod::Agent, Vec::new());
    let y = ssh("y", AuthMethod::Agent, vec![x.id]);
    if let ProfileKind::Ssh(s) = &mut x.kind {
        s.jump = vec![y.id];
    }
    let a = archive(ArchiveKind::Full, &[&x, &y]);
    let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    assert_eq!(
        p.entries[0].error.as_deref(),
        Some("Its jump chain loops back to itself.")
    );
    assert!(p.entries[1].error.is_some());
    let applied = apply(&p, &a, &ProfileStore::default(), dir.path(), false).unwrap();
    assert!(applied.store.profiles.is_empty());
}

#[test]
fn imported_profiles_get_the_stores_validation() {
    let taken = ssh("taken", AuthMethod::Agent, Vec::new());
    let local_store = store(&[&taken]);
    let mut empty_host = ssh("empty", AuthMethod::Agent, Vec::new());
    if let ProfileKind::Ssh(s) = &mut empty_host.kind {
        s.host = " ".into();
    }
    let mut bad_tunnel = ssh("tunnel", AuthMethod::Agent, Vec::new());
    if let ProfileKind::Ssh(s) = &mut bad_tunnel.kind {
        s.tunnels = vec![TunnelDef {
            name: "web".into(),
            kind: TunnelKind::Local {
                bind: "127.0.0.1".into(),
                port: 0,
                host: "web".into(),
                host_port: 80,
            },
            start: StartMode::OnDemand,
        }];
    }
    // Matched by id, but renamed to the name of another local profile.
    let other_local = ssh("other", AuthMethod::Agent, Vec::new());
    let local_store = {
        let mut s = local_store;
        s.profiles.push(other_local.clone());
        s
    };
    let mut renamed = other_local.clone();
    renamed.name = "taken".into();
    let mut nameless = ssh("x", AuthMethod::Agent, Vec::new());
    nameless.name = "  ".into();
    let missing = ssh("lost", AuthMethod::Agent, vec![ProfileId::new()]);
    let a = archive(
        ArchiveKind::Full,
        &[&empty_host, &bad_tunnel, &renamed, &nameless, &missing],
    );
    let mut p = plan(&local_store, &a, &no_schemes()).unwrap();
    set_choice(&mut p, 2, Choice::Overwrite);
    let errors: Vec<_> = p.entries.iter().map(|e| e.error.as_deref()).collect();
    assert_eq!(
        errors,
        vec![
            Some("Its host is empty."),
            Some("Tunnel web needs a port above 0."),
            Some("A profile named taken already exists."),
            Some("Its name is empty."),
            Some("It jumps through a profile that is not in the archive or on this computer."),
        ]
    );
    let dir = key_dir();
    let applied = apply(&p, &a, &local_store, dir.path(), false).unwrap();
    assert_eq!(applied.store, local_store);
}

#[test]
fn full_overwrite_replaces_carried_secrets() {
    let db = ssh("db", AuthMethod::Password, Vec::new());
    let local_store = store(&[&db]);
    let mut changed = db.clone();
    if let ProfileKind::Ssh(s) = &mut changed.kind {
        s.user = "admin".into();
    }
    let mut a = archive(ArchiveKind::Full, &[&changed]);
    a.secrets = vec![
        secret(db.id, SecretKind::Password, PASSWORD),
        // Not the method the profile uses: never stored.
        secret(db.id, SecretKind::KeyPassphrase, PASSPHRASE),
    ];
    let mut p = plan(&local_store, &a, &no_schemes()).unwrap();
    assert!(p.has_secrets);
    set_choice(&mut p, 0, Choice::Overwrite);
    let dir = key_dir();
    let applied = apply(&p, &a, &local_store, dir.path(), false).unwrap();
    assert_eq!(applied.store.profiles, vec![changed]);
    assert_eq!(applied.secret_ops.len(), 1);
    assert_eq!(applied.secret_ops[0].profile, db.id);
    assert_eq!(applied.secret_ops[0].kind, SecretKind::Password);
    assert_eq!(applied.secret_ops[0].value.as_str(), PASSWORD);
    // Secret values never show in debug output.
    assert!(!format!("{:?}", applied.secret_ops).contains(PASSWORD));
}

#[test]
fn a_carried_passphrase_is_stored_and_flagged_whatever_the_flag_says() {
    // An archive from a version that gated on the flag, or a key whose flag was never
    // set: the passphrase it carries is stored, and the profile says its key has one.
    let db = ssh("db", key(Some("/home/me/.ssh/id_db"), false), Vec::new());
    let mut a = archive(ArchiveKind::Full, &[&db]);
    a.secrets = vec![secret(db.id, SecretKind::KeyPassphrase, PASSPHRASE)];
    let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    let dir = key_dir();
    let applied = apply(&p, &a, &ProfileStore::default(), dir.path(), false).unwrap();
    assert_eq!(applied.secret_ops.len(), 1);
    assert_eq!(applied.secret_ops[0].kind, SecretKind::KeyPassphrase);
    assert_eq!(applied.secret_ops[0].value.as_str(), PASSPHRASE);
    assert_eq!(
        auth(by_name(&applied.store, "db")),
        &key(Some("/home/me/.ssh/id_db"), true)
    );
    // Without a carried passphrase the flag stays as the archive has it.
    let a = archive(ArchiveKind::Full, &[&db]);
    let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    let applied = apply(&p, &a, &ProfileStore::default(), dir.path(), false).unwrap();
    assert!(applied.secret_ops.is_empty());
    assert_eq!(
        auth(by_name(&applied.store, "db")),
        &key(Some("/home/me/.ssh/id_db"), false)
    );
}

#[test]
fn only_local_key_paths_are_taken_from_an_archive() {
    use crate::archive_import::archive_key_path_ok as ok;
    use std::path::Path;
    assert!(ok(Path::new("~/.ssh/id_ed25519")));
    assert!(ok(Path::new("~/./.ssh/id_ed25519")));
    assert!(ok(Path::new("/home/me/.ssh/id_ed25519")));
    // Rooted on the current drive on Windows: still local.
    assert!(ok(Path::new("/x")));
    // Only the first folder is a device tree: a key inside a folder named dev is fine.
    assert!(ok(Path::new("/home/me/dev/id_ed25519")));
    #[cfg(windows)]
    {
        assert!(ok(Path::new("C:\\Users\\me\\.ssh\\id_ed25519")));
        // A drive without a root depends on that drive's current folder.
        assert!(!ok(Path::new("C:id_ed25519")));
        assert!(!ok(Path::new("\\\\server\\share\\id")));
        assert!(!ok(Path::new("\\\\?\\UNC\\server\\share\\id")));
    }
    // Not a local path anywhere but Windows.
    #[cfg(not(windows))]
    assert!(!ok(Path::new("C:\\Users\\me\\.ssh\\id_ed25519")));
    for hostile in [
        "\\\\host\\share\\id",
        "\\\\server\\x",
        "//host/share/id",
        "//server/x",
        "\\\\.\\pipe\\id",
        "\\\\?\\C:\\id",
        "\\??\\UNC\\host\\share\\x",
        "/??/UNC/host/share/x",
        "/dev/zero",
        "/dev",
        "/./dev/zero",
        "/.//dev/zero",
        "/DEV/zero",
        "/Dev/zero",
        "/proc/self/fd/0",
        "/PROC/self/fd/0",
        "/home/../dev/zero",
        "/home/me/../../dev/zero",
        "~/../../dev/zero",
        "id_ed25519",
        ".ssh/id_ed25519",
        "~user/.ssh/id",
        "~",
        "",
    ] {
        assert!(!ok(Path::new(hostile)), "{hostile}");
    }
}

#[test]
fn hostile_key_paths_are_dropped_on_import() {
    let unc = ssh("unc", key(Some("\\\\host\\share\\id"), false), Vec::new());
    let dev = ssh("dev", key(Some("/dev/zero"), false), Vec::new());
    let relative = ssh("relative", key(Some("keys/id"), false), Vec::new());
    let home = ssh("home", key(Some("~/.ssh/id_home"), false), Vec::new());
    let local_dev = ssh("dev", key(None, false), Vec::new());
    let local_store = store(&[&local_dev]);
    let a = archive(ArchiveKind::Full, &[&unc, &dev, &relative, &home]);
    // `dev` matches a local profile with no key path: Overwrite takes the archive's.
    let dev_id = dev.id;
    let mut p = plan(&local_store, &a, &no_schemes()).unwrap();
    let i = p
        .entries
        .iter()
        .position(|e| e.incoming.id == dev_id)
        .unwrap();
    set_choice(&mut p, i, Choice::Overwrite);
    let dir = key_dir();
    let applied = apply(&p, &a, &local_store, dir.path(), false).unwrap();
    // Dropped: the profile asks for a key file on its first connect.
    for name in ["unc", "dev", "relative"] {
        assert_eq!(
            auth(by_name(&applied.store, name)),
            &key(None, false),
            "{name}"
        );
    }
    assert_eq!(
        auth(by_name(&applied.store, "home")),
        &key(Some("~/.ssh/id_home"), false)
    );
    // The preview shows the path that is kept, and only that one.
    let shown: Vec<Option<PathBuf>> = (0..p.entries.len())
        .map(|i| p.kept_key_path(i).map(PathBuf::from))
        .collect();
    let home_at = p
        .entries
        .iter()
        .position(|e| e.incoming.name == "home")
        .unwrap();
    for (i, path) in shown.iter().enumerate() {
        if i == home_at {
            assert_eq!(
                path.as_deref(),
                Some(std::path::Path::new("~/.ssh/id_home"))
            );
        } else {
            assert_eq!(path, &None, "{}", p.entries[i].incoming.name);
        }
    }
}

#[test]
fn shared_overwrite_keeps_local_secrets_and_key_path() {
    let db = ssh("db", key(Some("/home/me/.ssh/id_db"), true), Vec::new());
    let local_store = store(&[&db]);
    let mut shared = db.clone();
    if let ProfileKind::Ssh(s) = &mut shared.kind {
        s.auth = key(None, false);
        s.host = "db2.example.com".into();
    }
    let a = archive(ArchiveKind::Shared, &[&shared]);
    let mut p = plan(&local_store, &a, &no_schemes()).unwrap();
    assert!(!p.has_secrets);
    set_choice(&mut p, 0, Choice::Overwrite);
    let dir = key_dir();
    let applied = apply(&p, &a, &local_store, dir.path(), false).unwrap();
    let after = &applied.store.profiles[0];
    assert_eq!(after.id, db.id);
    assert_eq!(host(after), "db2.example.com");
    // The local key path and passphrase flag survive; no secret is touched.
    assert_eq!(auth(after), &key(Some("/home/me/.ssh/id_db"), true));
    assert!(applied.secret_ops.is_empty());
    assert!(applied.key_writes.is_empty());
}

#[test]
fn shared_archive_claims_are_ignored() {
    // A crafted shared archive carrying everything a shared archive must not.
    let shell = local("shell");
    let db = ssh("db", key(Some("/attacker/id_db"), true), Vec::new());
    let web = ssh("web", AuthMethod::Password, Vec::new());
    let mut a = archive(ArchiveKind::Shared, &[&shell, &db, &web]);
    a.secrets = vec![
        secret(db.id, SecretKind::KeyPassphrase, PASSPHRASE),
        secret(web.id, SecretKind::Password, PASSWORD),
    ];
    a.keys = vec![archived_key("id_db", KEY_BYTES, &[db.id])];

    let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    assert!(!p.has_secrets);
    assert!(!p.has_keys);
    assert_eq!(p.entries.len(), 2);
    let dir = key_dir();
    let applied = apply(&p, &a, &ProfileStore::default(), dir.path(), false).unwrap();
    assert!(applied.secret_ops.is_empty());
    assert!(applied.key_writes.is_empty());
    assert!(
        applied
            .store
            .profiles
            .iter()
            .all(|p| matches!(p.kind, ProfileKind::Ssh(_)))
    );
    assert_eq!(auth(by_name(&applied.store, "db")), &key(None, false));
    let text = toml::to_string(&applied.store).unwrap();
    assert!(!text.contains("/attacker"));
    assert!(!text.contains("shell"));
}

#[test]
fn existing_key_file_is_never_replaced() {
    let dir = key_dir();
    let existing = dir.path().join("id_ed25519");
    std::fs::write(&existing, OTHER_KEY).unwrap();

    let db = ssh("db", key(Some("/elsewhere/id_ed25519"), false), Vec::new());
    let mut a = archive(ArchiveKind::Full, &[&db]);
    a.keys = vec![archived_key("id_ed25519", KEY_BYTES, &[db.id])];
    let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    assert!(p.has_keys);
    let applied = apply(&p, &a, &ProfileStore::default(), dir.path(), false).unwrap();

    let new_path = dir.path().join("id_ed25519-2");
    assert_eq!(applied.key_writes.len(), 1);
    assert_eq!(applied.key_writes[0].path, new_path);
    assert_eq!(applied.key_writes[0].bytes.as_slice(), KEY_BYTES);
    assert_eq!(
        auth(&applied.store.profiles[0]),
        &AuthMethod::Key {
            path: Some(new_path),
            passphrase: false
        }
    );
    // Key bytes never show in debug output.
    let shown = format!("{:?}", applied.key_writes);
    assert!(!shown.contains("imported-4d1f"), "{shown}");
    // Nothing on disk changed.
    assert_eq!(std::fs::read(&existing).unwrap(), OTHER_KEY);
    assert!(!dir.path().join("id_ed25519-2").exists());
}

#[test]
fn identical_key_file_is_reused() {
    let dir = key_dir();
    let existing = dir.path().join("id_ed25519");
    std::fs::write(&existing, KEY_BYTES).unwrap();

    let db = ssh("db", key(Some("/elsewhere/id_ed25519"), false), Vec::new());
    let web = ssh("web", key(Some("/elsewhere/id_ed25519"), false), Vec::new());
    let mut a = archive(ArchiveKind::Full, &[&db, &web]);
    a.keys = vec![archived_key("id_ed25519", KEY_BYTES, &[db.id, web.id])];
    let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    let applied = apply(&p, &a, &ProfileStore::default(), dir.path(), false).unwrap();
    assert!(applied.key_writes.is_empty());
    for p in &applied.store.profiles {
        assert_eq!(
            auth(p),
            &AuthMethod::Key {
                path: Some(existing.clone()),
                passphrase: false
            }
        );
    }
}

#[test]
fn key_file_names_are_reduced_to_plain_names() {
    let dir = key_dir();
    let cases = [
        ("../../etc/passwd", "passwd"),
        ("/abs/id_rsa", "id_rsa"),
        ("C:\\keys\\id_win", "id_win"),
        ("..", "id_imported"),
        (".", "id_imported"),
        ("", "id_imported"),
        (".bashrc", "id_imported"),
        ("bad\nname", "id_imported"),
        ("authorized_keys", "id_imported"),
        ("Config", "id_imported"),
        ("NUL", "id_imported"),
        ("con.txt", "id_imported"),
        ("\\\\server\\x", "x"),
        ("a/b", "b"),
        ("id\0nul", "id_imported"),
        ("id_ed25519.pub", "id_imported"),
        ("ID_RSA.PUB", "id_imported"),
    ];
    let long = "k".repeat(129);
    let cases: Vec<(&str, &str)> = cases
        .into_iter()
        .chain([(long.as_str(), "id_imported"), (&long[..128], &long[..128])])
        .collect();
    for (archived, expected) in cases {
        let db = ssh("db", key(None, false), Vec::new());
        let mut a = archive(ArchiveKind::Full, &[&db]);
        a.keys = vec![archived_key(archived, KEY_BYTES, &[db.id])];
        let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
        let applied = apply(&p, &a, &ProfileStore::default(), dir.path(), false).unwrap();
        assert_eq!(applied.key_writes.len(), 1, "{archived:?}");
        let path = &applied.key_writes[0].path;
        assert_eq!(path, &dir.path().join(expected), "{archived:?}");
        assert_eq!(path.parent(), Some(dir.path()));
    }

    // Two archived keys reduced to the same name never share a file.
    let db = ssh("db", key(None, false), Vec::new());
    let web = ssh("web", key(None, false), Vec::new());
    let mut a = archive(ArchiveKind::Full, &[&db, &web]);
    a.keys = vec![
        archived_key("a/id_x", KEY_BYTES, &[db.id]),
        archived_key("b/ID_X", OTHER_KEY, &[web.id]),
    ];
    let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    let applied = apply(&p, &a, &ProfileStore::default(), dir.path(), false).unwrap();
    let paths: Vec<_> = applied.key_writes.iter().map(|w| w.path.clone()).collect();
    assert_eq!(
        paths,
        vec![dir.path().join("id_x"), dir.path().join("ID_X-2")]
    );
}

#[test]
fn oversized_archived_key_is_never_written() {
    let dir = key_dir();
    let db = ssh("db", key(Some("/elsewhere/id_big"), false), Vec::new());
    let mut a = archive(ArchiveKind::Full, &[&db]);
    let big = vec![b'k'; MAX_KEY_BYTES as usize + 1];
    a.keys = vec![archived_key("id_big", &big, &[db.id])];
    let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    assert!(!p.has_keys);
    assert_eq!(
        p.warnings,
        vec!["The key file id_big is larger than 1 MB, so it is not a key and was left out."]
    );
    let applied = apply(&p, &a, &ProfileStore::default(), dir.path(), false).unwrap();
    assert!(applied.key_writes.is_empty());
    // The profile keeps the path the archive gave it.
    assert_eq!(
        auth(&applied.store.profiles[0]),
        &key(Some("/elsewhere/id_big"), false)
    );
}

#[test]
fn existing_key_that_is_not_a_regular_file_is_never_read_as_identical() {
    let dir = key_dir();
    std::fs::create_dir(dir.path().join("id_dir")).unwrap();
    let db = ssh("db", key(None, false), Vec::new());
    let mut a = archive(ArchiveKind::Full, &[&db]);
    a.keys = vec![archived_key("id_dir", KEY_BYTES, &[db.id])];
    let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    let applied = apply(&p, &a, &ProfileStore::default(), dir.path(), false).unwrap();
    assert_eq!(applied.key_writes[0].path, dir.path().join("id_dir-2"));
}

#[test]
fn scheme_clash_imports_copy() {
    let mine = user_scheme("user-1", "Ocean");
    let same = user_scheme("user-2", "Dusk");
    let (_dir, local_schemes) = schemes_with(&[&mine, &same]);

    let mut theirs = mine.clone();
    theirs.colors.background.r ^= 0x10;
    let fresh = user_scheme("user-3", "Forest");
    let mut a1 = ssh("a1", AuthMethod::Agent, Vec::new());
    a1.color_scheme = Some("user-1".into());
    let mut a2 = ssh("a2", AuthMethod::Agent, Vec::new());
    a2.color_scheme = Some("user-2".into());
    let mut a3 = ssh("a3", AuthMethod::Agent, Vec::new());
    a3.color_scheme = Some("user-3".into());
    let mut a = archive(ArchiveKind::Full, &[&a1, &a2, &a3]);
    a.schemes = vec![theirs.clone(), same.clone(), fresh.clone()];

    let p = plan(&ProfileStore::default(), &a, &local_schemes).unwrap();
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    let outcomes: Vec<_> = p.schemes.iter().map(|s| s.outcome).collect();
    assert_eq!(
        outcomes,
        vec![
            SchemeOutcome::Renamed,
            SchemeOutcome::Reused,
            SchemeOutcome::Imported
        ]
    );
    let copy = &p.schemes[0].scheme;
    assert_ne!(copy.id, "user-1");
    assert!(schemes::is_plain_id(&copy.id));
    assert_eq!(copy.name, "Ocean (imported)");
    assert_eq!(copy.colors, theirs.colors);

    let dir = key_dir();
    let applied = apply(&p, &a, &ProfileStore::default(), dir.path(), false).unwrap();
    assert_eq!(
        by_name(&applied.store, "a1").color_scheme.as_deref(),
        Some(copy.id.as_str())
    );
    assert_eq!(
        by_name(&applied.store, "a2").color_scheme.as_deref(),
        Some("user-2")
    );
    assert_eq!(
        by_name(&applied.store, "a3").color_scheme.as_deref(),
        Some("user-3")
    );
    let written: Vec<_> = applied
        .scheme_writes
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(written, vec![copy.id.as_str(), "user-3"]);
    // Every write is saveable through the scheme files' own path.
    let out = tempfile::tempdir().unwrap();
    for s in &applied.scheme_writes {
        schemes::save_user(out.path(), s).unwrap();
    }
}

#[test]
fn archived_schemes_get_the_scheme_file_rules() {
    // A built-in id is the built-in: never imported, references keep pointing at it.
    let mut fake_builtin = user_scheme("ember", "Not Ember");
    fake_builtin.colors.background.g ^= 0x22;
    // Ids that cannot name a scheme file.
    let bad_id = user_scheme("../../evil", "Evil");
    let upper = user_scheme("User-UPPER", "Upper");
    // Larger than a scheme file may be.
    let huge = user_scheme("user-huge", &"n".repeat(70 * 1024));
    let ok = user_scheme("user-ok", "Fine");

    let mut p1 = ssh("p1", AuthMethod::Agent, Vec::new());
    p1.color_scheme = Some("ember".into());
    let mut p2 = ssh("p2", AuthMethod::Agent, Vec::new());
    p2.color_scheme = Some("../../evil".into());
    let mut p3 = ssh("p3", AuthMethod::Agent, Vec::new());
    p3.color_scheme = Some("User-UPPER".into());
    let mut p4 = ssh("p4", AuthMethod::Agent, Vec::new());
    p4.color_scheme = Some("user-huge".into());
    let mut p5 = ssh("p5", AuthMethod::Agent, Vec::new());
    p5.color_scheme = Some("user-ok".into());
    let mut p6 = ssh("p6", AuthMethod::Agent, Vec::new());
    p6.color_scheme = Some("user-nowhere".into());
    let mut a = archive(ArchiveKind::Full, &[&p1, &p2, &p3, &p4, &p5, &p6]);
    a.schemes = vec![fake_builtin, bad_id, upper, huge, ok];

    let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    let ids: Vec<_> = p.schemes.iter().map(|s| s.scheme.id.as_str()).collect();
    assert_eq!(ids, vec!["user-ok"]);
    assert_eq!(p.warnings.len(), 3, "{:?}", p.warnings);
    assert!(
        p.warnings
            .iter()
            .all(|w| w.ends_with("is not a valid color scheme, so it was left out."))
    );
    // Warnings never carry a huge name.
    assert!(p.warnings.iter().all(|w| w.len() < 200));

    let dir = key_dir();
    let applied = apply(&p, &a, &ProfileStore::default(), dir.path(), false).unwrap();
    let scheme_of = |n: &str| by_name(&applied.store, n).color_scheme.clone();
    assert_eq!(scheme_of("p1").as_deref(), Some("ember"));
    assert_eq!(scheme_of("p2"), None);
    assert_eq!(scheme_of("p3"), None);
    assert_eq!(scheme_of("p4"), None);
    assert_eq!(scheme_of("p5").as_deref(), Some("user-ok"));
    assert_eq!(scheme_of("p6"), None);
    let written: Vec<_> = applied
        .scheme_writes
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(written, vec!["user-ok"]);
}

/// `id` as installed from the gallery: credit and a matching, untouched `[gallery]` block.
fn gallery_scheme(id: &str) -> Scheme {
    let mut s = user_scheme(&format!("gallery-{id}"), "Nord");
    s.credit = Some(schemes::Credit {
        project: "Nord".into(),
        author: "Someone".into(),
        url: "https://example.org/nord".into(),
        license: "MIT".into(),
        copyright: "Copyright (c) 2026 Someone".into(),
        license_file: "LICENSES/nord-MIT.txt".into(),
        description: "A theme.".into(),
        derived: false,
    });
    s.gallery = Some(schemes::GalleryInfo {
        id: id.into(),
        version: 1,
        colors_hash: schemes::install_hash(&s),
    });
    assert!(!schemes::is_edited(&s));
    s
}

#[test]
fn archived_gallery_schemes_count_as_edited() {
    // A crafted archive carries a matching `[gallery]` block: it is dropped all the same.
    let theirs = gallery_scheme("nord");
    let mut p1 = ssh("p1", AuthMethod::Agent, Vec::new());
    p1.color_scheme = Some(theirs.id.clone());
    let mut a = archive(ArchiveKind::Full, &[&p1]);
    a.schemes = vec![theirs.clone()];
    let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    assert_eq!(p.schemes.len(), 1);
    assert_eq!(p.schemes[0].outcome, SchemeOutcome::Imported);
    let imported = &p.schemes[0].scheme;
    assert_eq!(imported.id, "gallery-nord");
    assert_eq!(imported.credit, theirs.credit);
    assert!(imported.gallery.is_none());
    assert!(schemes::is_edited(imported));
    let dir = key_dir();
    let applied = apply(&p, &a, &ProfileStore::default(), dir.path(), false).unwrap();
    assert!(applied.scheme_writes.iter().all(|s| s.gallery.is_none()));

    // The same theme installed here, unchanged: the local one (with its block) is reused,
    // not copied, since archives never carry the block.
    let (_dir, local) = schemes_with(&[&theirs]);
    assert!(local.get("gallery-nord").unwrap().gallery.is_some());
    let exported = Scheme {
        gallery: None,
        ..theirs.clone()
    };
    a.schemes = vec![exported];
    let p = plan(&ProfileStore::default(), &a, &local).unwrap();
    assert_eq!(p.schemes[0].outcome, SchemeOutcome::Reused);
    assert!(p.schemes[0].scheme.gallery.is_some());

    // A changed copy of it is a renamed copy without the block.
    let mut changed = theirs.clone();
    changed.colors.background.r ^= 0x10;
    a.schemes = vec![changed];
    let p = plan(&ProfileStore::default(), &a, &local).unwrap();
    assert_eq!(p.schemes[0].outcome, SchemeOutcome::Renamed);
    assert!(p.schemes[0].scheme.gallery.is_none());
}

#[test]
fn archived_unsafe_credit_is_dropped() {
    let mut theirs = gallery_scheme("nord");
    theirs.credit.as_mut().unwrap().project = "No\u{202E}rd".into();
    let mut ok = user_scheme("user-ok", "Fine");
    ok.credit = gallery_scheme("other").credit;
    let mut a = archive(ArchiveKind::Full, &[]);
    a.schemes = vec![theirs.clone(), ok.clone()];
    let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    let ids: Vec<_> = p.schemes.iter().map(|s| s.scheme.id.as_str()).collect();
    assert_eq!(ids, vec!["gallery-nord", "user-ok"]);
    assert!(p.schemes[0].scheme.credit.is_none());
    assert_eq!(p.schemes[0].scheme.colors, theirs.colors);
    assert_eq!(p.schemes[1].scheme.credit, ok.credit);
}

#[test]
fn read_only_profiles_refuse_the_import() {
    let db = ssh("db", AuthMethod::Agent, Vec::new());
    let a = archive(ArchiveKind::Full, &[&db]);
    let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    let dir = key_dir();
    let err = apply(&p, &a, &ProfileStore::default(), dir.path(), true).unwrap_err();
    assert_eq!(
        err,
        "Profiles are read-only until profiles.toml is fixed, so nothing was imported."
    );
}

#[test]
fn a_stale_plan_is_refused() {
    let db = ssh("db", AuthMethod::Agent, Vec::new());
    let a = archive(ArchiveKind::Full, &[&db]);
    let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    let meanwhile = store(&[&ssh("new", AuthMethod::Agent, Vec::new())]);
    let dir = key_dir();
    let err = apply(&p, &a, &meanwhile, dir.path(), false).unwrap_err();
    assert_eq!(
        err,
        "The profiles changed since the archive was opened. Open it again to import."
    );
}

#[test]
fn duplicate_ids_in_the_archive_import_once() {
    let db = ssh("db", AuthMethod::Agent, Vec::new());
    let mut twin = db.clone();
    twin.name = "twin".into();
    let a = archive(ArchiveKind::Full, &[&db, &twin]);
    let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    assert!(p.entries[0].error.is_none());
    assert_eq!(
        p.entries[1].error.as_deref(),
        Some("It appears more than once in the archive.")
    );
    let dir = key_dir();
    let applied = apply(&p, &a, &ProfileStore::default(), dir.path(), false).unwrap();
    assert_eq!(applied.store.profiles, vec![db]);
}

#[test]
fn overwrite_that_breaks_a_local_jump_is_blocked() {
    // Locally db jumps through hop; the archive's hop (same id) is a local shell.
    let hop = ssh("hop", AuthMethod::Agent, Vec::new());
    let db = ssh("db", AuthMethod::Agent, vec![hop.id]);
    let local_store = store(&[&hop, &db]);
    let mut shell = local("hop");
    shell.id = hop.id;
    let a = archive(ArchiveKind::Full, &[&shell]);
    let mut p = plan(&local_store, &a, &no_schemes()).unwrap();
    assert_eq!(p.entries[0].matched, Some(hop.id));
    set_choice(&mut p, 0, Choice::Overwrite);
    assert_eq!(
        p.entries[0].error.as_deref(),
        Some("It would make the jump chain of db invalid.")
    );
    let dir = key_dir();
    let applied = apply(&p, &a, &local_store, dir.path(), false).unwrap();
    assert_eq!(applied.store, local_store);

    // Back to Skip, the entry has nothing wrong with it.
    set_choice(&mut p, 0, Choice::Skip);
    assert!(p.entries[0].error.is_none());
}

#[test]
fn over_limit_archives_are_refused() {
    const REFUSED: &str = "This archive is too large or malformed to import.";
    let refused = |a: &Archive| {
        assert_eq!(
            plan(&ProfileStore::default(), a, &no_schemes()).unwrap_err(),
            REFUSED
        );
    };

    let many: Vec<Profile> = (0..1001)
        .map(|i| ssh(&format!("p{i}"), AuthMethod::Agent, Vec::new()))
        .collect();
    let mut a = archive(ArchiveKind::Full, &[]);
    a.profiles = many.clone();
    refused(&a);
    a.profiles.pop();
    assert!(plan(&ProfileStore::default(), &a, &no_schemes()).is_ok());

    let hops: Vec<ProfileId> = many[..17].iter().map(|p| p.id).collect();
    let wide = ssh("wide", AuthMethod::Agent, hops.clone());
    refused(&archive(ArchiveKind::Full, &[&wide]));
    let ok = ssh("ok", AuthMethod::Agent, hops[..16].to_vec());
    assert!(
        plan(
            &ProfileStore::default(),
            &archive(ArchiveKind::Full, &[&ok]),
            &no_schemes()
        )
        .is_ok()
    );
    let twice = ssh("twice", AuthMethod::Agent, vec![hops[0], hops[0]]);
    refused(&archive(ArchiveKind::Full, &[&twice]));

    let mut a = archive(ArchiveKind::Full, &[]);
    a.schemes = (0..201)
        .map(|i| user_scheme(&format!("user-{i}"), "S"))
        .collect();
    refused(&a);

    let mut a = archive(ArchiveKind::Full, &[]);
    a.keys = (0..201)
        .map(|i| archived_key(&format!("id_{i}"), KEY_BYTES, &[]))
        .collect();
    refused(&a);

    // At most a password and a passphrase per profile.
    let mut a = archive(ArchiveKind::Full, &[]);
    a.secrets = (0..2001)
        .map(|_| secret(ProfileId::new(), SecretKind::Password, PASSWORD))
        .collect();
    refused(&a);
    a.secrets.pop();
    assert!(plan(&ProfileStore::default(), &a, &no_schemes()).is_ok());

    // A key's profile list: at most 1000 ids, none twice.
    let ids: Vec<ProfileId> = many.iter().map(|p| p.id).collect();
    let mut a = archive(ArchiveKind::Full, &[]);
    a.keys = vec![archived_key("id_k", KEY_BYTES, &ids)];
    refused(&a);
    a.keys = vec![archived_key("id_k", KEY_BYTES, &ids[..1000])];
    assert!(plan(&ProfileStore::default(), &a, &no_schemes()).is_ok());
    a.keys = vec![archived_key("id_k", KEY_BYTES, &[ids[0], ids[0]])];
    refused(&a);
}

#[test]
fn crafted_jump_shapes_plan_quickly() {
    use std::time::{Duration, Instant};
    // Forty levels, each profile jumping through both profiles of the level below.
    let mut levels = vec![[
        ssh("a0", AuthMethod::Agent, Vec::new()),
        ssh("b0", AuthMethod::Agent, Vec::new()),
    ]];
    for i in 1..40 {
        let below = vec![levels[i - 1][0].id, levels[i - 1][1].id];
        levels.push([
            ssh(&format!("a{i}"), AuthMethod::Agent, below.clone()),
            ssh(&format!("b{i}"), AuthMethod::Agent, below),
        ]);
    }
    let mut diamond = archive(ArchiveKind::Full, &[]);
    diamond.profiles = levels.into_iter().flatten().collect();

    // A thousand-link chain whose root is missing: every link is blocked in turn.
    let mut chain = vec![ssh("c0", AuthMethod::Agent, vec![ProfileId::new()])];
    for i in 1..1000 {
        let prev = chain[i - 1].id;
        chain.push(ssh(&format!("c{i}"), AuthMethod::Agent, vec![prev]));
    }
    let mut long = archive(ArchiveKind::Full, &[]);
    long.profiles = chain;

    // Diamond levels 0 to 4 expand to 0, 2, 6, 14 and 30 hops; the 70 profiles above
    // would go through more than 32 jump hosts.
    for (a, blocked) in [(diamond, 70), (long, 1000)] {
        let start = Instant::now();
        let mut p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
        // Re-validating on a choice is as quick.
        set_choice(&mut p, 0, Choice::KeepBoth);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
        let found = p.entries.iter().filter(|e| e.error.is_some()).count();
        assert_eq!(found, blocked);
    }
}

#[test]
fn chains_over_32_hops_are_blocked() {
    let mut chain = vec![ssh("c0", AuthMethod::Agent, Vec::new())];
    for i in 1..=33 {
        let prev = chain[i - 1].id;
        chain.push(ssh(&format!("c{i}"), AuthMethod::Agent, vec![prev]));
    }
    let mut a = archive(ArchiveKind::Full, &[]);
    a.profiles = chain;
    let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    assert!(p.entries[..33].iter().all(|e| e.error.is_none()));
    assert_eq!(
        p.entries[33].error.as_deref(),
        Some("It goes through more than 32 jump hosts.")
    );
}

#[test]
fn full_overwrite_without_key_file_keeps_local_key_path() {
    let db = ssh("db", key(Some("/home/me/.ssh/id_db"), false), Vec::new());
    let keyless = ssh("keyless", key(None, false), Vec::new());
    let local_store = store(&[&db, &keyless]);
    let mut theirs = db.clone();
    if let ProfileKind::Ssh(s) = &mut theirs.kind {
        s.auth = key(Some("/other/machine/id_db"), true);
    }
    let mut their_keyless = keyless.clone();
    if let ProfileKind::Ssh(s) = &mut their_keyless.kind {
        s.auth = key(Some("/other/machine/id_k"), false);
    }
    let a = archive(ArchiveKind::Full, &[&theirs, &their_keyless]);
    let mut p = plan(&local_store, &a, &no_schemes()).unwrap();
    set_choice(&mut p, 0, Choice::Overwrite);
    set_choice(&mut p, 1, Choice::Overwrite);
    let dir = key_dir();
    let applied = apply(&p, &a, &local_store, dir.path(), false).unwrap();
    // The local path stays; the passphrase flag travels with the archive's secrets.
    assert_eq!(
        auth(by_name(&applied.store, "db")),
        &key(Some("/home/me/.ssh/id_db"), true)
    );
    // Without a local path, the archive's profile is taken as it is.
    assert_eq!(
        auth(by_name(&applied.store, "keyless")),
        &key(Some("/other/machine/id_k"), false)
    );

    // With the key file in the archive, the profile points at the placed file.
    let mut a = a;
    a.keys = vec![archived_key("id_db", KEY_BYTES, &[db.id])];
    let applied = apply(&p, &a, &local_store, dir.path(), false).unwrap();
    assert_eq!(
        auth(by_name(&applied.store, "db")),
        &AuthMethod::Key {
            path: Some(dir.path().join("id_db")),
            passphrase: true
        }
    );
}

#[test]
fn repeated_schemes_are_reported_once_each() {
    let first = user_scheme("user-1", "Ocean");
    let mut second = user_scheme("user-1", "Ocean again");
    second.colors.background.r ^= 0x01;
    let mut a = archive(ArchiveKind::Full, &[]);
    a.schemes = vec![first, second];
    let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    assert_eq!(p.schemes.len(), 1);
    assert_eq!(p.schemes[0].scheme.name, "Ocean");
    assert_eq!(
        p.warnings,
        vec!["The archive lists the color scheme Ocean twice; only the first is used."]
    );
}

#[test]
fn has_keys_counts_only_keys_imported_profiles_use() {
    let db = ssh("db", key(None, false), Vec::new());
    let agent = ssh("agent", AuthMethod::Agent, Vec::new());
    let mut a = archive(ArchiveKind::Full, &[&db, &agent]);
    a.keys = vec![
        archived_key("unused", KEY_BYTES, &[ProfileId::new()]),
        archived_key("agents", KEY_BYTES, &[agent.id]),
    ];
    let p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    assert!(!p.has_keys);
    a.keys.push(archived_key("id_db", KEY_BYTES, &[db.id]));
    let mut p = plan(&ProfileStore::default(), &a, &no_schemes()).unwrap();
    assert!(p.has_keys);
    // Leaving the key's only profile out leaves the key out too.
    set_choice(&mut p, 0, Choice::Skip);
    assert!(!p.has_keys);
}

#[test]
fn a_jump_through_an_entry_left_out_names_it() {
    // The hop is in the archive but blocked (empty host): its dependent says so by name.
    let mut hop = ssh("hop", AuthMethod::Agent, Vec::new());
    if let ProfileKind::Ssh(s) = &mut hop.kind {
        s.host = String::new();
    }
    let db = ssh("db", AuthMethod::Agent, vec![hop.id]);
    let a = archive(ArchiveKind::Full, &[&hop, &db]);
    let local_store = store(&[]);
    let mut p = plan(&local_store, &a, &no_schemes()).unwrap();
    let errors: Vec<_> = p.entries.iter().map(|e| e.error.as_deref()).collect();
    assert_eq!(
        errors,
        vec![
            Some("Its host is empty."),
            Some("It jumps through hop, which is not imported."),
        ]
    );

    // A new hop set to Skip is not imported either.
    let good_hop = ssh("hop", AuthMethod::Agent, Vec::new());
    let db = ssh("db", AuthMethod::Agent, vec![good_hop.id]);
    let a = archive(ArchiveKind::Full, &[&good_hop, &db]);
    p = plan(&local_store, &a, &no_schemes()).unwrap();
    assert_eq!(p.entries[1].error, None);
    set_choice(&mut p, 0, Choice::Skip);
    assert_eq!(
        p.entries[1].error.as_deref(),
        Some("It jumps through hop, which is not imported.")
    );
}
