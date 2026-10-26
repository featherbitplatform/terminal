//! Session lifecycle: starting local and SSH sessions, their events and terminal input.

use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
use crate::tabs::{self, InputRoute, Pane, TabState, WindowState};
use crate::tiles::TilesMsg;

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
        let grid = self.initial_grid(wid, id);
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
        pane: PaneId,
        output: TerminalOutput,
    ) -> Task<Message> {
        match output {
            TerminalOutput::Input(bytes) => {
                let Some(pane) = self.windows.get_mut(&wid).and_then(|w| {
                    let (t, p) = w.locate(pane)?;
                    w.tabs[t].panes.get_mut(p)
                }) else {
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
                self.grids.insert(pane, grid);
                let settle = self.pane_resized(pane, grid);
                Task::batch([settle, self.find_resized(pane)])
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

/// Quiet period after a pane's last resize before its session hears of it: dragging a divider
/// or a window edge resizes on every frame, and each remote resize makes the program redraw.
const RESIZE_SETTLE: Duration = Duration::from_millis(100);

impl App {
    /// The grid a pane's new terminal starts at. A pane that was laid out keeps its grid; a new
    /// one (a new tab, or a split) gets the grid of its rectangle in the window's last known
    /// area, with the cell size of the tab's focused pane (else of any laid-out pane of the
    /// window). Before the window has laid out: `DEFAULT_GRID`.
    pub(crate) fn initial_grid(&self, wid: window::Id, pane: PaneId) -> GridSize {
        if let Some(g) = self.grids.get(&pane) {
            return *g;
        }
        let Some(window) = self.windows.get(&wid) else {
            return DEFAULT_GRID;
        };
        let Some((t, _)) = window.locate(pane) else {
            return DEFAULT_GRID;
        };
        let tab = &window.tabs[t];
        let cell = self.grids.get(&tab.focused).or_else(|| {
            window
                .tabs
                .iter()
                .flat_map(|t| &t.panes)
                .find_map(|p| self.grids.get(&p.id))
        });
        let (Some(cell), Some(area)) = (cell, self.areas.get(&wid)) else {
            return DEFAULT_GRID;
        };
        let rect = tab
            .shown_layout()
            .rects(iced::Rectangle::with_size(*area))
            .into_iter()
            .find(|(id, _)| *id == pane);
        match rect {
            Some((_, r)) => fbterm_term::grid_size_from_bounds(
                r.size(),
                iced::Size::new(cell.cell_width, cell.cell_height),
            ),
            None => DEFAULT_GRID,
        }
    }

    /// A pane's view resized its terminal to `grid`. The session gets the latest grid once no
    /// newer one came for `RESIZE_SETTLE` (or at once when a divider drag ends).
    pub(crate) fn pane_resized(&mut self, pane: PaneId, grid: GridSize) -> Task<Message> {
        self.resize_generation += 1;
        let generation = self.resize_generation;
        self.pending_resizes.insert(pane, (generation, grid));
        Task::perform(
            async { tokio::time::sleep(RESIZE_SETTLE).await },
            move |()| Message::ResizeSettled(pane, generation),
        )
    }

    /// The quiet period of a pane's resize `generation` passed.
    pub(crate) fn resize_settled(&mut self, pane: PaneId, generation: u64) {
        match self.pending_resizes.get(&pane) {
            Some((g, grid)) if *g == generation => {
                let grid = *grid;
                self.pending_resizes.remove(&pane);
                self.resize_session(pane, grid);
            }
            _ => {}
        }
    }

    /// Hand every waiting resize of `wid`'s panes to its session now.
    fn flush_resizes(&mut self, wid: window::Id) {
        let Some(window) = self.windows.get(&wid) else {
            return;
        };
        let ready: Vec<(PaneId, GridSize)> = self
            .pending_resizes
            .iter()
            .filter(|(id, _)| window.locate(**id).is_some())
            .map(|(id, (_, grid))| (*id, *grid))
            .collect();
        for (pane, grid) in ready {
            self.pending_resizes.remove(&pane);
            self.resize_session(pane, grid);
        }
    }

    fn resize_session(&self, pane: PaneId, grid: GridSize) {
        let session = self.windows.values().find_map(|w| {
            let (t, p) = w.locate(pane)?;
            w.tabs[t].panes[p].session.as_ref()
        });
        if let Some(s) = session {
            s.resize(grid);
        }
    }

    /// Panes that closed: forget their sizes.
    pub(crate) fn forget_panes(&mut self, panes: &[PaneId]) {
        for id in panes {
            self.grids.remove(id);
            self.pending_resizes.remove(id);
        }
    }

    pub(crate) fn tiles_msg(&mut self, wid: window::Id, msg: TilesMsg) -> Task<Message> {
        if let TilesMsg::Area(size) = msg {
            self.areas.insert(wid, size);
            return Task::none();
        }
        if let TilesMsg::EndDrag = msg {
            self.flush_resizes(wid);
            return Task::none();
        }
        if let TilesMsg::Focus(pane) = msg {
            return self.focus_pane(wid, pane);
        }
        let area = self.areas.get(&wid).copied();
        let Some(tab) = self
            .windows
            .get_mut(&wid)
            .and_then(WindowState::active_tab_mut)
        else {
            return Task::none();
        };
        let mut changed = false;
        match msg {
            TilesMsg::DragDivider { path, ratio } => {
                if tab.maximized.is_some() {
                    return Task::none();
                }
                let before = tab.layout.clone();
                tab.layout.set_ratio(&path, ratio);
                // Never squeeze a pane below the minimum: the divider stops there.
                let cell = self.grids.get(&tab.focused).map_or(DEFAULT_GRID, |g| *g);
                if let Some(area) = area
                    && !tab.layout.fits(
                        iced::Rectangle::with_size(area),
                        iced::Size::new(cell.cell_width, cell.cell_height),
                    )
                {
                    tab.layout = before.clone();
                }
                changed = tab.layout != before;
            }
            TilesMsg::Equalize(path) => {
                if tab.maximized.is_none() {
                    tab.layout.equalize(&path);
                    changed = true;
                }
            }
            TilesMsg::DropTile { pane, at } => {
                // Only the arrangement changes: sessions and terminals stay with their panes,
                // and each moved pane's view reports its new grid through the resize path.
                if tab.maximized.is_none() && tab.layout.move_pane(pane, at) {
                    self.save_state();
                }
                return Task::none();
            }
            TilesMsg::Focus(_) | TilesMsg::EndDrag | TilesMsg::Area(_) => {}
        }
        if changed {
            // A drag moves the divider continuously: save once it settles.
            return self.save_state_soon();
        }
        Task::none()
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
    use fbterm_term::GridSize;
    use iced::{Size, window};

    use crate::app::{App, Message};
    use crate::layout::Side;
    use crate::restore_tests::{paths_with, profile};
    use crate::tabs::{Pane, Tab, TabState, WindowState};
    use crate::tiles::TilesMsg;

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

    #[test]
    fn split_pane_starts_at_its_rect_grid() {
        let p = profile("a");
        let (mut app, _) = App::build(paths_with(std::slice::from_ref(&p)), true);
        app.state_saving_enabled = false;
        let wid = window::Id::unique();
        let first = Pane::for_profile(&p);
        let a = first.id;
        let mut w = WindowState::new();
        w.push(Tab::single(first));
        app.windows.clear();
        app.windows.insert(wid, w);
        // The lone pane filled the tiles' area and reported its grid (9x18 cells).
        let _ = app.update(Message::Tiles(wid, TilesMsg::Area(Size::new(984.0, 600.0))));
        app.grids.insert(
            a,
            GridSize {
                cols: 109,
                rows: 33,
                cell_width: 9.0,
                cell_height: 18.0,
            },
        );
        // Split by hand, as the split action will.
        let second = Pane::for_profile(&p);
        let b = second.id;
        let tab = &mut app.windows.get_mut(&wid).unwrap().tabs[0];
        tab.layout.split(a, Side::Right, b);
        tab.panes.push(second);

        let _ = app.start(wid, b);

        let tab = &app.windows[&wid].tabs[0];
        let size = tab.pane(b).unwrap().terminal().unwrap().lock().size();
        // Its rect: (984 - 4) / 2 = 490 px wide (54 columns of 9), 600 px high (33 rows of 18).
        assert_eq!(
            size,
            GridSize {
                cols: 54,
                rows: 33,
                cell_width: 9.0,
                cell_height: 18.0,
            }
        );
    }

    #[cfg(unix)]
    mod unix {
        use std::time::{Duration, Instant};

        use fbterm_config::{LocalProfile, Paths, ProfileId};
        use fbterm_session::{SessionEvent, SessionHandle};
        use fbterm_term::{GridSize, SharedTerminal, TerminalOutput};
        use feathercritty::index::{Column, Line};
        use iced::window;

        use super::two_panes;
        use crate::app::{App, DEFAULT_GRID, Message};
        use crate::keys::Action;
        use crate::layout::PaneId;
        use crate::tabs::{self, Pane, Tab, TabState, WindowState};
        use crate::tiles::TilesMsg;

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

        fn app_with(first: Pane, second: Pane) -> (App, window::Id) {
            let dir = std::env::temp_dir().join(format!("fbterm-pane-test-{}", ProfileId::new()));
            std::fs::create_dir_all(&dir).unwrap();
            let (mut app, _) = App::build(Paths { config_dir: dir }, true);
            app.state_saving_enabled = false;
            let wid = window::Id::unique();
            let mut w = WindowState::new();
            w.push(two_panes(first, second));
            app.windows.clear();
            app.windows.insert(wid, w);
            (app, wid)
        }

        fn size_of(app: &App, wid: window::Id, pane: PaneId) -> GridSize {
            let tab = &app.windows[&wid].tabs[0];
            tab.pane(pane).unwrap().terminal().unwrap().lock().size()
        }

        fn grid(cols: u16, rows: u16) -> GridSize {
            GridSize {
                cols,
                rows,
                ..DEFAULT_GRID
            }
        }

        fn wait_for(term: &SharedTerminal, text: &str) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !screen(term).contains(text) {
                assert!(
                    Instant::now() < deadline,
                    "no {text:?} in:\n{}",
                    screen(term)
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        #[test]
        fn resize_goes_to_its_pane() {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let first = connected("one", shell("sleep 30", tx.clone()));
            let second = connected("two", shell("sleep 30", tx));
            let (a, b) = (first.id, second.id);
            let (mut app, wid) = app_with(first, second);
            let g = grid(100, 30);

            let _ = app.update(Message::Terminal(wid, b, TerminalOutput::Resized(g)));
            let generation = app.pending_resizes[&b].0;
            let _ = app.update(Message::ResizeSettled(b, generation));

            assert_eq!(size_of(&app, wid, b), g);
            assert_eq!(
                size_of(&app, wid, a),
                DEFAULT_GRID,
                "the other pane is untouched"
            );
            assert_eq!(app.grids.get(&b), Some(&g));
            assert!(!app.grids.contains_key(&a));
        }

        #[test]
        fn divider_drag_debounces_session_resize() {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let script = "trap 'stty size' WINCH; echo ready; while :; do sleep 0.05; done";
            let first = connected("one", shell(script, tx.clone()));
            let second = connected("two", shell("sleep 30", tx));
            let a = first.id;
            let term = first.terminal().unwrap().clone();
            let (mut app, wid) = app_with(first, second);
            wait_for(&term, "ready");

            // Ten resizes within the quiet period: the session hears none of them yet.
            let mut generations = Vec::new();
            for i in 0..10 {
                let g = grid(90 + i, 30);
                let _ = app.update(Message::Terminal(wid, a, TerminalOutput::Resized(g)));
                generations.push(app.pending_resizes[&a].0);
                assert_eq!(size_of(&app, wid, a), DEFAULT_GRID);
            }
            // The timers of the older resizes find a newer one and do nothing.
            for &stale in &generations[..9] {
                let _ = app.update(Message::ResizeSettled(a, stale));
                assert_eq!(size_of(&app, wid, a), DEFAULT_GRID);
            }
            // The last one settles: one session resize, with the last grid.
            let _ = app.update(Message::ResizeSettled(a, generations[9]));
            assert_eq!(size_of(&app, wid, a), grid(99, 30));
            assert!(app.pending_resizes.is_empty());
            wait_for(&term, "30 99");
            for cols in 90..99 {
                assert!(!screen(&term).contains(&format!("30 {cols}")), "{cols}");
            }

            // Releasing the divider flushes at once.
            for i in 0..5 {
                let g = grid(70 + i, 25);
                let _ = app.update(Message::Terminal(wid, a, TerminalOutput::Resized(g)));
            }
            let last = app.pending_resizes[&a].0;
            assert_eq!(size_of(&app, wid, a), grid(99, 30));
            let _ = app.update(Message::Tiles(wid, TilesMsg::EndDrag));
            assert_eq!(size_of(&app, wid, a), grid(74, 25));
            assert!(app.pending_resizes.is_empty());
            let _ = app.update(Message::ResizeSettled(a, last));
            assert_eq!(size_of(&app, wid, a), grid(74, 25));
            wait_for(&term, "25 74");
        }

        /// Wait until every session in `ids` has reported a `Dirty`, so each has its flag set.
        fn wait_for_dirty(
            rx: &mut tokio::sync::mpsc::UnboundedReceiver<(
                fbterm_session::SessionId,
                SessionEvent,
            )>,
            ids: &[fbterm_session::SessionId],
        ) {
            let mut seen = std::collections::HashSet::new();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !ids.iter().all(|id| seen.contains(id)) {
                match rx.try_recv() {
                    Ok((id, SessionEvent::Dirty)) => {
                        seen.insert(id);
                    }
                    Ok(_) => {}
                    Err(_) if Instant::now() > deadline => panic!("no Dirty from every session"),
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        }

        fn flag_pending(w: &WindowState, tab: usize, pane: PaneId) -> bool {
            w.tabs[tab]
                .pane(pane)
                .and_then(|p| p.session.as_ref())
                .unwrap()
                .take_dirty()
        }

        #[test]
        fn dirty_takes_only_visible_panes() {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let busy = |name, tx: &Sink| connected(name, shell("echo hi; sleep 30", tx.clone()));
            let (first, second, hidden) = (busy("a", &tx), busy("b", &tx), busy("c", &tx));
            let (a, b, c) = (first.id, second.id, hidden.id);
            let sessions: Vec<_> = [&first, &second, &hidden]
                .iter()
                .map(|p| p.session.as_ref().unwrap().id)
                .collect();
            let mut w = WindowState::new();
            w.push(two_panes(first, second));
            w.push(Tab::single(hidden));
            w.active = 0;
            wait_for_dirty(&mut rx, &sessions);

            // A pane hidden by a maximized one leaves its flag pending, like a background tab.
            w.tabs[0].maximized = Some(a);
            assert!(!tabs::on_dirty(&w, b), "a pane the maximized one hides");
            assert!(flag_pending(&w, 0, b), "its flag is left set");
            assert!(tabs::on_dirty(&w, a));
            assert!(!flag_pending(&w, 0, a), "the shown pane took its flag");

            // Un-maximized, every pane of the tab is visible.
            w.tabs[0].maximized = None;
            assert!(tabs::on_dirty(&w, a));
            assert!(tabs::on_dirty(&w, b));
            assert!(!tabs::on_dirty(&w, c), "a pane of a background tab");
            assert!(flag_pending(&w, 1, c), "its flag is left set");
        }

        /// Zooming changes the font the pane's view is drawn with; the view then measures a
        /// bigger cell on its next layout and event (the redraw every message causes), and the
        /// smaller grid it reports reaches the session through the resize debounce.
        #[test]
        fn zoom_resizes_the_panes_grid_and_session() {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let script = "trap 'stty size' WINCH; echo ready; while :; do sleep 0.05; done";
            let first = connected("one", shell(script, tx.clone()));
            let second = connected("two", shell("sleep 30", tx));
            let a = first.id;
            let term = first.terminal().unwrap().clone();
            let (mut app, wid) = app_with(first, second);
            wait_for(&term, "ready");
            // The pane's view fills 800 x 400 px with 8 x 16 px cells.
            let bounds = iced::Size::new(800.0, 400.0);
            let cell = |font: f32| iced::Size::new(font * 8.0 / 13.0, (font * 1.2).ceil());
            let font = |app: &App| {
                let t = app.windows[&wid].active_tab().unwrap();
                app.pane_style(wid, t, t.pane(a).unwrap()).font_size
            };
            let before = fbterm_term::grid_size_from_bounds(bounds, cell(font(&app)));

            let _ = app.update(Message::Shortcut(wid, Action::ZoomIn));
            let zoomed = font(&app);
            assert_eq!(zoomed, app.settings.font_size + 1.0);
            // What the view reports after measuring the bigger cell.
            let after = fbterm_term::grid_size_from_bounds(bounds, cell(zoomed));
            assert!(
                after.cols < before.cols && after.rows < before.rows,
                "{before:?} -> {after:?}"
            );
            let _ = app.update(Message::Terminal(wid, a, TerminalOutput::Resized(after)));
            let generation = app.pending_resizes[&a].0;
            let _ = app.update(Message::ResizeSettled(a, generation));
            assert_eq!(size_of(&app, wid, a), after);
            wait_for(&term, &format!("{} {}", after.rows, after.cols));
        }

        #[test]
        fn drop_tile_moves_without_reconnect() {
            use crate::layout::{Drop, Layout, Side};
            use feathercritty::grid::Dimensions;
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let first = connected("one", shell("seq 1 200; echo ready; sleep 30", tx.clone()));
            let second = connected("two", shell("echo also; sleep 30", tx));
            let (a, b) = (first.id, second.id);
            let sessions = [
                first.session.as_ref().unwrap().id,
                second.session.as_ref().unwrap().id,
            ];
            let term = first.terminal().unwrap().clone();
            let (mut app, wid) = app_with(first, second);
            wait_for(&term, "ready");
            let history = term.lock().term().grid().history_size();
            assert!(history > 0, "the first pane has scrollback");

            let _ = app.update(Message::Tiles(
                wid,
                TilesMsg::DropTile {
                    pane: a,
                    at: Drop::Tile(b, Side::Bottom),
                },
            ));

            let tab = &app.windows[&wid].tabs[0];
            assert_eq!(tab.layout.panes(), vec![b, a], "a now sits under b");
            assert!(matches!(
                tab.layout,
                Layout::Split {
                    axis: crate::layout::Axis::Column,
                    ..
                }
            ));
            let ids: Vec<_> = [a, b]
                .iter()
                .map(|p| tab.pane(*p).unwrap().session.as_ref().unwrap().id)
                .collect();
            assert_eq!(ids, sessions, "the same sessions, not new ones");
            assert!(
                [a, b]
                    .iter()
                    .all(|p| tab.pane(*p).unwrap().state == TabState::Connected)
            );
            let moved = tab.pane(a).unwrap().terminal().unwrap();
            assert!(std::sync::Arc::ptr_eq(moved, &term), "the same terminal");
            assert_eq!(moved.lock().term().grid().history_size(), history);
            assert!(screen(moved).contains("ready"));
        }

        #[test]
        fn unmaximizing_takes_the_redraws_left_pending() {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let first = connected("one", shell("echo hi; sleep 30", tx.clone()));
            let second = connected("two", shell("echo hi; sleep 30", tx));
            let (a, b) = (first.id, second.id);
            let sessions = [
                first.session.as_ref().unwrap().id,
                second.session.as_ref().unwrap().id,
            ];
            let (mut app, wid) = app_with(first, second);
            wait_for_dirty(&mut rx, &sessions);

            let toggle = |app: &mut App| {
                let _ = app.update(Message::Shortcut(wid, Action::MaximizePane));
            };
            toggle(&mut app);
            assert_eq!(app.windows[&wid].tabs[0].maximized, Some(a));
            // Maximizing took the shown pane's flag and left the hidden one's.
            toggle(&mut app);
            assert_eq!(app.windows[&wid].tabs[0].maximized, None);
            assert!(
                !flag_pending(&app.windows[&wid], 0, b),
                "showing the pane took its pending redraw"
            );
        }
    }
}
