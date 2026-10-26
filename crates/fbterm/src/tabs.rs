//! Tabs and windows as plain state, so their transitions are testable without a GUI.

use std::sync::atomic::{AtomicU64, Ordering};

use fbterm_config::{AccentStyle, Profile, ProfileId, Rgb};
use fbterm_session::{SessionEvent, SessionHandle};
use fbterm_term::SharedTerminal;

use crate::layout::{Layout, PaneId};

/// Identifies one tab for the lifetime of the process (indices move when tabs close).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

impl TabId {
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabState {
    /// Restored or freshly created; nothing started yet.
    NotConnected,
    Connecting,
    Connected,
    Disconnected(String),
    Exited(Option<i32>),
}

/// One session inside a tab: its terminal, state, title and find bar.
pub struct Pane {
    pub id: PaneId,
    pub profile: ProfileId,
    /// Profile name, shown until the program sets a title.
    pub name: String,
    pub title: String,
    pub accent: Option<Rgb>,
    /// How the accent shows on the tab.
    pub accent_style: AccentStyle,
    pub state: TabState,
    pub session: Option<SessionHandle>,
    /// Shown while there is no session (connecting, or a connect that failed), so notices
    /// render like terminal output and Enter reaches the pane.
    pub placeholder: Option<SharedTerminal>,
    /// Aborts the in-flight connect when dropped (closing a connecting pane abandons the dial).
    pub connecting: Option<iced::task::Handle>,
    /// The find bar, while open.
    pub find: Option<crate::find::FindBar>,
    /// Font size steps away from the setting (0: the setting).
    pub zoom: i8,
}

impl Pane {
    pub fn new(profile: ProfileId, name: impl Into<String>, accent: Option<Rgb>) -> Self {
        let name = name.into();
        Self {
            id: PaneId::next(),
            profile,
            title: name.clone(),
            name,
            accent,
            accent_style: AccentStyle::default(),
            state: TabState::NotConnected,
            session: None,
            placeholder: None,
            connecting: None,
            find: None,
            zoom: 0,
        }
    }

    /// A pane for a saved profile: its name, accent and accent style.
    pub fn for_profile(p: &Profile) -> Self {
        Self {
            accent_style: p.accent_style,
            ..Self::new(p.id, p.name.clone(), p.accent)
        }
    }

    /// The terminal to draw: the session's, else the placeholder.
    pub fn terminal(&self) -> Option<&SharedTerminal> {
        self.session
            .as_ref()
            .map(|s| &s.terminal)
            .or(self.placeholder.as_ref())
    }
}

/// A tab: one or more panes laid out by `layout`. The tab bar shows the focused pane.
pub struct Tab {
    pub id: TabId,
    pub layout: Layout,
    /// Every pane of `layout`, in no particular order.
    pub panes: Vec<Pane>,
    pub focused: PaneId,
    /// The pane filling the whole tab, while one is maximized.
    pub maximized: Option<PaneId>,
    /// Panes by most recent focus, latest first.
    pub recent: Vec<PaneId>,
}

impl Tab {
    /// A tab holding just `pane`.
    pub fn single(pane: Pane) -> Self {
        let id = pane.id;
        Self {
            id: TabId::next(),
            layout: Layout::Pane(id),
            panes: vec![pane],
            focused: id,
            maximized: None,
            recent: vec![id],
        }
    }

    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    #[allow(dead_code)] // used by tab merge and tile moves (tiling task 10)
    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    /// Focus `pane`: the one way focus changes, so `recent` stays right (clicks, directional
    /// focus, a find jump, the refocus after a close). A pane the maximized one hides is shown
    /// by un-maximizing; the caller then runs `App::shown` for the panes that
    /// came back. Returns whether anything changed (false for a pane not in this tab).
    pub fn focus(&mut self, pane: PaneId) -> bool {
        if !self.layout.contains(pane) {
            return false;
        }
        let unhidden = self.maximized.is_some_and(|m| m != pane);
        if unhidden {
            self.maximized = None;
        }
        if self.focused == pane {
            return unhidden;
        }
        self.focused = pane;
        self.recent.retain(|p| *p != pane);
        self.recent.insert(0, pane);
        true
    }

