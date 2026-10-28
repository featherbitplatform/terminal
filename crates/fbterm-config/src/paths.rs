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

    /// The log of the current run (what stderr shows, without colors).
    pub fn run_log(&self) -> PathBuf {
        self.config_dir.join("fbterm.log")
    }

    /// The log of the run before, kept when a new run starts.
    pub fn previous_run_log(&self) -> PathBuf {
        self.config_dir.join("fbterm.previous.log")
    }

    /// Crash and start-up failure reports, newest last.
    pub fn crash_log(&self) -> PathBuf {
        self.config_dir.join("crash.log")
    }

    /// The older reports, moved aside when `crash.log` grows too large.
    pub fn previous_crash_log(&self) -> PathBuf {
        self.config_dir.join("crash.previous.log")
    }

    /// Empty marker: a report was written that the next launch has not mentioned yet.
    pub fn crash_pending(&self) -> PathBuf {
        self.config_dir.join("crash.pending")
    }
}
