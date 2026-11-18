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
use crate::tabs::{self, InputRoute, Pane, Tab, TabState, WindowState};
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
    /// A terminal with the settings applied, in Graphite's colors; [`App::start`] gives it its
    /// pane's scheme.
    pub(crate) fn new_terminal(&self, grid: GridSize) -> SharedTerminal {
        let mut term = Terminal::new(grid, self.settings.scrollback_lines);
        term.set_kitty_keyboard(self.settings.kitty_keyboard);
        term.set_images(self.settings.inline_images, self.settings.scrollback_images);
        term.set_grapheme_clusters(self.settings.grapheme_clusters);
        term.set_history_compression(self.settings.history_compression_keep());
        term.reset_grapheme_clusters();
        self.style.scheme.apply_to(&mut term);
        Arc::new(Mutex::new(term))
    }

    /// (Re)start a pane's session: spawn a local program, or dial an SSH profile.
    pub(crate) fn start(&mut self, wid: window::Id, id: PaneId) -> Task<Message> {
        let (grid, provisional) = self.initial_grid(wid, id);
        // Only a pane the window really draws can learn its cells soon enough for waiting to pay off:
        // one behind another tab, or behind a maximized sibling, lays out no sooner than that changes.
        // So note who is drawn now...
        let shown_now = self
            .windows
            .get(&wid)
            .and_then(|w| w.active_tab())
            .is_some_and(|t| pane_is_drawn(t, id));
        // ...and stop whatever the panes this window has just hidden from waiting: starting a pane
        // takes the window's attention from them, and `--perf-tabs` or the debug openers start a run
        // of tabs in one update, so without this every pane but the last would buffer up to the hold's
        // cap before parsing it at the same provisional cells it would have parsed at anyway.
        self.release_hidden_holds(wid);
        tracing::info!(pane = id.0, "starting pane");
        let fresh_terminal = self.new_terminal(grid);
        let events = self.events.clone();
        let profile: Option<Profile> = self
            .windows
            .get(&wid)
            .and_then(|w| w.locate(id).map(|(t, p)| w.tabs[t].panes[p].profile))
            .and_then(|p| self.profiles.get(p).cloned());
        let Some(scheme) = self
            .windows
            .get(&wid)
            .and_then(|w| w.locate(id).map(|(t, p)| &w.tabs[t].panes[p]))
            .map(|pane| self.scheme_colors(pane))
        else {
            return Task::none();
        };
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
                {
                    let mut term = t.lock();
                    term.feed(tabs::RESTART_RESET);
                    // A new program never inherits mode 2027 from the one before.
                    term.reset_grapheme_clusters();
                }
                (t, false)
            }
            None => (fresh_terminal, true),
        };
        // A pane whose window has not laid out yet starts at a guessed grid, and its program may
        // already be drawing pictures sized for cells measured with an assumed scale. Hold what it
        // writes until its own view reports the grid the window really has (see
        // `Terminal::hold_until_sized`); `terminal_output` and `connected` deliver whatever that
        // release parses. Three things have to be true: this is a new terminal (a restart continues in
        // one that was laid out long ago), its grid is a guess (`initial_grid` says so), and the window
        // draws this pane now - the only panes whose views run before their programs get going.
        if fresh && provisional && shown_now {
            terminal.lock().hold_until_sized();
        }
        // New or continued, local or remote: the pane's scheme as it is now.
        scheme.apply_to(&mut terminal.lock());
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
        // Output this pane held before its session existed - the window had not laid out when the
        // dial finished, so its view's resize parsed those bytes on the app's thread - queued its
        // answers with nobody to route them. This handle's router is what delivers them; the resize
        // above may have released the hold just now, which is why this comes after it and before the
        // pane's own notices, so everything reaches the program in the order it was produced. With
        // nothing queued this does nothing at all.
        handle.deliver_events();
        let pane = &mut window.tabs[t].panes[p];
        let profile = pane.profile;
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
        // The profile's first connected shell starts its "with the shell" tunnels.
        self.sync_shell_tunnels(profile);
        // And SFTP is checked on its connection, for the tab bar's files button.
        self.check_sftp(profile);
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
        let profile = pane.profile;
        if let Some(text) = tabs::apply_event(pane, &event) {
            write_notice(pane, &text, false);
        }
        if matches!(
            event,
            SessionEvent::Exited(_) | SessionEvent::Disconnected(_)
        ) {
            // The profile's last shell ending stops its "with the shell" tunnels.
            self.sync_shell_tunnels(profile);
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
                        let (id, profile) = (pane.id, pane.profile);
                        let task = self.start(wid, id);
                        // Its tunnels come back once it connects (`connected`).
                        self.sync_shell_tunnels(profile);
                        task
                    }
                    InputRoute::Ignore => Task::none(),
                }
            }
            TerminalOutput::Resized(grid) => {
                // A new pane held its output until this arrived (see `App::start`): the view that
                // reports this grid is what released it, and a resize on this thread has no feed
                // after it, so ask for whatever that parse queued before anything else acts on it.
                self.deliver_pane_events(pane);
                // New columns reflow the history (a lines-only resize does not); the first
                // layout of a pane has nothing earlier to compare with.
                let reflowed = self
                    .grids
                    .get(&pane)
                    .is_some_and(|old| old.cols != grid.cols);
                self.grids.insert(pane, grid);
                let settle = self.pane_resized(pane, grid, reflowed);
                Task::batch([settle, self.find_resized(pane)])
            }
            // The widget already put the text on the clipboard.
            TerminalOutput::Copy(_) => Task::none(),
            TerminalOutput::ContextMenu { at, can_copy } => {
                self.open_context_menu(wid, pane, at, can_copy)
            }
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
    /// The grid a pane's new terminal starts at, and whether that guess is **provisional**: the
    /// flag is set when nothing this window has laid out could tell us better, so the cells the
    /// program will draw in are still unknown and the pane should hold its output until its view
    /// reports a real grid (see [`fbterm_term::Terminal::hold_until_sized`]).
    ///
    /// A pane that was laid out keeps its grid; a new one (a new tab, or a split) gets the grid of
    /// its rectangle in the window's last known area, with the cell size of the tab's focused pane
    /// (else of any laid-out pane of the window), at the window's physical scale. A pane only has a
    /// laid-out grid, and an area, once its view has reported one - which it does not do until the
    /// window reports its scale - so reaching for the default means the scale is unknown too.
    pub(crate) fn initial_grid(&self, wid: window::Id, pane: PaneId) -> (GridSize, bool) {
        if let Some(g) = self.grids.get(&pane) {
            return (*g, false);
        }
        let Some(window) = self.windows.get(&wid) else {
            return (DEFAULT_GRID, true);
        };
        let Some((t, _)) = window.locate(pane) else {
            return (DEFAULT_GRID, true);
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
            return (DEFAULT_GRID, true);
        };
        let rect = tab
            .shown_layout()
            .rects(iced::Rectangle::with_size(*area))
            .into_iter()
            .find(|(id, _)| *id == pane);
        match rect {
            // Real cells from a laid-out sibling, in a window that has reported its scale.
            Some((_, r)) => (
                fbterm_term::grid_size_from_bounds(
                    r.size(),
                    iced::Size::new(cell.cell_width, cell.cell_height),
                    self.physical_scale(wid),
                ),
                false,
            ),
            None => (DEFAULT_GRID, true),
        }
    }

    /// A pane's view resized its terminal to `grid` (`reflowed`: its columns changed). The
    /// session gets the latest grid once no newer one came for `RESIZE_SETTLE` (or at once when
    /// a divider drag ends).
    pub(crate) fn pane_resized(
        &mut self,
        pane: PaneId,
        grid: GridSize,
        reflowed: bool,
    ) -> Task<Message> {
        self.resize_generation += 1;
        let generation = self.resize_generation;
        let reflowed = reflowed || self.pending_resizes.get(&pane).is_some_and(|p| p.2);
        self.pending_resizes
            .insert(pane, (generation, grid, reflowed));
        Task::perform(
            async { tokio::time::sleep(RESIZE_SETTLE).await },
            move |()| Message::ResizeSettled(pane, generation),
        )
    }

    /// The quiet period of a pane's resize `generation` passed.
    pub(crate) fn resize_settled(&mut self, pane: PaneId, generation: u64) -> Task<Message> {
        match self.pending_resizes.get(&pane) {
            Some((g, grid, reflowed)) if *g == generation => {
                let (grid, reflowed) = (*grid, *reflowed);
                self.pending_resizes.remove(&pane);
                self.resize_session(pane, grid);
                self.release_after_reflow(reflowed.then_some(pane))
            }
            _ => Task::none(),
        }
    }

    /// Hand every waiting resize of `wid`'s panes to its session now.
    fn flush_resizes(&mut self, wid: window::Id) -> Task<Message> {
        let Some(window) = self.windows.get(&wid) else {
            return Task::none();
        };
        let ready: Vec<(PaneId, GridSize, bool)> = self
            .pending_resizes
            .iter()
            .filter(|(id, _)| window.locate(**id).is_some())
            .map(|(id, (_, grid, reflowed))| (*id, *grid, *reflowed))
            .collect();
        let mut reflowed = Vec::new();
        for (pane, grid, r) in ready {
            self.pending_resizes.remove(&pane);
            self.resize_session(pane, grid);
            if r {
                reflowed.push(pane);
            }
        }
        self.release_after_reflow(reflowed)
    }

    /// Settled resizes reflowed `panes`: a terminal with history compression on rebuilt its
    /// history uncompressed, and the worker compresses it again. Once it drains, the memory that
    /// frees goes back to the OS (one release waiting at most, however many steps a drag had).
    /// Only the user's own resizes get here: programs cannot resize the terminal. Each such
    /// terminal is queued on the worker here, since a silent program would never queue it.
    fn release_after_reflow(&self, panes: impl IntoIterator<Item = PaneId>) -> Task<Message> {
        let mut compressed = false;
        for pane in panes {
            let term = self.windows.values().find_map(|w| {
                let (t, p) = w.locate(pane)?;
                w.tabs[t].panes[p].terminal()
            });
            let Some(term) = term else { continue };
            let (on, jobs) = {
                let t = term.lock();
                (t.history_compression().is_some(), t.has_history_jobs())
            };
            if jobs {
                fbterm_term::history_worker::enqueue(term);
            }
            compressed |= on;
        }
        if compressed {
            crate::settings_window::release_when_worker_idle()
        } else {
            Task::none()
        }
    }

    /// The live session of a pane, if it has one: a pane showing only its placeholder terminal does
    /// not, and neither does one whose profile could not start.
    fn session_of(&self, pane: PaneId) -> Option<&SessionHandle> {
        self.windows.values().find_map(|w| {
            let (t, p) = w.locate(pane)?;
            w.tabs[t].panes[p].session.as_ref()
        })
    }

    fn resize_session(&self, pane: PaneId, grid: GridSize) {
        if let Some(s) = self.session_of(pane) {
            s.resize(grid);
        }
    }

    /// Deliver what a pane's terminal queued with nobody to route it. A new pane holds its output
    /// until its view reports the grid its window really has (see `App::start`), and that resize
    /// happens on this thread, where no feed follows it - so whatever that parse produced (its
    /// replies, its title, its pictures) has to be asked for by hand. Never while holding the
    /// terminal's lock: delivery hands pictures to the image store, which locks the terminal.
    fn deliver_pane_events(&self, pane: PaneId) {
        if let Some(s) = self.session_of(pane) {
            s.deliver_events();
        }
    }

    /// Parse whatever the panes of `wid` that its window cannot show have held for their first real
    /// grid. A hidden pane's view runs no sooner than it is shown again, so waiting for it only buffers
    /// what its program writes in the meantime - up to the hold's cap - before parsing it at the same
    /// provisional cells it would have been parsed at anyway (see `App::start`, which releases them as
    /// it takes the window's attention). A pane with nothing held is untouched.
    fn release_hidden_holds(&self, wid: window::Id) {
        let Some(window) = self.windows.get(&wid) else {
            return;
        };
        for (i, tab) in window.tabs.iter().enumerate() {
            for pane in &tab.panes {
                // The shown tab's panes lay out on the next draw; anything else waits for the user.
                if i == window.active && pane_is_drawn(tab, pane.id) {
                    continue;
                }
                if let Some(term) = pane.terminal() {
                    term.lock().sized();
                }
            }
        }
    }

    /// Set a divider's ratio unless a pane would end up below the minimum size: the old
    /// ratio stays then. Returns whether the layout changed.
    fn set_ratio_if_fits(
        tab: &mut crate::tabs::Tab,
        set: impl FnOnce(&mut crate::layout::Layout),
        area: Option<iced::Size>,
        cell: GridSize,
    ) -> bool {
        let before = tab.layout.clone();
        set(&mut tab.layout);
        if let Some(area) = area
            && !tab.layout.fits(
                iced::Rectangle::with_size(area),
                iced::Size::new(cell.cell_width, cell.cell_height),
            )
        {
            tab.layout = before.clone();
        }
        tab.layout != before
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
            return self.flush_resizes(wid);
        }
        if let TilesMsg::Focus(pane) = msg {
            return self.focus_pane(wid, pane);
        }
        if let TilesMsg::DropTile { pane, at } = msg {
            // Only the arrangement changes: sessions and terminals stay with their panes, and
            // each moved pane's view reports its new grid through the resize path. Refused
            // while maximized and when a pane would end up below the minimum size.
            let maximized = self
                .windows
                .get(&wid)
                .and_then(WindowState::active_tab)
                .is_none_or(|t| t.maximized.is_some());
            if maximized {
                return Task::none();
            }
            if let Some(moved) = self.moved_layout(wid, pane, at)
                && let Some(tab) = self
                    .windows
                    .get_mut(&wid)
                    .and_then(WindowState::active_tab_mut)
                && tab.layout.contains(pane)
            {
                tab.layout = moved;
                self.save_state();
            }
            return Task::none();
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
                changed = Self::set_ratio_if_fits(
                    tab,
                    |l| l.set_ratio(&path, ratio),
                    area,
                    self.grids.get(&tab.focused).map_or(DEFAULT_GRID, |g| *g),
                );
            }
            TilesMsg::Equalize(path) => {
                if tab.maximized.is_none() {
                    changed = Self::set_ratio_if_fits(
                        tab,
                        |l| l.equalize(&path),
                        area,
                        self.grids.get(&tab.focused).map_or(DEFAULT_GRID, |g| *g),
                    );
                }
            }
            TilesMsg::Focus(_)
            | TilesMsg::EndDrag
            | TilesMsg::Area(_)
            | TilesMsg::DropTile { .. } => {}
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

