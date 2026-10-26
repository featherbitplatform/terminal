//! Watching the configuration directory for hand edits, as an iced subscription.
//!
//! The directory is watched, not the files: editors save by writing a temp file and renaming
//! it over the original, which a watch on the file itself would lose. Its `schemes/` folder
//! (one file per color scheme) is watched the same way.

use std::path::{Path, PathBuf};
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
    /// A file in `schemes/` (or the folder itself).
    pub schemes: bool,
}

impl Changed {
    /// Note an event on `path`, from the watch on `dir` (the config directory) or on its
    /// `schemes/` folder. Scheme files are told apart by their folder, not their name: a scheme
    /// may be called `profiles.toml`.
    /// The watcher may spell paths differently from `dir` (symlinks resolved), so a folder
    /// named `schemes` other than `dir` itself counts too.
    fn note(&mut self, dir: &Path, path: &Path) {
        let schemes = dir.join("schemes");
        let is_schemes =
            |p: &Path| p == schemes || (p != dir && p.file_name().is_some_and(|n| n == "schemes"));
        if is_schemes(path) || path.parent().is_some_and(is_schemes) {
            self.schemes = true;
            return;
        }
        match path.file_name().and_then(|n| n.to_str()) {
            Some("profiles.toml") => self.profiles = true,
            Some("settings.toml") => self.settings = true,
            _ => {}
        }
    }

    fn any(self) -> bool {
        self.profiles || self.settings || self.schemes
    }
}

/// What the subscription is keyed on: one watch per directory.
#[derive(Debug, Clone, Hash)]
pub struct ConfigDir(pub PathBuf);

type Watching = Option<(
    PathBuf,
    notify::RecommendedWatcher,
    UnboundedReceiver<PathBuf>,
)>;

fn start(dir: &Path) -> Watching {
    if let Err(e) = std::fs::create_dir_all(dir.join("schemes")) {
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
            for path in event.paths {
                let _ = tx.send(path);
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
    let schemes = dir.join("schemes");
    if let Err(e) = watcher.watch(&schemes, RecursiveMode::NonRecursive) {
        tracing::warn!(dir = %schemes.display(), error = %e, "scheme changes will not be picked up");
    }
    Some((dir.to_path_buf(), watcher, rx))
}

/// Debounced change reports for `profiles.toml`, `settings.toml` and `schemes/` in the
/// directory.
pub fn config_stream(dir: &ConfigDir) -> impl Stream<Item = Changed> + use<> {
    iced::futures::stream::unfold(start(&dir.0), |state| async move {
        let (dir, watcher, mut rx) = state?;
        loop {
            let mut changed = Changed::default();
            // Wait for the first relevant event, then until things go quiet.
            let first = rx.recv().await?;
            changed.note(&dir, &first);
            while let Ok(next) = tokio::time::timeout(DEBOUNCE, rx.recv()).await {
                changed.note(&dir, &next?);
            }
            if changed.any() {
                return Some((changed, Some((dir, watcher, rx))));
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheme_files_are_told_apart_by_their_folder() {
        let dir = Path::new("/cfg");
        let mut c = Changed::default();
        c.note(dir, Path::new("/cfg/schemes/profiles.toml"));
        assert_eq!(
            c,
            Changed {
                schemes: true,
                ..Changed::default()
            }
        );
        let mut c = Changed::default();
        c.note(dir, Path::new("/cfg/profiles.toml"));
        c.note(dir, Path::new("/cfg/settings.toml"));
        c.note(dir, Path::new("/cfg/state.toml"));
        assert_eq!(
            c,
            Changed {
                profiles: true,
                settings: true,
                schemes: false,
            }
        );
        let mut c = Changed::default();
        c.note(dir, Path::new("/cfg/schemes"));
        assert!(c.schemes);
    }
}
