//! Watching the configuration directory for hand edits, as an iced subscription.
//!
//! The directory is watched, not the files: editors save by writing a temp file and renaming
//! it over the original, which a watch on the file itself would lose.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use iced::futures::Stream;
use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

/// Quiet period after the last event before reporting (an editor save is several events).
pub const DEBOUNCE: Duration = Duration::from_millis(250);

/// Which watched files changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Changed {
    pub profiles: bool,
    pub settings: bool,
}

impl Changed {
    fn note(&mut self, name: &std::ffi::OsStr) {
        match name.to_str() {
            Some("profiles.toml") => self.profiles = true,
            Some("settings.toml") => self.settings = true,
            _ => {}
        }
    }

    fn any(self) -> bool {
        self.profiles || self.settings
    }
}

/// What the subscription is keyed on: one watch per directory.
#[derive(Debug, Clone, Hash)]
pub struct ConfigDir(pub PathBuf);

type Watching = Option<(notify::RecommendedWatcher, UnboundedReceiver<OsString>)>;

fn start(dir: &PathBuf) -> Watching {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!(dir = %dir.display(), error = %e, "config changes will not be picked up");
        return None;
    }
    let (tx, rx) = unbounded_channel();
    let mut watcher =
        match notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else { return };
            // Our own reads are not changes.
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            for name in event.paths.iter().filter_map(|p| p.file_name()) {
                let _ = tx.send(name.to_os_string());
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(error = %e, "config changes will not be picked up");
                return None;
            }
        };
    if let Err(e) = watcher.watch(dir, RecursiveMode::NonRecursive) {
        tracing::warn!(dir = %dir.display(), error = %e, "config changes will not be picked up");
        return None;
    }
    Some((watcher, rx))
}

/// Debounced change reports for `profiles.toml` and `settings.toml` in the directory.
pub fn config_stream(dir: &ConfigDir) -> impl Stream<Item = Changed> + use<> {
    iced::futures::stream::unfold(start(&dir.0), |state| async move {
        let (watcher, mut rx) = state?;
        loop {
            let mut changed = Changed::default();
            // Wait for the first relevant event, then until things go quiet.
            let first = rx.recv().await?;
            changed.note(&first);
            while let Ok(next) = tokio::time::timeout(DEBOUNCE, rx.recv()).await {
                changed.note(&next?);
            }
            if changed.any() {
                return Some((changed, Some((watcher, rx))));
            }
        }
    })
}