    /// Take `id` out of the tab and focus the pane focused before it (else the first in reading
    /// order) if it had focus. `None` for a pane not in the tab and for the last pane: closing
    /// the tab then is the caller's job.
    pub fn close_pane(&mut self, id: PaneId) -> Option<Pane> {
        if self.panes.len() < 2 {
            return None;
        }
        let at = self.panes.iter().position(|p| p.id == id)?;
        self.layout.remove(id)?;
        let pane = self.panes.remove(at);
        self.recent.retain(|p| *p != id);
        if self.maximized == Some(id) {
            self.maximized = None;
        }
        if self.focused == id {
            let next = self
                .recent
                .iter()
                .copied()
                .find(|p| self.layout.contains(*p))
                .or_else(|| self.layout.panes().first().copied());
            if let Some(next) = next {
                self.focus(next);
            }
        }
        Some(pane)
    }

    pub fn focused_pane(&self) -> &Pane {
        self.pane(self.focused)
            .or(self.panes.first())
            .expect("a tab always has a pane")
    }

    pub fn focused_pane_mut(&mut self) -> &mut Pane {
        let i = self
            .panes
            .iter()
            .position(|p| p.id == self.focused)
            .unwrap_or(0);
        &mut self.panes[i]
    }

    /// The panes drawn on screen, in reading order: every pane, or only the maximized one.
    pub fn visible_panes(&self) -> Vec<PaneId> {
        match self.maximized {
            Some(id) if self.layout.contains(id) => vec![id],
            _ => self.layout.panes(),
        }
    }

    /// The layout as drawn: the whole tree, or just the maximized pane.
    pub fn shown_layout(&self) -> Layout {
        match self.maximized {
            Some(id) if self.layout.contains(id) => Layout::Pane(id),
            _ => self.layout.clone(),
        }
    }

    /// The focused pane's terminal.
    pub fn terminal(&self) -> Option<&SharedTerminal> {
        self.focused_pane().terminal()
    }

    /// The focused pane's title.
    pub fn title(&self) -> &str {
        &self.focused_pane().title
    }

    /// The focused pane's profile name.
    #[allow(dead_code)] // read by tests; part of the tab's read surface
    pub fn name(&self) -> &str {
        &self.focused_pane().name
    }

    pub fn accent(&self) -> Option<Rgb> {
        self.focused_pane().accent
    }

    pub fn accent_style(&self) -> AccentStyle {
        self.focused_pane().accent_style
    }

    pub fn state(&self) -> &TabState {
        &self.focused_pane().state
    }
}

/// What to do with input the terminal widget produced for a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputRoute {
    /// Write it to the session.
    Send,
    /// Enter on a disconnected (or never connected) tab: connect again.
    Reconnect,
    /// Enter on a tab whose program exited: start it again.
    Respawn,
    /// Nothing can take input right now.
    Ignore,
}

pub fn route_input(state: &TabState, bytes: &[u8]) -> InputRoute {
    let enter = bytes == b"\r";
    match state {
        TabState::Connected => InputRoute::Send,
        TabState::Disconnected(_) | TabState::NotConnected if enter => InputRoute::Reconnect,
        TabState::Exited(_) if enter => InputRoute::Respawn,
        _ => InputRoute::Ignore,
    }
}

/// The dim notice written into a tab's terminal when it enters `state`, if any.
pub fn notice(state: &TabState) -> Option<String> {
    match state {
        TabState::Disconnected(reason) if reason.is_empty() => {
            Some("disconnected, press Enter to reconnect".into())
        }
        TabState::Disconnected(reason) => {
            Some(format!("disconnected ({reason}), press Enter to reconnect"))
        }
        TabState::Exited(Some(code)) if *code != 0 => Some(format!(
            "process exited with code {code}, press Enter to restart"
        )),
        TabState::Exited(_) => Some("process exited, press Enter to restart".into()),
        _ => None,
    }
}

