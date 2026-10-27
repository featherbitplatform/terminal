//! Planning and applying profile imports: pure, no UI (the import dialog calls [`plan`],
//! [`set_choice`] and [`apply`]).
//!
//! An archive is untrusted input: anyone can hand over a file. So nothing here writes to disk
//! (the only file access is reading an existing key file in the chosen key folder, to see
//! whether it is identical), and everything the archive says is checked again:
//!
//! - A shared archive's contents are a claim. Its secrets and key files are ignored, its local
//!   profiles dropped and its key paths cleared, whatever the file holds.
//! - Imported profiles get the profile store's own validation (names, hosts, tunnels, jump
//!   references, jumps through non-SSH profiles, cycles); an entry that would be invalid shows
//!   the problem in [`ImportEntry::error`] and is not imported.
//! - Color schemes get the scheme file rules (plain non-built-in id, the scheme file checks,
//!   the scheme file size cap); a scheme with a built-in id is the built-in. Invalid schemes are
//!   left out with a warning and the profiles using them follow the global scheme.
//! - Key file names are reduced to one plain file name inside the key folder, key files over
//!   [`MAX_KEY_BYTES`] are left out with a warning, and an existing file is never replaced.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use fbterm_config::profile::{AuthMethod, ProfileKind};
use fbterm_config::schemes::{self, BUILTIN_IDS, Scheme, SchemeSet};
use fbterm_config::{Profile, ProfileId, ProfileStore, ValidationError};
use fbterm_secrets::SecretKind;
use fbterm_secrets::archive::{Archive, ArchiveKind, ArchivedKey, MAX_KEY_BYTES};
use zeroize::Zeroizing;

const READ_ONLY: &str =
    "Profiles are read-only until profiles.toml is fixed, so nothing was imported.";
const STALE: &str = "The profiles changed since the archive was opened. Open it again to import.";
const TWICE: &str = "It appears more than once in the archive.";
const NO_NAME: &str = "Its name is empty.";
const MISSING: &str = "It jumps through a profile that is not in the archive or on this computer.";
const TOO_LARGE: &str = "This archive is too large or malformed to import.";

/// The most an archive may carry: far beyond any real setup, small enough that planning stays
/// quick whatever shape a crafted archive has.
const MAX_PROFILES: usize = 1000;
const MAX_JUMPS: usize = 16;
const MAX_SCHEMES: usize = 200;
const MAX_KEYS: usize = 200;
/// A password or a passphrase per profile.
const MAX_SECRETS: usize = 2 * MAX_PROFILES;

/// The name a key file gets when the archived name is not a plain, harmless file name.
const FALLBACK_KEY_NAME: &str = "id_imported";

/// Files OpenSSH reads from `~/.ssh` (compared ignoring case): a key file never takes one of
/// these names, even where none exists yet.
const SSH_FILES: [&str; 8] = [
    "authorized_keys",
    "authorized_keys2",
    "config",
    "known_hosts",
    "known_hosts2",
    "environment",
    "rc",
    "allowed_signers",
];

/// How many `-2`, `-3`, ... names are tried for a key file before giving up.
const MAX_KEY_NAME_TRIES: u32 = 1000;

/// What to do with an archived profile.
///
/// For a profile that matches a local one: `Skip` keeps the local profile (references inside
/// the archive then point at it), `Overwrite` replaces it with what the archive carries, and
/// `KeepBoth` adds the archived one as a copy with a new id and the name `{name} (imported)`.
/// For a profile without a match, `Skip` leaves it out and either other choice adds it with
/// its own id and name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Choice {
    Skip,
    Overwrite,
    KeepBoth,
}

/// One archived profile in the import preview.
#[derive(Clone, Debug)]
pub(crate) struct ImportEntry {
    /// The archived profile as it would be imported: for a shared archive without key path,
    /// its color scheme already pointing at the scheme it will use.
    pub incoming: Profile,
    /// The local profile with the same id or, failing that, the same name.
    pub matched: Option<ProfileId>,
    /// `Skip` by default for a match; `KeepBoth` (add it) otherwise.
    pub choice: Choice,
    /// Why this profile cannot be imported with its current choice (it is then left out).
    pub error: Option<String>,
    /// The id a Keep both copy gets, fixed when planning.
    copy_id: ProfileId,
    /// A problem no choice resolves (a repeated id, an empty name).
    flaw: Option<String>,
}

/// What happens to an archived color scheme.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SchemeOutcome {
    /// A local scheme with the same id and content is used.
    Reused,
    /// Imported under its own id.
    Imported,
    /// A local scheme has the id with other content: imported with a new id and the name
    /// `{name} (imported)`.
    Renamed,
}

