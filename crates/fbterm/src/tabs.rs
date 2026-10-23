//! Tabs and windows as plain state, so their transitions are testable without a GUI.

use std::sync::atomic::{AtomicU64, Ordering};

use fbterm_config::{ProfileId, Rgb};
use fbterm_session::{SessionEvent, SessionHandle};
use fbterm_term::SharedTerminal;

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

pub struct Tab {
    pub id: TabId,
    pub profile: ProfileId,
    /// Profile name, shown until the program sets a title.
    pub name: String,
    pub title: String,
    pub accent: Option<Rgb>,
    pub state: TabState,
    pub session: Option<SessionHandle>,
    /// Shown while there is no session (connecting, or a connect that failed), so notices
    /// render like terminal output and Enter reaches the tab.
    pub placeholder: Option<SharedTerminal>,
    /// Aborts the in-flight connect when dropped (closing a connecting tab abandons the dial).
    pub connecting: Option<iced::task::Handle>,
}

impl Tab {
    pub fn new(profile: ProfileId, name: impl Into<String>, accent: Option<Rgb>) -> Self {
        let name = name.into();
        Self {
            id: TabId::next(),
            profile,
            title: name.clone(),
            name,
            accent,
            state: TabState::NotConnected,
            session: None,
            placeholder: None,
            connecting: None,
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
/// its first column of an empty screen), cursor hidden; scrollback untouched.
pub fn notice_bytes(text: &str, fresh: bool) -> Vec<u8> {
    let lead = if fresh { "" } else { "\r\n" };
    format!("{lead}\x1b[0m\x1b[?25l\x1b[2m{text}\x1b[0m\r\n").into_bytes()
}

/// Apply a session event to its tab. Returns the notice to print, if the state changed to one
/// that has one.
pub fn apply_event(tab: &mut Tab, event: &SessionEvent) -> Option<String> {
    match event {
        SessionEvent::Title(t) => {
            tab.title = if t.is_empty() {
                tab.name.clone()
            } else {
                t.clone()
            };
            None
        }
        SessionEvent::Exited(code) => {
            if matches!(tab.state, TabState::Exited(_) | TabState::Disconnected(_)) {
                return None;
            }
            tab.state = TabState::Exited(*code);
            notice(&tab.state)
        }
        SessionEvent::Disconnected(reason) => {
            if matches!(tab.state, TabState::Exited(_) | TabState::Disconnected(_)) {
                return None;
            }
            tab.state = TabState::Disconnected(reason.clone());
            notice(&tab.state)
        }
        SessionEvent::Dirty | SessionEvent::Bell | SessionEvent::ClipboardStore(_) => None,
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

    pub fn find(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
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

    fn tab(name: &str) -> Tab {
        Tab::new(ProfileId::new(), name, None)
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
        w.tabs.iter().map(|t| t.name.as_str()).collect()
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
        let mut t = tab("zsh");
        t.state = TabState::Connected;
        let n = apply_event(&mut t, &SessionEvent::Exited(Some(0))).unwrap();
        assert_eq!(n, "process exited, press Enter to restart");
        assert_eq!(t.state, TabState::Exited(Some(0)));
        assert_eq!(apply_event(&mut t, &SessionEvent::Exited(Some(0))), None);

        let mut t = tab("prod");
        t.state = TabState::Connected;
        let n = apply_event(&mut t, &SessionEvent::Disconnected("eof".into())).unwrap();
        assert_eq!(n, "disconnected (eof), press Enter to reconnect");
        assert_eq!(t.state, TabState::Disconnected("eof".into()));
        // An SSH shell ending reports both; the first wins.
        assert_eq!(apply_event(&mut t, &SessionEvent::Exited(None)), None);
    }

    #[test]
    fn notice_bytes_are_dim_on_their_own_line() {
        assert_eq!(
            notice_bytes("x", false),
            b"\r\n\x1b[0m\x1b[?25l\x1b[2mx\x1b[0m\r\n".to_vec()
        );
        assert_eq!(
            notice_bytes("x", true),
            b"\x1b[0m\x1b[?25l\x1b[2mx\x1b[0m\r\n".to_vec()
        );
    }

    #[test]
    fn title_replaces_profile_name_and_empty_restores_it() {
        let mut t = tab("zsh");
        apply_event(&mut t, &SessionEvent::Title("vim notes.md".into()));
        assert_eq!(t.title, "vim notes.md");
        apply_event(&mut t, &SessionEvent::Title(String::new()));
        assert_eq!(t.title, "zsh");
    }

    #[test]
    fn closing_the_active_last_tab_selects_the_previous() {
        let mut w = window(&["a", "b", "c"], 2);
        let closed = w.close(2).unwrap();
        assert_eq!(closed.tab.name, "c");
        assert!(!closed.window_empty);
        assert_eq!(w.active, 1);
        assert_eq!(names(&w), ["a", "b"]);
    }

    #[test]
    fn closing_keeps_the_active_tab_when_another_closes() {
        let mut w = window(&["a", "b", "c"], 2);
        w.close(0);
        assert_eq!(w.active_tab().unwrap().name, "c");

        let mut w = window(&["a", "b", "c"], 0);
        w.close(2);
        assert_eq!(w.active_tab().unwrap().name, "a");
    }

    #[test]
    fn closing_the_active_middle_tab_selects_its_successor() {
        let mut w = window(&["a", "b", "c"], 1);
        w.close(1);
        assert_eq!(w.active_tab().unwrap().name, "c");
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