/// The notice as terminal bytes: dim, on a line of its own (unless the terminal is still at
/// its first column of an empty screen); scrollback untouched.
pub fn notice_bytes(text: &str, fresh: bool) -> Vec<u8> {
    let lead = if fresh { "" } else { "\r\n" };
    format!("{lead}\x1b[0m\x1b[2m{text}\x1b[0m\r\n").into_bytes()
}

/// Fed into a tab's terminal before a new session continues in it: undoes the modes the
/// previous program may have left on (alternate screen, hidden cursor, mouse reporting,
/// bracketed paste, application keys, scroll region, colors) without clearing the screen or
/// moving the cursor, so the new output starts below the old content.
pub const RESTART_RESET: &[u8] = b"\x1b[?1049l\x1b[0m\x1b[?25h\x1b[?1l\x1b>\x1b[?6l\
\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?7h\
\x1b7\x1b[r\x1b8";

/// A session in `pane` reported `Dirty`. Only a pane on screen (a visible pane of the active
/// tab) takes the flag (re-arming its next `Dirty`) and asks for a redraw; any other pane leaves
/// it pending, so its session stays quiet until it is shown. Returns whether to redraw.
pub fn on_dirty(window: &WindowState, pane: PaneId) -> bool {
    let Some(tab) = window.active_tab() else {
        return false;
    };
    if !tab.visible_panes().contains(&pane) {
        return false;
    }
    if let Some(s) = tab.pane(pane).and_then(|p| p.session.as_ref()) {
        s.take_dirty();
    }
    true
}

/// The active tab of `window` has just been shown (selected, created, its window focused):
/// take the pending redraw of every pane on screen. Returns whether one was pending.
pub fn on_shown(window: &WindowState) -> bool {
    let Some(tab) = window.active_tab() else {
        return false;
    };
    tab.visible_panes()
        .into_iter()
        .filter_map(|id| tab.pane(id).and_then(|p| p.session.as_ref()))
        .fold(false, |any, s| s.take_dirty() | any)
}

/// Install a running session in its pane and mark the pane connected. A session that already
/// ended before it was installed (its `Exited` or `Disconnected` event found no pane) is
/// applied at once; returns that end's notice.
pub fn install_session(pane: &mut Pane, handle: SessionHandle) -> Option<String> {
    let ended = handle.ended();
    pane.session = Some(handle);
    pane.placeholder = None;
    pane.connecting = None;
    pane.state = TabState::Connected;
    ended.and_then(|end| apply_event(pane, &end.into()))
}

/// Apply a session event to its pane. Returns the notice to print, if the state changed to one
/// that has one.
pub fn apply_event(pane: &mut Pane, event: &SessionEvent) -> Option<String> {
    match event {
        SessionEvent::Title(t) => {
            pane.title = if t.is_empty() {
                pane.name.clone()
            } else {
                t.clone()
            };
            None
        }
        SessionEvent::Exited(code) => {
            if matches!(pane.state, TabState::Exited(_) | TabState::Disconnected(_)) {
                return None;
            }
            pane.state = TabState::Exited(*code);
            notice(&pane.state)
        }
        SessionEvent::Disconnected(reason) => {
            if matches!(pane.state, TabState::Exited(_) | TabState::Disconnected(_)) {
                return None;
            }
            pane.state = TabState::Disconnected(reason.clone());
            notice(&pane.state)
        }
        SessionEvent::Dirty | SessionEvent::Bell | SessionEvent::Released => None,
    }
}

#[derive(Default)]
pub struct WindowState {
    pub tabs: Vec<Tab>,
    pub active: usize,
}

/// A tab taken out of its window.
pub struct Closed {
    pub tab: Tab,
    /// The window has no tabs left and should close.
    pub window_empty: bool,
}

