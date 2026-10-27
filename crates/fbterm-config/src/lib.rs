//! Configuration loading, saved profiles and shell discovery.

pub mod keys;
pub mod paths;
pub mod profile;
pub mod schemes;
pub mod settings;
pub mod settings_edit;
pub mod shells;
pub mod state;
pub mod store;

pub use paths::Paths;
pub use profile::{
    AccentStyle, AuthMethod, LocalProfile, Profile, ProfileId, ProfileKind, Rgb, SshProfile,
    StartMode, TunnelDef, TunnelKind,
};
pub use settings::{MaxFps, SETTINGS_TEMPLATE, Settings, ThemeMode};
pub use settings_edit::SettingChange;
pub use shells::discover_shells;
pub use state::{SavedAxis, SavedLayout, SavedSplitTab, SavedState, SavedTab, SavedWindow};
pub use store::{ConfigError, MAX_JUMP_CHAIN, ProfileStore, ValidationError};
