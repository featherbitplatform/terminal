use std::path::PathBuf;

use crate::profile::{LocalProfile, Profile, ProfileId, ProfileKind};

/// Parse the contents of `/etc/shells`: absolute paths, one per line,
/// ignoring blanks and `#` comments.
pub fn parse_etc_shells(contents: &str) -> Vec<PathBuf> {
    contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .collect()
}

/// The id of a discovered shell: derived from its path, so the same shell has the same id on
/// every launch (these profiles are never written to disk, and saved tabs refer to them by id).
fn stable_id(program: &std::path::Path) -> ProfileId {
    // FNV-1a, 128 bit, then marked as a (name-based) version 8 UUID.
    let mut h: u128 = 0x6c62272e07bb014262b821756295c58d;
    for b in program.to_string_lossy().as_bytes() {
        h ^= u128::from(*b);
        h = h.wrapping_mul(0x0000000001000000000000000000013b);
    }
    ProfileId(uuid::Builder::from_custom_bytes(h.to_be_bytes()).into_uuid())
}

fn local_profile(program: PathBuf) -> Profile {
    let name = program
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.display().to_string());
    Profile {
        id: stable_id(&program),
        name,
        group: None,
        accent: None,
        kind: ProfileKind::Local(LocalProfile {
            program,
            args: Vec::new(),
            cwd: None,
            env: Default::default(),
        }),
    }
}

fn dedup_existing(candidates: impl IntoIterator<Item = PathBuf>) -> Vec<Profile> {
    let mut seen = Vec::new();
    for c in candidates {
        if c.is_file() && !seen.contains(&c) {
            seen.push(c);
        }
    }
    seen.into_iter().map(local_profile).collect()
}

/// Shells available on this machine, as ready-to-use local profiles.
pub fn discover_shells() -> Vec<Profile> {
    #[cfg(unix)]
    {
        let mut candidates = Vec::new();
        if let Some(shell) = std::env::var_os("SHELL").filter(|s| !s.is_empty()) {
            candidates.push(PathBuf::from(shell));
        }
        if let Ok(text) = std::fs::read_to_string("/etc/shells") {
            candidates.extend(parse_etc_shells(&text));
        }
        dedup_existing(candidates)
    }
    #[cfg(windows)]
    {
        let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        if let Some(root) = std::env::var_os("SystemRoot") {
            let sys = PathBuf::from(root).join("System32");
            dirs.push(sys.join("WindowsPowerShell").join("v1.0"));
            dirs.push(sys);
        }
        let find = |exe: &str| dirs.iter().map(|d| d.join(exe)).find(|p| p.is_file());
        let mut candidates: Vec<PathBuf> = ["pwsh.exe", "powershell.exe", "cmd.exe"]
            .iter()
            .filter_map(|e| find(e))
            .collect();
        if let Some(pf) = std::env::var_os("ProgramFiles") {
            candidates.push(PathBuf::from(pf).join("Git").join("bin").join("bash.exe"));
        }
        candidates.extend(find("wsl.exe"));
        dedup_existing(candidates)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Vec::new()
    }
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn discover_reads_etc_shells_format() {
        let shells = super::parse_etc_shells("/bin/zsh\n# c\n/bin/bash\n");
        let names: Vec<_> = shells
            .iter()
            .map(|p| p.file_stem().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["zsh", "bash"]);
    }

    #[test]
    fn discover_shells_puts_login_shell_first_without_duplicates() {
        let shells = super::discover_shells();
        let programs: Vec<_> = shells
            .iter()
            .filter_map(|p| match &p.kind {
                crate::ProfileKind::Local(l) => Some(l.program.clone()),
                _ => None,
            })
            .collect();
        let mut unique = programs.clone();
        unique.dedup();
        assert_eq!(programs.len(), unique.len());
        if let Some(sh) = std::env::var_os("SHELL").map(std::path::PathBuf::from)
            && sh.is_file()
        {
            assert_eq!(programs[0], sh);
        }
    }

    #[test]
    fn discovered_shells_keep_their_id_across_launches() {
        let zsh = super::stable_id(std::path::Path::new("/bin/zsh"));
        assert_eq!(zsh, super::stable_id(std::path::Path::new("/bin/zsh")));
        assert_ne!(zsh, super::stable_id(std::path::Path::new("/bin/bash")));
    }
}