/// One valid user scheme of the archive.
#[derive(Clone, Debug)]
pub(crate) struct SchemeEntry {
    /// The id the archive's profiles use.
    pub archived_id: String,
    /// The scheme as it will be used: the local one when reused, else as it will be saved.
    pub scheme: Scheme,
    pub outcome: SchemeOutcome,
}

/// The import preview of an opened archive.
#[derive(Clone, Debug)]
pub(crate) struct ImportPlan {
    pub entries: Vec<ImportEntry>,
    pub schemes: Vec<SchemeEntry>,
    /// A full archive with saved passwords or passphrases.
    pub has_secrets: bool,
    /// A full archive with key files that imported key profiles use.
    pub has_keys: bool,
    /// Things left out of the archive (invalid schemes, oversized key files), for the preview.
    pub warnings: Vec<String>,
    kind: ArchiveKind,
    /// The local profiles the plan was made against.
    local: ProfileStore,
    /// The archived profiles of each key file that can be written (full archives only).
    key_users: Vec<Vec<ProfileId>>,
}

/// A saved secret to store for an imported profile.
pub(crate) struct SecretOp {
    pub profile: ProfileId,
    pub kind: SecretKind,
    pub value: Zeroizing<String>,
}

impl fmt::Debug for SecretOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretOp")
            .field("profile", &self.profile)
            .field("kind", &self.kind)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// A key file to create. `path` is always directly inside the key folder and was free when
/// planned: the app creates it with `create_new` (never replacing a file that appeared since),
/// readable and writable by the user only (mode `0600`), and stops the import if that fails.
pub(crate) struct NewKeyFile {
    pub path: PathBuf,
    pub bytes: Zeroizing<Vec<u8>>,
}

impl fmt::Debug for NewKeyFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewKeyFile")
            .field("path", &self.path)
            .field(
                "bytes",
                &format_args!("<{} bytes redacted>", self.bytes.len()),
            )
            .finish()
    }
}

/// Everything an import changes, computed without touching disk. The app writes the key
/// files, then the schemes (through `schemes::save_user` only), then `profiles.toml` (through
/// the usual save path), then the secrets.
#[derive(Debug)]
pub(crate) struct Applied {
    /// The local profiles with the import applied.
    pub store: ProfileStore,
    pub secret_ops: Vec<SecretOp>,
    pub key_writes: Vec<NewKeyFile>,
    pub scheme_writes: Vec<Scheme>,
}

/// The import preview of `archive` against the local profiles and schemes. An archive beyond
/// the limits (more than 1000 profiles, 200 schemes or 200 key files, a profile with more
/// than 16 jumps or the same jump twice) is refused as a whole.
pub(crate) fn plan(
    local: &ProfileStore,
    archive: &Archive,
    schemes: &SchemeSet,
) -> Result<ImportPlan, String> {
    if !within_limits(archive) {
        return Err(TOO_LARGE.into());
    }
    let kind = archive.kind;
    let full = kind == ArchiveKind::Full;
    let mut warnings = Vec::new();
    let (scheme_entries, dropped) = plan_schemes(archive, schemes, &mut warnings);

    let mut key_users = Vec::new();
    if full {
        for key in &archive.keys {
            if key.bytes.len() as u64 > MAX_KEY_BYTES {
                warnings.push(format!(
                    "The key file {} is larger than 1 MB, so it is not a key and was left out.",
                    shown(&plain_key_name(&key.file_name))
                ));
            } else {
                key_users.push(key.profiles.clone());
            }
        }
    }

    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for archived in &archive.profiles {
        let Some(mut incoming) = claimed(archived, kind) else {
            continue;
        };
        incoming.color_scheme = incoming
            .color_scheme
            .take()
            .and_then(|id| scheme_for(&id, &scheme_entries, &dropped, schemes));
        let flaw = if !seen.insert(incoming.id) {
            Some(TWICE.to_string())
        } else if incoming.name.trim().is_empty() {
            Some(NO_NAME.to_string())
        } else {
            None
        };
        let matched = local.get(incoming.id).map(|p| p.id).or_else(|| {
            local
                .profiles
                .iter()
                .find(|p| p.name == incoming.name)
                .map(|p| p.id)
        });
        entries.push(ImportEntry {
            incoming,
            choice: if matched.is_some() {
                Choice::Skip
            } else {
                Choice::KeepBoth
            },
            matched,
            error: None,
            copy_id: ProfileId::new(),
            flaw,
        });
    }

    let mut plan = ImportPlan {
        entries,
        schemes: scheme_entries,
        has_secrets: full && !archive.secrets.is_empty(),
        has_keys: false,
        warnings,
        kind,
        local: local.clone(),
        key_users,
    };
    revalidate(&mut plan);
    Ok(plan)
}

