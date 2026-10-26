//! Measurement mode (`dev/perf/sessions.sh`), a tool and not a debug aid: works in release builds.
//!
//! - `--perf-tabs <N> --perf-cmd "<shell command>"`: open N local tabs in one window, each
//!   running the command through the user's shell (`$SHELL -c`), the last tab active. The flag
//!   implies no session restore, no state saving and in-memory secrets (no keychain access).
//! - Each tab's shell gets `FBTERM_PERF_TAB` (1-based) and `FBTERM_PERF_TABS` in its environment.
//! - Without `FBTERM_CONFIG_DIR` a fresh temporary config dir is used, so perf profiles can never
//!   reach the real `profiles.toml`.
//! - `--perf-exit-after <secs>`: quit cleanly after that long.
//!
//! There is no `--help`; this comment is the documentation.

use std::sync::OnceLock;
use std::time::Duration;

use fbterm_config::{LocalProfile, Profile, ProfileId, ProfileKind};
use iced::Task;

use crate::app::{App, Message};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PerfOpts {
    pub tabs: usize,
    pub cmd: String,
    pub exit_after: Option<u64>,
}

static PERF: OnceLock<PerfOpts> = OnceLock::new();

/// The perf options of this run, if any.
pub(crate) fn opts() -> Option<&'static PerfOpts> {
    PERF.get()
}

/// Parse the perf flags out of `args` (the program name excluded). Unknown arguments are
/// ignored; `Ok(None)` when no perf flag is present.
pub(crate) fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Option<PerfOpts>, String> {
    let mut args = args.into_iter();
    let (mut tabs, mut cmd, mut exit_after) = (None, None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--perf-tabs" => {
                let v = args.next().ok_or("--perf-tabs needs a number")?;
                let n: usize = v.parse().map_err(|_| format!("bad --perf-tabs: {v}"))?;
                if n == 0 {
                    return Err("--perf-tabs must be at least 1".into());
                }
                tabs = Some(n);
            }
            "--perf-cmd" => cmd = Some(args.next().ok_or("--perf-cmd needs a command")?),
            "--perf-exit-after" => {
                let v = args.next().ok_or("--perf-exit-after needs seconds")?;
                exit_after = Some(
                    v.parse()
                        .map_err(|_| format!("bad --perf-exit-after: {v}"))?,
                );
            }
            _ => {}
        }
    }
    match (tabs, cmd) {
        (None, None) => Ok(None),
        (Some(tabs), cmd) => Ok(Some(PerfOpts {
            tabs,
            cmd: cmd.unwrap_or_else(|| "sleep 100000".into()),
            exit_after,
        })),
        (None, Some(_)) => Err("--perf-cmd needs --perf-tabs".into()),
    }
}

/// Parse the process arguments and remember them. Exits with a message on a bad flag.
pub(crate) fn init_from_env() {
    match parse(std::env::args().skip(1)) {
        Ok(Some(o)) => {
            let _ = PERF.set(o);
        }
        Ok(None) => {}
        Err(e) => {
            eprintln!("fbterm: {e}");
            std::process::exit(2);
        }
    }
}

impl App {
    /// The perf window: N tabs running the command, the last one active; plus the exit timer.
    pub(crate) fn open_perf_tabs(&mut self, o: &PerfOpts) -> Task<Message> {
        let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
        let mut ids = Vec::new();
        for i in 0..o.tabs {
            let id = ProfileId::new();
            self.profiles.profiles.push(Profile {
                id,
                name: format!("perf-{}", i + 1),
                group: None,
                accent: None,
                accent_style: Default::default(),
                kind: ProfileKind::Local(LocalProfile {
                    program: shell.clone().into(),
                    args: vec!["-c".into(), o.cmd.clone()],
                    cwd: None,
                    // The command can branch on its own tab (1-based) and the tab count.
                    env: [
                        ("FBTERM_PERF_TAB".to_string(), (i + 1).to_string()),
                        ("FBTERM_PERF_TABS".to_string(), o.tabs.to_string()),
                    ]
                    .into(),
                }),
            });
            ids.push(id);
        }
        let (wid, open) = self.new_window(crate::restore::DEFAULT_SIZE);
        let mut tasks = vec![open];
        for id in ids {
            tasks.push(self.open_tab(wid, id));
        }
        if let Some(w) = self.windows.get_mut(&wid) {
            w.active = w.tabs.len().saturating_sub(1);
        }
        if let Some(secs) = o.exit_after {
            tasks.push(
                Task::future(tokio::time::sleep(Duration::from_secs(secs))).then(|()| iced::exit()),
            );
        }
        Task::batch(tasks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: &[&str]) -> Result<Option<PerfOpts>, String> {
        parse(a.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_flags_is_normal_run() {
        assert_eq!(p(&[]), Ok(None));
        assert_eq!(p(&["-psn_0_1"]), Ok(None));
    }

    #[test]
    fn all_flags() {
        let o = p(&[
            "--perf-tabs",
            "20",
            "--perf-cmd",
            "yes",
            "--perf-exit-after",
            "60",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            o,
            PerfOpts {
                tabs: 20,
                cmd: "yes".into(),
                exit_after: Some(60)
            }
        );
    }

    #[test]
    fn errors() {
        assert!(p(&["--perf-tabs"]).is_err());
        assert!(p(&["--perf-tabs", "x"]).is_err());
        assert!(p(&["--perf-tabs", "0"]).is_err());
        assert!(p(&["--perf-cmd", "yes"]).is_err());
        assert!(p(&["--perf-tabs", "2", "--perf-exit-after", "-1"]).is_err());
    }
}
