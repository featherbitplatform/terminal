//! The palette's config actions: open `settings.toml` (creating a commented template first) and
//! reveal the config folder in the file manager.

use std::path::{Path, PathBuf};
use std::process::Command;

use fbterm_config::{Paths, Settings};

use crate::sessions::{opener, spawn_reaped};

/// Absolute form of `p`, so a path can never be taken for an option by the opener.
fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Create `settings.toml` from the commented template if it does not exist, then open it with
/// the system's default app. An existing file is never touched.
pub fn open_settings_file(paths: &Paths) {
    let path = paths.settings();
    if let Err(e) = Settings::write_template_if_missing(&path) {
        tracing::warn!(error = %e, "could not create settings.toml");
        return;
    }
    let path = absolute(&path);
    spawn_reaped(opener(&path.to_string_lossy()), "settings.toml");
}

/// Show the config folder in the file manager (selecting `settings.toml` on macOS and
/// Windows when it exists). The folder is created if missing.
pub fn reveal_config_folder(paths: &Paths) {
    if let Err(e) = std::fs::create_dir_all(&paths.config_dir) {
        tracing::warn!(error = %e, "could not create the config folder");
        return;
    }
    let dir = absolute(&paths.config_dir);
    let file = absolute(&paths.settings());
    spawn_reaped(reveal_command(&dir, &file, file.exists()), "config folder");
}

/// Show `file` in the file manager (selected on macOS and Windows; its folder on Linux).
pub fn reveal_file(file: &Path) {
    let file = absolute(file);
    let dir = file
        .parent()
        .map_or_else(|| file.clone(), Path::to_path_buf);
    spawn_reaped(reveal_command(&dir, &file, file.exists()), "exported file");
}

fn reveal_command(dir: &Path, file: &Path, file_exists: bool) -> Command {
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("open");
        if file_exists {
            cmd.arg("-R").arg(file);
        } else {
            cmd.arg(dir);
        }
        cmd
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = Command::new("explorer");
        if file_exists {
            // One argument, written as Explorer parses it (`/select,"path"`); Windows paths
            // cannot contain a double quote.
            cmd.raw_arg(format!("/select,\"{}\"", file.display()));
        } else {
            cmd.arg(dir);
        }
        cmd
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = (file, file_exists);
        let mut cmd = Command::new("xdg-open");
        cmd.arg(dir);
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_passes_paths_as_arguments() {
        let dir = Path::new("/tmp/fb dir; echo hi");
        let file = dir.join("settings.toml");
        let cmd = reveal_command(dir, &file, false);
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args.len(), 1);
        assert_eq!(args[0], dir.as_os_str());
    }

    #[test]
    fn reveal_selects_an_existing_file_where_the_platform_can() {
        let dir = Path::new("/tmp/fb dir");
        let file = dir.join("settings.toml");
        let cmd = reveal_command(dir, &file, true);
        let args: Vec<_> = cmd.get_args().collect();
        #[cfg(target_os = "macos")]
        {
            assert_eq!(cmd.get_program(), "open");
            assert_eq!(args, [std::ffi::OsStr::new("-R"), file.as_os_str()]);
        }
        #[cfg(windows)]
        {
            assert_eq!(cmd.get_program(), "explorer");
            assert_eq!(args.len(), 1);
            let arg = args[0].to_string_lossy();
            assert!(arg.starts_with("/select,\""), "{arg}");
            assert!(arg.ends_with("settings.toml\""), "{arg}");
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            assert_eq!(cmd.get_program(), "xdg-open");
            assert_eq!(args, [dir.as_os_str()]);
        }
    }
}
