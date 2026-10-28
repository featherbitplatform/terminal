//! Tests for `archive_build`: key files live in temporary folders and secrets in a
//! `MemoryStore`, so nothing touches the real keychain or `~/.ssh`.

use std::path::{Path, PathBuf};

use fbterm_config::profile::{
    AuthMethod, LocalProfile, ProfileKind, SshProfile, StartMode, TunnelDef, TunnelKind,
};
use fbterm_config::schemes::{self, SchemeSet};
use fbterm_config::{Profile, ProfileId, ProfileStore};
use fbterm_secrets::archive::{self, ArchiveKind};
use fbterm_secrets::{MemoryStore, SecretError, SecretKind, SecretStore};

use crate::archive_build::build;

const PASSWORD: &str = "pw-saved-7c1e9a";
const PASSPHRASE: &str = "pp-saved-3b8d02";
const ENV_VALUE: &str = "tok-local-5f2a71";
const KEY_BYTES: &[u8] = b"-----BEGIN OPENSSH PRIVATE KEY-----\nkeymaterial-9e4c\n";
const ARCHIVE_PASSWORD: &str = "archive password";

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

fn key(path: &Path, passphrase: bool) -> AuthMethod {
    AuthMethod::Key {
        path: Some(path.to_path_buf()),
        passphrase,
    }
}

fn local(name: &str) -> Profile {
    Profile {
        id: ProfileId::new(),
        name: name.into(),
        group: None,
        accent: None,
        accent_style: Default::default(),
        color_scheme: None,
        kind: ProfileKind::Local(LocalProfile {
            program: "/bin/local-shell-prog".into(),
            args: vec!["-l".into()],
            cwd: Some("/work/local-cwd".into()),
            env: [("API_TOKEN".to_string(), ENV_VALUE.to_string())].into(),
        }),
    }
}

