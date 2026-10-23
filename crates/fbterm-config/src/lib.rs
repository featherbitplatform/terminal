//! Configuration loading, saved profiles and shell discovery.

pub mod paths;
pub mod profile;
pub mod settings;
pub mod shells;
pub mod store;

pub use paths::Paths;
pub use profile::{AuthMethod, LocalProfile, Profile, ProfileId, ProfileKind, Rgb, SshProfile};
pub use settings::{Settings, ThemeMode};
pub use shells::discover_shells;
pub use store::{ConfigError, ProfileStore, ValidationError};