/// Whether the archive stays within what an import handles (see [`plan`]).
fn within_limits(archive: &Archive) -> bool {
    archive.profiles.len() <= MAX_PROFILES
        && archive.schemes.len() <= MAX_SCHEMES
        && archive.keys.len() <= MAX_KEYS
        && archive.secrets.len() <= MAX_SECRETS
        && archive.keys.iter().all(|k| {
            let distinct: HashSet<&ProfileId> = k.profiles.iter().collect();
            k.profiles.len() <= MAX_PROFILES && distinct.len() == k.profiles.len()
        })
        && archive.profiles.iter().all(|p| match &p.kind {
            ProfileKind::Ssh(ssh) => {
                let distinct: HashSet<&ProfileId> = ssh.jump.iter().collect();
                ssh.jump.len() <= MAX_JUMPS && distinct.len() == ssh.jump.len()
            }
            ProfileKind::Local(_) => true,
        })
}

/// Change one entry's choice and check every entry again (references follow the choice).
pub(crate) fn set_choice(plan: &mut ImportPlan, i: usize, c: Choice) {
    if let Some(entry) = plan.entries.get_mut(i) {
        entry.choice = c;
        revalidate(plan);
    }
}

/// Apply the plan's choices to `local` (which must be the profiles the plan was made against)
/// and work out the key files, schemes and secrets to write. Nothing is written: existing
/// files in `key_dir` are only read, to reuse an identical key file. With `read_only` (the
/// app's profiles could not be loaded) nothing is imported.
pub(crate) fn apply(
    plan: &ImportPlan,
    archive: &Archive,
    local: &ProfileStore,
    key_dir: &Path,
    read_only: bool,
) -> Result<Applied, String> {
    if read_only {
        return Err(READ_ONLY.into());
    }
    if plan.local != *local {
        return Err(STALE.into());
    }
    let resolved = resolve(&plan.local, &plan.entries, plan.kind);
    let mut store = resolved.store;
    let index = first_entries(&plan.entries);
    let full = plan.kind == ArchiveKind::Full;

    let key_writes = if full {
        place_keys(&archive.keys, &index, &resolved.finals, &mut store, key_dir)?
    } else {
        Vec::new()
    };

    let mut secret_ops: Vec<SecretOp> = Vec::new();
    if full {
        let at = positions(&store);
        let mut stored: HashSet<(ProfileId, SecretKind)> = HashSet::new();
        for s in &archive.secrets {
            let Some(id) = index.get(&s.profile).and_then(|&i| resolved.finals[i]) else {
                continue;
            };
            let used = at.get(&id).and_then(|&k| secret_kind(&store.profiles[k]));
            if used == Some(s.kind) && stored.insert((id, s.kind)) {
                secret_ops.push(SecretOp {
                    profile: id,
                    kind: s.kind,
                    value: s.value.clone(),
                });
            }
        }
    }

    let imported: HashSet<ProfileId> = resolved.finals.iter().flatten().copied().collect();
    let used_schemes: HashSet<&str> = store
        .profiles
        .iter()
        .filter(|p| imported.contains(&p.id))
        .filter_map(|p| p.color_scheme.as_deref())
        .collect();
    let scheme_writes = plan
        .schemes
        .iter()
        .filter(|s| s.outcome != SchemeOutcome::Reused)
        .filter(|s| used_schemes.contains(s.scheme.id.as_str()))
        .map(|s| s.scheme.clone())
        .collect();

    Ok(Applied {
        store,
        secret_ops,
        key_writes,
        scheme_writes,
    })
}

/// What an archived profile may claim. A shared archive carries SSH profiles without key
/// paths only, whatever the file says: local profiles are dropped, key paths cleared.
fn claimed(profile: &Profile, kind: ArchiveKind) -> Option<Profile> {
    let mut p = profile.clone();
    if kind == ArchiveKind::Shared {
        let ProfileKind::Ssh(ssh) = &mut p.kind else {
            return None;
        };
        if let AuthMethod::Key { .. } = ssh.auth {
            ssh.auth = AuthMethod::Key {
                path: None,
                passphrase: false,
            };
        }
    }
    Some(p)
}

/// The secret a profile's method uses, if any.
fn secret_kind(p: &Profile) -> Option<SecretKind> {
    let ProfileKind::Ssh(ssh) = &p.kind else {
        return None;
    };
    match ssh.auth {
        AuthMethod::Password => Some(SecretKind::Password),
        AuthMethod::Key {
            passphrase: true, ..
        } => Some(SecretKind::KeyPassphrase),
        AuthMethod::Key {
            passphrase: false, ..
        }
        | AuthMethod::Agent => None,
    }
}