fn write_key(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn no_schemes() -> SchemeSet {
    let dir = tempfile::tempdir().unwrap();
    SchemeSet::load(dir.path()).0
}

fn ids(archive: &archive::Archive) -> Vec<ProfileId> {
    archive.profiles.iter().map(|p| p.id).collect()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn jump_chain_profiles_are_included() {
    let outer = ssh("outer", AuthMethod::Agent, Vec::new());
    let inner = ssh("inner", AuthMethod::Agent, vec![outer.id]);
    let target = ssh("target", AuthMethod::Agent, vec![inner.id]);
    let other = ssh("other", AuthMethod::Agent, Vec::new());
    let store = ProfileStore {
        profiles: vec![other.clone(), outer.clone(), inner.clone(), target.clone()],
    };
    let secrets = MemoryStore::new();
    for kind in [ArchiveKind::Full, ArchiveKind::Shared] {
        let (archive, warnings) =
            build(&store, &[target.id], kind, false, &secrets, &no_schemes()).unwrap();
        assert_eq!(archive.format, archive::FORMAT);
        assert_eq!(archive.kind, kind);
        // Store order, the unselected unrelated profile left out.
        assert_eq!(ids(&archive), vec![outer.id, inner.id, target.id]);
        assert!(warnings.is_empty(), "{warnings:?}");
    }
}

#[test]
fn shared_leaves_out_local_profiles() {
    let shell = local("shell");
    let server = ssh("server", AuthMethod::Agent, Vec::new());
    let tools = local("tools");
    let store = ProfileStore {
        profiles: vec![shell.clone(), server.clone(), tools.clone()],
    };
    let secrets = MemoryStore::new();
    let selected = [tools.id, shell.id, server.id];
    let (shared, warnings) = build(
        &store,
        &selected,
        ArchiveKind::Shared,
        false,
        &secrets,
        &no_schemes(),
    )
    .unwrap();
    assert_eq!(ids(&shared), vec![server.id]);
    assert_eq!(
        warnings,
        vec![
            "Local profiles are not part of a shared archive, so shell, tools were left out."
                .to_string()
        ]
    );

    // Nothing left to share is an error, not an empty archive.
    let err = build(
        &store,
        &[shell.id, tools.id],
        ArchiveKind::Shared,
        false,
        &secrets,
        &no_schemes(),
    )
    .unwrap_err();
    assert_eq!(err, "A shared archive needs at least one SSH profile.");

    // A full archive carries the local profiles as they are, without a warning.
    let (full, warnings) = build(
        &store,
        &selected,
        ArchiveKind::Full,
        false,
        &secrets,
        &no_schemes(),
    )
    .unwrap();
    assert_eq!(full.profiles, vec![shell, server, tools]);
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn broken_jump_chains_still_export() {
    // A cycle (a -> b -> a), a jump to a profile that does not exist, and a local profile
    // used as a hop: all invalid in the store, none of them blocks or hangs the export.
    let mut a = ssh("a", AuthMethod::Agent, Vec::new());
    let b = ssh("b", AuthMethod::Agent, vec![a.id]);
    let hop = local("hop");
    let missing = ProfileId::new();
    if let ProfileKind::Ssh(s) = &mut a.kind {
        s.jump = vec![b.id, missing, hop.id];
    }
    let store = ProfileStore {
        profiles: vec![a.clone(), b.clone(), hop.clone()],
    };
    let secrets = MemoryStore::new();

    let (full, warnings) = build(
        &store,
        &[a.id],
        ArchiveKind::Full,
        false,
        &secrets,
        &no_schemes(),
    )
    .unwrap();
    assert_eq!(ids(&full), vec![a.id, b.id, hop.id]);
    assert!(warnings.is_empty(), "{warnings:?}");

    // Shared drops the local hop (with the warning); the jump list keeps its id, which names
    // nothing local.
    let (shared, warnings) = build(
        &store,
        &[a.id],
        ArchiveKind::Shared,
        false,
        &secrets,
        &no_schemes(),
    )
    .unwrap();
    assert_eq!(ids(&shared), vec![a.id, b.id]);
    let ProfileKind::Ssh(s) = &shared.profiles[0].kind else {
        panic!("not ssh")
    };
    assert_eq!(s.jump, vec![b.id, missing, hop.id]);
    assert_eq!(
        warnings,
        vec!["Local profiles are not part of a shared archive, so hop was left out.".to_string()]
    );
}

#[test]
fn a_saved_passphrase_travels_whatever_the_flag_says() {
    // A passphrase remembered while connecting is saved even when the profile never said
    // its key has one: a full archive carries it, and its copy of the profile says so.
    let dir = tempfile::tempdir().unwrap();
    let key_path = write_key(dir.path(), "id_plain", KEY_BYTES);
    let unflagged = ssh("unflagged", key(&key_path, false), Vec::new());
    let nothing = ssh("nothing", key(&key_path, false), Vec::new());
    let store = ProfileStore {
        profiles: vec![unflagged.clone(), nothing.clone()],
    };
    let secrets = MemoryStore::new();
    secrets
        .set(&unflagged.id, SecretKind::KeyPassphrase, "pp-typed-0c2f")
        .unwrap();
    let (full, _) = build(
        &store,
        &[unflagged.id, nothing.id],
        ArchiveKind::Full,
        false,
        &secrets,
        &no_schemes(),
    )
    .unwrap();
    assert_eq!(full.secrets.len(), 1);
    assert_eq!(full.secrets[0].profile, unflagged.id);
    assert_eq!(full.secrets[0].kind, SecretKind::KeyPassphrase);
    assert_eq!(full.secrets[0].value.as_str(), "pp-typed-0c2f");
    let flag = |id: ProfileId| match &full.profiles.iter().find(|p| p.id == id).unwrap().kind {
        ProfileKind::Ssh(SshProfile {
            auth: AuthMethod::Key { passphrase, .. },
            ..
        }) => *passphrase,
        _ => panic!("key profile"),
    };
    assert!(flag(unflagged.id), "the archived copy says the key has one");
    assert!(!flag(nothing.id), "nothing saved: unchanged");
}

#[test]
fn shared_archive_contains_no_secrets_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = write_key(dir.path(), "team_key_4d9b", KEY_BYTES);
    let key_dir = dir.path().to_string_lossy().into_owned();

    let bastion = ssh("bastion", key(&key_path, true), Vec::new());
    let mut db = ssh("db", AuthMethod::Password, vec![bastion.id]);
    if let ProfileKind::Ssh(s) = &mut db.kind {
        s.tunnels.push(TunnelDef {
            name: "pg".into(),
            kind: TunnelKind::Local {
                bind: "127.0.0.1".into(),
                port: 5432,
                host: "localhost".into(),
                host_port: 5432,
            },
            start: StartMode::WithShell,
        });
    }
    let shell = local("local-only-shell");
    let store = ProfileStore {
        profiles: vec![bastion.clone(), db.clone(), shell.clone()],
    };
    let secrets = MemoryStore::new();
    secrets.set(&db.id, SecretKind::Password, PASSWORD).unwrap();
    secrets
        .set(&bastion.id, SecretKind::KeyPassphrase, PASSPHRASE)
        .unwrap();
    // Secrets saved under kinds the profile does not use are left out too.
    secrets
        .set(&bastion.id, SecretKind::Password, "pw-stale-81aa")
        .unwrap();

    // Keys requested: a shared archive ignores the request.
    let (built, _) = build(
        &store,
        &[db.id, shell.id],
        ArchiveKind::Shared,
        true,
        &secrets,
        &no_schemes(),
    )
    .unwrap();
    assert!(built.secrets.is_empty());
    assert!(built.keys.is_empty());
    let sealed = archive::seal(&built, ARCHIVE_PASSWORD).unwrap();

    let plain = archive::open_plaintext(&sealed, ARCHIVE_PASSWORD).unwrap();
    let opened = archive::open(&sealed, ARCHIVE_PASSWORD).unwrap();
    let reserialized = toml::to_string(&opened).unwrap();

    let forbidden: Vec<Vec<u8>> = [
        PASSWORD.as_bytes(),
        PASSPHRASE.as_bytes(),
        b"pw-stale-81aa",
        KEY_BYTES,
        b"keymaterial-9e4c",
        key_path.to_string_lossy().as_bytes(),
        key_dir.as_bytes(),
        b"team_key_4d9b",
        ENV_VALUE.as_bytes(),
        b"API_TOKEN",
        b"local-only-shell",
        b"local-shell-prog",
        b"local-cwd",
    ]
    .iter()
    .map(|n| n.to_vec())
    .collect();
    for needle in &forbidden {
        let shown = String::from_utf8_lossy(needle);
        assert!(!contains(&plain, needle), "plaintext holds {shown}");
        assert!(
            !contains(reserialized.as_bytes(), needle),
            "archive holds {shown}"
        );
    }

    // What a shared archive does carry: the method as a hint, jumps and tunnels.
    assert_eq!(ids(&opened), vec![bastion.id, db.id]);
    let ProfileKind::Ssh(b) = &opened.profiles[0].kind else {
        panic!("not ssh")
    };
    assert_eq!(
        b.auth,
        AuthMethod::Key {
            path: None,
            passphrase: false
        }
    );
    let ProfileKind::Ssh(d) = &opened.profiles[1].kind else {
        panic!("not ssh")
    };
    assert_eq!(d.auth, AuthMethod::Password);
    assert_eq!(d.jump, vec![bastion.id]);
    assert_eq!(d.tunnels.len(), 1);
    assert!(opened.secrets.is_empty() && opened.keys.is_empty());
}

#[test]
fn full_includes_secrets_and_optional_keys() {
    let dir = tempfile::tempdir().unwrap();
    let key_path = write_key(dir.path(), "id_ed25519", KEY_BYTES);
    let bastion = ssh("bastion", key(&key_path, true), Vec::new());
    let db = ssh("db", AuthMethod::Password, vec![bastion.id]);
    let store = ProfileStore {
        profiles: vec![bastion.clone(), db.clone()],
    };
    let secrets = MemoryStore::new();
    secrets.set(&db.id, SecretKind::Password, PASSWORD).unwrap();
    secrets
        .set(&bastion.id, SecretKind::KeyPassphrase, PASSPHRASE)
        .unwrap();

    // Without key files: profiles as they are, both secrets, no keys.
    let (full, warnings) = build(
        &store,
        &[db.id],
        ArchiveKind::Full,
        false,
        &secrets,
        &no_schemes(),
    )
    .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(full.profiles, vec![bastion.clone(), db.clone()]);
    let mut saved: Vec<_> = full
        .secrets
        .iter()
        .map(|s| (s.profile, s.kind, s.value.as_str().to_owned()))
        .collect();
    saved.sort_by_key(|s| s.0 != bastion.id);
    assert_eq!(
        saved,
        vec![
            (bastion.id, SecretKind::KeyPassphrase, PASSPHRASE.to_owned()),
            (db.id, SecretKind::Password, PASSWORD.to_owned()),
        ]
    );
    assert!(full.keys.is_empty());

    // With key files.
    let (full, warnings) = build(
        &store,
        &[db.id],
        ArchiveKind::Full,
        true,
        &secrets,
        &no_schemes(),
    )
    .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(full.keys.len(), 1);
    assert_eq!(full.keys[0].file_name, "id_ed25519");
    assert_eq!(full.keys[0].bytes.as_slice(), KEY_BYTES);
    assert_eq!(full.keys[0].profiles, vec![bastion.id]);
}

#[test]
fn missing_key_is_left_out_with_warning() {
    let dir = tempfile::tempdir().unwrap();
    let present = write_key(dir.path(), "id_present", KEY_BYTES);
    let missing = dir.path().join("id_gone");
    let folder = dir.path().join("id_folder");
    std::fs::create_dir(&folder).unwrap();
    let ok = ssh("ok", key(&present, false), Vec::new());
    let gone = ssh("gone", key(&missing, false), Vec::new());
    let odd = ssh("odd", key(&folder, false), Vec::new());
    let store = ProfileStore {
        profiles: vec![ok.clone(), gone.clone(), odd.clone()],
    };
    let (full, warnings) = build(
        &store,
        &[ok.id, gone.id, odd.id],
        ArchiveKind::Full,
        true,
        &MemoryStore::new(),
        &no_schemes(),
    )
    .unwrap();
    // The export still succeeds, with every profile and the readable key.
    assert_eq!(ids(&full), vec![ok.id, gone.id, odd.id]);
    assert_eq!(full.keys.len(), 1);
    assert_eq!(full.keys[0].profiles, vec![ok.id]);
    assert_eq!(
        warnings,
        vec![
            format!(
                "The key file {} of gone could not be read, so it was left out.",
                missing.display()
            ),
            format!(
                "The key file {} of odd could not be read, so it was left out.",
                folder.display()
            ),
        ]
    );
}

#[test]
fn shared_key_stored_once() {
    let dir = tempfile::tempdir().unwrap();
    let other_dir = dir.path().join("other");
    std::fs::create_dir(&other_dir).unwrap();
    let key_path = write_key(dir.path(), "id_ed25519", KEY_BYTES);
    // The same file through another spelling of its path.
    let same_file = dir.path().join("other").join("..").join("id_ed25519");
    // A different key with the same file name.
    let other_key = write_key(&other_dir, "id_ed25519", b"other key bytes");

    let a = ssh("a", key(&key_path, false), Vec::new());
    let b = ssh("b", key(&same_file, false), Vec::new());
    let c = ssh("c", key(&other_key, false), Vec::new());
    let store = ProfileStore {
        profiles: vec![a.clone(), b.clone(), c.clone()],
    };
    let (full, warnings) = build(
        &store,
        &[a.id, b.id, c.id],
        ArchiveKind::Full,
        true,
        &MemoryStore::new(),
        &no_schemes(),
    )
    .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(full.keys.len(), 2);
    assert_eq!(full.keys[0].file_name, "id_ed25519");
    assert_eq!(full.keys[0].bytes.as_slice(), KEY_BYTES);
    assert_eq!(full.keys[0].profiles, vec![a.id, b.id]);
    assert_eq!(full.keys[1].file_name, "id_ed25519-2");
    assert_eq!(full.keys[1].bytes.as_slice(), b"other key bytes");
    assert_eq!(full.keys[1].profiles, vec![c.id]);
}

#[test]
fn oversized_key_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let limit = archive::MAX_KEY_BYTES as usize;
    let big = write_key(dir.path(), "id_big", &vec![b'k'; limit + 1]);
    let edge = write_key(dir.path(), "id_edge", &vec![b'k'; limit]);
    let p = ssh("p", key(&big, false), Vec::new());
    let q = ssh("q", key(&edge, false), Vec::new());
    let store = ProfileStore {
        profiles: vec![p.clone(), q.clone()],
    };
    let (full, warnings) = build(
        &store,
        &[p.id, q.id],
        ArchiveKind::Full,
        true,
        &MemoryStore::new(),
        &no_schemes(),
    )
    .unwrap();
    assert_eq!(ids(&full), vec![p.id, q.id]);
    // Exactly 1 MB is still accepted.
    assert_eq!(full.keys.len(), 1);
    assert_eq!(full.keys[0].file_name, "id_edge");
    assert_eq!(full.keys[0].bytes.len(), limit);
    assert_eq!(
        warnings,
        vec![format!(
            "The key file {} of p is larger than 1 MB, so it is not a key and was left out.",
            big.display()
        )]
    );
}

#[cfg(unix)]
#[test]
fn key_through_a_link_is_read_from_its_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = write_key(dir.path(), "real_key", KEY_BYTES);
    let link = dir.path().join("id_link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let a = ssh("a", key(&link, false), Vec::new());
    let b = ssh("b", key(&target, false), Vec::new());
    let store = ProfileStore {
        profiles: vec![a.clone(), b.clone()],
    };
    let (full, warnings) = build(
        &store,
        &[a.id, b.id],
        ArchiveKind::Full,
        true,
        &MemoryStore::new(),
        &no_schemes(),
    )
    .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    // One file on disk, stored once under the name the first profile knows it by.
    assert_eq!(full.keys.len(), 1);
    assert_eq!(full.keys[0].file_name, "id_link");
    assert_eq!(full.keys[0].profiles, vec![a.id, b.id]);
}

#[test]
fn exported_scheme_keeps_credit_and_drops_gallery() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = schemes::builtins().remove(1);
    s.builtin = false;
    s.id = "gallery-nord".into();
    s.credit = Some(schemes::Credit {
        project: "Nord".into(),
        author: "A".into(),
        url: "https://example.org".into(),
        license: "MIT".into(),
        copyright: "c".into(),
        license_file: "LICENSE".into(),
        description: "d".into(),
        derived: false,
    });
    s.gallery = Some(schemes::GalleryInfo {
        id: "nord".into(),
        version: 1,
        colors_hash: schemes::colors_hash(&s.colors),
    });
    schemes::save_user(dir.path(), &s).unwrap();
    let (set, errors) = SchemeSet::load(dir.path());
    assert!(errors.is_empty(), "{errors:?}");
    assert!(set.get("gallery-nord").unwrap().gallery.is_some());
    let mut p = ssh("a", AuthMethod::Agent, Vec::new());
    p.color_scheme = Some(s.id.clone());
    let store = ProfileStore {
        profiles: vec![p.clone()],
    };
    let (archive, _) = build(
        &store,
        &[p.id],
        ArchiveKind::Full,
        false,
        &MemoryStore::new(),
        &set,
    )
    .unwrap();
    assert_eq!(archive.schemes.len(), 1);
    assert_eq!(archive.schemes[0].credit, s.credit);
    assert!(archive.schemes[0].gallery.is_none());
}

