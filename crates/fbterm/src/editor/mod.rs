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
    ProfileStore, Rgb, SshProfile, StartMode, TunnelDef, TunnelKind, ValidationError,
};
use fbterm_secrets::{SecretChange, SecretKind, SecretStore, Zeroizing};

use crate::redact::Redacted;

mod duplicate;
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
    /// Save without asking for the main password first (it was asked, or cancelled: the
    /// secret changes then fail and are reported).
    SaveNoAsk,
    /// Re-read `profiles.toml` (offered while it failed to load).
    Reload,
    /// Open the file picker for the key path.
    PickKey,
    /// Open the Color schemes window.
    EditSchemes,
    /// `Export…`: the archive window in its Export mode.
    Export,
    /// `Import…`: the archive window in its Import mode.
    Import,
    /// `Duplicate`: save a copy of the selected profile (see [`duplicate`]).
    Duplicate,
    /// Duplicate this profile without asking for the main password first (it was asked, or
    /// cancelled: the copy is then made without its saved secrets).
    DuplicateNoAsk(ProfileId),
    KeyPicked(Option<PathBuf>),

    Select(ProfileId),
    NewSsh,
    NewLocal,
    Delete,
    Name(String),
    Group(String),
    Accent(Option<Rgb>),
    /// A name from `ui::PROFILE_ICONS`; none is the default for the kind.
    Icon(Option<&'static str>),
    AccentStyle(AccentStyle),
    /// The profile's own scheme id, or `None` to follow the global one.
    ColorScheme(Option<String>),
    Host(String),
    Port(String),
    User(String),
    Auth(AuthChoice),
    /// The typed password or key passphrase (saved to secret storage on save).
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
    TunnelAdd,
    TunnelRemove(usize),
    TunnelName(usize, String),
    TunnelKind(usize, KindChoice),
    TunnelBind(usize, String),
    TunnelPort(usize, String),
    TunnelHost(usize, String),
    TunnelHostPort(usize, String),
    TunnelStart(usize, StartMode),
}

/// Which kind of tunnel a row is, without the details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindChoice {
    Local,
    Remote,
    Socks,
}

impl KindChoice {
    pub(super) fn of(kind: &TunnelKind) -> Self {
        match kind {
            TunnelKind::Local { .. } => Self::Local,
            TunnelKind::Remote { .. } => Self::Remote,
            TunnelKind::Socks { .. } => Self::Socks,
        }
    }
}

/// The warning under a tunnel that listens beyond this machine, if it does.
pub fn tunnel_warning(t: &TunnelDef) -> Option<String> {
    t.non_loopback().then(|| {
        format!(
            "Listens on {}: other machines can reach this tunnel.",
            t.bind().trim()
        )
    })
}

/// Parse a typed port: empty is 0 (not set), otherwise a valid port; `None` rejects the input.
fn typed_port(v: &str, allow_zero: bool) -> Option<u16> {
    if v.is_empty() {
        return Some(0);
    }
    if !v.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    v.parse::<u16>().ok().filter(|&p| allow_zero || p > 0)
}

