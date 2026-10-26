//! Profile editor state: the form behind the editor window, as plain data so its rules are
//! testable without a GUI. The window itself is drawn by [`view`].
//!
//! The editor works on drafts of every profile and touches the real store only in
//! [`EditorState::save`]: it validates, writes `profiles.toml` atomically and only then changes
//! the [`SecretStore`] (typed passwords and passphrases are stored there, never in a profile;
//! secrets of deleted profiles and of auth methods no longer used are removed).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fbterm_config::{
    AccentStyle, AuthMethod, ConfigError, LocalProfile, Profile, ProfileId, ProfileKind,
    ProfileStore, Rgb, SshProfile, ValidationError,
};
use fbterm_secrets::{SecretKind, SecretStore};

use crate::redact::Redacted;

mod ssh_view;
pub mod view;

/// The accent presets: the design system's identity palette (dark values), plus "none".
pub const ACCENTS: [Rgb; 8] = [
    rgb(0xb19ff3), // violet
    rgb(0x70b3f7), // blue
    rgb(0x30c2d8), // cyan
    rgb(0x6ac48c), // green
    rgb(0x9dbb63), // lime
    rgb(0xdea052), // amber
    rgb(0xf08f87), // red
    rgb(0xe78db9), // pink
];

const fn rgb(v: u32) -> Rgb {
    Rgb {
        r: (v >> 16) as u8,
        g: (v >> 8) as u8,
        b: v as u8,
    }
}

/// How an SSH draft authenticates, without the details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthChoice {
    Password,
    Key,
    Agent,
}

