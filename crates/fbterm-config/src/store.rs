use std::collections::HashSet;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    DuplicateName(String),
    MissingJump { profile: String, missing: ProfileId },
    JumpNotSsh { profile: String },
    JumpCycle { profile: String },
    EmptyHost(String),
    EmptyTunnelName { profile: String },
    DuplicateTunnel { profile: String, name: String },
    TunnelPort { profile: String, name: String },
    TunnelTargetPort { profile: String, name: String },
    EmptyTunnelBind { profile: String, name: String },
    EmptyTunnelHost { profile: String, name: String },
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
                match self.get(j) {
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
            if matches!(
                self.jump_chain(p.id),
                Err(ValidationError::JumpCycle { .. })
            ) {
                errors.push(ValidationError::JumpCycle {
                    profile: p.name.clone(),
                });
            }
        }
        errors
    }

    /// Hops to traverse before reaching `id`, in connection order (target
    /// excluded). Hops that themselves jump are expanded first.
    pub fn jump_chain(&self, id: ProfileId) -> Result<Vec<&Profile>, ValidationError> {
        let target = self.get(id).ok_or(ValidationError::MissingJump {
            profile: id.to_string(),
            missing: id,
        })?;
        let mut out = Vec::new();
        let mut stack = vec![id];
        self.walk(target, &mut stack, &mut out)?;
        Ok(out)
    }

    fn walk<'a>(
        &'a self,
        profile: &'a Profile,
        stack: &mut Vec<ProfileId>,
        out: &mut Vec<&'a Profile>,
    ) -> Result<(), ValidationError> {
        let ProfileKind::Ssh(SshProfile { jump, .. }) = &profile.kind else {
            return Ok(());
        };
        for &j in jump {
            let hop = self.get(j).ok_or_else(|| ValidationError::MissingJump {
                profile: profile.name.clone(),
                missing: j,
            })?;
            if !matches!(hop.kind, ProfileKind::Ssh(_)) {
                return Err(ValidationError::JumpNotSsh {
                    profile: profile.name.clone(),
                });
            }
            if stack.contains(&j) {
                return Err(ValidationError::JumpCycle {
                    profile: profile.name.clone(),
                });
            }
            stack.push(j);
            self.walk(hop, stack, out)?;
            stack.pop();
            out.push(hop);
        }
        Ok(())
    }
}
