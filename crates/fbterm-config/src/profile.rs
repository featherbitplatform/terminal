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

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub id: ProfileId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<Rgb>,
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