/// What the editor window asks for. The first group is handled by the app (windows, files,
/// disk); the rest edit the selected draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorMsg {
    Open,
    Close,
    Save,
    /// Re-read `profiles.toml` (offered while it failed to load).
    Reload,
    /// Open the file picker for the key path.
    PickKey,
    KeyPicked(Option<PathBuf>),

    Select(ProfileId),
    NewSsh,
    NewLocal,
    Delete,
    Name(String),
    Group(String),
    Accent(Option<Rgb>),
    AccentStyle(AccentStyle),
    Host(String),
    Port(String),
    User(String),
    Auth(AuthChoice),
    /// The typed password or key passphrase (stored in the keychain on save).
    Secret(Redacted),
    KeyPath(String),
    KeyPassphrase(bool),
    JumpAdd(ProfileId),
    JumpRemove(usize),
    JumpUp(usize),
    JumpDown(usize),
    Program(String),
    Args(String),
    Cwd(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDraft {
    pub program: String,
    /// Arguments as typed, split on whitespace once edited.
    pub args_text: String,
    args: Vec<String>,
    pub cwd: String,
    env: BTreeMap<String, String>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct SshDraft {
    pub host: String,
    pub port: String,
    pub user: String,
    pub auth: AuthChoice,
    pub key_path: String,
    pub key_passphrase: bool,
    pub jump: Vec<ProfileId>,
    /// Typed secret for the chosen auth; empty keeps whatever the keychain has.
    pub secret: String,
}

impl std::fmt::Debug for SshDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshDraft")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("auth", &self.auth)
            .field("jump", &self.jump)
            .field("secret", &"<redacted>")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftKind {
    Local(LocalDraft),
    Ssh(SshDraft),
}

/// One profile as the form holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub id: ProfileId,
    pub name: String,
    pub group: String,
    pub accent: Option<Rgb>,
    pub accent_style: AccentStyle,
    /// Carried through unchanged until the form edits it.
    pub color_scheme: Option<String>,
    pub kind: DraftKind,
}

impl Draft {
    fn from_profile(p: &Profile) -> Self {
        let kind = match &p.kind {
            ProfileKind::Local(l) => DraftKind::Local(LocalDraft {
                program: l.program.display().to_string(),
                args_text: l.args.join(" "),
                args: l.args.clone(),
                cwd: l
                    .cwd
                    .as_ref()
                    .map(|c| c.display().to_string())
                    .unwrap_or_default(),
                env: l.env.clone(),
            }),
            ProfileKind::Ssh(s) => {
                let (auth, key_path, key_passphrase) = match &s.auth {
                    AuthMethod::Password => (AuthChoice::Password, String::new(), false),
                    AuthMethod::Key { path, passphrase } => {
                        (AuthChoice::Key, path.display().to_string(), *passphrase)
                    }
                    AuthMethod::Agent => (AuthChoice::Agent, String::new(), false),
                };
                DraftKind::Ssh(SshDraft {
                    host: s.host.clone(),
                    port: s.port.to_string(),
                    user: s.user.clone(),
                    auth,
                    key_path,
                    key_passphrase,
                    jump: s.jump.clone(),
                    secret: String::new(),
                })
            }
        };
        Self {
            id: p.id,
            name: p.name.clone(),
            group: p.group.clone().unwrap_or_default(),
            accent: p.accent,
            accent_style: p.accent_style,
            color_scheme: p.color_scheme.clone(),
            kind,
        }
    }

    /// The profile this draft describes. Never carries a secret.
    pub fn to_profile(&self) -> Profile {
        let kind = match &self.kind {
            DraftKind::Local(l) => ProfileKind::Local(LocalProfile {
                program: PathBuf::from(l.program.trim()),
                args: l.args.clone(),
                cwd: Some(l.cwd.trim())
                    .filter(|c| !c.is_empty())
                    .map(PathBuf::from),
                env: l.env.clone(),
            }),
            DraftKind::Ssh(s) => ProfileKind::Ssh(SshProfile {
                host: s.host.trim().to_owned(),
                port: s.port.parse().unwrap_or(22),
                user: s.user.trim().to_owned(),
                auth: match s.auth {
                    AuthChoice::Password => AuthMethod::Password,
                    AuthChoice::Key => AuthMethod::Key {
                        path: PathBuf::from(s.key_path.trim()),
                        passphrase: s.key_passphrase,
                    },
                    AuthChoice::Agent => AuthMethod::Agent,
                },
                jump: s.jump.clone(),
            }),
        };
        Profile {
            id: self.id,
            name: self.name.trim().to_owned(),
            group: Some(self.group.trim())
                .filter(|g| !g.is_empty())
                .map(str::to_owned),
            accent: self.accent,
            // Without an accent there is nothing to show, so the style goes back to the
            // default (and is not written to the file).
            accent_style: if self.accent.is_some() {
                self.accent_style
            } else {
                AccentStyle::default()
            },
            color_scheme: self.color_scheme.clone(),
            kind,
        }
    }

    pub fn is_ssh(&self) -> bool {
        matches!(self.kind, DraftKind::Ssh(_))
    }

    /// The secret typed for this draft and the kind it is stored as, if any.
    fn typed_secret(&self) -> Option<(SecretKind, &str)> {
        let DraftKind::Ssh(s) = &self.kind else {
            return None;
        };
        if s.secret.is_empty() {
            return None;
        }
        match s.auth {
            AuthChoice::Password => Some((SecretKind::Password, &s.secret)),
            AuthChoice::Key if s.key_passphrase => Some((SecretKind::KeyPassphrase, &s.secret)),
            _ => None,
        }
    }
}

/// The editor window's state.
pub struct EditorState {
    drafts: Vec<Draft>,
    pub selected: Option<ProfileId>,
    /// Saved profiles deleted in the editor; their secrets go on the next save.
    removed: Vec<ProfileId>,
    /// Problems found by the last save attempt.
    pub errors: Vec<ValidationError>,
    /// A one-line message: a refused delete, a failed write, "Saved".
    pub notice: Option<String>,
    /// `profiles.toml` failed to load: the editor shows why and changes nothing.
    pub read_only: Option<ConfigError>,
    /// Unsaved edits exist.
    pub dirty: bool,
    secrets: Arc<dyn SecretStore>,
    /// Whether the key file at this path (as typed) is an RSA private key; refreshed when the
    /// path or the selection changes, so drawing never touches the disk.
    key_check: Option<(String, bool)>,
}

/// The advice shown (editor, terminal) when a login uses an RSA key.
pub const RSA_ADVICE: &str =
    "This login uses an RSA key. Consider switching to an ed25519 key (ssh-keygen -t ed25519).";

/// Key files are small; anything bigger is not read.
const MAX_KEY_FILE: u64 = 64 * 1024;

/// Whether the file at `path` (`~` expanded) is an RSA private key; unreadable means no.
fn key_file_is_rsa(path: &str) -> bool {
    use std::io::Read;
    let path = std::path::Path::new(path);
    let path = match path.strip_prefix("~") {
        Ok(rest) => match directories::BaseDirs::new() {
            Some(d) => d.home_dir().join(rest),
            None => return false,
        },
        Err(_) => path.to_path_buf(),
    };
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_KEY_FILE).read_to_string(&mut text))
        .is_ok()
        && fbterm_session::ssh::private_key_is_rsa(&text)
}