/// Edit one tunnel; returns whether anything changed.
fn edit_tunnel(draft: &mut SshDraft, msg: EditorMsg) -> bool {
    let SshDraft {
        tunnels, parked, ..
    } = draft;
    parked.resize(tunnels.len(), (String::new(), 0));
    match msg {
        EditorMsg::TunnelAdd => {
            let name = (1..)
                .map(|i| format!("Tunnel {i}"))
                .find(|n| tunnels.iter().all(|t| t.name != *n))
                .expect("an unused name exists");
            parked.push((String::new(), 0));
            tunnels.push(TunnelDef {
                name,
                kind: TunnelKind::Local {
                    bind: "127.0.0.1".into(),
                    port: 0,
                    host: String::new(),
                    host_port: 0,
                },
                start: StartMode::default(),
            });
            return true;
        }
        EditorMsg::TunnelRemove(i) if i < tunnels.len() => {
            tunnels.remove(i);
            parked.remove(i);
            return true;
        }
        _ => {}
    }
    let (EditorMsg::TunnelName(i, _)
    | EditorMsg::TunnelKind(i, _)
    | EditorMsg::TunnelBind(i, _)
    | EditorMsg::TunnelPort(i, _)
    | EditorMsg::TunnelHost(i, _)
    | EditorMsg::TunnelHostPort(i, _)
    | EditorMsg::TunnelStart(i, _)) = msg
    else {
        return false;
    };
    let (Some(t), Some(park)) = (tunnels.get_mut(i), parked.get_mut(i)) else {
        return false;
    };
    match msg {
        EditorMsg::TunnelName(_, v) => t.name = v,
        EditorMsg::TunnelStart(_, v) => t.start = v,
        EditorMsg::TunnelKind(_, choice) => {
            if KindChoice::of(&t.kind) == choice {
                return false;
            }
            let (bind, port, host, host_port) = match std::mem::replace(
                &mut t.kind,
                TunnelKind::Socks {
                    bind: String::new(),
                    port: 0,
                },
            ) {
                TunnelKind::Local {
                    bind,
                    port,
                    host,
                    host_port,
                }
                | TunnelKind::Remote {
                    bind,
                    port,
                    host,
                    host_port,
                } => (bind, port, host, host_port),
                TunnelKind::Socks { bind, port } => (bind, port, park.0.clone(), park.1),
            };
            if choice == KindChoice::Socks {
                *park = (host.clone(), host_port);
            }
            t.kind = match choice {
                KindChoice::Local => TunnelKind::Local {
                    bind,
                    port,
                    host,
                    host_port,
                },
                KindChoice::Remote => TunnelKind::Remote {
                    bind,
                    port,
                    host,
                    host_port,
                },
                KindChoice::Socks => TunnelKind::Socks { bind, port },
            };
        }
        EditorMsg::TunnelBind(_, v) => match &mut t.kind {
            TunnelKind::Local { bind, .. }
            | TunnelKind::Remote { bind, .. }
            | TunnelKind::Socks { bind, .. } => *bind = v,
        },
        EditorMsg::TunnelPort(_, v) => {
            // A remote bind may use 0: the server picks the port.
            let allow_zero = matches!(t.kind, TunnelKind::Remote { .. });
            let Some(new) = typed_port(&v, allow_zero) else {
                return false;
            };
            match &mut t.kind {
                TunnelKind::Local { port, .. }
                | TunnelKind::Remote { port, .. }
                | TunnelKind::Socks { port, .. } => *port = new,
            }
        }
        EditorMsg::TunnelHost(_, v) => match &mut t.kind {
            TunnelKind::Local { host, .. } | TunnelKind::Remote { host, .. } => *host = v,
            TunnelKind::Socks { .. } => return false,
        },
        EditorMsg::TunnelHostPort(_, v) => match &mut t.kind {
            TunnelKind::Local { host_port, .. } | TunnelKind::Remote { host_port, .. } => {
                let Some(new) = typed_port(&v, false) else {
                    return false;
                };
                *host_port = new;
            }
            TunnelKind::Socks { .. } => return false,
        },
        _ => return false,
    }
    true
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
    pub tunnels: Vec<TunnelDef>,
    /// Per tunnel, the target a SOCKS switch set aside, so Local, SOCKS, Local keeps it.
    parked: Vec<(String, u16)>,
    /// Typed secret for the chosen auth; empty keeps whatever is saved.
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
    pub icon: Option<String>,
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
                    AuthMethod::Key { path, passphrase } => (
                        AuthChoice::Key,
                        path.as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                        *passphrase,
                    ),
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
                    tunnels: s.tunnels.clone(),
                    parked: Vec::new(),
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
            icon: p.icon.clone(),
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
                    // No path yet: connecting asks for the key file and remembers it.
                    AuthChoice::Key => AuthMethod::Key {
                        path: Some(s.key_path.trim())
                            .filter(|p| !p.is_empty())
                            .map(PathBuf::from),
                        passphrase: s.key_passphrase,
                    },
                    AuthChoice::Agent => AuthMethod::Agent,
                },
                jump: s.jump.clone(),
                tunnels: s.tunnels.clone(),
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
            icon: self.icon.clone(),
            color_scheme: self.color_scheme.clone(),
            kind,
        }
    }

    pub fn is_ssh(&self) -> bool {
        matches!(self.kind, DraftKind::Ssh(_))
    }

    /// Whether this draft's authentication uses a saved secret of `kind`.
    fn uses_kind(&self, kind: SecretKind) -> bool {
        match &self.kind {
            DraftKind::Ssh(s) => match kind {
                SecretKind::Password => s.auth == AuthChoice::Password,
                SecretKind::KeyPassphrase => s.auth == AuthChoice::Key && s.key_passphrase,
            },
            DraftKind::Local(_) => false,
        }
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
    /// Kinds of secret a saved profile no longer uses whose removal failed; retried on the
    /// next save unless the profile uses that kind again by then.
    removed_kinds: Vec<(ProfileId, SecretKind)>,
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
    /// The Name field, focused when a duplicate opens for renaming.
    pub name_input: iced::widget::Id,
    /// Tells this editor apart from one opened (or reloaded) after it.
    instance: u64,
}

