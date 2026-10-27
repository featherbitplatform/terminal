//! Building profile archives for export: pure, no UI (the export dialog calls [`build`]).
//!
//! A shared archive is safe by construction: it is assembled from SSH profiles with their
//! key path and passphrase flag cleared, and its secret and key lists are never filled,
//! whatever the store holds.

// The export dialog that calls `build` lands with the export and import UI.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use fbterm_config::profile::{AuthMethod, ProfileKind};
use fbterm_config::schemes::SchemeSet;
use fbterm_config::{Profile, ProfileId, ProfileStore};
use fbterm_secrets::archive::{Archive, ArchiveKind, ArchivedKey, ArchivedSecret, MAX_KEY_BYTES};
use fbterm_secrets::{SecretKind, SecretStore};
use zeroize::Zeroizing;

const GONE: &str = "A selected profile no longer exists.";
const SECRETS_UNREADABLE: &str = "The saved passwords and passphrases could not be read.";

/// Build an archive of the `selected` profiles plus every profile their jump chains reach,
/// in the store's order. Returns the archive and warnings for the export dialog (key files
/// left out).
///
/// - `Full`: profiles as they are, the saved secret each SSH profile's method uses, and with
///   `include_keys` the key files (each file once; missing, unreadable or over
///   [`MAX_KEY_BYTES`] files are left out with a warning).
/// - `Shared`: SSH profiles only, with key profiles reduced to "key" without a path or a
///   passphrase flag; no secrets and no key files (`include_keys` is ignored).
///
/// User color schemes the archived profiles use travel with them.
pub(crate) fn build(
    store: &ProfileStore,
    selected: &[ProfileId],
    kind: ArchiveKind,
    include_keys: bool,
    secrets: &dyn SecretStore,
    schemes: &SchemeSet,
) -> Result<(Archive, Vec<String>), String> {
    let wanted = closure(store, selected)?;
    let mut archive = Archive::new(
        kind,
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    );
    let mut warnings = Vec::new();

    archive.profiles = store
        .profiles
        .iter()
        .filter(|p| wanted.contains(&p.id))
        .filter_map(|p| match kind {
            ArchiveKind::Full => Some(p.clone()),
            ArchiveKind::Shared => shared_copy(p),
        })
        .collect();

    if kind == ArchiveKind::Full {
        archive.secrets = saved_secrets(&archive.profiles, secrets)?;
        if include_keys {
            archive.keys = key_files(&archive.profiles, &mut warnings);
        }
    }

    for p in &archive.profiles {
        let Some(id) = &p.color_scheme else { continue };
        if let Some(scheme) = schemes.get(id)
            && !scheme.builtin
            && !archive.schemes.iter().any(|s| s.id == scheme.id)
        {
            archive.schemes.push(scheme.clone());
        }
    }
    Ok((archive, warnings))
}

/// The selected profiles and every profile reachable through their jump chains. A jump to a
/// profile that does not exist is skipped (the profile is already invalid in the store).
fn closure(store: &ProfileStore, selected: &[ProfileId]) -> Result<HashSet<ProfileId>, String> {
    let mut seen = HashSet::new();
    let mut todo = Vec::new();
    for &id in selected {
        if store.get(id).is_none() {
            return Err(GONE.into());
        }
        todo.push(id);
    }
    while let Some(id) = todo.pop() {
        let Some(profile) = store.get(id) else {
            continue;
        };
        if !seen.insert(id) {
            continue;
        }
        if let ProfileKind::Ssh(ssh) = &profile.kind {
            todo.extend(ssh.jump.iter().copied());
        }
    }
    Ok(seen)
}

/// The shared form of a profile: `None` for a local profile; an SSH profile without its key
/// path or passphrase flag. Everything else (host, user, jumps, tunnels, scheme) travels.
fn shared_copy(profile: &Profile) -> Option<Profile> {
    let ProfileKind::Ssh(ssh) = &profile.kind else {
        return None;
    };
    let mut ssh = ssh.clone();
    ssh.auth = match ssh.auth {
        AuthMethod::Password => AuthMethod::Password,
        AuthMethod::Key { .. } => AuthMethod::Key {
            path: None,
            passphrase: false,
        },
        AuthMethod::Agent => AuthMethod::Agent,
    };
    Some(Profile {
        kind: ProfileKind::Ssh(ssh),
        ..profile.clone()
    })
}