/// The first entry of each archived id.
fn first_entries(entries: &[ImportEntry]) -> HashMap<ProfileId, usize> {
    let mut index = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        index.entry(e.incoming.id).or_insert(i);
    }
    index
}

fn revalidate(plan: &mut ImportPlan) {
    let resolved = resolve(&plan.local, &plan.entries, plan.kind);
    let index = first_entries(&plan.entries);
    let at = positions(&resolved.store);
    plan.has_keys = plan.key_users.iter().flatten().any(|id| {
        index
            .get(id)
            .and_then(|&i| resolved.finals[i])
            .and_then(|id| at.get(&id))
            .is_some_and(|&k| uses_key(&resolved.store.profiles[k]))
    });
    for (entry, error) in plan.entries.iter_mut().zip(resolved.errors) {
        entry.error = error;
    }
}

/// Where each profile is in the store, for lookups by id in linear time overall.
fn positions(store: &ProfileStore) -> HashMap<ProfileId, usize> {
    store
        .profiles
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id, i))
        .collect()
}

/// Whether a profile signs in with a key.
fn uses_key(p: &Profile) -> bool {
    matches!(&p.kind, ProfileKind::Ssh(ssh) if matches!(ssh.auth, AuthMethod::Key { .. }))
}

/// The local profiles with the entries applied, plus what happened to each entry.
struct Resolved {
    store: ProfileStore,
    /// The id each imported entry has in `store`; `None` for skipped or blocked entries.
    finals: Vec<Option<ProfileId>>,
    /// Why each blocked entry is left out.
    errors: Vec<Option<String>>,
}

/// Apply the entries' choices, then validate the result; entries that make it invalid are
/// blocked and the rest applied again, until nothing new is blocked (each round blocks at
/// least one more entry, so this ends). Every round is linear in profiles plus jumps, and an
/// entry jumping through a blocked new profile is blocked in the same round, so a long chain
/// of archived profiles does not take a round per link.
fn resolve(local: &ProfileStore, entries: &[ImportEntry], kind: ArchiveKind) -> Resolved {
    let index = first_entries(entries);
    let baseline = local.validate();
    // Entries jumping through each entry, by the archived jump.
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); entries.len()];
    for (i, e) in entries.iter().enumerate() {
        if let ProfileKind::Ssh(ssh) = &e.incoming.kind {
            for j in &ssh.jump {
                if let Some(&k) = index.get(j) {
                    dependents[k].push(i);
                }
            }
        }
    }
    let mut blocked: Vec<Option<String>> = entries.iter().map(|e| e.flaw.clone()).collect();
    loop {
        // Two entries replacing the same local profile: the first one wins.
        let mut targets = HashSet::new();
        for (i, e) in entries.iter().enumerate() {
            if blocked[i].is_some() || e.choice != Choice::Overwrite {
                continue;
            }
            if let Some(m) = e.matched
                && !targets.insert(m)
            {
                let name = local.get(m).map(|p| p.name.as_str()).unwrap_or_default();
                blocked[i] = Some(format!(
                    "Another profile in the archive already replaces {}.",
                    shown(name)
                ));
            }
        }

        let (store, finals) = build(local, entries, &blocked, &index, kind);
        let round = Round::new(entries, &index, &store, &finals);
        let mut newly = Vec::new();
        for error in store.validate() {
            for (i, why) in round.culprits(&error, &baseline) {
                if blocked[i].is_none() {
                    blocked[i] = Some(why);
                    newly.push(i);
                }
            }
        }
        if newly.is_empty() {
            for (i, e) in entries.iter().enumerate() {
                // A skipped entry is not imported, so nothing is wrong with it.
                if e.choice == Choice::Skip && e.flaw.is_none() {
                    blocked[i] = None;
                }
            }
            return Resolved {
                store,
                finals,
                errors: blocked,
            };
        }
        // A blocked profile without a local match is not there to jump through.
        while let Some(k) = newly.pop() {
            if entries[k].matched.is_some() {
                continue;
            }
            for &i in &dependents[k] {
                if blocked[i].is_none() && entries[i].choice != Choice::Skip {
                    blocked[i] = Some(not_imported(&entries[k].incoming.name));
                    newly.push(i);
                }
            }
        }
    }
}

/// Whether an entry adds or replaces a profile.
fn importing(e: &ImportEntry, blocked: &Option<String>) -> bool {
    blocked.is_none() && e.choice != Choice::Skip
}

/// The id an imported entry has in the result.
fn final_id(e: &ImportEntry) -> ProfileId {
    match (e.matched, e.choice) {
        (Some(m), Choice::Overwrite) => m,
        (Some(_), _) => e.copy_id,
        (None, _) => e.incoming.id,
    }
}

