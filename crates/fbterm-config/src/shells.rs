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

/// Shells known to start as a login shell with a lone `-l` argument.
const LOGIN_FLAG_SHELLS: [&str; 10] = [
    "sh", "bash", "zsh", "fish", "dash", "ksh", "mksh", "tcsh", "csh", "nu",
];

/// Arguments a discovered shell starts with. On Unix a known shell starts as a login shell, so
/// it reads the system and user profiles (`/etc/zprofile`, `~/.zprofile`, `~/.bash_profile`,
/// ...) that set `PATH` and friends: an app started from the desktop inherits only a minimal
/// environment. Windows shells and unknown programs get no arguments.
fn discovered_args(program: &std::path::Path) -> Vec<String> {
    if cfg!(unix)
        && program
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| LOGIN_FLAG_SHELLS.contains(&n))
    {
        vec!["-l".to_owned()]
    } else {
        Vec::new()
    }
}

fn profile_name(program: &std::path::Path) -> String {
    program
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.display().to_string())
}

fn local_profile(program: PathBuf) -> Profile {
    let name = profile_name(&program);
    Profile {
        id: stable_id(&program),
        name,
        group: None,
        accent: None,
        accent_style: Default::default(),
        kind: ProfileKind::Local(LocalProfile {
            args: discovered_args(&program),
            program,
            cwd: None,
            env: Default::default(),
        }),
    }
}

/// A discovered shell candidate: its path, whether it is a file, and its canonical path if
/// it could be resolved.
type Candidate = (PathBuf, bool, Option<PathBuf>);

/// Keep one shell per profile name (file stem), first candidate wins, so `$SHELL` stays first
/// with its exact path. Candidates that resolve to an already kept file (symlinks, usrmerge
/// `/bin` versus `/usr/bin`) are dropped too. Pure over its input.
fn pick_unique(candidates: impl IntoIterator<Item = Candidate>) -> Vec<PathBuf> {
    let mut kept: Vec<PathBuf> = Vec::new();
    let mut names = std::collections::HashSet::new();
    let mut real: Vec<PathBuf> = Vec::new();
    for (path, is_file, canonical) in candidates {
        if !is_file || kept.contains(&path) {
            continue;
        }
        if canonical.as_ref().is_some_and(|c| real.contains(c)) {
            continue;
        }
        if !names.insert(profile_name(&path)) {
            continue;
        }
        real.extend(canonical);
        kept.push(path);
    }
    kept
}

fn dedup_existing(candidates: impl IntoIterator<Item = PathBuf>) -> Vec<Profile> {
    pick_unique(candidates.into_iter().map(|c| {
        let is_file = c.is_file();
        let canonical = std::fs::canonicalize(&c).ok();
        (c, is_file, canonical)
    }))
    .into_iter()
    .map(local_profile)
    .collect()
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
    fn discovered_unix_shells_start_as_login_shells() {
        for sh in [
            "/bin/sh",
            "/bin/bash",
            "/bin/zsh",
            "/usr/local/bin/fish",
            "/bin/dash",
            "/bin/ksh",
            "/bin/tcsh",
        ] {
            let p = super::local_profile(sh.into());
            let crate::ProfileKind::Local(l) = &p.kind else {
                panic!("local")
            };
            assert_eq!(l.args, ["-l"], "{sh}");
        }
        assert!(super::discovered_args(std::path::Path::new("/opt/bin/myshell")).is_empty());
        for p in super::discover_shells() {
            let crate::ProfileKind::Local(l) = &p.kind else {
                continue;
            };
            let name = l.program.file_name().unwrap().to_str().unwrap();
            if super::LOGIN_FLAG_SHELLS.contains(&name) {
                assert_eq!(l.args, ["-l"], "{name}");
            }
        }
    }

    #[test]
    fn discovered_shells_keep_their_id_across_launches() {
        let zsh = super::stable_id(std::path::Path::new("/bin/zsh"));
        assert_eq!(zsh, super::stable_id(std::path::Path::new("/bin/zsh")));
        assert_ne!(zsh, super::stable_id(std::path::Path::new("/bin/bash")));
    }
}

#[cfg(test)]
mod dedup_tests {
    use std::path::PathBuf;

    fn cand(p: &str) -> super::Candidate {
        (PathBuf::from(p), true, None)
    }

    #[test]
    fn usrmerge_duplicates_collapse_to_one_per_name() {
        let kept =
            super::pick_unique(["/bin/bash", "/usr/bin/bash", "/bin/sh", "/usr/bin/sh"].map(cand));
        assert_eq!(kept, [PathBuf::from("/bin/bash"), PathBuf::from("/bin/sh")]);
    }

    #[test]
    fn first_candidate_keeps_its_exact_path_and_missing_are_skipped() {
        let mut missing = cand("/nope/zsh");
        missing.1 = false;
        let kept = super::pick_unique([
            cand("/usr/bin/bash"),
            missing,
            cand("/bin/bash"),
            cand("/bin/zsh"),
        ]);
        assert_eq!(
            kept,
            [PathBuf::from("/usr/bin/bash"), PathBuf::from("/bin/zsh")]
        );
    }

    #[test]
    fn symlinked_shells_with_other_names_are_dropped() {
        let real = Some(PathBuf::from("/usr/bin/bash"));
        let kept = super::pick_unique([
            (PathBuf::from("/bin/bash"), true, real.clone()),
            (PathBuf::from("/bin/rbash"), true, real),
        ]);
        assert_eq!(kept, [PathBuf::from("/bin/bash")]);
    }

    #[test]
    fn discovered_shells_pass_validation() {
        let store = crate::ProfileStore {
            profiles: super::discover_shells(),
        };
        assert!(store.validate().is_empty(), "{:?}", store.validate());
    }
}