/// Whether this pane gets a view on screen while its tab is the one its window shows: behind a
/// maximized sibling there is nothing to lay out until that is undone (see `Tab::shown_layout`).
fn pane_is_drawn(tab: &Tab, pane: PaneId) -> bool {
    tab.pane(pane).is_some() && tab.maximized.is_none_or(|m| m == pane)
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
pub(crate) mod pane_tests {
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

    /// Everything a terminal's screen shows, to check that parsed output landed on it.
    fn grid_text(terminal: &fbterm_term::SharedTerminal) -> String {
        use feathercritty::index::{Column, Line};
        let term = terminal.lock();
        let (cols, rows) = (term.size().cols as usize, term.size().rows as i32);
        let grid = term.term().grid();
        let mut out = String::new();
        for row in 0..rows {
            for col in 0..cols {
                out.push(grid.screen()[Line(row)][Column(col)].c);
            }
            out.push('\n');
        }
        out
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
    fn page_tab_opens_in_the_last_terminal_window_while_another_window_has_focus() {
        let (mut app, _) = App::build(paths_with(&[]), true);
        app.state_saving_enabled = false;
        let (a, b) = (window::Id::unique(), window::Id::unique());
        app.windows.clear();
        app.windows.insert(a, WindowState::new());
        app.windows.insert(b, WindowState::new());
        // The Color schemes window has focus; `b` was the terminal window focused last.
        app.focused = Some(window::Id::unique());
        app.last_terminal = Some(b);
        let (wid, _) = app.show_page(crate::tabs::Page::Settings);
        assert_eq!(wid, b);
        assert_eq!(app.windows[&b].tabs.len(), 1);
        assert!(app.windows[&a].tabs.is_empty());
    }

    #[test]
    fn page_tab_refuses_splits() {
        let p = profile("a");
        let (mut app, _) = App::build(paths_with(std::slice::from_ref(&p)), true);
        app.state_saving_enabled = false;
        let wid = window::Id::unique();
        let mut w = WindowState::new();
        w.push(Tab::page(crate::tabs::Page::Settings));
        app.windows.clear();
        app.windows.insert(wid, w);
        let _ = app.split_pane(wid, Side::Right);
        let _ = app.open_in_split(wid, p.id, Side::Right);
        assert_eq!(app.windows[&wid].tabs[0].panes.len(), 1);
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
                scale: 1.0,
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
                scale: 1.0,
            }
        );
    }

    /// A pane whose window has not laid out waits for its cells only while it is the one the window
    /// shows: a pane behind another tab lays out no sooner than that tab is shown, so its output is
    /// parsed at once, as it was before this hold existed. `--perf-tabs` and the debug openers rely on
    /// that - they start a run of tabs in one update, and only the last one is ever shown first.
    #[test]
    fn a_pane_in_a_background_tab_holds_nothing() {
        let p = profile("a");
        let (mut app, _) = App::build(paths_with(std::slice::from_ref(&p)), true);
        app.state_saving_enabled = false;
        let (pa, pb) = (Pane::for_profile(&p), Pane::for_profile(&p));
        let (a, b) = (pa.id, pb.id);
        let wid = window::Id::unique();
        let mut w = WindowState::new();
        w.push(Tab::single(pa)); // `push` makes it the shown tab...
        w.push(Tab::single(pb)); // ...and this one replaces it: `a` starts already hidden.
        app.windows.clear();
        app.windows.insert(wid, w);

        let _ = app.start(wid, a);
        let _ = app.start(wid, b);
        let term = |id| {
            app.windows[&wid]
                .tabs
                .iter()
                .find_map(|t| t.pane(id))
                .unwrap()
                .terminal()
                .cloned()
                .unwrap()
        };

        assert!(
            !term(a).lock().holding(),
            "a pane behind the shown tab parses at once"
        );
        assert!(
            term(b).lock().holding(),
            "the shown pane waits for the cells its window really has"
        );
        // The style is what tells the view there is nothing honest to report yet: 0.0 until the
        // window says otherwise (`App::physical_scale_if_known`).
        let tab = &app.windows[&wid].active_tab().unwrap();
        assert_eq!(
            app.pane_style(wid, tab, tab.pane(b).unwrap()).scale,
            0.0,
            "no display reported"
        );
        let _ = app.update(Message::WindowScale(wid, 2.0));
        let tab = &app.windows[&wid].active_tab().unwrap();
        assert_eq!(
            app.pane_style(wid, tab, tab.pane(b).unwrap()).scale,
            app.physical_scale(wid),
            "and the pane's cells follow the display it reports"
        );
    }

    /// A pane started into a tab the window does not show has no view running for it at all, so it
    /// parses at once: that is what `FBTERM_SPLITS` does, replacing an existing tab's panes and
    /// starting each one while another tab is shown. Nothing would release a hold armed in that
    /// situation either - the sweep over hidden panes runs when a pane starts, before its own hold
    /// would be armed, not after it.
    #[test]
    fn a_pane_started_into_a_hidden_tab_holds_nothing() {
        let p = profile("a");
        let (mut app, _) = App::build(paths_with(std::slice::from_ref(&p)), true);
        app.state_saving_enabled = false;
        let (hidden_pane, shown_pane) = (Pane::for_profile(&p), Pane::for_profile(&p));
        let (hidden, shown) = (hidden_pane.id, shown_pane.id);
        let wid = window::Id::unique();
        let mut w = WindowState::new();
        w.push(Tab::single(hidden_pane)); // its tab is behind the next one from here on
        w.push(Tab::single(shown_pane));
        app.windows.clear();
        app.windows.insert(wid, w);

        let term_of = |app: &App, id| {
            app.windows[&wid]
                .tabs
                .iter()
                .find_map(|t| t.pane(id))
                .unwrap()
                .terminal()
                .cloned()
                .unwrap()
        };

        let _ = app.start(wid, hidden);
        assert!(
            !term_of(&app, hidden).lock().holding(),
            "a pane whose tab is not shown parses at once"
        );
        let _ = app.start(wid, shown);
        assert!(
            term_of(&app, shown).lock().holding(),
            "the shown pane still waits for the cells its window really has"
        );
    }

    /// Starting a pane takes the window's attention from the panes behind it: whatever they were
    /// holding is parsed then, at the grid they have, instead of buffering until they are shown (up to
    /// the hold's cap) and parsing at the same provisional cells anyway.
    #[test]
    fn starting_a_new_tab_stops_what_a_hidden_pane_was_holding() {
        let p = profile("a");
        let (mut app, _) = App::build(paths_with(std::slice::from_ref(&p)), true);
        app.state_saving_enabled = false;
        let pa = Pane::for_profile(&p);
        let a = pa.id;
        let wid = window::Id::unique();
        let mut w = WindowState::new();
        w.push(Tab::single(pa));
        app.windows.clear();
        app.windows.insert(wid, w);

        let _ = app.start(wid, a);
        let hidden = app.windows[&wid].tabs[0]
            .pane(a)
            .unwrap()
            .terminal()
            .cloned()
            .unwrap();
        assert!(hidden.lock().holding(), "the shown pane waits");
        // The notice `start` wrote for this failed-to-spawn profile is in there too: while the pane
        // holds, everything it is fed waits, even the app's own text.
        let before = hidden.lock().held_bytes();
        let written: &[u8] = b"written while waiting\r\n";
        hidden.lock().feed(written);
        assert_eq!(
            hidden.lock().held_bytes(),
            before + written.len(),
            "and it buffers what arrives"
        );

        // A new tab shows, and its pane starts: `a` is behind it now and will not lay out until the
        // user goes back to it.
        let pb = Pane::for_profile(&p);
        let b = pb.id;
        app.windows.get_mut(&wid).unwrap().push(Tab::single(pb));
        let _ = app.start(wid, b);

        assert!(
            !hidden.lock().holding(),
            "the pane this window stopped showing stops waiting"
        );
        assert_eq!(hidden.lock().held_bytes(), 0, "what it held is parsed");
        assert!(
            grid_text(&hidden).contains("written while waiting"),
            "parsed onto its grid, not dropped"
        );
        let shown = app.windows[&wid]
            .active_tab()
            .unwrap()
            .pane(b)
            .unwrap()
            .terminal()
            .cloned()
            .unwrap();
        assert!(
            shown.lock().holding(),
            "the new shown pane waits for its own cells"
        );
    }

    /// An SSH pane whose dial outlived its window's first layout: the view's resize parses what the
    /// pane held, on the app's thread, and its answer to the program is queued with no session
    /// installed yet to route it. Installing the handle has to deliver it, or a program blocking on
    /// the reply waits for output that may never come.
    #[test]
    fn held_replies_reach_a_session_that_connects_after_the_release() {
        use fbterm_config::{AuthMethod, ProfileKind, SshProfile};
        use fbterm_session::SessionHandle;
        use fbterm_term::{Terminal, TerminalOutput};

        let ssh = fbterm_config::Profile {
            icon: None,
            id: fbterm_config::ProfileId::new(),
            name: "remote".into(),
            group: None,
            accent: None,
            accent_style: Default::default(),
            color_scheme: None,
            kind: ProfileKind::Ssh(SshProfile {
                host: "127.0.0.1".into(),
                port: 1,
                user: "sample".into(),
                auth: AuthMethod::Agent,
                jump: vec![],
                tunnels: Vec::new(),
            }),
        };
        let (mut app, _) = App::build(paths_with(std::slice::from_ref(&ssh)), true);
        app.state_saving_enabled = false;
        let pane = Pane::for_profile(&ssh);
        let id = pane.id;
        let wid = window::Id::unique();
        let mut w = WindowState::new();
        w.push(Tab::single(pane));
        app.windows.clear();
        app.windows.insert(wid, w);

        // The dial is a task this fixture never runs, so the pane stays `Connecting` with its terminal
        // and no session - exactly while a pooled connection would already be opening its shell.
        let _ = app.start(wid, id);
        let term = app.windows[&wid].tabs[0]
            .pane(id)
            .unwrap()
            .terminal()
            .cloned()
            .unwrap();
        assert!(term.lock().holding(), "the pane waits for its first grid");
        assert!(
            app.windows[&wid].tabs[0]
                .pane(id)
                .unwrap()
                .session
                .is_none()
        );

        // What the remote program prints as its shell starts: a device attributes query.
        term.lock().feed(b"\x1b[c");

        // The window reports its display, the view lays out and releases what waited. A resize has no
        // feed after it, and this pane has no session to ask for one.
        let real = GridSize {
            cols: 100,
            rows: 30,
            cell_width: 9.0,
            cell_height: 18.0,
            scale: 2.0,
        };
        term.lock().resize(real);
        let _ = app.update(Message::Terminal(wid, id, TerminalOutput::Resized(real)));

        // The dial lands now. What the released query produced has to reach this handle's program.
        let (handle, mut input) = SessionHandle::recording(term.clone());
        let _ = app.update(Message::Connected(
            id,
            crate::app::Delivered::new(handle),
            crate::app::DEFAULT_GRID,
        ));

        // What a terminal answers that query with, at the grid this pane really has.
        let mut probe = Terminal::new(real, 1);
        probe.feed(b"\x1b[c");
        let expected: Vec<u8> = probe
            .take_events()
            .into_iter()
            .filter_map(|e| match e {
                fbterm_term::TermEvent::PtyWrite(b) => Some(b),
                _ => None,
            })
            .flatten()
            .collect();
        assert!(!expected.is_empty(), "the fixture's query is answered");
        let written: Vec<u8> = input.inputs().concat();
        assert_eq!(
            written, expected,
            "the released query was not answered to the session that connected after it"
        );
    }

    #[cfg(unix)]
    pub(crate) mod unix {
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

        pub(crate) type Sink = tokio::sync::mpsc::UnboundedSender<(
            fbterm_session::SessionId,
            fbterm_session::SessionEvent,
        )>;

        pub(crate) fn shell(script: &str, tx: Sink) -> SessionHandle {
            let local = LocalProfile {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                cwd: None,
                env: Default::default(),
            };
            fbterm_session::spawn_local(&local, crate::app::DEFAULT_GRID, 100, tx).expect("spawn")
        }

        pub(crate) fn connected(name: &str, session: SessionHandle) -> Pane {
            let mut pane = Pane::new(ProfileId::new(), name, None);
            pane.session = Some(session);
            pane.state = TabState::Connected;
            pane
        }

        pub(crate) fn screen(term: &SharedTerminal) -> String {
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

        /// A pane whose window has never laid out starts at a guessed grid, so its output waits for
        /// the cells that grid will really have: parsing at a guessed size is what measured pictures
        /// wrong (see `App::start`). The wait ends when the view reports the grid it measured with
        /// the scale its window reported.
        #[test]
        fn a_pane_started_before_its_window_laid_out_holds_until_the_view_reports_a_grid() {
            let (mut app, wid) = app_with(
                Pane::new(ProfileId::new(), "one", None),
                Pane::new(ProfileId::new(), "two", None),
            );
            let id = app.windows[&wid].tabs[0].panes[0].id;
            let _ = app.start(wid, id);
            let term = app.windows[&wid].tabs[0]
                .pane(id)
                .unwrap()
                .terminal()
                .cloned()
                .unwrap();

            assert!(
                term.lock().holding(),
                "nothing told this pane its cells yet"
            );
            term.lock().feed(b"printed before the layout\r\n");
            assert!(!screen(&term).contains("printed"), "held, not parsed");

            // The window reports its display, the layout runs, and the view measures its cells with
            // that scale: `resize` is what the widget then does, the message what it publishes.
            let real = GridSize {
                cols: 100,
                rows: 30,
                cell_width: 9.0,
                cell_height: 18.0,
                scale: 2.0,
            };
            term.lock().resize(real);
            let _ = app.update(Message::Terminal(wid, id, TerminalOutput::Resized(real)));

            assert!(!term.lock().holding(), "the first real grid released it");
            assert!(
                screen(&term).contains("printed"),
                "what it held is parsed now"
            );
            assert_eq!(app.grids[&id], real, "the app learned the grid");
        }

        /// The other shape a first layout takes: the terminal already has exactly the grid the view
        /// measures, so nothing resizes and `sized` is what releases the hold. That release is still
        /// reported (see `TerminalView::sync_size`), which is how the app ends up knowing this pane's
        /// cells at all - `App::start` no longer notes a grid the view has not confirmed, and the next
        /// new pane in this window is measured against what views have reported.
        #[test]
        fn a_first_layout_that_changes_nothing_still_tells_the_app_the_grid() {
            let (mut app, wid) = app_with(
                Pane::new(ProfileId::new(), "one", None),
                Pane::new(ProfileId::new(), "two", None),
            );
            let id = app.windows[&wid].tabs[0].panes[0].id;
            let _ = app.start(wid, id);
            let term = app.windows[&wid].tabs[0]
                .pane(id)
                .unwrap()
                .terminal()
                .cloned()
                .unwrap();
            assert!(
                term.lock().holding(),
                "nothing told this pane its cells yet"
            );
            assert!(
                !app.grids.contains_key(&id),
                "start notes nothing the view has not confirmed"
            );
            term.lock().feed(b"printed before the layout\r\n");

            // The window's first honest pass measures the same grid the terminal already has: the
            // widget releases the hold with `sized` and publishes this pane's grid.
            let same = term.lock().size();
            assert!(
                term.lock().sized(),
                "the hold released, nothing was resized"
            );
            let _ = app.update(Message::Terminal(wid, id, TerminalOutput::Resized(same)));

            assert!(
                !term.lock().holding(),
                "the release happened at the first layout"
            );
            assert!(screen(&term).contains("printed"), "what it held is parsed");
            assert_eq!(
                app.grids[&id], same,
                "and the app learned it from that report"
            );
        }

        /// The same pane started once its window has laid out and reported a grid: nothing about its
        /// cells is unknown, so nothing waits.
        #[test]
        fn a_pane_started_after_its_window_laid_out_holds_nothing() {
            let (mut app, wid) = app_with(
                Pane::new(ProfileId::new(), "one", None),
                Pane::new(ProfileId::new(), "two", None),
            );
            let id = app.windows[&wid].tabs[0].panes[0].id;
            app.grids.insert(id, grid(100, 30));

            let _ = app.start(wid, id);
            let term = app.windows[&wid].tabs[0]
                .pane(id)
                .unwrap()
                .terminal()
                .cloned()
                .unwrap();

            assert!(!term.lock().holding(), "its grid was already known");
            assert_eq!(term.lock().size(), grid(100, 30), "it started at that grid");
            term.lock().feed(b"printed at once\r\n");
            assert!(screen(&term).contains("printed"));
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

        pub(crate) fn wait_for(term: &SharedTerminal, text: &str) {
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

        /// A window moving to a display of another scale changes the pixel sizes programs are told.
        /// It travels the path an ordinary resize already takes: the style carries the new scale, so
        /// the grid the view reports at its next layout is a new one, and that reaches the pane's
        /// grid, its terminal and (after the quiet period) its session.
        #[test]
        fn scale_change_resends_pixel_sizes() {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let first = connected("one", shell("sleep 30", tx.clone()));
            let second = connected("two", shell("sleep 30", tx));
            let a = first.id;
            let (mut app, wid) = app_with(first, second);
            // The assertions below are in whole pixels.
            app.ui_scale = 1.0;

            // Laid out at the window's scale 1: 100x30 cells of 8x16 logical px.
            let cell = iced::Size::new(8.0, 16.0);
            let bounds = iced::Size::new(800.0, 480.0);
            // What the view reports: its style's scale, in the grid it measured at these bounds.
            let reported = |app: &App| {
                let tab = app.windows[&wid].active_tab().unwrap();
                let pane = tab.pane(a).unwrap();
                fbterm_term::grid_size_from_bounds(
                    bounds,
                    cell,
                    app.pane_style(wid, tab, pane).scale,
                )
            };

            let at_one = reported(&app);
            assert_eq!(at_one.scale, 1.0);
            let _ = app.update(Message::Terminal(wid, a, TerminalOutput::Resized(at_one)));
            let generation = app.pending_resizes[&a].0;
            let _ = app.update(Message::ResizeSettled(a, generation));
            assert_eq!(app.grids.get(&a), Some(&at_one));
            assert_eq!(at_one.physical_area(), (800, 480));

            // The window moves to a 2x display. Nothing resizes it: only its scale is reported.
            let _ = app.update(Message::WindowScale(wid, 2.0));
            let tab = app.windows[&wid].active_tab().unwrap();
            assert_eq!(app.pane_style(wid, tab, tab.pane(a).unwrap()).scale, 2.0);

            // So the grid its view reports at the next layout is a new one: same cells, new pixels.
            let at_two = reported(&app);
            assert_eq!((at_two.cols, at_two.rows), (100, 30), "cells stay cells");
            assert_eq!(at_two.scale, 2.0);
            assert_eq!(at_two.physical_area(), (1600, 960));

            let _ = app.update(Message::Terminal(wid, a, TerminalOutput::Resized(at_two)));
            // What the session is handed once the quiet period passes: the scaled grid, whose pixels
            // `pty_size` and `ssh_pixels` turn into the pixel fields programs read.
            assert_eq!(app.pending_resizes[&a].1.physical_area(), (1600, 960));
            let generation = app.pending_resizes[&a].0;
            let _ = app.update(Message::ResizeSettled(a, generation));

            // The pane's grid and its terminal both carry the new scale.
            assert_eq!(app.grids.get(&a), Some(&at_two));
            assert_eq!(size_of(&app, wid, a), at_two);
            // And so does the size the emulator answers programs with (`CSI 16 t` is one cell).
            let term = app.windows[&wid].tabs[0]
                .pane(a)
                .unwrap()
                .terminal()
                .unwrap();
            let answer = {
                let mut t = term.lock();
                t.feed(b"\x1b[16t");
                String::from_utf8(
                    t.take_events()
                        .into_iter()
                        .filter_map(|e| match e {
                            fbterm_term::TermEvent::PtyWrite(b) => Some(b),
                            _ => None,
                        })
                        .flatten()
                        .collect::<Vec<u8>>(),
                )
                .unwrap()
            };
            assert_eq!(answer, "\x1b[6;32;16t", "cells of 16x32 physical px");
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
            let before = fbterm_term::grid_size_from_bounds(bounds, cell(font(&app)), 1.0);

            let _ = app.update(Message::Shortcut(wid, Action::ZoomIn));
            let zoomed = font(&app);
            assert_eq!(zoomed, app.settings.font_size + 1.0);
            // What the view reports after measuring the bigger cell.
            let after = fbterm_term::grid_size_from_bounds(bounds, cell(zoomed), 1.0);
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

        /// After a drop, the grid each moved pane's view reports goes through the debounced
        /// resize path to its session (the view's side is `a_moved_tile_reports_its_new_grid`).
        #[test]
        fn dropped_tile_resizes_its_session() {
            use crate::layout::{Drop, Side};
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let script = "trap 'stty size' WINCH; echo ready; while :; do sleep 0.05; done";
            let first = connected("one", shell(script, tx.clone()));
            let second = connected("two", shell(script, tx));
            let (a, b) = (first.id, second.id);
            let (ta, tb) = (
                first.terminal().unwrap().clone(),
                second.terminal().unwrap().clone(),
            );
            let (mut app, wid) = app_with(first, second);
            wait_for(&ta, "ready");
            wait_for(&tb, "ready");
            let area = iced::Size::new(984.0, 600.0);
            let _ = app.update(Message::Tiles(wid, TilesMsg::Area(area)));

            let _ = app.update(Message::Tiles(
                wid,
                TilesMsg::DropTile {
                    pane: a,
                    at: Drop::Edge(Side::Bottom),
                },
            ));

            // What each view reports once laid out at its new rectangle (8 x 16 px cells).
            let cell = iced::Size::new(8.0, 16.0);
            let rects = app.windows[&wid].tabs[0]
                .layout
                .rects(iced::Rectangle::with_size(area));
            for (pane, rect) in rects {
                let g = fbterm_term::grid_size_from_bounds(rect.size(), cell, 1.0);
                let _ = app.update(Message::Terminal(wid, pane, TerminalOutput::Resized(g)));
                // Not yet: the session hears it once the quiet period passes.
                assert_eq!(size_of(&app, wid, pane), DEFAULT_GRID, "{pane:?}");
                let generation = app.pending_resizes[&pane].0;
                let _ = app.update(Message::ResizeSettled(pane, generation));
                assert_eq!(size_of(&app, wid, pane), g, "{pane:?}");
                let term = if pane == a { &ta } else { &tb };
                wait_for(term, &format!("{} {}", g.rows, g.cols));
            }
            assert!(app.pending_resizes.is_empty());
            assert_eq!(app.windows[&wid].tabs[0].layout.panes(), vec![b, a]);
        }

        #[test]
        fn merge_keeps_sessions() {
            use crate::layout::{Drop, Side};
            use feathercritty::grid::Dimensions;
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let first = connected("one", shell("seq 1 200; echo ready; sleep 30", tx.clone()));
            let second = connected("two", shell("echo also; sleep 30", tx.clone()));
            let third = connected("three", shell("echo third; sleep 30", tx));
            let (a, b, c) = (first.id, second.id, third.id);
            let sessions: Vec<_> = [&first, &second, &third]
                .iter()
                .map(|p| p.session.as_ref().unwrap().id)
                .collect();
            let term = first.terminal().unwrap().clone();
            // Tab 0: one | two; tab 1: three (active).
            let (mut app, wid) = app_with(first, second);
            let w = app.windows.get_mut(&wid).unwrap();
            w.push(Tab::single(third));
            let (target, source) = (w.tabs[0].id, w.tabs[1].id);
            wait_for(&term, "ready");
            let history = term.lock().term().grid().history_size();
            assert!(history > 0, "the first pane has scrollback");
            let requests = crate::memory::requests();

            let _ = app.merge_tab(wid, source, target, Drop::Tile(a, Side::Left));
            let _ = app.merge_tab(wid, target, source, Drop::Edge(Side::Top));

            let w = &app.windows[&wid];
            assert_eq!(w.tabs.len(), 1);
            let tab = &w.tabs[0];
            assert_eq!(tab.layout.panes(), vec![c, a, b]);
            let ids: Vec<_> = [a, b, c]
                .iter()
                .map(|p| tab.pane(*p).unwrap().session.as_ref().unwrap().id)
                .collect();
            assert_eq!(ids, sessions, "the same sessions, not new ones");
            assert!(
                [a, b, c]
                    .iter()
                    .all(|p| tab.pane(*p).unwrap().state == TabState::Connected)
            );
            let moved = tab.pane(a).unwrap().terminal().unwrap();
            assert!(std::sync::Arc::ptr_eq(moved, &term), "the same terminal");
            assert_eq!(moved.lock().term().grid().history_size(), history);
            assert!(screen(moved).contains("ready"));
            // Panes moved, nothing was freed: no close-tab release.
            assert_eq!(crate::memory::requests(), requests);
            assert!(app.pending_release.is_empty());

            // And back out to a tab of its own: still the same session.
            let _ = app.tile_to_tab(wid, a, 1);
            let w = &app.windows[&wid];
            let moved = w.tabs[1].pane(a).unwrap();
            assert_eq!(moved.session.as_ref().unwrap().id, sessions[0]);
            assert!(std::sync::Arc::ptr_eq(moved.terminal().unwrap(), &term));
            assert_eq!(crate::memory::requests(), requests);
            assert!(app.pending_release.is_empty());
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
