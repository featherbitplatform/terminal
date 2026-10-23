//! Configuration loading, saved profiles and shell discovery.

pub mod paths;
pub mod profile;
pub mod settings;
pub mod settings_edit;
pub mod shells;
pub mod state;
pub mod store;

pub use paths::Paths;
pub use profile::{AuthMethod, LocalProfile, Profile, ProfileId, ProfileKind, Rgb, SshProfile};
pub use settings::{SETTINGS_TEMPLATE, Settings, ThemeMode};
pub use settings_edit::SettingChange;
pub use shells::discover_shells;
pub use state::{SavedState, SavedWindow};
pub use store::{ConfigError, ProfileStore, ValidationError};