/// The local profiles with every importing entry applied, in order: replacements in place,
/// additions at the end. Jumps to archived profiles follow those profiles: to the copy or
/// replacement when imported, to the matched local profile when not.
fn build(
    local: &ProfileStore,
    entries: &[ImportEntry],
    blocked: &[Option<String>],
    index: &HashMap<ProfileId, usize>,
    kind: ArchiveKind,
) -> (ProfileStore, Vec<Option<ProfileId>>) {
    let finals: Vec<Option<ProfileId>> = entries
        .iter()
        .zip(blocked)
        .map(|(e, b)| importing(e, b).then(|| final_id(e)))
        .collect();
    let target = |j: ProfileId| match index.get(&j) {
        Some(&k) => finals[k].or(entries[k].matched).unwrap_or(j),
        None => j,
    };

    let mut store = local.clone();
    let position: HashMap<ProfileId, usize> = store
        .profiles
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id, i))
        .collect();
    let mut names: HashSet<String> = store.profiles.iter().map(|p| p.name.clone()).collect();
    for (e, final_id) in entries.iter().zip(&finals) {
        let Some(final_id) = *final_id else {
            continue;
        };
        let mut p = e.incoming.clone();
        if let ProfileKind::Ssh(ssh) = &mut p.kind {
            for j in &mut ssh.jump {
                *j = target(*j);
            }
        }
        match (e.matched, e.choice) {
            (Some(m), Choice::Overwrite) => {
                if let Some(&at) = position.get(&m) {
                    let slot = &mut store.profiles[at];
                    names.insert(p.name.clone());
                    *slot = replaced(slot, p, kind);
                }
            }
            (Some(_), _) => {
                p.id = final_id;
                p.name = copy_name(&names, &p.name);
                names.insert(p.name.clone());
                store.profiles.push(p);
            }
            (None, _) => {
                names.insert(p.name.clone());
                store.profiles.push(p);
            }
        }
    }
    (store, finals)
}

/// `incoming` replacing `old`, keeping `old`'s id. Overwrite replaces only what the archive
/// carries:
/// - A shared archive carries no key path or passphrase flag, so a key profile keeps the local
///   ones (and its saved secrets, which a shared import never touches).
/// - A full archive's key path points into another machine, so a key profile with a local
///   path keeps it and takes the passphrase flag (which travels with the archived secret);
///   a key file the archive carries replaces the path later (`place_keys`). Archive paths are
///   never looked at on disk.
fn replaced(old: &Profile, mut incoming: Profile, kind: ArchiveKind) -> Profile {
    incoming.id = old.id;
    if let ProfileKind::Ssh(new) = &mut incoming.kind
        && let ProfileKind::Ssh(old) = &old.kind
        && let AuthMethod::Key {
            path: new_path,
            passphrase: new_passphrase,
        } = &mut new.auth
        && let AuthMethod::Key {
            path: old_path,
            passphrase: old_passphrase,
        } = &old.auth
    {
        match kind {
            ArchiveKind::Shared => {
                *new_path = old_path.clone();
                *new_passphrase = *old_passphrase;
            }
            ArchiveKind::Full => {
                if old_path.is_some() {
                    *new_path = old_path.clone();
                }
            }
        }
    }
    incoming
}

/// `{name} (imported)`, or `{name} (imported 2)`, ... when that is taken too.
fn copy_name(names: &HashSet<String>, name: &str) -> String {
    let first = format!("{name} (imported)");
    if !names.contains(&first) {
        return first;
    }
    (2..)
        .map(|k| format!("{name} (imported {k})"))
        .find(|n| !names.contains(n))
        .expect("a free name")
}

/// Lookups for assigning one round's validation errors to entries.
struct Round<'a> {
    /// Importing entries by the name they have in the result, in entry order.
    by_name: HashMap<&'a str, Vec<usize>>,
    /// The jumps of profiles the import neither adds nor replaces, by name (the first one).
    untouched: HashMap<&'a str, &'a [ProfileId]>,
    /// Importing entries that replace a local profile, by its id.
    replacements: Vec<(ProfileId, usize)>,
    /// The name of each archived profile, by its archived id.
    archived: HashMap<ProfileId, &'a str>,
}

