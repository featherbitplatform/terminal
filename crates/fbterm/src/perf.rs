//! Measurement mode (`dev/perf/sessions.sh`), a tool and not a debug aid: works in release builds.
//!
//! - `--perf-tabs <N> --perf-cmd "<shell command>"`: open N local tabs in one window, each
//!   running the command through the user's shell (`$SHELL -c`), the last tab active. The flag
//!   implies no session restore, no state saving and in-memory secrets (no keychain access).
//! - Each tab's shell gets `FBTERM_PERF_TAB` (1-based) and `FBTERM_PERF_TABS` in its environment.
//! - Without `FBTERM_CONFIG_DIR` a fresh temporary config dir is used, so perf profiles can never
//!   reach the real `profiles.toml`.
//! - `--perf-panes <N> --perf-cmd "<shell command>"`: one tab holding N panes (two columns,
//!   rows filled left to right), each running the command; `FBTERM_PERF_TAB` is the pane's
//!   1-based number and `FBTERM_PERF_TABS` is N (dev/perf scenario H). Not combinable with
//!   `--perf-tabs`.
//! - `--perf-exit-after <secs>`: quit cleanly after that long.
//! - `--perf-close-after <secs>`: after that long, close every tab but the active (last) one,
//!   as a user closing them would (dev/perf scenario G).
//!
//! There is no `--help`; this comment is the documentation.

use std::sync::OnceLock;
use std::time::Duration;

use fbterm_config::{LocalProfile, Profile, ProfileId, ProfileKind};
use iced::Task;

use crate::app::{App, Message};
use crate::layout::{Axis, Layout, PaneId};
use crate::tabs::{Pane, Tab};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PerfOpts {
    pub tabs: usize,
    /// Panes in the single tab (`--perf-panes`); 0 when the run opens `tabs` tabs instead.
    pub panes: usize,
    pub cmd: String,
    pub exit_after: Option<u64>,
    pub close_after: Option<u64>,
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
    let (mut tabs, mut panes, mut cmd, mut exit_after, mut close_after) =
        (None, None, None, None, None);
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
            "--perf-panes" => {
                let v = args.next().ok_or("--perf-panes needs a number")?;
                let n: usize = v.parse().map_err(|_| format!("bad --perf-panes: {v}"))?;
                if n == 0 {
                    return Err("--perf-panes must be at least 1".into());
                }
                panes = Some(n);
            }
            "--perf-cmd" => cmd = Some(args.next().ok_or("--perf-cmd needs a command")?),
            "--perf-exit-after" => {
                let v = args.next().ok_or("--perf-exit-after needs seconds")?;
                exit_after = Some(
                    v.parse()
                        .map_err(|_| format!("bad --perf-exit-after: {v}"))?,
                );
            }
            "--perf-close-after" => {
                let v = args.next().ok_or("--perf-close-after needs seconds")?;
                close_after = Some(
                    v.parse()
                        .map_err(|_| format!("bad --perf-close-after: {v}"))?,
                );
            }
            _ => {}
        }
    }
    let (tabs, panes) = match (tabs, panes) {
        (Some(_), Some(_)) => return Err("--perf-tabs and --perf-panes exclude each other".into()),
        (Some(t), None) => (t, 0),
        (None, Some(p)) => (1, p),
        (None, None) => {
            return match cmd {
                None => Ok(None),
                Some(_) => Err("--perf-cmd needs --perf-tabs or --perf-panes".into()),
            };
        }
    };
    Ok(Some(PerfOpts {
        tabs,
        panes,
        cmd: cmd.unwrap_or_else(|| "sleep 100000".into()),
        exit_after,
        close_after,
    }))
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
        let count = if o.panes > 0 { o.panes } else { o.tabs };
        for i in 0..count {
            let id = ProfileId::new();
            self.profiles.profiles.push(Profile {
                icon: None,
                id,
                name: format!("perf-{}", i + 1),
                group: None,
                accent: None,
                accent_style: Default::default(),
                color_scheme: None,
                kind: ProfileKind::Local(LocalProfile {
                    program: shell.clone().into(),
                    args: vec!["-c".into(), o.cmd.clone()],
                    cwd: None,
                    // The command can branch on its own tab (1-based) and the tab count.
                    env: [
                        ("FBTERM_PERF_TAB".to_string(), (i + 1).to_string()),
                        ("FBTERM_PERF_TABS".to_string(), count.to_string()),
                    ]
                    .into(),
                }),
            });
            ids.push(id);
        }
        let (wid, open) = self.new_window(crate::restore::DEFAULT_SIZE);
        let mut tasks = vec![open];
        if o.panes > 0 {
            tasks.push(self.open_perf_panes(wid, &ids));
        } else {
            for id in ids {
                tasks.push(self.open_tab(wid, id));
            }
        }
        if let Some(w) = self.windows.get_mut(&wid) {
            w.active = w.tabs.len().saturating_sub(1);
        }
        if let Some(secs) = o.exit_after {
            tasks.push(
                Task::future(tokio::time::sleep(Duration::from_secs(secs))).then(|()| iced::exit()),
            );
        }
        if let Some(secs) = o.close_after {
            tasks.push(
                Task::future(tokio::time::sleep(Duration::from_secs(secs)))
                    .map(move |()| Message::PerfCloseOthers(wid)),
            );
        }
        Task::batch(tasks)
    }

    /// One tab holding a pane per profile, in two columns (rows filled left to right).
    fn open_perf_panes(&mut self, wid: iced::window::Id, profiles: &[ProfileId]) -> Task<Message> {
        let mut panes: Vec<Pane> = profiles
            .iter()
            .filter_map(|id| self.profiles.get(*id).map(Pane::for_profile))
            .collect();
        if panes.is_empty() {
            return Task::none();
        }
        let ids: Vec<PaneId> = panes.iter().map(|p| p.id).collect();
        let mut tab = Tab::single(panes.remove(0));
        tab.panes.extend(panes);
        tab.layout = grid_layout(&ids);
        let Some(window) = self.windows.get_mut(&wid) else {
            return Task::none();
        };
        window.push(tab);
        Task::batch(ids.into_iter().map(|id| self.start(wid, id)))
    }

    /// Close every tab of `wid` but the active one, one `close_tab` at a time.
    pub(crate) fn perf_close_others(&mut self, wid: iced::window::Id) -> Task<Message> {
        let mut tasks = Vec::new();
        while let Some(w) = self.windows.get(&wid) {
            if w.tabs.len() <= 1 {
                break;
            }
            let victim = if w.active == 0 { 1 } else { 0 };
            tasks.push(self.close_tab(wid, victim));
        }
        Task::batch(tasks)
    }
}

