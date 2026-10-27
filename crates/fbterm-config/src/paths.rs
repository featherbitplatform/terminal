use std::path::PathBuf;

use directories::ProjectDirs;

/// Locations of the user's configuration files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config_dir: PathBuf,
}

impl Paths {
    /// Resolve the per-user configuration directory for the current platform.
    pub fn from_env() -> Self {
        let config_dir = ProjectDirs::from("io", "featherbit", "fbterm")
            .map(|d| d.config_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        Self { config_dir }
    }

    pub fn profiles(&self) -> PathBuf {
        self.config_dir.join("profiles.toml")
    }

    pub fn settings(&self) -> PathBuf {
        self.config_dir.join("settings.toml")
    }

    pub fn state(&self) -> PathBuf {
        self.config_dir.join("state.toml")
    }

    /// The encrypted secrets vault (`age`, unlocked with the main password).
    pub fn vault(&self) -> PathBuf {
        self.config_dir.join("secrets.fbvault")
    }

    /// The Windows system storage for saved secrets: a file protected with DPAPI for the
    /// user's Windows account.
    pub fn dpapi(&self) -> PathBuf {
        self.config_dir.join("secrets.dpapi")
    }

    /// Directory holding one TOML file per user color scheme.
    pub fn schemes(&self) -> PathBuf {
        self.config_dir.join("schemes")
    }
}