impl<'a> Round<'a> {
    fn new(
        entries: &'a [ImportEntry],
        index: &HashMap<ProfileId, usize>,
        store: &'a ProfileStore,
        finals: &[Option<ProfileId>],
    ) -> Self {
        let entry_of: HashMap<ProfileId, usize> = finals
            .iter()
            .enumerate()
            .filter_map(|(i, id)| id.map(|id| (id, i)))
            .collect();
        let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut untouched: HashMap<&str, &[ProfileId]> = HashMap::new();
        for p in &store.profiles {
            match entry_of.get(&p.id) {
                Some(&i) => by_name.entry(p.name.as_str()).or_default().push(i),
                None => {
                    let jumps: &[ProfileId] = match &p.kind {
                        ProfileKind::Ssh(ssh) => &ssh.jump,
                        ProfileKind::Local(_) => &[],
                    };
                    untouched.entry(p.name.as_str()).or_insert(jumps);
                }
            }
        }
        for list in by_name.values_mut() {
            list.sort_unstable();
        }
        let replacements = (0..entries.len())
            .filter_map(|i| {
                finals[i]
                    .filter(|id| Some(*id) == entries[i].matched)
                    .map(|id| (id, i))
            })
            .collect();
        let archived = index
            .iter()
            .map(|(id, &i)| (*id, entries[i].incoming.name.as_str()))
            .collect();
        Self {
            by_name,
            untouched,
            replacements,
            archived,
        }
    }

    /// The entries to block for a validation error of the result, with the reason shown on
    /// each.
    fn culprits(
        &self,
        error: &ValidationError,
        baseline: &[ValidationError],
    ) -> Vec<(usize, String)> {
        let name = error_profile(error);
        let named = self
            .by_name
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let why = match error {
            // A jump left pointing at an archived profile that is not imported (skipped
            // without a local match).
            ValidationError::MissingJump { missing, .. } => match self.archived.get(missing) {
                Some(hop) => not_imported(hop),
                None => message(error),
            },
            _ => message(error),
        };

        if let ValidationError::DuplicateName(_) = error {
            // Against a local profile every importing one is wrong; among imported ones the
            // first keeps the name.
            let skip = usize::from(!self.untouched.contains_key(name));
            return named.iter().skip(skip).map(|&i| (i, why.clone())).collect();
        }
        if !named.is_empty() {
            return named.iter().map(|&i| (i, why.clone())).collect();
        }
        if baseline.contains(error) {
            return Vec::new();
        }

        // A local profile the import made invalid (only a replacement can): block the
        // replacements it jumps through, or every replacement when none of them is a direct
        // hop.
        let hops = self.untouched.get(name).copied().unwrap_or_default();
        let direct: Vec<usize> = self
            .replacements
            .iter()
            .filter(|(id, _)| hops.contains(id))
            .map(|&(_, i)| i)
            .collect();
        let chosen = if direct.is_empty() {
            self.replacements.iter().map(|&(_, i)| i).collect()
        } else {
            direct
        };
        let why = format!("It would make the jump chain of {} invalid.", shown(name));
        chosen.into_iter().map(|i| (i, why.clone())).collect()
    }
}

/// The profile a validation error is about.
fn error_profile(error: &ValidationError) -> &str {
    match error {
        ValidationError::DuplicateName(name) | ValidationError::EmptyHost(name) => name,
        ValidationError::MissingJump { profile, .. }
        | ValidationError::JumpNotSsh { profile }
        | ValidationError::JumpCycle { profile }
        | ValidationError::JumpChainTooLong { profile }
        | ValidationError::EmptyTunnelName { profile }
        | ValidationError::DuplicateTunnel { profile, .. }
        | ValidationError::TunnelPort { profile, .. }
        | ValidationError::TunnelTargetPort { profile, .. }
        | ValidationError::EmptyTunnelBind { profile, .. }
        | ValidationError::EmptyTunnelHost { profile, .. } => profile,
    }
}

/// Why an entry jumping through the archived profile `hop` is blocked when `hop` is skipped
/// without a local match, or blocked itself.
fn not_imported(hop: &str) -> String {
    format!("It jumps through {}, which is not imported.", shown(hop))
}

/// A validation error as a sentence about the entry it blocks.
fn message(error: &ValidationError) -> String {
    match error {
        ValidationError::DuplicateName(name) => {
            format!("A profile named {} already exists.", shown(name))
        }
        ValidationError::MissingJump { .. } => MISSING.into(),
        ValidationError::JumpNotSsh { .. } => {
            "It jumps through a profile that is not an SSH profile.".into()
        }
        ValidationError::JumpCycle { .. } => "Its jump chain loops back to itself.".into(),
        ValidationError::JumpChainTooLong { .. } => format!(
            "It goes through more than {} jump hosts.",
            fbterm_config::MAX_JUMP_CHAIN
        ),
        ValidationError::EmptyHost(_) => "Its host is empty.".into(),
        ValidationError::EmptyTunnelName { .. } => "One of its tunnels has no name.".into(),
        ValidationError::DuplicateTunnel { name, .. } => {
            format!("Two of its tunnels are named {}.", shown(name))
        }
        ValidationError::TunnelPort { name, .. } => {
            format!("Tunnel {} needs a port above 0.", shown(name))
        }
        ValidationError::TunnelTargetPort { name, .. } => {
            format!("Tunnel {} needs a target port.", shown(name))
        }
        ValidationError::EmptyTunnelBind { name, .. } => {
            format!("Tunnel {} has an empty bind address.", shown(name))
        }
        ValidationError::EmptyTunnelHost { name, .. } => {
            format!("Tunnel {} has an empty host.", shown(name))
        }
    }
}