/// The next editor's [`EditorState::instance`].
static NEXT_INSTANCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

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
            removed_kinds: Vec::new(),
            errors: Vec::new(),
            notice: None,
            read_only,
            dirty: false,
            secrets,
            key_check: None,
            name_input: iced::widget::Id::unique(),
            instance: NEXT_INSTANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
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

    /// A connection remembered the key file chosen for key profile `id` (already saved in the
    /// profiles): its draft shows it, unless a path was typed there meanwhile.
    pub fn key_file_remembered(&mut self, id: ProfileId, path: &Path) {
        if let Some(DraftKind::Ssh(s)) = self
            .drafts
            .iter_mut()
            .find(|d| d.id == id)
            .map(|d| &mut d.kind)
            && s.auth == AuthChoice::Key
            && s.key_path.trim().is_empty()
        {
            s.key_path = path.display().to_string();
        }
        self.refresh_key_check();
    }

    /// A connection saved the key passphrase of key profile `id` (the profile is already
    /// marked as having one): its draft says so too, so a later save does not remove it.
    pub fn passphrase_remembered(&mut self, id: ProfileId) {
        if let Some(DraftKind::Ssh(s)) = self
            .drafts
            .iter_mut()
            .find(|d| d.id == id)
            .map(|d| &mut d.kind)
            && s.auth == AuthChoice::Key
        {
            s.key_passphrase = true;
        }
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

    /// `copy`, a saved duplicate of saved profile `original`, joins the drafts right after it,
    /// selected. Only called while nothing is unsaved, so the drafts are the saved profiles.
    pub fn insert_copy(&mut self, original: ProfileId, copy: &Profile) {
        let pos = self
            .drafts
            .iter()
            .position(|d| d.id == original)
            .map_or(self.drafts.len(), |i| i + 1);
        self.drafts.insert(pos, Draft::from_profile(copy));
        self.selected = Some(copy.id);
        self.errors.clear();
        self.refresh_key_check();
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
            icon: None,
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
                tunnels: Vec::new(),
                parked: Vec::new(),
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

    /// The scheme `id` was deleted and the saved profiles no longer use it: drafts that still
    /// name it follow the global scheme too.
    pub fn forget_scheme(&mut self, id: &str) {
        for d in &mut self.drafts {
            if d.color_scheme.as_deref() == Some(id) {
                d.color_scheme = None;
            }
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
        for &(id, kind) in &self.removed_kinds {
            let retry = SecretOp::RemoveKind { id, kind };
            let still_unused = self.draft(id).is_some_and(|d| !d.uses_kind(kind));
            if still_unused && !removes.contains(&retry) {
                removes.push(retry);
            }
        }
        removes.extend(self.removed.iter().map(|&id| SecretOp::Remove(id)));
        sets.extend(removes);
        Ok(Prepared { store, ops: sets })
    }

    /// Save: validate, write `path` atomically, then replace `store` and apply the secret
    /// changes (stores first, then removals). A refused or failed write changes nothing: the
    /// drafts keep their typed secrets. A secret the store refuses stays typed in its draft and
    /// the editor stays dirty. Runs the secret changes here: the app runs them on the
    /// blocking pool instead ([`Self::write`], [`run_secret_ops`], [`Self::finish`]).
    #[cfg(test)]
    pub fn save(&mut self, store: &mut ProfileStore, path: &Path) -> Result<(), SaveError> {
        let ops = self.write(store, path)?;
        let results = run_secret_ops(self.secrets.as_ref(), ops);
        self.finish(results)
    }

    /// Tells this editor apart from one opened (or reloaded) after it.
    pub fn instance(&self) -> u64 {
        self.instance
    }

    /// The store the secret changes of a save go to.
    pub fn secret_store(&self) -> Arc<dyn SecretStore> {
        self.secrets.clone()
    }

    /// The first half of a save: validate, write `path` atomically and replace `store`.
    /// Returns the secret changes still to make.
    pub fn write(
        &mut self,
        store: &mut ProfileStore,
        path: &Path,
    ) -> Result<Vec<SecretOp>, SaveError> {
        let prepared = self.prepare(store).map_err(SaveError::Invalid)?;
        prepared.store.save(path).map_err(SaveError::Write)?;
        *store = prepared.store;
        self.errors.clear();
        self.removed.clear();
        self.removed_kinds.clear();
        Ok(prepared.ops)
    }

    /// The second half of a save: take in how each secret change went. A stored secret is
    /// cleared from its draft (unless something else was typed there meanwhile); a failed
    /// removal of a deleted profile's secrets is retried on the next save.
    pub fn finish(&mut self, results: SecretResults) -> Result<(), SaveError> {
        let mut failures = Vec::new();
        for (op, result) in results {
            let what = op_text(&op, |id| {
                self.draft(id)
                    .map_or_else(|| id.to_string(), |d| d.name.clone())
            });
            match result {
                Ok(()) => {
                    if let SecretOp::Set { id, value, .. } = op
                        && let Some(DraftKind::Ssh(s)) = self
                            .drafts
                            .iter_mut()
                            .find(|d| d.id == id)
                            .map(|d| &mut d.kind)
                        && s.secret == value.0
                    {
                        s.secret.clear();
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "a secret change failed after saving profiles");
                    if let SecretOp::Remove(id) = op {
                        // Retried on the next save.
                        self.removed.push(id);
                    }
                    if let SecretOp::RemoveKind { id, kind } = op {
                        self.removed_kinds.push((id, kind));
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

    /// Whether the secret changes a save would make now need the vault: any, when it must
    /// be unlocked; only stores, when it does not exist yet (nothing to remove then).
    pub fn needs_vault(
        &mut self,
        current: &ProfileStore,
        need: fbterm_secrets::MainPassword,
    ) -> bool {
        let errors = self.errors.clone();
        let needs = match self.prepare(current) {
            Ok(p) => match need {
                fbterm_secrets::MainPassword::Unlock => !p.ops.is_empty(),
                fbterm_secrets::MainPassword::Set => {
                    p.ops.iter().any(|op| matches!(op, SecretOp::Set { .. }))
                }
            },
            Err(_) => false,
        };
        // Only a real save reports problems.
        self.errors = errors;
        needs
    }
}

/// How each secret change of a save went (errors as sentences, never secret values).
pub type SecretResults = Vec<(SecretOp, Result<(), String>)>;

/// Make the secret changes of a save as one batch ([`SecretStore::apply_changes`]: one
/// vault write), in order. Every change gets the batch's outcome: the vault's batch is all or
/// nothing, and on a failure the drafts keep their typed secrets and deleted profiles are
/// retried, so saving again is safe. May reach the vault: never on the UI thread.
pub fn run_secret_ops(secrets: &dyn SecretStore, ops: Vec<SecretOp>) -> SecretResults {
    if ops.is_empty() {
        return Vec::new();
    }
    let changes: Vec<SecretChange> = ops
        .iter()
        .map(|op| match op {
            SecretOp::Set { id, kind, value } => {
                SecretChange::Set((*id, *kind, Zeroizing::new(value.0.clone())))
            }
            SecretOp::RemoveKind { id, kind } => SecretChange::RemoveKind(*id, *kind),
            SecretOp::Remove(id) => SecretChange::Remove(*id),
        })
        .collect();
    let result = secrets.apply_changes(&changes).map_err(|e| e.to_string());
    ops.into_iter().map(|op| (op, result.clone())).collect()
}

/// The failures of a save whose editor was closed meanwhile, as [`EditorState::finish`] words
/// them, naming profiles as saved in `profiles`.
pub fn failures_without_editor(results: SecretResults, profiles: &ProfileStore) -> Vec<String> {
    results
        .into_iter()
        .filter_map(|(op, result)| {
            let e = result.err()?;
            tracing::warn!(error = %e, "a secret change failed after saving profiles");
            let what = op_text(&op, |id| {
                profiles
                    .get(id)
                    .map_or_else(|| id.to_string(), |p| p.name.clone())
            });
            Some(format!("could not {what}: {e}"))
        })
        .collect()
}

/// What secret change `op` makes, naming profiles with `name`.
fn op_text(op: &SecretOp, name: impl Fn(ProfileId) -> String) -> String {
    match op {
        SecretOp::Set { id, kind, .. } => {
            format!("store the {} of {}", kind_name(*kind), name(*id))
        }
        SecretOp::RemoveKind { id, kind } => {
            format!("remove the old {} of {}", kind_name(*kind), name(*id))
        }
        SecretOp::Remove(id) => format!("remove the secrets of a deleted profile ({id})"),
    }
}

fn kind_name(kind: SecretKind) -> &'static str {
    match kind {
        SecretKind::Password => "password",
        SecretKind::KeyPassphrase => "key passphrase",
    }
}

/// The notice of an editor opened while a [`SecretJob`] still runs.
pub const SAVING: &str = "Saving\u{2026}";

/// A saved-secret write started by the editor's Save or Duplicate. It is the app's, not the
/// editor's, so closing the editor never drops the hold while the write runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretJob {
    pub seq: u64,
    pub kind: JobKind,
    /// The [`EditorState::instance`] that started it: only that editor takes in the result
    /// (not one opened or reloaded since).
    pub editor: u64,
}

/// What started a [`SecretJob`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Save,
    Duplicate,
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
        (EditorMsg::Icon(v), _) => d.icon = v.map(str::to_owned),
        (EditorMsg::AccentStyle(v), _) => d.accent_style = v,
        (EditorMsg::ColorScheme(v), _) => d.color_scheme = v,
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
        (
            m @ (EditorMsg::TunnelAdd
            | EditorMsg::TunnelRemove(_)
            | EditorMsg::TunnelName(..)
            | EditorMsg::TunnelKind(..)
            | EditorMsg::TunnelBind(..)
            | EditorMsg::TunnelPort(..)
            | EditorMsg::TunnelHost(..)
            | EditorMsg::TunnelHostPort(..)
            | EditorMsg::TunnelStart(..)),
            DraftKind::Ssh(s),
        ) => return edit_tunnel(s, m),
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
mod duplicate_tests;
#[cfg(test)]
mod job_tests;
#[cfg(test)]
mod tests;
