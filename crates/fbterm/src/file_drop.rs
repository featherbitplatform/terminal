//! Files dropped from the OS on a terminal window whose file panel does not show a server
//! (closed, or open over a local pane): their paths are typed into the window's focused pane,
//! as one paste.
//!
//! The files of one drop are gathered like the panel gathers them for an upload (one OS event
//! per file, see `file_panel::queue`); once the drop settles they go as the paste chord's text
//! goes ([`fbterm_term::paste_input`], so a program that asked for bracketed paste gets it
//! bracketed): each path quoted for the shell the pane runs, joined by single spaces, with one
//! trailing space and no newline, so nothing runs by itself. A pane with no live session
//! (connecting, disconnected, exited) gets nothing. Only the user's drop types a path; nothing
//! a program in the terminal sends can.

use std::path::PathBuf;

use fbterm_config::ProfileKind;
use fbterm_term::TerminalOutput;
use iced::{Task, window};

use crate::app::{App, Message};
use crate::tabs::{TabState, WindowState};

/// How a dropped path is written for the shell that reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Quoting {
    /// A local shell on Windows (PowerShell or cmd): double quotes when needed.
    Windows,
    /// A POSIX shell (local on macOS and Linux, or any SSH pane): single quotes when needed.
    /// `windows_host` also lets `\` through bare (the path is a Windows one).
    Posix { windows_host: bool },
}

impl Quoting {
    /// The quoting for a pane: SSH panes talk to a POSIX shell wherever fbterm runs; a local
    /// pane uses its host's.
    pub(crate) fn for_pane(ssh: bool) -> Self {
        let windows_host = cfg!(windows);
        if windows_host && !ssh {
            Self::Windows
        } else {
            Self::Posix { windows_host }
        }
    }
}

/// Characters that make a path need double quotes in a Windows shell (besides a space).
const WINDOWS_SPECIAL: &str = "&()[]{}^=;!'+,`~$@#%";
/// Characters (besides ASCII letters and digits) a POSIX shell takes bare in a path.
const POSIX_SAFE: &str = "_./~+-:@%,=";

/// `path` written so the shell reads it back as one word, unchanged.
pub(crate) fn quote_path(path: &str, quoting: Quoting) -> String {
    match quoting {
        Quoting::Windows => {
            let special = path.is_empty()
                || path
                    .chars()
                    .any(|c| c == ' ' || WINDOWS_SPECIAL.contains(c));
            // Windows paths cannot hold `"`, so nothing inside needs escaping.
            if special {
                format!("\"{path}\"")
            } else {
                path.to_owned()
            }
        }
        Quoting::Posix { windows_host } => {
            let safe = |c: char| {
                c.is_ascii_alphanumeric() || POSIX_SAFE.contains(c) || (windows_host && c == '\\')
            };
            if !path.is_empty() && path.chars().all(safe) {
                path.to_owned()
            } else {
                format!("'{}'", path.replace('\'', r"'\''"))
            }
        }
    }
}

/// The text a drop of `paths` types: each quoted, joined by spaces, one trailing space.
pub(crate) fn drop_text(paths: &[PathBuf], quoting: Quoting) -> String {
    let mut text = paths
        .iter()
        .map(|p| quote_path(&p.to_string_lossy(), quoting))
        .collect::<Vec<_>>()
        .join(" ");
    text.push(' ');
    text
}

impl App {
    /// Whether the OS drop on `wid` types paths: a terminal window whose panel shows no
    /// server to upload to (closed, or open over a local pane).
    pub(crate) fn drop_types_paths(&self, wid: window::Id) -> bool {
        self.windows.contains_key(&wid) && self.shown_profile(wid).is_none()
    }