/// An untrusted name as shown in a message: no control characters, at most 60 characters.
fn shown(name: &str) -> String {
    let clean: String = name.chars().filter(|c| !c.is_control()).collect();
    if clean.chars().count() <= 60 {
        clean
    } else {
        let cut: String = clean.chars().take(57).collect();
        format!("{cut}...")
    }
}

/// The archive's user schemes, checked like scheme files, with what happens to each; and the
/// ids of the invalid ones left out (their profiles follow the global scheme).
fn plan_schemes(
    archive: &Archive,
    local: &SchemeSet,
    warnings: &mut Vec<String>,
) -> (Vec<SchemeEntry>, HashSet<String>) {
    let mut entries: Vec<SchemeEntry> = Vec::new();
    let mut dropped = HashSet::new();
    // The name of the first copy of each id.
    let mut seen: HashMap<&str, &str> = HashMap::new();
    for s in &archive.schemes {
        // A built-in id is the built-in, whatever the archive's copy says.
        if BUILTIN_IDS.contains(&s.id.as_str()) {
            continue;
        }
        let name = if s.name.trim().is_empty() {
            &s.id
        } else {
            &s.name
        };
        if let Some(first) = seen.get(s.id.as_str()) {
            warnings.push(format!(
                "The archive lists the color scheme {} twice; only the first is used.",
                shown(first)
            ));
            continue;
        }
        seen.insert(s.id.as_str(), name.as_str());
        let entry = match local.get(&s.id) {
            Some(mine) if same_scheme(mine, s) => Some(SchemeEntry {
                archived_id: s.id.clone(),
                scheme: mine.clone(),
                outcome: SchemeOutcome::Reused,
            }),
            Some(_) => valid_scheme(Scheme {
                id: schemes::new_user_id(),
                name: format!("{} (imported)", s.name),
                builtin: false,
                ..s.clone()
            })
            .map(|scheme| SchemeEntry {
                archived_id: s.id.clone(),
                scheme,
                outcome: SchemeOutcome::Renamed,
            }),
            None => valid_scheme(s.clone()).map(|scheme| SchemeEntry {
                archived_id: s.id.clone(),
                scheme,
                outcome: SchemeOutcome::Imported,
            }),
        };
        match entry {
            Some(entry) => entries.push(entry),
            None => {
                dropped.insert(s.id.clone());
                warnings.push(format!(
                    "{} is not a valid color scheme, so it was left out.",
                    shown(name)
                ));
            }
        }
    }
    (entries, dropped)
}

/// The scheme when it could be a scheme file (`schemes::is_valid_user`: a plain, non-built-in
/// id, within the scheme file size cap, passing the scheme file checks).
fn valid_scheme(scheme: Scheme) -> Option<Scheme> {
    schemes::is_valid_user(&scheme).then_some(scheme)
}

/// Same id and content (a loaded user scheme is never built-in).
fn same_scheme(mine: &Scheme, theirs: &Scheme) -> bool {
    !mine.builtin
        && mine
            == &Scheme {
                builtin: false,
                ..theirs.clone()
            }
}

/// The scheme an archived profile's scheme id resolves to: a built-in as it is, an archived
/// scheme to the scheme it becomes, an invalid archived one or an unknown id to none (the
/// global scheme), a local scheme the archive did not carry as it is.
fn scheme_for(
    id: &str,
    entries: &[SchemeEntry],
    dropped: &HashSet<String>,
    local: &SchemeSet,
) -> Option<String> {
    if BUILTIN_IDS.contains(&id) {
        return Some(id.to_string());
    }
    if let Some(e) = entries.iter().find(|e| e.archived_id == id) {
        return Some(e.scheme.id.clone());
    }
    if dropped.contains(id) {
        return None;
    }
    local.get(id).filter(|s| !s.builtin).map(|s| s.id.clone())
}

/// The archived key file name reduced to one plain, harmless file name: the last part of any
/// path, or [`FALLBACK_KEY_NAME`] when that is empty, a dot name, hidden, too long, has
/// control or reserved characters, is a reserved device name or a file OpenSSH reads.
fn plain_key_name(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    if harmless(last) {
        last.to_string()
    } else {
        FALLBACK_KEY_NAME.to_string()
    }
}