/// The saved secret each SSH profile's method uses: the password of a password profile, the
/// passphrase of a key profile. A store that cannot be read stops the export rather than
/// silently making an archive without secrets.
fn saved_secrets(
    profiles: &[Profile],
    secrets: &dyn SecretStore,
) -> Result<Vec<ArchivedSecret>, String> {
    let mut out = Vec::new();
    for p in profiles {
        let ProfileKind::Ssh(ssh) = &p.kind else {
            continue;
        };
        let kind = match ssh.auth {
            AuthMethod::Password => SecretKind::Password,
            AuthMethod::Key { .. } => SecretKind::KeyPassphrase,
            AuthMethod::Agent => continue,
        };
        let value = secrets
            .get(&p.id, kind)
            .map_err(|_| SECRETS_UNREADABLE.to_string())?;
        if let Some(value) = value {
            out.push(ArchivedSecret {
                profile: p.id,
                kind,
                value: Zeroizing::new(value),
            });
        }
    }
    Ok(out)
}

/// The key files the profiles use, each file (by canonical path) stored once under its own
/// file name, made distinct (`id_ed25519-2`) when two different files share one.
fn key_files(profiles: &[Profile], warnings: &mut Vec<String>) -> Vec<ArchivedKey> {
    // (canonical path, index into `keys`) of files already read.
    let mut read: Vec<(PathBuf, usize)> = Vec::new();
    // (configured path, profile names, why) of files left out, in order.
    let mut left_out: Vec<(PathBuf, Vec<String>, Problem)> = Vec::new();
    let mut keys: Vec<ArchivedKey> = Vec::new();

    for p in profiles {
        let ProfileKind::Ssh(ssh) = &p.kind else {
            continue;
        };
        let AuthMethod::Key {
            path: Some(path), ..
        } = &ssh.auth
        else {
            continue;
        };
        let expanded = expand_home(path);
        let canonical = std::fs::canonicalize(&expanded).ok();
        if let Some(canonical) = &canonical
            && let Some((_, i)) = read.iter().find(|(c, _)| c == canonical)
        {
            keys[*i].profiles.push(p.id);
            continue;
        }
        let result = match canonical {
            Some(canonical) => read_key(&canonical).map(|bytes| (canonical, bytes)),
            None => Err(Problem::Unreadable),
        };
        match result {
            Ok((canonical, bytes)) => {
                let file_name = distinct_name(&keys, path);
                read.push((canonical, keys.len()));
                keys.push(ArchivedKey {
                    file_name,
                    bytes,
                    profiles: vec![p.id],
                });
            }
            Err(problem) => match left_out.iter_mut().find(|(c, _, _)| c == path) {
                Some((_, names, _)) => names.push(p.name.clone()),
                None => left_out.push((path.clone(), vec![p.name.clone()], problem)),
            },
        }
    }

    for (path, names, problem) in left_out {
        let names = names.join(", ");
        let path = path.display();
        warnings.push(match problem {
            Problem::Unreadable => {
                format!("The key file {path} of {names} could not be read, so it was left out.")
            }
            Problem::TooLarge => format!(
                "The key file {path} of {names} is larger than 1 MB, so it is not a key and was left out."
            ),
        });
    }
    keys
}

#[derive(Clone, Copy)]
enum Problem {
    Unreadable,
    TooLarge,
}

/// Read a key file: a regular file only (never a folder, pipe or device), at most
/// [`MAX_KEY_BYTES`], into a wiped buffer.
fn read_key(path: &Path) -> Result<Zeroizing<Vec<u8>>, Problem> {
    // `path` is canonical, so this is the link's target, not the link.
    let meta = std::fs::metadata(path).map_err(|_| Problem::Unreadable)?;
    if !meta.is_file() {
        return Err(Problem::Unreadable);
    }
    if meta.len() > MAX_KEY_BYTES {
        return Err(Problem::TooLarge);
    }
    let file = File::open(path).map_err(|_| Problem::Unreadable)?;
    // The opened file again, in case the path changed between the checks and the open.
    let meta = file.metadata().map_err(|_| Problem::Unreadable)?;
    if !meta.is_file() {
        return Err(Problem::Unreadable);
    }
    if meta.len() > MAX_KEY_BYTES {
        return Err(Problem::TooLarge);
    }
    // Sized for the whole file plus the probe byte, so reading never reallocates (which
    // would leave an unwiped copy behind) unless the file grows meanwhile.
    let mut bytes = Zeroizing::new(Vec::with_capacity(meta.len() as usize + 1));
    file.take(MAX_KEY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Problem::Unreadable)?;
    if bytes.len() as u64 > MAX_KEY_BYTES {
        return Err(Problem::TooLarge);
    }
    Ok(bytes)
}

/// The key's file name (never a directory), with `-2`, `-3`, ... added when a different key
/// already uses it.
fn distinct_name(keys: &[ArchivedKey], path: &Path) -> String {
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "key".into());
    let taken = |name: &str| keys.iter().any(|k| k.file_name == name);
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|name| !taken(name))
        .expect("an unused name")
}

/// `~/...` paths are relative to the home directory, as when connecting.
fn expand_home(path: &Path) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~")
        && let Some(dirs) = directories::BaseDirs::new()
    {
        return dirs.home_dir().join(rest);
    }
    path.to_path_buf()
}
