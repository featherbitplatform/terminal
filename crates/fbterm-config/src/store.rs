use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::profile::{Profile, ProfileId, ProfileKind, SshProfile, TunnelKind};

/// Failure to read, parse or write a configuration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub path: PathBuf,
    /// 1-based line of a syntax error, when known.
    pub line: Option<usize>,
    pub message: String,
}

impl ConfigError {
    pub(crate) fn new(path: &Path, line: Option<usize>, message: String) -> Self {
        Self {
            path: path.to_path_buf(),
            line,
            message,
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "{}:{}: {}", self.path.display(), line, self.message),
            None => write!(f, "{}: {}", self.path.display(), self.message),
        }
    }
}

impl std::error::Error for ConfigError {}

/// The most hops a profile's expanded jump chain may have.
pub const MAX_JUMP_CHAIN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    DuplicateName(String),
    MissingJump {
        profile: String,
        missing: ProfileId,
    },
    JumpNotSsh {
        profile: String,
    },
    JumpCycle {
        profile: String,
    },
    /// The expanded jump chain has more than [`MAX_JUMP_CHAIN`] hops.
    JumpChainTooLong {
        profile: String,
    },
    EmptyHost(String),
    EmptyTunnelName {
        profile: String,
    },
    DuplicateTunnel {
        profile: String,
        name: String,
    },
    TunnelPort {
        profile: String,
        name: String,
    },
    TunnelTargetPort {
        profile: String,
        name: String,
    },
    EmptyTunnelBind {
        profile: String,
        name: String,
    },
    EmptyTunnelHost {
        profile: String,
        name: String,
    },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateName(n) => write!(f, "duplicate profile name {n:?}"),
            Self::MissingJump { profile, missing } => {
                write!(
                    f,
                    "profile {profile:?} jumps through unknown profile {missing}"
                )
            }
            Self::JumpNotSsh { profile } => {
                write!(f, "profile {profile:?} jumps through a non-SSH profile")
            }
            Self::JumpCycle { profile } => write!(f, "profile {profile:?} has a jump cycle"),
            Self::JumpChainTooLong { profile } => write!(
                f,
                "profile {profile:?} goes through more than {MAX_JUMP_CHAIN} jump hosts"
            ),
            Self::EmptyHost(n) => write!(f, "profile {n:?} has an empty host"),
            Self::EmptyTunnelName { profile } => {
                write!(f, "profile {profile:?} has a tunnel without a name")
            }
            Self::DuplicateTunnel { profile, name } => {
                write!(f, "profile {profile:?} has two tunnels named {name:?}")
            }
            Self::TunnelPort { profile, name } => {
                write!(
                    f,
                    "tunnel {name:?} of profile {profile:?} needs a port above 0"
                )
            }
            Self::TunnelTargetPort { profile, name } => {
                write!(
                    f,
                    "tunnel {name:?} of profile {profile:?} needs a target port"
                )
            }
            Self::EmptyTunnelBind { profile, name } => {
                write!(
                    f,
                    "tunnel {name:?} of profile {profile:?} has an empty bind address"
                )
            }
            Self::EmptyTunnelHost { profile, name } => {
                write!(
                    f,
                    "tunnel {name:?} of profile {profile:?} has an empty host"
                )
            }
        }
    }
}

impl std::error::Error for ValidationError {}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProfileStore {
    #[serde(default)]
    pub profiles: Vec<Profile>,
}

pub(crate) fn line_of(text: &str, offset: usize) -> usize {
    let end = offset.min(text.len());
    text.as_bytes()[..end]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1
}

/// Write `bytes` to a temp file next to `path`, then rename over it. An existing file's
/// permissions carry over to the new one.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    let io = |e: std::io::Error| ConfigError::new(path, None, e.to_string());
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(dir).map_err(io)?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{file_name}.{}.tmp", ProfileId::new()));
    let permissions = std::fs::metadata(path).ok().map(|m| m.permissions());
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        if let Some(p) = permissions {
            std::fs::set_permissions(&tmp, p)?;
        }
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(io(e));
    }
    Ok(())
}