impl EditorState {
    pub fn new(
        store: &ProfileStore,
        secrets: Arc<dyn SecretStore>,
        read_only: Option<ConfigError>,
    ) -> Self {
        let drafts: Vec<Draft> = store.profiles.iter().map(Draft::from_profile).collect();
        let mut state = Self {
            selected: drafts.first().map(|d| d.id),
            drafts,
            removed: Vec::new(),
            errors: Vec::new(),
            notice: None,
            read_only,
            dirty: false,
            secrets,
            key_check: None,
        };
        state.refresh_key_check();
        state
    }

    /// Re-check the selected draft's key file when its path differs from the last check.
    fn refresh_key_check(&mut self) {
        let path = match self.selected_draft().map(|d| &d.kind) {
            Some(DraftKind::Ssh(s))
                if s.auth == AuthChoice::Key && !s.key_path.trim().is_empty() =>
            {
                s.key_path.trim().to_owned()
            }
            _ => {
                self.key_check = None;
                return;
            }
        };
        if self.key_check.as_ref().is_some_and(|(p, _)| *p == path) {
            return;
        }
        let rsa = key_file_is_rsa(&path);
        self.key_check = Some((path, rsa));
    }

    /// Whether the selected draft uses a key file that is an RSA private key.
    pub fn selected_key_is_rsa(&self) -> bool {
        let path = match self.selected_draft().map(|d| &d.kind) {
            Some(DraftKind::Ssh(s)) if s.auth == AuthChoice::Key => s.key_path.trim(),
            _ => return false,
        };
        self.key_check
            .as_ref()
            .is_some_and(|(p, rsa)| *rsa && p == path)
    }

    #[cfg(test)]
    pub fn drafts(&self) -> &[Draft] {
        &self.drafts
    }

    pub fn draft(&self, id: ProfileId) -> Option<&Draft> {
        self.drafts.iter().find(|d| d.id == id)
    }

    pub fn selected_draft(&self) -> Option<&Draft> {
        self.selected.and_then(|id| self.draft(id))
    }

    fn selected_mut(&mut self) -> Option<&mut Draft> {
        let id = self.selected?;
        self.drafts.iter_mut().find(|d| d.id == id)
    }

    /// Drafts by group: named groups alphabetically (case-insensitive), ungrouped ones last
    /// under an empty name; store order within a group.
    pub fn groups(&self) -> Vec<(String, Vec<&Draft>)> {
        let mut groups: BTreeMap<(bool, String), (String, Vec<&Draft>)> = BTreeMap::new();
        for d in &self.drafts {
            let name = d.group.trim();
            groups
                .entry((name.is_empty(), name.to_lowercase()))
                .or_insert_with(|| (name.to_owned(), Vec::new()))
                .1
                .push(d);
        }
        groups.into_values().collect()
    }