#[test]
fn used_user_schemes_are_included() {
    let dir = tempfile::tempdir().unwrap();
    let mut used = schemes::builtins().remove(1);
    used.id = schemes::new_user_id();
    used.name = "Used".into();
    used.builtin = false;
    let mut unused = used.clone();
    unused.id = schemes::new_user_id();
    unused.name = "Unused".into();
    schemes::save_user(dir.path(), &used).unwrap();
    schemes::save_user(dir.path(), &unused).unwrap();
    let (set, errors) = SchemeSet::load(dir.path());
    assert!(errors.is_empty(), "{errors:?}");

    let mut a = ssh("a", AuthMethod::Agent, Vec::new());
    a.color_scheme = Some(used.id.clone());
    let mut b = ssh("b", AuthMethod::Agent, vec![a.id]);
    // A built-in travels as its id only; the profile keeps the setting.
    b.color_scheme = Some("harbor".into());
    let mut c = ssh("c", AuthMethod::Agent, Vec::new());
    c.color_scheme = Some(used.id.clone());
    let mut gone = ssh("gone", AuthMethod::Agent, Vec::new());
    gone.color_scheme = Some("user-missing".into());
    let store = ProfileStore {
        profiles: vec![a.clone(), b.clone(), c.clone(), gone.clone()],
    };
    for kind in [ArchiveKind::Full, ArchiveKind::Shared] {
        let (archive, _) = build(
            &store,
            &[b.id, c.id, gone.id],
            kind,
            false,
            &MemoryStore::new(),
            &set,
        )
        .unwrap();
        // The scheme of a jump-chain profile counts, once.
        let mut expected = used.clone();
        expected.builtin = false;
        assert_eq!(archive.schemes, vec![expected]);
        assert_eq!(archive.profiles[1].color_scheme.as_deref(), Some("harbor"));
        assert_eq!(
            archive.profiles[2].color_scheme.as_deref(),
            Some(used.id.as_str())
        );
    }
}

