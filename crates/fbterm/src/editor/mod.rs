//! Profile editor state: the form behind the editor window, as plain data so its rules are
//! testable without a GUI. The window itself is drawn by [`view`].
//!
//! The editor works on drafts of every profile and touches the real store only in
//! [`EditorState::apply`], which validates first and never hands back an invalid store. Typed
//! passwords and passphrases go to the [`SecretStore`] there, never into a profile.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use fbterm_config::{
    AuthMethod, ConfigError, LocalProfile, Profile, ProfileId, ProfileKind, ProfileStore, Rgb,
    SshProfile, ValidationError,
};
use fbterm_secrets::{SecretKind, SecretStore};

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
    Host(String),
    Port(String),
    User(String),
    Auth(AuthChoice),
    /// The typed password or key passphrase (stored in the keychain on save).
    Secret(String),
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
}

impl EditorState {
    pub fn new(
        store: &ProfileStore,
        secrets: Arc<dyn SecretStore>,
        read_only: Option<ConfigError>,
    ) -> Self {
        let drafts: Vec<Draft> = store.profiles.iter().map(Draft::from_profile).collect();
        Self {
            selected: drafts.first().map(|d| d.id),
            drafts,
            removed: Vec::new(),
            errors: Vec::new(),
            notice: None,
            read_only,
            dirty: false,
            secrets,
        }
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
            kind,
        };
        let id = draft.id;
        self.drafts.push(draft);
        self.selected = Some(id);
        self.dirty = true;
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

    /// Validate the drafts and, if valid, replace `store` with them, store typed secrets and
    /// forget the secrets of deleted profiles. Invalid drafts leave everything untouched.
    pub fn apply(&mut self, store: &mut ProfileStore) -> Result<(), Vec<ValidationError>> {
        let candidate = ProfileStore {
            profiles: self.drafts.iter().map(Draft::to_profile).collect(),
        };
        let errors = candidate.validate();
        if !errors.is_empty() {
            self.errors = errors.clone();
            return Err(errors);
        }
        for d in &self.drafts {
            if let Some((kind, value)) = d.typed_secret()
                && let Err(e) = self.secrets.set(&d.id, kind, value)
            {
                tracing::warn!(profile = %d.name, error = %e, "could not store secret");
            }
        }
        for id in self.removed.drain(..) {
            if let Err(e) = self.secrets.remove(&id) {
                tracing::warn!(profile = %id, error = %e, "could not remove secrets");
            }
        }
        for d in &mut self.drafts {
            if let DraftKind::Ssh(s) = &mut d.kind {
                s.secret.clear();
            }
        }
        *store = candidate;
        self.errors.clear();
        self.dirty = false;
        Ok(())
    }
}

/// Apply one field edit to a draft; returns whether it changed anything.
fn edit(d: &mut Draft, msg: EditorMsg) -> bool {
    match (msg, &mut d.kind) {
        (EditorMsg::Name(v), _) => d.name = v,
        (EditorMsg::Group(v), _) => d.group = v,
        (EditorMsg::Accent(v), _) => d.accent = v,
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
        (EditorMsg::Secret(v), DraftKind::Ssh(s)) => s.secret = v,
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