    /// SSH profiles `id` may add to its jump chain: every SSH draft except itself and the
    /// hops it already has.
    pub fn jump_options(&self, id: ProfileId) -> Vec<(ProfileId, String)> {
        let chain: &[ProfileId] = match self.draft(id).map(|d| &d.kind) {
            Some(DraftKind::Ssh(s)) => &s.jump,
            _ => &[],
        };
        self.drafts
            .iter()
            .filter(|d| d.is_ssh() && d.id != id && !chain.contains(&d.id))
            .map(|d| (d.id, d.name.clone()))
            .collect()
    }

    /// Names of the drafts that jump through `id`.
    pub fn dependants(&self, id: ProfileId) -> Vec<String> {
        self.drafts
            .iter()
            .filter(|d| matches!(&d.kind, DraftKind::Ssh(s) if s.jump.contains(&id)))
            .map(|d| d.name.clone())
            .collect()
    }

    /// Delete a draft. Refused, with the dependants' names, while other profiles jump through it.
    pub fn delete(&mut self, id: ProfileId) -> Result<(), Vec<String>> {
        let dependants = self.dependants(id);
        if !dependants.is_empty() {
            return Err(dependants);
        }
        let Some(pos) = self.drafts.iter().position(|d| d.id == id) else {
            return Ok(());
        };
        self.drafts.remove(pos);
        self.removed.push(id);
        self.dirty = true;
        if self.selected == Some(id) {
            self.selected = self
                .drafts
                .get(pos)
                .or_else(|| self.drafts.last())
                .map(|d| d.id);
        }
        self.refresh_key_check();
        Ok(())
    }

    fn unique_name(&self, base: &str) -> String {
        let taken = |n: &str| self.drafts.iter().any(|d| d.name == n);
        if !taken(base) {
            return base.to_owned();
        }
        (2..)
            .map(|i| format!("{base} {i}"))
            .find(|n| !taken(n))
            .expect("an unused name exists")
    }

    fn add(&mut self, name: &str, kind: DraftKind) -> ProfileId {
        let draft = Draft {
            id: ProfileId::new(),
            name: self.unique_name(name),
            group: String::new(),
            accent: None,
            accent_style: AccentStyle::default(),
            color_scheme: None,
            kind,
        };
        let id = draft.id;
        self.drafts.push(draft);
        self.selected = Some(id);
        self.dirty = true;
        self.refresh_key_check();
        id
    }

    /// A new SSH draft (empty host), selected.
    pub fn new_ssh(&mut self) -> ProfileId {
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default();
        self.add(
            "New SSH profile",
            DraftKind::Ssh(SshDraft {
                host: String::new(),
                port: "22".into(),
                user,
                auth: AuthChoice::Agent,
                key_path: String::new(),
                key_passphrase: false,
                jump: Vec::new(),
                secret: String::new(),
            }),
        )
    }

    /// A new local draft running the login shell, selected.
    pub fn new_local(&mut self) -> ProfileId {
        let program = std::env::var("SHELL").unwrap_or_else(|_| {
            if cfg!(windows) {
                "powershell.exe".into()
            } else {
                "/bin/sh".into()
            }
        });
        self.add(
            "New local profile",
            DraftKind::Local(LocalDraft {
                program,
                args_text: String::new(),
                args: Vec::new(),
                cwd: String::new(),
                env: BTreeMap::new(),
            }),
        )
    }

    /// Apply a form message. Edits are ignored while read-only; app-level messages are not
    /// handled here.
    pub fn update(&mut self, msg: EditorMsg) {
        self.apply(msg);
        // Every path that can change the selection or its key path ends here.
        self.refresh_key_check();
    }

    fn apply(&mut self, msg: EditorMsg) {
        if let EditorMsg::Select(id) = msg {
            if self.draft(id).is_some() {
                self.selected = Some(id);
                self.notice = None;
            }
            return;
        }
        if self.read_only.is_some() {
            return;
        }
        match msg {
            EditorMsg::NewSsh => {
                self.new_ssh();
                return;
            }
            EditorMsg::NewLocal => {
                self.new_local();
                return;
            }
            EditorMsg::Delete => {
                if let Some(id) = self.selected {
                    self.notice = match self.delete(id) {
                        Ok(()) => None,
                        Err(names) => Some(format!(
                            "Cannot delete: {} jump through this profile",
                            names.join(", ")
                        )),
                    };
                }
                return;
            }
            EditorMsg::KeyPicked(None) => return,
            _ => {}
        }
        let Some(d) = self.selected_mut() else {
            return;
        };
        let changed = edit(d, msg);
        if changed {
            self.dirty = true;
            self.notice = None;
        }
    }