    /// Type the paths of one settled drop into `wid`'s focused pane, if its session is live.
    pub(crate) fn type_dropped_paths(
        &mut self,
        wid: window::Id,
        paths: &[PathBuf],
    ) -> Task<Message> {
        if paths.is_empty() {
            return Task::none();
        }
        let Some(pane) = self
            .windows
            .get(&wid)
            .and_then(WindowState::active_tab)
            .map(|t| t.focused_pane())
            .filter(|p| p.state == TabState::Connected)
        else {
            return Task::none();
        };
        let Some(terminal) = pane.session.as_ref().map(|s| s.terminal.clone()) else {
            return Task::none();
        };
        let ssh = matches!(
            self.profiles.get(pane.profile).map(|p| &p.kind),
            Some(ProfileKind::Ssh(_))
        );
        let id = pane.id;
        let text = drop_text(paths, Quoting::for_pane(ssh));
        let bytes = fbterm_term::paste_input(&mut terminal.lock(), &text);
        self.terminal_output(wid, id, TerminalOutput::Input(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POSIX: Quoting = Quoting::Posix {
        windows_host: false,
    };
    const POSIX_ON_WINDOWS: Quoting = Quoting::Posix { windows_host: true };

    #[test]
    fn posix_safe_paths_go_bare() {
        for p in [
            "/home/me/notes.txt",
            "~/a_b-c+d.tar.gz",
            "/srv/host:8080/user@box/50%,x=y",
            "relative/Path9",
        ] {
            assert_eq!(quote_path(p, POSIX), p);
        }
    }

    #[test]
    fn posix_quotes_everything_else() {
        assert_eq!(quote_path("/tmp/my file.txt", POSIX), "'/tmp/my file.txt'");
        assert_eq!(quote_path("/tmp/it's", POSIX), r"'/tmp/it'\''s'");
        assert_eq!(quote_path("/tmp/a\"b", POSIX), "'/tmp/a\"b'");
        assert_eq!(quote_path("/tmp/$HOME*", POSIX), "'/tmp/$HOME*'");
        assert_eq!(quote_path("/tmp/a\\b", POSIX), "'/tmp/a\\b'");
        // Anything beyond ASCII is quoted.
        assert_eq!(quote_path("/tmp/caf\u{e9}", POSIX), "'/tmp/caf\u{e9}'");
        assert_eq!(quote_path("/tmp/\u{65e5}", POSIX), "'/tmp/\u{65e5}'");
        assert_eq!(quote_path("", POSIX), "''");
    }

    #[test]
    fn posix_from_a_windows_host_takes_backslashes_bare() {
        assert_eq!(
            quote_path(r"C:\Users\me\a.txt", POSIX_ON_WINDOWS),
            r"C:\Users\me\a.txt"
        );
        assert_eq!(
            quote_path(r"C:\Users\me\my file.txt", POSIX_ON_WINDOWS),
            r"'C:\Users\me\my file.txt'"
        );
        assert_eq!(quote_path(r"C:\it's", POSIX_ON_WINDOWS), r"'C:\it'\''s'");
    }

    #[test]
    fn windows_quotes_spaces_and_specials_only() {
        let w = Quoting::Windows;
        assert_eq!(quote_path(r"C:\Users\me\a.txt", w), r"C:\Users\me\a.txt");
        assert_eq!(
            quote_path(r"C:\Program Files\x.exe", w),
            r#""C:\Program Files\x.exe""#
        );
        for c in WINDOWS_SPECIAL.chars() {
            let p = format!(r"D:\a{c}b");
            assert_eq!(quote_path(&p, w), format!("\"{p}\""), "{c}");
        }
        // A quote mark inside needs no escaping: double quotes take `'` as is.
        assert_eq!(quote_path(r"C:\it's", w), r#""C:\it's""#);
        // Unicode alone is fine bare.
        assert_eq!(quote_path("C:\\caf\u{e9}", w), "C:\\caf\u{e9}");
        assert_eq!(quote_path("", w), "\"\"");
    }

    #[test]
    fn a_drop_types_paths_joined_by_spaces_with_one_trailing() {
        let paths = [PathBuf::from("/a/b.txt"), PathBuf::from("/c/d e.txt")];
        assert_eq!(drop_text(&paths, POSIX), "/a/b.txt '/c/d e.txt' ");
        assert_eq!(drop_text(&paths[..1], POSIX), "/a/b.txt ");
    }

    #[test]
    fn ssh_panes_are_posix_wherever_fbterm_runs() {
        let windows_host = cfg!(windows);
        assert_eq!(Quoting::for_pane(true), Quoting::Posix { windows_host });
        let local = if windows_host {
            Quoting::Windows
        } else {
            Quoting::Posix {
                windows_host: false,
            }
        };
        assert_eq!(Quoting::for_pane(false), local);
    }

    /// App-level drops into a real shell running `cat -v`, so what was typed shows up.
    #[cfg(unix)]
    mod app {
        use fbterm_config::{Paths, ProfileId};
        use fbterm_term::SharedTerminal;

        use super::*;
        use crate::file_panel::FilePanelMsg;
        use crate::layout::PaneId;
        use crate::sessions::pane_tests::unix::{connected, screen, shell, wait_for};
        use crate::tabs::Tab;

        /// The session's event receiver, kept so the session lives as long as the test.
        type Events = tokio::sync::mpsc::UnboundedReceiver<(
            fbterm_session::SessionId,
            fbterm_session::SessionEvent,
        )>;

        /// An app with one window whose single (local) pane runs `cat -v`, not echoing.
        fn app_with_cat() -> (App, window::Id, PaneId, SharedTerminal, Events) {
            let dir = std::env::temp_dir().join(format!("fbterm-drop-test-{}", ProfileId::new()));
            std::fs::create_dir_all(&dir).unwrap();
            let (mut app, _) = App::build(Paths { config_dir: dir }, true);
            app.state_saving_enabled = false;
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let pane = connected("cat", shell("stty -echo; echo ready; cat -v", tx));
            let term = pane.terminal().unwrap().clone();
            let id = pane.id;
            let wid = window::Id::unique();
            let mut w = WindowState::new();
            w.push(Tab::single(pane));
            app.windows.clear();
            app.windows.insert(wid, w);
            wait_for(&term, "ready");
            (app, wid, id, term, rx)
        }

        fn drop(app: &mut App, wid: window::Id, paths: &[&str]) {
            for p in paths {
                let _ = app.update(Message::FilePanel(FilePanelMsg::Dropped(
                    wid,
                    PathBuf::from(p),
                )));
            }
        }

        fn settle(app: &mut App, wid: window::Id) {
            let _ = app.update(Message::FilePanel(FilePanelMsg::DropsSettled(wid)));
        }

        /// Types `text` and Enter into `pane` the way the keyboard does.
        fn type_line(app: &mut App, wid: window::Id, pane: PaneId, text: &str) {
            let bytes = format!("{text}\r").into_bytes();
            let _ = app.update(Message::Terminal(wid, pane, TerminalOutput::Input(bytes)));
        }

        #[test]
        fn one_drop_of_two_files_is_one_bracketed_paste_in_order() {
            let (mut app, wid, pane, term, _events) = app_with_cat();
            term.lock().feed(b"\x1b[?2004h");
            drop(&mut app, wid, &["/tmp/a.txt", "/tmp/my file.txt"]);
            // Nothing is typed until the drop settles.
            assert_eq!(app.file_panels.dropped[&wid].len(), 2);
            settle(&mut app, wid);
            assert!(app.file_panels.dropped.is_empty());
            // No newline of its own: the line ends only with the Enter typed after it.
            type_line(&mut app, wid, pane, "|end");
            wait_for(&term, "^[[200~/tmp/a.txt '/tmp/my file.txt' ^[[201~|end");
        }

        #[test]
        fn a_pane_without_a_live_session_gets_nothing() {
            let (mut app, wid, pane, term, _events) = app_with_cat();
            let set_state = |app: &mut App, state: TabState| {
                app.windows.get_mut(&wid).unwrap().tabs[0]
                    .pane_mut(pane)
                    .unwrap()
                    .state = state;
            };
            set_state(&mut app, TabState::Exited(Some(0)));
            drop(&mut app, wid, &["/tmp/exited.txt"]);
            settle(&mut app, wid);
            set_state(&mut app, TabState::Connecting);
            drop(&mut app, wid, &["/tmp/connecting.txt"]);
            settle(&mut app, wid);
            set_state(&mut app, TabState::Connected);
            type_line(&mut app, wid, pane, "after");
            wait_for(&term, "after");
            assert!(!screen(&term).contains(".txt"), "{}", screen(&term));
        }

        #[test]
        fn drops_on_other_windows_are_ignored() {
            let (mut app, wid, pane, term, _events) = app_with_cat();
            // A window that is not a terminal window (an editor, the settings).
            let other = window::Id::unique();
            drop(&mut app, other, &["/tmp/elsewhere.txt"]);
            assert!(app.file_panels.dropped.is_empty());
            settle(&mut app, other);
            type_line(&mut app, wid, pane, "still");
            wait_for(&term, "still");
            assert!(!screen(&term).contains("elsewhere"), "{}", screen(&term));
        }

        #[test]
        fn a_drop_with_the_panel_open_over_a_local_pane_is_typed() {
            let (mut app, wid, pane, term, _events) = app_with_cat();
            app.file_panels.show(wid);
            // The pane is local: the open panel has nowhere to upload to, so the path is typed.
            drop(&mut app, wid, &["/tmp/upload.txt"]);
            settle(&mut app, wid);
            type_line(&mut app, wid, pane, "|done");
            wait_for(&term, "/tmp/upload.txt |done");
        }
    }
}
