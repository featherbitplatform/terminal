use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

/// Stable identifier of a saved profile (serialized as a string).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileId(pub Uuid);

impl ProfileId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ProfileId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// An 8-bit RGB colour, serialized as `"#rrggbb"`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

impl FromStr for Rgb {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex = s
            .strip_prefix('#')
            .ok_or_else(|| format!("invalid colour {s:?}: expected \"#rrggbb\""))?;
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("invalid colour {s:?}: expected \"#rrggbb\""));
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).expect("validated hex");
        Ok(Self {
            r: byte(0),
            g: byte(2),
            b: byte(4),
        })
    }
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// How a profile's accent colour shows on its tabs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccentStyle {
    /// A thin stripe at the tab's left edge.
    #[default]
    Stripe,
    /// The whole tab tinted with the accent.
    Tab,
}

impl AccentStyle {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub id: ProfileId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<Rgb>,
    /// How the accent shows on tabs (`"stripe"` or `"tab"`); written only when not the default.
    #[serde(default, skip_serializing_if = "AccentStyle::is_default")]
    pub accent_style: AccentStyle,
    /// Id of the color scheme this profile's sessions use; none follows the global scheme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_scheme: Option<String>,
    #[serde(flatten)]
    pub kind: ProfileKind,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProfileKind {
    Local(LocalProfile),
    Ssh(SshProfile),
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LocalProfile {
    pub program: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

fn default_port() -> u16 {
    22
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SshProfile {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    pub auth: AuthMethod,
    #[serde(default)]
    pub jump: Vec<ProfileId>,
    /// Port forwards and SOCKS proxies riding this profile's connection.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tunnels: Vec<TunnelDef>,
}

fn loopback() -> String {
    "127.0.0.1".into()
}

/// A named port forward or SOCKS proxy of an SSH profile.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TunnelDef {
    pub name: String,
    #[serde(flatten)]
    pub kind: TunnelKind,
    /// When the tunnel starts; written only when not the default.
    #[serde(default, skip_serializing_if = "StartMode::is_default")]
    pub start: StartMode,
}

/// What a tunnel forwards. For `Remote`, `bind` and `port` are on the server and `host` and
/// `host_port` are seen from this machine.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TunnelKind {
    Local {
        #[serde(default = "loopback")]
        bind: String,
        port: u16,
        host: String,
        host_port: u16,
    },
    Remote {
        #[serde(default = "loopback")]
        bind: String,
        port: u16,
        host: String,
        host_port: u16,
    },
    Socks {
        #[serde(default = "loopback")]
        bind: String,
        port: u16,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartMode {
    /// Started by hand (or on demand) only.
    #[default]
    OnDemand,
    /// Runs while at least one shell of the profile is connected.
    WithShell,
}

impl StartMode {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl TunnelDef {
    /// The address the tunnel listens on.
    pub fn bind(&self) -> &str {
        match &self.kind {
            TunnelKind::Local { bind, .. }
            | TunnelKind::Remote { bind, .. }
            | TunnelKind::Socks { bind, .. } => bind,
        }
    }

    /// Whether the tunnel listens on something other than a loopback address, so other
    /// machines can reach it.
    pub fn non_loopback(&self) -> bool {
        let bind = self.bind().trim();
        let bind = bind
            .strip_prefix('[')
            .and_then(|b| b.strip_suffix(']'))
            .unwrap_or(bind);
        if bind.eq_ignore_ascii_case("localhost") {
            return false;
        }
        match bind.parse::<std::net::IpAddr>() {
            Ok(ip) => !ip.is_loopback(),
            Err(_) => true,
        }
    }
}

/// How to authenticate. Secrets are never stored in the profile file.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthMethod {
    Password,
    Key {
        path: PathBuf,
        #[serde(default)]
        passphrase: bool,
    },
    Agent,
}
