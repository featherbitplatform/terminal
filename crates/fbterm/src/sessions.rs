//! Session lifecycle: starting local and SSH sessions, their events and terminal input.

use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Instant;

use fbterm_config::{Profile, ProfileKind};
use fbterm_session::ssh::SshError;
use fbterm_session::{AuthWarning, SessionEvent, SessionHandle, SessionId, spawn_local_into};
use fbterm_term::{GridSize, SharedTerminal, Terminal, TerminalOutput};
use iced::Task;
use iced::futures::Stream;
use iced::window;
use parking_lot::Mutex;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::{App, DEFAULT_GRID, Delivered, Message};
use crate::layout::PaneId;
use crate::pacing::{OUTPUT_FPS, OutputRate, Pacer};
use crate::prompts::UiPrompter;
use crate::tabs::{self, InputRoute, Pane, Tab, TabState, WindowState};

type Events = UnboundedReceiver<(SessionId, SessionEvent)>;

/// Session events from every session, delivered to the update loop by one subscription.
#[derive(Clone)]
pub(crate) struct EventReceiver {
    pub(crate) rx: Arc<Mutex<Option<Events>>>,
    /// The session the user last typed into: its next output (the echo) skips pacing.
    typed: Arc<Mutex<Option<SessionId>>>,
    /// Most output-driven deliveries per second, read for every event.
    pub(crate) rate: OutputRate,
}

impl EventReceiver {
    pub(crate) fn new(rx: Events) -> Self {
        Self {
            rx: Arc::new(Mutex::new(Some(rx))),
            typed: Arc::new(Mutex::new(None)),
            rate: OutputRate::new(OUTPUT_FPS),
        }
    }

    /// The user sent input to a session; its echo should show on the next frame.
    pub(crate) fn typed(&self, id: SessionId) {
        *self.typed.lock() = Some(id);
    }
}

impl Hash for EventReceiver {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "fbterm-session-events".hash(state);
    }
}

/// Whether a `Dirty` from `id` is the echo of the user's input; only that session consumes it.
fn take_typed(slot: &Mutex<Option<SessionId>>, id: SessionId) -> bool {
    let mut slot = slot.lock();
    if *slot == Some(id) {
        *slot = None;
        true
    } else {
        false
    }
}

struct Paced {
    rx: Events,
    typed: Arc<Mutex<Option<SessionId>>>,
    pacer: Pacer<SessionId>,
    rate: OutputRate,
    ready: VecDeque<(SessionId, SessionEvent)>,
}

/// Session events with `Dirty` paced (see [`crate::pacing`]); other events pass at once.
pub(crate) fn session_stream(
    rx: &EventReceiver,
) -> impl Stream<Item = (SessionId, SessionEvent)> + use<> {
    let state = rx.rx.lock().take().map(|events| Paced {
        rx: events,
        typed: rx.typed.clone(),
        pacer: Pacer::new(rx.rate.get()),
        rate: rx.rate.clone(),
        ready: VecDeque::new(),
    });
    iced::futures::stream::unfold(state, |state| async move {
        let mut s = state?;
        loop {
            if let Some(event) = s.ready.pop_front() {
                return Some((event, Some(s)));
            }
            let received = match s.pacer.deadline() {
                Some(due) => match tokio::time::timeout_at(due.into(), s.rx.recv()).await {
                    Ok(received) => received,
                    Err(_) => {
                        let ids = s.pacer.fire(Instant::now());
                        s.ready
                            .extend(ids.into_iter().map(|id| (id, SessionEvent::Dirty)));
                        continue;
                    }
                },
                None => s.rx.recv().await,
            };
            let (id, event) = received?;
            if event == SessionEvent::Dirty {
                // The cap may have changed (settings, or the display a window is on).
                s.pacer.set_fps(s.rate.get());
                let typed = take_typed(&s.typed, id);
                let ids = s.pacer.dirty(id, Instant::now(), typed);
                s.ready
                    .extend(ids.into_iter().map(|id| (id, SessionEvent::Dirty)));
            } else {
                s.ready.push_back((id, event));
            }
        }
    })
}

impl App {
    pub(crate) fn new_terminal(&self, grid: GridSize) -> SharedTerminal {
        let mut term = Terminal::new(grid, self.settings.scrollback_lines);
        term.set_kitty_keyboard(self.settings.kitty_keyboard);
        self.style.scheme.apply_to(&mut term);
        Arc::new(Mutex::new(term))
    }