impl ProfileStore {
    /// Load profiles. A missing file is an empty store; a malformed file is an
    /// error and is never modified.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(ConfigError::new(path, None, e.to_string())),
        };
        toml::from_str(&text).map_err(|e| {
            let line = e.span().map(|s| line_of(&text, s.start));
            ConfigError::new(path, line, e.message().to_string())
        })
    }

    /// Atomically write the store (temp file in the same directory + rename).
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| ConfigError::new(path, None, e.to_string()))?;
        atomic_write(path, text.as_bytes())
    }

    pub fn get(&self, id: ProfileId) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn validate(&self) -> Vec<ValidationError> {
        let index: HashMap<ProfileId, &Profile> = self.profiles.iter().map(|p| (p.id, p)).collect();
        let (cycles, too_long) = self.chain_facts(&index);
        let mut errors = Vec::new();
        let mut seen = HashSet::new();
        let mut reported = HashSet::new();
        for p in &self.profiles {
            if !seen.insert(p.name.as_str()) && reported.insert(p.name.as_str()) {
                errors.push(ValidationError::DuplicateName(p.name.clone()));
            }
            let ProfileKind::Ssh(ssh) = &p.kind else {
                continue;
            };
            if ssh.host.trim().is_empty() {
                errors.push(ValidationError::EmptyHost(p.name.clone()));
            }
            let mut tunnel_names = HashSet::new();
            let mut reported_tunnels = HashSet::new();
            for t in &ssh.tunnels {
                let name = t.name.trim();
                if name.is_empty() {
                    errors.push(ValidationError::EmptyTunnelName {
                        profile: p.name.clone(),
                    });
                } else if !tunnel_names.insert(name) && reported_tunnels.insert(name) {
                    errors.push(ValidationError::DuplicateTunnel {
                        profile: p.name.clone(),
                        name: t.name.clone(),
                    });
                }
                // A remote bind may use 0: the server picks the port.
                let zero = matches!(
                    t.kind,
                    TunnelKind::Local { port: 0, .. } | TunnelKind::Socks { port: 0, .. }
                );
                if zero {
                    errors.push(ValidationError::TunnelPort {
                        profile: p.name.clone(),
                        name: t.name.clone(),
                    });
                }
            }
            for t in &ssh.tunnels {
                if let TunnelKind::Local { host_port: 0, .. }
                | TunnelKind::Remote { host_port: 0, .. } = t.kind
                {
                    errors.push(ValidationError::TunnelTargetPort {
                        profile: p.name.clone(),
                        name: t.name.clone(),
                    });
                }
                if t.bind().trim().is_empty() {
                    errors.push(ValidationError::EmptyTunnelBind {
                        profile: p.name.clone(),
                        name: t.name.clone(),
                    });
                }
                if let TunnelKind::Local { host, .. } | TunnelKind::Remote { host, .. } = &t.kind
                    && host.trim().is_empty()
                {
                    errors.push(ValidationError::EmptyTunnelHost {
                        profile: p.name.clone(),
                        name: t.name.clone(),
                    });
                }
            }
            for &j in &ssh.jump {
                match index.get(&j).copied() {
                    None => errors.push(ValidationError::MissingJump {
                        profile: p.name.clone(),
                        missing: j,
                    }),
                    Some(Profile {
                        kind: ProfileKind::Local(_),
                        ..
                    }) => errors.push(ValidationError::JumpNotSsh {
                        profile: p.name.clone(),
                    }),
                    Some(_) => {}
                }
            }
            if cycles.contains(&p.id) {
                errors.push(ValidationError::JumpCycle {
                    profile: p.name.clone(),
                });
            } else if too_long.contains(&p.id) {
                errors.push(ValidationError::JumpChainTooLong {
                    profile: p.name.clone(),
                });
            }
        }
        errors
    }

    /// The SSH profiles whose jump chain reaches a cycle (through hops that exist and are SSH
    /// profiles), and those whose expanded chain (as [`Self::jump_chain`] builds it) has more
    /// than [`MAX_JUMP_CHAIN`] hops. An iterative depth-first search with white, grey and black
    /// states and saturating hop counts, linear in profiles plus jumps: hand-edited files and
    /// imported archives can hold any shape, so no recursion, no repeated walks and no chain
    /// is ever expanded.
    fn chain_facts(
        &self,
        index: &HashMap<ProfileId, &Profile>,
    ) -> (HashSet<ProfileId>, HashSet<ProfileId>) {
        #[derive(Clone, Copy, PartialEq)]
        enum State {
            White,
            Grey,
            Black,
        }
        const CAP: usize = MAX_JUMP_CHAIN + 1;
        let hops = |id: ProfileId| -> &[ProfileId] {
            match index.get(&id).map(|p| &p.kind) {
                Some(ProfileKind::Ssh(ssh)) => &ssh.jump,
                _ => &[],
            }
        };
        let is_ssh =
            |id: &ProfileId| matches!(index.get(id).map(|p| &p.kind), Some(ProfileKind::Ssh(_)));
        let mut state: HashMap<ProfileId, State> = HashMap::new();
        let mut bad: HashSet<ProfileId> = HashSet::new();
        // Expanded chain length of each finished profile, saturating at CAP.
        let mut length: HashMap<ProfileId, usize> = HashMap::new();
        for root in &self.profiles {
            if !is_ssh(&root.id) || state.get(&root.id).is_some_and(|s| *s != State::White) {
                continue;
            }
            // (profile, index of the next hop to look at)
            let mut stack = vec![(root.id, 0usize)];
            state.insert(root.id, State::Grey);
            while let Some(top) = stack.last_mut() {
                let (id, next) = *top;
                let Some(&hop) = hops(id).get(next) else {
                    stack.pop();
                    state.insert(id, State::Black);
                    // Each hop contributes itself plus its own expanded chain; a hop that
                    // closes a cycle or does not exist is reported elsewhere.
                    let total = hops(id).iter().fold(0usize, |sum, h| {
                        let below = length.get(h).copied().unwrap_or(0);
                        sum.saturating_add(1 + below).min(CAP)
                    });
                    length.insert(id, total);
                    if bad.contains(&id)
                        && let Some((parent, _)) = stack.last()
                    {
                        bad.insert(*parent);
                    }
                    continue;
                };
                top.1 += 1;
                if !is_ssh(&hop) {
                    continue;
                }
                match state.get(&hop).copied().unwrap_or(State::White) {
                    State::White => {
                        state.insert(hop, State::Grey);
                        stack.push((hop, 0));
                    }
                    // Back to a profile on the current path: a cycle.
                    State::Grey => {
                        bad.insert(id);
                    }
                    State::Black => {
                        if bad.contains(&hop) {
                            bad.insert(id);
                        }
                    }
                }
            }
        }
        let too_long = length
            .into_iter()
            .filter(|&(_, n)| n > MAX_JUMP_CHAIN)
            .map(|(id, _)| id)
            .collect();
        (bad, too_long)
    }

    /// Hops to traverse before reaching `id`, in connection order (target
    /// excluded). Hops that themselves jump are expanded first. Iterative and bounded: a chain
    /// of more than [`MAX_JUMP_CHAIN`] hops (however deep or wide) is an error, found after at
    /// most that many hops, so a hand-edited file can never make this hang or overflow.
    pub fn jump_chain(&self, id: ProfileId) -> Result<Vec<&Profile>, ValidationError> {
        let target = self.get(id).ok_or(ValidationError::MissingJump {
            profile: id.to_string(),
            missing: id,
        })?;
        let too_long = || ValidationError::JumpChainTooLong {
            profile: target.name.clone(),
        };
        let mut out = Vec::new();
        // The profiles being expanded, outermost first, with the index of their next hop.
        // Every one but the target ends up in `out`, so the path stays within the cap too.
        let mut path: Vec<(&Profile, usize)> = vec![(target, 0)];
        while let Some(&mut (profile, ref mut next)) = path.last_mut() {
            let jump: &[ProfileId] = match &profile.kind {
                ProfileKind::Ssh(SshProfile { jump, .. }) => jump,
                ProfileKind::Local(_) => &[],
            };
            let Some(&j) = jump.get(*next) else {
                path.pop();
                if !path.is_empty() {
                    out.push(profile);
                    if out.len() > MAX_JUMP_CHAIN {
                        return Err(too_long());
                    }
                }
                continue;
            };
            *next += 1;
            let hop = self.get(j).ok_or_else(|| ValidationError::MissingJump {
                profile: profile.name.clone(),
                missing: j,
            })?;
            if !matches!(hop.kind, ProfileKind::Ssh(_)) {
                return Err(ValidationError::JumpNotSsh {
                    profile: profile.name.clone(),
                });
            }
            if path.iter().any(|(p, _)| p.id == j) {
                return Err(ValidationError::JumpCycle {
                    profile: profile.name.clone(),
                });
            }
            if out.len() + path.len() > MAX_JUMP_CHAIN {
                return Err(too_long());
            }
            path.push((hop, 0));
        }
        Ok(out)
    }
}