impl WindowState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a tab and make it active; returns its index.
    pub fn push(&mut self, tab: Tab) -> usize {
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.active
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    #[allow(dead_code)] // tab drag and merge (tiling task 10) look tabs up by id again
    pub fn find(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Where `pane` is: (tab index, index in that tab's `panes`).
    pub fn locate(&self, pane: PaneId) -> Option<(usize, usize)> {
        self.tabs.iter().enumerate().find_map(|(t, tab)| {
            let p = tab.panes.iter().position(|p| p.id == pane)?;
            Some((t, p))
        })
    }

    /// Make tab `index` active. Returns whether the active tab changed.
    pub fn select(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() || index == self.active {
            return false;
        }
        self.active = index;
        true
    }

    /// Select tab `n` as numbered in the tab bar (1-based).
    pub fn select_number(&mut self, n: u8) -> bool {
        match usize::from(n).checked_sub(1) {
            Some(i) => self.select(i),
            None => false,
        }
    }

    /// Next tab, wrapping.
    pub fn next(&mut self) -> bool {
        match self.tabs.len() {
            0 => false,
            n => self.select((self.active + 1) % n),
        }
    }

    /// Previous tab, wrapping.
    pub fn prev(&mut self) -> bool {
        match self.tabs.len() {
            0 => false,
            n => self.select((self.active + n - 1) % n),
        }
    }

    /// Remove tab `index`. The active tab stays active; closing the active tab selects the one
    /// that took its place, or the previous one when it was the last.
    pub fn close(&mut self, index: usize) -> Option<Closed> {
        if index >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(index);
        if index < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        Some(Closed {
            tab,
            window_empty: self.tabs.is_empty(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(name: &str) -> Pane {
        Pane::new(ProfileId::new(), name, None)
    }

    fn tab(name: &str) -> Tab {
        Tab::single(pane(name))
    }

    fn window(names: &[&str], active: usize) -> WindowState {
        let mut w = WindowState::new();
        for n in names {
            w.push(tab(n));
        }
        w.active = active;
        w
    }

    fn names(w: &WindowState) -> Vec<&str> {
        w.tabs.iter().map(Tab::name).collect()
    }

    #[test]
    fn single_tab_reads_its_pane() {
        let accent = Some(Rgb { r: 1, g: 2, b: 3 });
        let mut pane = Pane::new(ProfileId::new(), "zsh", accent);
        pane.title = "vim notes.md".into();
        pane.state = TabState::Connected;
        let term = std::sync::Arc::new(parking_lot::Mutex::new(fbterm_term::Terminal::new(
            fbterm_term::GridSize {
                cols: 20,
                rows: 5,
                cell_width: 8.0,
                cell_height: 16.0,
            },
            100,
        )));
        pane.placeholder = Some(term.clone());
        let id = pane.id;
        let tab = Tab::single(pane);
        assert_eq!(tab.panes.len(), 1);
        assert_eq!(tab.focused, id);
        assert_eq!(tab.layout, crate::layout::Layout::Pane(id));
        assert_eq!(tab.maximized, None);
        assert_eq!(tab.title(), "vim notes.md");
        assert_eq!(tab.name(), "zsh");
        assert_eq!(tab.accent(), accent);
        assert_eq!(tab.state(), &TabState::Connected);
        assert!(std::sync::Arc::ptr_eq(tab.terminal().unwrap(), &term));
        assert_eq!(tab.focused_pane().id, id);
        assert!(tab.pane(id).is_some());
        assert!(tab.pane(PaneId::next()).is_none());
        assert_eq!(tab.visible_panes(), vec![id]);
    }

    #[test]
    fn visible_panes_respects_maximized() {
        let first = Pane::new(ProfileId::new(), "a", None);
        let second = Pane::new(ProfileId::new(), "b", None);
        let (a, b) = (first.id, second.id);
        let mut tab = Tab::single(first);
        tab.layout.split(a, crate::layout::Side::Right, b);
        tab.panes.push(second);
        assert_eq!(tab.visible_panes(), vec![a, b]);
        tab.maximized = Some(b);
        assert_eq!(tab.visible_panes(), vec![b]);
        tab.maximized = None;
        assert_eq!(tab.visible_panes(), vec![a, b]);
    }

    /// A tab of three panes: `a` | (`b` over `c`), `a` focused.
    fn three() -> (Tab, [PaneId; 3]) {
        let (first, second, third) = (pane("a"), pane("b"), pane("c"));
        let (a, b, c) = (first.id, second.id, third.id);
        let mut tab = Tab::single(first);
        tab.layout.split(a, crate::layout::Side::Right, b);
        tab.layout.split(b, crate::layout::Side::Bottom, c);
        tab.panes.extend([second, third]);
        (tab, [a, b, c])
    }

    #[test]
    fn focus_records_recency_and_shows_the_pane() {
        let (mut tab, [a, b, c]) = three();
        assert!(!tab.focus(a), "already focused");
        assert!(tab.focus(c));
        assert_eq!((tab.focused, tab.recent.clone()), (c, vec![c, a]));
        assert!(tab.focus(b));
        assert!(tab.focus(c));
        assert_eq!(tab.recent, vec![c, b, a]);
        assert!(!tab.focus(PaneId::next()), "not in this tab");
        // A pane hidden by maximize is shown again by focusing another.
        tab.maximized = Some(c);
        assert!(tab.focus(a));
        assert_eq!(tab.maximized, None);
        assert_eq!(tab.visible_panes().len(), 3);
    }

    #[test]
    fn closing_a_pane_refocuses_the_most_recent_and_prunes() {
        let (mut tab, [a, b, c]) = three();
        tab.focus(c);
        tab.focus(b);
        // Close b: c was focused last.
        let gone = tab.close_pane(b).expect("b is in the tab");
        assert_eq!(gone.id, b);
        assert_eq!(tab.focused, c);
        assert_eq!(tab.recent, vec![c, a]);
        assert_eq!(tab.layout.panes(), vec![a, c]);
        assert_eq!(tab.panes.len(), 2);
        // Closing a pane that is not focused keeps the focus.
        assert!(tab.close_pane(a).is_some());
        assert_eq!(tab.focused, c);
        // The last pane stays: closing the tab is the caller's job.
        assert!(tab.close_pane(c).is_none());
        assert_eq!(tab.panes.len(), 1);
        assert!(tab.close_pane(PaneId::next()).is_none());
    }

    #[test]
    fn closing_the_maximized_pane_shows_the_others() {
        let (mut tab, [a, b, _]) = three();
        tab.focus(b);
        tab.maximized = Some(b);
        tab.close_pane(b);
        assert_eq!(tab.maximized, None);
        assert_eq!(tab.focused, a);
        assert_eq!(tab.visible_panes().len(), 2);
    }

    #[test]
    fn enter_on_disconnected_requests_reconnect() {
        let s = TabState::Disconnected("connection reset".into());
        assert_eq!(route_input(&s, b"\r"), InputRoute::Reconnect);
        assert_eq!(route_input(&s, b"x"), InputRoute::Ignore);
        assert_eq!(
            route_input(&TabState::NotConnected, b"\r"),
            InputRoute::Reconnect
        );
    }

    #[test]
    fn enter_on_exited_respawns() {
        assert_eq!(
            route_input(&TabState::Exited(Some(0)), b"\r"),
            InputRoute::Respawn
        );
        assert_eq!(
            route_input(&TabState::Exited(None), b"\r"),
            InputRoute::Respawn
        );
        assert_eq!(
            route_input(&TabState::Exited(None), b"ls\r"),
            InputRoute::Ignore
        );
    }

    #[test]
    fn connected_sends_and_connecting_ignores() {
        assert_eq!(route_input(&TabState::Connected, b"\r"), InputRoute::Send);
        assert_eq!(route_input(&TabState::Connected, b"ls"), InputRoute::Send);
        assert_eq!(
            route_input(&TabState::Connecting, b"\r"),
            InputRoute::Ignore
        );
    }

    #[test]
    fn session_end_sets_state_and_notice_once() {
        let mut t = pane("zsh");
        t.state = TabState::Connected;
        let n = apply_event(&mut t, &SessionEvent::Exited(Some(0))).unwrap();
        assert_eq!(n, "process exited, press Enter to restart");
        assert_eq!(t.state, TabState::Exited(Some(0)));
        assert_eq!(apply_event(&mut t, &SessionEvent::Exited(Some(0))), None);

        let mut t = pane("prod");
        t.state = TabState::Connected;
        let n = apply_event(&mut t, &SessionEvent::Disconnected("eof".into())).unwrap();
        assert_eq!(n, "disconnected (eof), press Enter to reconnect");
        assert_eq!(t.state, TabState::Disconnected("eof".into()));
        // An SSH shell ending reports both; the first wins.
        assert_eq!(apply_event(&mut t, &SessionEvent::Exited(None)), None);
    }

    #[test]
    fn restart_reset_keeps_old_content_and_continues_below() {
        use feathercritty::index::{Column, Line};
        let size = fbterm_term::GridSize {
            cols: 20,
            rows: 6,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let mut term = fbterm_term::Terminal::new(size, 100);
        // A program that wrote two lines, then left the alternate screen, a scroll region and
        // a hidden cursor behind.
        term.feed(b"aaa\r\nbbb\r\n\x1b[?1049h\x1b[2;4r\x1b[?25lalt");
        term.feed(RESTART_RESET);
        term.feed(b"ccc");
        let row = |r: i32| -> String {
            (0..3)
                .map(|c| term.term().grid().screen()[Line(r)][Column(c)].c)
                .collect()
        };
        assert_eq!([row(0), row(1), row(2)], ["aaa", "bbb", "ccc"]);
        assert!(term.mode().contains(fbterm_term::TermMode::SHOW_CURSOR));
        assert!(!term.mode().contains(fbterm_term::TermMode::ALT_SCREEN));
    }

    #[cfg(unix)]
    #[test]
    fn hidden_tab_leaves_dirty_pending_until_shown() {
        use std::time::{Duration, Instant};
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let local = fbterm_config::LocalProfile {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo hi; sleep 5".into()],
            cwd: None,
            env: Default::default(),
        };
        let size = fbterm_term::GridSize {
            cols: 80,
            rows: 24,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let session = fbterm_session::spawn_local(&local, size, 100, tx).expect("spawn");
        let mut w = WindowState::new();
        let mut hidden = tab("busy");
        hidden.panes[0].session = Some(session);
        let id = hidden.focused;
        w.push(hidden);
        w.push(tab("front"));
        // Wait for the hidden session's Dirty.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match rx.try_recv() {
                Ok((_, SessionEvent::Dirty)) => break,
                _ if Instant::now() > deadline => panic!("no Dirty"),
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        assert!(!on_dirty(&w, id), "hidden tab must not redraw");
        // Still pending: showing the tab takes it.
        assert!(w.select(0));
        assert!(on_shown(&w), "showing the tab takes the pending redraw");
        assert!(!on_shown(&w), "taken once");
        // Visible now: a Dirty re-arms and redraws.
        assert!(on_dirty(&w, id));
    }

    #[cfg(unix)]
    #[test]
    fn installing_an_already_ended_session_applies_its_end() {
        use std::time::{Duration, Instant};
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let start = |script: &str| {
            let local = fbterm_config::LocalProfile {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                cwd: None,
                env: Default::default(),
            };
            let size = fbterm_term::GridSize {
                cols: 80,
                rows: 24,
                cell_width: 8.0,
                cell_height: 16.0,
            };
            fbterm_session::spawn_local(&local, size, 100, tx.clone()).expect("spawn")
        };

        // The end was reported before the handle reached its tab.
        let session = start("exit 3");
        let deadline = Instant::now() + Duration::from_secs(10);
        while session.ended().is_none() {
            assert!(Instant::now() < deadline, "session never ended");
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut t = pane("early");
        t.state = TabState::Connecting;
        let notice = install_session(&mut t, session);
        assert_eq!(t.state, TabState::Exited(Some(3)));
        assert!(notice.is_some());
        assert!(t.session.is_some() && t.connecting.is_none() && t.placeholder.is_none());

        // A running session just connects.
        let mut t = pane("running");
        t.state = TabState::Connecting;
        assert_eq!(install_session(&mut t, start("sleep 5")), None);
        assert_eq!(t.state, TabState::Connected);
    }

    #[test]
    fn a_session_end_converts_to_its_event() {
        use fbterm_session::SessionEnd;
        let mut t = pane("ssh");
        t.state = TabState::Connected;
        assert!(apply_event(&mut t, &SessionEnd::Disconnected("gone".into()).into()).is_some());
        assert_eq!(t.state, TabState::Disconnected("gone".into()));
        assert_eq!(
            SessionEvent::from(SessionEnd::Exited(None)),
            SessionEvent::Exited(None)
        );
    }

    #[test]
    fn notice_bytes_are_dim_on_their_own_line() {
        assert_eq!(
            notice_bytes("x", false),
            b"\r\n\x1b[0m\x1b[2mx\x1b[0m\r\n".to_vec()
        );
        assert_eq!(
            notice_bytes("x", true),
            b"\x1b[0m\x1b[2mx\x1b[0m\r\n".to_vec()
        );
    }

    #[test]
    fn title_replaces_profile_name_and_empty_restores_it() {
        let mut t = pane("zsh");
        apply_event(&mut t, &SessionEvent::Title("vim notes.md".into()));
        assert_eq!(t.title, "vim notes.md");
        apply_event(&mut t, &SessionEvent::Title(String::new()));
        assert_eq!(t.title, "zsh");
    }

    #[test]
    fn closing_the_active_last_tab_selects_the_previous() {
        let mut w = window(&["a", "b", "c"], 2);
        let closed = w.close(2).unwrap();
        assert_eq!(closed.tab.name(), "c");
        assert!(!closed.window_empty);
        assert_eq!(w.active, 1);
        assert_eq!(names(&w), ["a", "b"]);
    }

    #[test]
    fn closing_keeps_the_active_tab_when_another_closes() {
        let mut w = window(&["a", "b", "c"], 2);
        w.close(0);
        assert_eq!(w.active_tab().unwrap().name(), "c");

        let mut w = window(&["a", "b", "c"], 0);
        w.close(2);
        assert_eq!(w.active_tab().unwrap().name(), "a");
    }

    #[test]
    fn closing_the_active_middle_tab_selects_its_successor() {
        let mut w = window(&["a", "b", "c"], 1);
        w.close(1);
        assert_eq!(w.active_tab().unwrap().name(), "c");
    }

    #[test]
    fn closing_the_last_tab_closes_the_window() {
        let mut w = window(&["a"], 0);
        let closed = w.close(0).unwrap();
        assert!(closed.window_empty);
        assert!(w.tabs.is_empty());
        assert!(w.close(0).is_none());
    }

    #[test]
    fn next_prev_wrap_and_numbers_select() {
        let mut w = window(&["a", "b", "c"], 0);
        assert!(w.prev());
        assert_eq!(w.active, 2);
        assert!(w.next());
        assert_eq!(w.active, 0);
        assert!(w.select_number(2));
        assert_eq!(w.active, 1);
        assert!(!w.select_number(9), "no ninth tab");
        assert!(!w.select_number(0));
        assert_eq!(w.active, 1);
        let mut one = window(&["a"], 0);
        assert!(!one.next());
    }

    #[test]
    fn push_selects_the_new_tab() {
        let mut w = window(&["a"], 0);
        assert_eq!(w.push(tab("b")), 1);
        assert_eq!(w.active, 1);
        assert_eq!(w.find(w.tabs[1].id), Some(1));
    }
}