    /// Validate the drafts against the saved `current` store. Returns the store to write and
    /// the secret changes to make once it is written; nothing is changed yet.
    pub fn prepare(&mut self, current: &ProfileStore) -> Result<Prepared, Vec<ValidationError>> {
        let store = ProfileStore {
            profiles: self.drafts.iter().map(Draft::to_profile).collect(),
        };
        let errors = store.validate();
        if !errors.is_empty() {
            self.errors = errors.clone();
            return Err(errors);
        }
        let mut sets = Vec::new();
        let mut removes = Vec::new();
        for d in &self.drafts {
            if let Some((kind, value)) = d.typed_secret() {
                sets.push(SecretOp::Set {
                    id: d.id,
                    kind,
                    value: Redacted(value.to_owned()),
                });
            }
            let (Some(old), DraftKind::Ssh(new)) = (current.get(d.id), &d.kind) else {
                continue;
            };
            let ProfileKind::Ssh(old) = &old.kind else {
                continue;
            };
            let had_password = old.auth == AuthMethod::Password;
            let had_passphrase = matches!(
                old.auth,
                AuthMethod::Key {
                    passphrase: true,
                    ..
                }
            );
            if had_password && new.auth != AuthChoice::Password {
                removes.push(SecretOp::RemoveKind {
                    id: d.id,
                    kind: SecretKind::Password,
                });
            }
            if had_passphrase && !(new.auth == AuthChoice::Key && new.key_passphrase) {
                removes.push(SecretOp::RemoveKind {
                    id: d.id,
                    kind: SecretKind::KeyPassphrase,
                });
            }
        }
        removes.extend(self.removed.iter().map(|&id| SecretOp::Remove(id)));
        sets.extend(removes);
        Ok(Prepared { store, ops: sets })
    }

    /// Save: validate, write `path` atomically, then replace `store` and apply the secret
    /// changes (stores first, then removals). A refused or failed write changes nothing: the
    /// drafts keep their typed secrets. A secret the store refuses stays typed in its draft and
    /// the editor stays dirty.
    pub fn save(&mut self, store: &mut ProfileStore, path: &Path) -> Result<(), SaveError> {
        let prepared = self.prepare(store).map_err(SaveError::Invalid)?;
        prepared.store.save(path).map_err(SaveError::Write)?;
        *store = prepared.store;
        self.errors.clear();
        self.removed.clear();
        let mut failures = Vec::new();
        for op in prepared.ops {
            let name = |id: ProfileId| {
                self.draft(id)
                    .map_or_else(|| id.to_string(), |d| d.name.clone())
            };
            let (result, what) = match &op {
                SecretOp::Set { id, kind, value } => (
                    self.secrets.set(id, *kind, &value.0),
                    format!("store the {} of {}", kind_name(*kind), name(*id)),
                ),
                SecretOp::RemoveKind { id, kind } => (
                    self.secrets.remove_kind(id, *kind),
                    format!("remove the old {} of {}", kind_name(*kind), name(*id)),
                ),
                SecretOp::Remove(id) => (
                    self.secrets.remove(id),
                    format!("remove the secrets of a deleted profile ({id})"),
                ),
            };
            match result {
                Ok(()) => {
                    if let SecretOp::Set { id, .. } = op
                        && let Some(DraftKind::Ssh(s)) = self
                            .drafts
                            .iter_mut()
                            .find(|d| d.id == id)
                            .map(|d| &mut d.kind)
                    {
                        s.secret.clear();
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "keychain change failed after saving profiles");
                    if let SecretOp::Remove(id) = op {
                        // Retried on the next save.
                        self.removed.push(id);
                    }
                    failures.push(format!("could not {what}: {e}"));
                }
            }
        }
        if failures.is_empty() {
            self.dirty = false;
            Ok(())
        } else {
            self.dirty = true;
            Err(SaveError::Secrets(failures))
        }
    }
}