#[test]
fn unknown_selected_profile_is_an_error() {
    let store = ProfileStore::default();
    let err = build(
        &store,
        &[ProfileId::new()],
        ArchiveKind::Full,
        false,
        &MemoryStore::new(),
        &no_schemes(),
    )
    .unwrap_err();
    assert_eq!(err, "A selected profile no longer exists.");
}

/// A store whose reads fail, like a keychain that refuses access.
struct Unreadable;

impl SecretStore for Unreadable {
    fn get(&self, _: &ProfileId, _: SecretKind) -> Result<Option<String>, SecretError> {
        Err(SecretError::Backend("denied".into()))
    }
    fn set(&self, _: &ProfileId, _: SecretKind, _: &str) -> Result<(), SecretError> {
        Err(SecretError::Backend("denied".into()))
    }
    fn remove(&self, _: &ProfileId) -> Result<(), SecretError> {
        Err(SecretError::Backend("denied".into()))
    }
    fn remove_kind(&self, _: &ProfileId, _: SecretKind) -> Result<(), SecretError> {
        Err(SecretError::Backend("denied".into()))
    }
}

#[test]
fn unreadable_secrets_stop_a_full_export_only() {
    let server = ssh("server", AuthMethod::Password, Vec::new());
    let store = ProfileStore {
        profiles: vec![server.clone()],
    };
    let err = build(
        &store,
        &[server.id],
        ArchiveKind::Full,
        false,
        &Unreadable,
        &no_schemes(),
    )
    .unwrap_err();
    assert_eq!(
        err,
        "The saved passwords and passphrases could not be read."
    );
    // A shared archive never reads secrets.
    let (shared, _) = build(
        &store,
        &[server.id],
        ArchiveKind::Shared,
        false,
        &Unreadable,
        &no_schemes(),
    )
    .unwrap();
    assert_eq!(ids(&shared), vec![server.id]);
}