    /// (Re)start a pane's session: spawn a local program, or dial an SSH profile.
    pub(crate) fn start(&mut self, wid: window::Id, id: PaneId) -> Task<Message> {
        let grid = self.grids.get(&wid).copied().unwrap_or(DEFAULT_GRID);
        tracing::info!(pane = id.0, "starting pane");
        let fresh_terminal = self.new_terminal(grid);
        let events = self.events.clone();
        let profile: Option<Profile> = self
            .windows
            .get(&wid)
            .and_then(|w| w.locate(id).map(|(t, p)| w.tabs[t].panes[p].profile))
            .and_then(|p| self.profiles.get(p).cloned());
        let scheme = self.style.scheme;
        let Some(pane) = self.windows.get_mut(&wid).and_then(|w| {
            let (t, p) = w.locate(id)?;
            w.tabs[t].panes.get_mut(p)
        }) else {
            return Task::none();
        };
        // A restart continues in the pane's terminal, so its output and scrollback stay; the
        // first start gets a new one.
        let previous = pane.terminal().cloned();
        // Close the previous session (and drop any dial in flight) before anything touches
        // the terminal again: once closed, its lingering output is discarded, never fed.
        if let Some(old) = pane.session.take() {
            old.close();
        }
        pane.connecting = None;
        let (terminal, fresh) = match previous {
            Some(t) => {
                t.lock().feed(tabs::RESTART_RESET);
                (t, false)
            }
            None => (fresh_terminal, true),
        };
        pane.title = pane.name.clone();
        pane.placeholder = Some(terminal.clone());
        let Some(profile) = profile else {
            pane.state = TabState::Disconnected("profile not found".into());
            write_notice(pane, "this profile no longer exists", fresh);
            return Task::none();
        };
        match &profile.kind {
            ProfileKind::Local(local) => {
                match spawn_local_into(local, terminal, events) {
                    Ok(handle) => {
                        if fresh {
                            scheme.apply_to(&mut handle.terminal.lock());
                        }
                        pane.session = Some(handle);
                        pane.placeholder = None;
                        pane.state = TabState::Connected;
                    }
                    Err(e) => {
                        let text = format!(
                            "could not start {}: {e}, press Enter to retry",
                            profile.name
                        );
                        pane.state = TabState::Disconnected(e.to_string());
                        write_notice(pane, &text, fresh);
                    }
                }
                Task::none()
            }
            ProfileKind::Ssh(ssh) => {
                pane.state = TabState::Connecting;
                write_notice(
                    pane,
                    &format!("connecting to {}@{}:{}", ssh.user, ssh.host, ssh.port),
                    fresh,
                );
                let pool = self.pool.clone();
                let store = self.profiles.clone();
                let pid = profile.id;
                let requested = terminal.lock().size();
                let terminal = terminal.clone();
                // Questions of this dial open over this pane's window.
                let prompter = Arc::new(UiPrompter::new(Some(wid), self.prompt_tx.clone()));
                let dial = async move {
                    pool.open_shell_prompting(&store, &pid, terminal, events, prompter)
                        .await
                };
                let (task, handle) = Task::perform(dial, move |r| match r {
                    Ok(h) => Message::Connected(id, Delivered::new(h), requested),
                    Err(SshError::HostKeyChanged { host, port }) => {
                        let reason = SshError::HostKeyChanged {
                            host: host.clone(),
                            port,
                        }
                        .to_string();
                        Message::HostKeyChanged {
                            pane: id,
                            host,
                            port,
                            reason,
                        }
                    }
                    Err(e) => Message::ConnectFailed(id, e.to_string()),
                })
                .abortable();
                pane.connecting = Some(handle.abort_on_drop());
                task
            }
        }
    }

    /// `requested`: the size the remote pty was opened with. The window may have resized the
    /// terminal while the dial was in flight; the remote side then gets the current size.
    pub(crate) fn connected(&mut self, id: PaneId, handle: SessionHandle, requested: GridSize) {
        // A pane closed meanwhile drops the handle here, closing the shell. The session writes
        // into the pane's own terminal, which already carries the color scheme.
        let Some((wid, window, (t, p))) = self.windows.iter_mut().find_map(|(wid, w)| {
            let at = w.locate(id)?;
            Some((*wid, w, at))
        }) else {
            return;
        };
        let current = handle.terminal.lock().size();
        if current != requested {
            handle.resize(current);
        }
        let pane = &mut window.tabs[t].panes[p];
        let warnings = handle.auth_warnings().to_vec();
        if let Some(text) = tabs::install_session(pane, handle) {
            write_notice(pane, &text, false);
        }
        // Once per pane: each pane's own handle carries its connection's warnings.
        for warning in warnings {
            match warning {
                AuthWarning::RsaUserKey => write_notice(pane, crate::editor::RSA_ADVICE, false),
            }
        }
        // The shell's reader started before the handle got here, so a `Dirty` it raised went
        // nowhere and is still pending: take it as a visible pane does (the redraw after this
        // update shows the output); a hidden pane, or a pane in a window that cannot be seen,
        // keeps it pending until it is shown.
        if !self.occluded.contains(&wid) {
            tabs::on_dirty(window, id);
        }
    }