/// Panes in two columns, rows filled left to right and stacked in equal shares.
fn grid_layout(ids: &[PaneId]) -> Layout {
    fn row(pair: &[PaneId]) -> Layout {
        match pair {
            [a, b] => Layout::Split {
                axis: Axis::Row,
                ratio: 0.5,
                first: Box::new(Layout::Pane(*a)),
                second: Box::new(Layout::Pane(*b)),
            },
            [a] => Layout::Pane(*a),
            _ => unreachable!("chunks of at most two"),
        }
    }
    fn stack(rows: &[&[PaneId]]) -> Layout {
        match rows {
            [only] => row(only),
            [first, rest @ ..] => Layout::Split {
                axis: Axis::Column,
                ratio: 1.0 / rows.len() as f32,
                first: Box::new(row(first)),
                second: Box::new(stack(rest)),
            },
            [] => unreachable!("at least one pane"),
        }
    }
    let rows: Vec<&[PaneId]> = ids.chunks(2).collect();
    stack(&rows)
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
                panes: 0,
                cmd: "yes".into(),
                exit_after: Some(60),
                close_after: None
            }
        );
    }

    #[test]
    fn close_after() {
        let o = p(&["--perf-tabs", "3", "--perf-close-after", "5"])
            .unwrap()
            .unwrap();
        assert_eq!(o.close_after, Some(5));
        assert!(p(&["--perf-tabs", "3", "--perf-close-after", "x"]).is_err());
    }

    #[test]
    fn panes_make_one_tab() {
        let o = p(&["--perf-panes", "4", "--perf-cmd", "yes"])
            .unwrap()
            .unwrap();
        assert_eq!((o.tabs, o.panes), (1, 4));
        assert!(p(&["--perf-panes", "0"]).is_err());
        assert!(p(&["--perf-panes", "2", "--perf-tabs", "2"]).is_err());
    }

    #[test]
    fn grid_holds_every_pane_once() {
        for n in 1..=7u64 {
            let ids: Vec<PaneId> = (1..=n).map(PaneId).collect();
            let mut got = grid_layout(&ids).panes();
            got.sort();
            assert_eq!(got, ids);
        }
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