fn harmless(name: &str) -> bool {
    let reserved_char = |c: char| {
        c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '/' | '\\')
    };
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && !name.ends_with('.')
        && !name.ends_with(' ')
        && !name.chars().any(reserved_char)
        && !SSH_FILES.iter().any(|f| f.eq_ignore_ascii_case(name))
        // Never a private key where a public key is looked for.
        && !name.to_ascii_lowercase().ends_with(".pub")
        && !windows_device(name)
}

/// `con`, `prn`, `aux`, `nul`, `com1`-`com9`, `lpt1`-`lpt9`, with or without an extension.
fn windows_device(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_lowercase();
    match stem.as_str() {
        "con" | "prn" | "aux" | "nul" => true,
        s => {
            (s.starts_with("com") || s.starts_with("lpt"))
                && s.len() == 4
                && s.as_bytes()[3].is_ascii_digit()
        }
    }
}

/// Where each key file goes and which files must be created. Only keys used by imported key
/// profiles are placed; each takes its plain name, or `-2`, `-3`, ... when a different file
/// (or anything that is not a regular file) has it. An identical file is reused.
fn place_keys(
    keys: &[ArchivedKey],
    index: &HashMap<ProfileId, usize>,
    finals: &[Option<ProfileId>],
    store: &mut ProfileStore,
    key_dir: &Path,
) -> Result<Vec<NewKeyFile>, String> {
    let mut writes: Vec<NewKeyFile> = Vec::new();
    let mut placed: HashSet<ProfileId> = HashSet::new();
    // Placing keys only changes key paths, so positions stay valid.
    let at = positions(store);
    for key in keys {
        if key.bytes.len() as u64 > MAX_KEY_BYTES {
            continue;
        }
        let users: Vec<ProfileId> = key
            .profiles
            .iter()
            .filter_map(|id| index.get(id).and_then(|&i| finals[i]))
            .filter(|id| !placed.contains(id))
            .filter(|id| at.get(id).is_some_and(|&k| uses_key(&store.profiles[k])))
            .collect();
        if users.is_empty() {
            continue;
        }

        let base = plain_key_name(&key.file_name);
        let mut chosen = None;
        for n in 1..=MAX_KEY_NAME_TRIES {
            let name = if n == 1 {
                base.clone()
            } else {
                format!("{base}-{n}")
            };
            let path = key_dir.join(&name);
            // Names compared ignoring case, as on case-insensitive file systems.
            if let Some(w) = writes.iter().find(|w| {
                w.path
                    .file_name()
                    .is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(&name))
            }) {
                if w.bytes.as_slice() == key.bytes.as_slice() {
                    chosen = Some(w.path.clone());
                    break;
                }
                continue;
            }
            match existing(&path, &key.bytes) {
                Existing::Free => {
                    writes.push(NewKeyFile {
                        path: path.clone(),
                        bytes: key.bytes.clone(),
                    });
                    chosen = Some(path);
                    break;
                }
                Existing::Same => {
                    chosen = Some(path);
                    break;
                }
                Existing::Taken => {}
            }
        }
        let Some(path) = chosen else {
            return Err(format!(
                "No free file name was found for the key file {} in {}.",
                shown(&base),
                key_dir.display()
            ));
        };

        for id in users {
            placed.insert(id);
            if let Some(&k) = at.get(&id)
                && let ProfileKind::Ssh(ssh) = &mut store.profiles[k].kind
                && let AuthMethod::Key { path: slot, .. } = &mut ssh.auth
            {
                *slot = Some(path.clone());
            }
        }
    }
    Ok(writes)
}

enum Existing {
    /// Nothing has the name.
    Free,
    /// A regular file with exactly these bytes.
    Same,
    /// Anything else: another file, a folder, a link, something unreadable.
    Taken,
}

/// What has `path` now, compared with `bytes`. Reads only regular files (never through a
/// link, never a pipe or device), at most [`MAX_KEY_BYTES`] plus one byte.
fn existing(path: &Path, bytes: &[u8]) -> Existing {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Existing::Free,
        Err(_) => return Existing::Taken,
    };
    if !meta.file_type().is_file() || meta.len() != bytes.len() as u64 {
        return Existing::Taken;
    }
    let Ok(file) = File::open(path) else {
        return Existing::Taken;
    };
    // The opened file again, in case the path changed between the check and the open.
    match file.metadata() {
        Ok(meta) if meta.is_file() && meta.len() == bytes.len() as u64 => {}
        _ => return Existing::Taken,
    }
    let mut found = Zeroizing::new(Vec::with_capacity(bytes.len() + 1));
    if file
        .take(MAX_KEY_BYTES + 1)
        .read_to_end(&mut found)
        .is_err()
    {
        return Existing::Taken;
    }
    if found.as_slice() == bytes {
        Existing::Same
    } else {
        Existing::Taken
    }
}