    pub(crate) fn session_event(&mut self, sid: SessionId, event: SessionEvent) -> Task<Message> {
        if event == SessionEvent::Released {
            // The session of a pane the user closed: no pane is left to show it.
            self.release_swept();
            return Task::none();
        }
        let Some((wid, window, pane)) = self.windows.iter_mut().find_map(|(wid, w)| {
            let id = w
                .tabs
                .iter()
                .flat_map(|t| &t.panes)
                .find(|p| p.session.as_ref().is_some_and(|s| s.id == sid))?
                .id;
            Some((*wid, w, id))
        }) else {
            return Task::none();
        };
        if event == SessionEvent::Dirty {
            // A visible pane re-arms here, before the redraw that follows this update locks
            // the terminal. A hidden pane, or any pane of a window that cannot be seen (see
            // `crate::visibility`), leaves the flag pending, so its session raises no further
            // Dirty until it is shown (see `App::shown`).
            if !self.occluded.contains(&wid) && tabs::on_dirty(window, pane) {
                // A find bar on the shown pane follows its output.
                return self.find_output(pane);
            }
            return Task::none();
        }
        let Some((t, p)) = window.locate(pane) else {
            return Task::none();
        };
        let pane = &mut window.tabs[t].panes[p];
        if let Some(text) = tabs::apply_event(pane, &event) {
            write_notice(pane, &text, false);
        }
        Task::none()
    }

    pub(crate) fn terminal_output(
        &mut self,
        wid: window::Id,
        output: TerminalOutput,
    ) -> Task<Message> {
        match output {
            TerminalOutput::Input(bytes) => {
                let Some(pane) = self
                    .windows
                    .get_mut(&wid)
                    .and_then(WindowState::active_tab_mut)
                    .map(Tab::focused_pane_mut)
                else {
                    return Task::none();
                };
                match tabs::route_input(&pane.state, &bytes) {
                    InputRoute::Send => {
                        if let Some(s) = &pane.session {
                            self.receiver.typed(s.id);
                            s.send(bytes);
                        }
                        Task::none()
                    }
                    InputRoute::Reconnect | InputRoute::Respawn => {
                        let id = pane.id;
                        self.start(wid, id)
                    }
                    InputRoute::Ignore => Task::none(),
                }
            }
            TerminalOutput::Resized(grid) => {
                self.grids.insert(wid, grid);
                if let Some(s) = self
                    .windows
                    .get(&wid)
                    .and_then(WindowState::active_tab)
                    .and_then(|t| t.focused_pane().session.as_ref())
                {
                    s.resize(grid);
                }
                self.find_resized(wid)
            }
            // The widget already put the text on the clipboard.
            TerminalOutput::Copy(_) => Task::none(),
            TerminalOutput::OpenUrl(url) => {
                open_url(&url);
                Task::none()
            }
        }
    }
}

/// Write a dim notice into the pane's terminal, keeping its scrollback.
/// `fresh`: the terminal was just created and is still empty.
pub(crate) fn write_notice(pane: &Pane, text: &str, fresh: bool) {
    if let Some(term) = pane.terminal() {
        term.lock().feed(&tabs::notice_bytes(text, fresh));
    }
}

/// What to do with a link the user clicked. The link comes from program output, so only
/// schemes whose handlers never run local programs are opened.
#[derive(Debug, PartialEq, Eq)]
enum LinkAction {
    /// Hand the link to the system opener.
    Open,
    /// A local file: show it in the file manager, never open (and so never run) it.
    Reveal(std::path::PathBuf),
    Refuse,
}

/// Decide what a click on `url` does: web and mail links open; `file://` links to an
/// absolute path are revealed; everything else (`javascript:`, `data:`, `ssh:`, custom app
/// schemes, malformed file links) is refused.
fn link_action(url: &str) -> LinkAction {
    let lower = url.to_ascii_lowercase();
    if ["http://", "https://", "mailto:"]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return LinkAction::Open;
    }
    if lower.starts_with("file://") {
        // file://host/path: the authority (often the host name of the machine that printed
        // the link) is ignored; only the path is used.
        let rest = &url["file://".len()..];
        let path = rest.find('/').map_or("", |i| &rest[i..]);
        let path = path.split(['?', '#']).next().unwrap_or_default();
        return match percent_decode(path) {
            Some(p) if p.starts_with('/') && !p.contains('\0') => {
                LinkAction::Reveal(std::path::PathBuf::from(p))
            }
            _ => LinkAction::Refuse,
        };
    }
    LinkAction::Refuse
}