fn kind_name(kind: SecretKind) -> &'static str {
    match kind {
        SecretKind::Password => "password",
        SecretKind::KeyPassphrase => "key passphrase",
    }
}

/// A secret-store change a save makes once `profiles.toml` is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretOp {
    Set {
        id: ProfileId,
        kind: SecretKind,
        value: Redacted,
    },
    /// The profile no longer uses this kind (auth changed, passphrase unchecked).
    RemoveKind { id: ProfileId, kind: SecretKind },
    /// The profile was deleted.
    Remove(ProfileId),
}

/// A validated store, ready to write, and the secret changes that follow the write.
#[derive(Debug)]
pub struct Prepared {
    pub store: ProfileStore,
    pub ops: Vec<SecretOp>,
}

/// Why a save did not fully happen.
#[derive(Debug)]
pub enum SaveError {
    /// Nothing was written; the problems are in `EditorState::errors`.
    Invalid(Vec<ValidationError>),
    /// `profiles.toml` could not be written; nothing changed.
    Write(ConfigError),
    /// The profiles were saved, but these secret changes failed.
    Secrets(Vec<String>),
}

/// Apply one field edit to a draft; returns whether it changed anything.
fn edit(d: &mut Draft, msg: EditorMsg) -> bool {
    match (msg, &mut d.kind) {
        (EditorMsg::Name(v), _) => d.name = v,
        (EditorMsg::Group(v), _) => d.group = v,
        (EditorMsg::Accent(v), _) => d.accent = v,
        (EditorMsg::AccentStyle(v), _) => d.accent_style = v,
        (EditorMsg::Host(v), DraftKind::Ssh(s)) => s.host = v,
        (EditorMsg::Port(v), DraftKind::Ssh(s)) => {
            // Digits only, a valid port or empty while typing.
            if !(v.is_empty() || v.parse::<u16>().is_ok_and(|p| p > 0)) {
                return false;
            }
            s.port = v;
        }
        (EditorMsg::User(v), DraftKind::Ssh(s)) => s.user = v,
        (EditorMsg::Auth(v), DraftKind::Ssh(s)) => {
            if s.auth != v {
                s.secret.clear();
            }
            s.auth = v;
        }
        (EditorMsg::Secret(v), DraftKind::Ssh(s)) => s.secret = v.0,
        (EditorMsg::KeyPath(v), DraftKind::Ssh(s)) => s.key_path = v,
        (EditorMsg::KeyPicked(Some(p)), DraftKind::Ssh(s)) => {
            s.key_path = p.display().to_string();
        }
        (EditorMsg::KeyPassphrase(v), DraftKind::Ssh(s)) => {
            s.key_passphrase = v;
            if !v {
                s.secret.clear();
            }
        }
        (EditorMsg::JumpAdd(id), DraftKind::Ssh(s)) => {
            if id == d.id || s.jump.contains(&id) {
                return false;
            }
            s.jump.push(id);
        }
        (EditorMsg::JumpRemove(i), DraftKind::Ssh(s)) if i < s.jump.len() => {
            s.jump.remove(i);
        }
        (EditorMsg::JumpUp(i), DraftKind::Ssh(s)) if i > 0 && i < s.jump.len() => {
            s.jump.swap(i - 1, i);
        }
        (EditorMsg::JumpDown(i), DraftKind::Ssh(s)) if i + 1 < s.jump.len() => {
            s.jump.swap(i, i + 1);
        }
        (EditorMsg::Program(v), DraftKind::Local(l)) => l.program = v,
        (EditorMsg::Args(v), DraftKind::Local(l)) => {
            l.args = v.split_whitespace().map(str::to_owned).collect();
            l.args_text = v;
        }
        (EditorMsg::Cwd(v), DraftKind::Local(l)) => l.cwd = v,
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests;