/// Decode `%XX` escapes; `None` for a malformed escape or a result that is not UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Act on a clicked link (see [`link_action`]).
fn open_url(url: &str) {
    let cmd = match link_action(url) {
        LinkAction::Open => opener(url),
        LinkAction::Reveal(path) => match reveal(&path) {
            Some(cmd) => cmd,
            None => {
                tracing::info!(path = %path.display(), "not opening a file link on this system");
                return;
            }
        },
        LinkAction::Refuse => {
            tracing::warn!(%url, "not opening link with an unsupported scheme");
            return;
        }
    };
    spawn_reaped(cmd, url);
}

/// The system opener for a web or mail link.
pub(crate) fn opener(url: &str) -> std::process::Command {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(url);
    cmd
}

/// Show a local file in the file manager without opening it. Only macOS has a reveal that
/// cannot launch the file; elsewhere file links are logged and left alone.
#[cfg(target_os = "macos")]
fn reveal(path: &std::path::Path) -> Option<std::process::Command> {
    let mut cmd = std::process::Command::new("open");
    // `path` is absolute, so it can never be taken for an option.
    cmd.arg("-R").arg(path);
    Some(cmd)
}

#[cfg(not(target_os = "macos"))]
fn reveal(_path: &std::path::Path) -> Option<std::process::Command> {
    None
}

/// Start `cmd` (the system opener) and reap it; `what` names the thing being opened, for the log.
pub(crate) fn spawn_reaped(mut cmd: std::process::Command, what: &str) {
    match cmd.spawn() {
        // Reap the opener so it does not linger as a zombie.
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => tracing::warn!(%what, error = %e, "could not start the system opener"),
    }
}

#[cfg(test)]
mod tests {
    use super::{LinkAction, link_action, take_typed};
    use fbterm_session::SessionId;
    use parking_lot::Mutex;

    #[test]
    fn typing_echo_is_consumed_only_by_its_own_session() {
        let slot = Mutex::new(Some(SessionId(1)));
        assert!(!take_typed(&slot, SessionId(2)), "other session's output");
        assert!(take_typed(&slot, SessionId(1)), "the typed-into session");
        assert!(!take_typed(&slot, SessionId(1)), "consumed once");
    }

    #[test]
    fn web_and_mail_links_open() {
        for url in [
            "http://example.com",
            "https://example.com/a?b#c",
            "HTTPS://EXAMPLE.COM",
            "mailto:someone@example.com",
        ] {
            assert_eq!(link_action(url), LinkAction::Open, "{url}");
        }
    }

    #[test]
    fn file_links_are_revealed_never_opened() {
        assert_eq!(
            link_action("file:///Applications/Calculator.app"),
            LinkAction::Reveal("/Applications/Calculator.app".into())
        );
        assert_eq!(
            link_action("file://build-host/home/me/My%20File.txt"),
            LinkAction::Reveal("/home/me/My File.txt".into())
        );
        assert_eq!(
            link_action("FILE://localhost/tmp/x?q#frag"),
            LinkAction::Reveal("/tmp/x".into())
        );
    }

    #[test]
    fn other_schemes_and_bad_file_links_are_refused() {
        for url in [
            "javascript:alert(1)",
            "data:text/html,hi",
            "ssh://host",
            "vscode://file/x",
            "x-custom-app:run",
            "ftp://example.com",
            "/etc/passwd",
            "",
            "file://",
            "file://hostonly",
            "file:///bad%zz",
            "file:///a%00b",
        ] {
            assert_eq!(link_action(url), LinkAction::Refuse, "{url}");
        }
    }
}

#[cfg(test)]
mod pane_tests {
    use iced::window;

    use crate::app::App;
    use crate::layout::Side;
    use crate::restore_tests::{paths_with, profile};
    use crate::tabs::{Pane, Tab, TabState, WindowState};

    /// Two panes side by side in one tab, as a split would make them.
    fn two_panes(first: Pane, second: Pane) -> Tab {
        let (a, b) = (first.id, second.id);
        let mut tab = Tab::single(first);
        tab.layout.split(a, Side::Right, b);
        tab.panes.push(second);
        tab
    }

    #[test]
    fn shown_starts_every_pane_of_the_active_tab() {
        let p = profile("a");
        let (mut app, _) = App::build(paths_with(std::slice::from_ref(&p)), true);
        app.state_saving_enabled = false;
        let wid = window::Id::unique();
        let mut w = WindowState::new();
        w.push(two_panes(Pane::for_profile(&p), Pane::for_profile(&p)));
        w.push(Tab::single(Pane::for_profile(&p)));
        w.active = 0;
        app.windows.clear();
        app.windows.insert(wid, w);
        let _ = app.shown(wid);
        let w = &app.windows[&wid];
        assert!(
            w.tabs[0]
                .panes
                .iter()
                .all(|p| p.state != TabState::NotConnected),
            "both panes of the shown tab started"
        );
        assert_eq!(w.tabs[1].panes[0].state, TabState::NotConnected);
    }

    #[cfg(unix)]
    mod unix {
        use std::time::{Duration, Instant};

        use fbterm_config::{LocalProfile, Paths, ProfileId};
        use fbterm_session::{SessionEvent, SessionHandle};
        use fbterm_term::SharedTerminal;
        use feathercritty::index::{Column, Line};
        use iced::window;

        use super::two_panes;
        use crate::app::{App, Message};
        use crate::tabs::{self, Pane, Tab, TabState, WindowState};

        type Sink = tokio::sync::mpsc::UnboundedSender<(
            fbterm_session::SessionId,
            fbterm_session::SessionEvent,
        )>;

        fn shell(script: &str, tx: Sink) -> SessionHandle {
            let local = LocalProfile {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                cwd: None,
                env: Default::default(),
            };
            fbterm_session::spawn_local(&local, crate::app::DEFAULT_GRID, 100, tx).expect("spawn")
        }

        fn connected(name: &str, session: SessionHandle) -> Pane {
            let mut pane = Pane::new(ProfileId::new(), name, None);
            pane.session = Some(session);
            pane.state = TabState::Connected;
            pane
        }

        fn screen(term: &SharedTerminal) -> String {
            let t = term.lock();
            let size = t.size();
            let grid = t.term().grid();
            (0..i32::from(size.rows))
                .map(|r| {
                    (0..usize::from(size.cols))
                        .map(|c| grid.screen()[Line(r)][Column(c)].c)
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        }

        #[test]
        fn events_reach_their_pane() {
            let dir = std::env::temp_dir().join(format!("fbterm-pane-test-{}", ProfileId::new()));
            std::fs::create_dir_all(&dir).unwrap();
            let (mut app, _) = App::build(Paths { config_dir: dir }, true);
            app.state_saving_enabled = false;
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let first = connected("one", shell("sleep 30", tx.clone()));
            let second = connected("two", shell("sleep 30", tx));
            let sid = second.session.as_ref().unwrap().id;
            let (a, b) = (first.id, second.id);
            let wid = window::Id::unique();
            let mut w = WindowState::new();
            w.push(two_panes(first, second));
            app.windows.clear();
            app.windows.insert(wid, w);

            let _ = app.update(Message::Session(sid, SessionEvent::Exited(Some(1))));

            let tab = &app.windows[&wid].tabs[0];
            let (one, two) = (tab.pane(a).unwrap(), tab.pane(b).unwrap());
            assert_eq!(
                one.state,
                TabState::Connected,
                "the other pane is untouched"
            );
            assert_eq!(two.state, TabState::Exited(Some(1)));
            let notice = "process exited with code 1";
            assert!(screen(two.terminal().unwrap()).contains(notice));
            assert!(!screen(one.terminal().unwrap()).contains(notice));
        }

        #[test]
        fn dirty_takes_only_visible_panes() {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let hidden = connected("busy", shell("echo hi; sleep 30", tx));
            let c = hidden.id;
            let first = Pane::new(ProfileId::new(), "a", None);
            let second = Pane::new(ProfileId::new(), "b", None);
            let (a, b) = (first.id, second.id);
            let mut w = WindowState::new();
            w.push(two_panes(first, second));
            w.push(Tab::single(hidden));
            w.active = 0;
            // Wait for the background pane's Dirty.
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match rx.try_recv() {
                    Ok((_, SessionEvent::Dirty)) => break,
                    _ if Instant::now() > deadline => panic!("no Dirty"),
                    _ => std::thread::sleep(Duration::from_millis(10)),
                }
            }
            assert!(tabs::on_dirty(&w, a));
            assert!(tabs::on_dirty(&w, b));
            assert!(!tabs::on_dirty(&w, c), "a pane of a background tab");
            let session = w.tabs[1].panes[0].session.as_ref().unwrap();
            assert!(session.take_dirty(), "its flag is left set");
        }
    }
}
