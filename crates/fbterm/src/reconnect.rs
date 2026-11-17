//! Bringing dropped sessions back: the `Reconnect` shortcut (every dropped pane of the active
//! tab), the tab menu's `Reconnect` or `Restart` item (of the right-clicked tab) and the
//! button on the bar over a dropped pane (that pane). Enter on such a pane does the same
//! through `tabs::route_input`.
//!
//! Only a pane whose session ended (`Disconnected`, `Exited`) is ever restarted here, so a
//! stale click or key never replaces a live or starting session.

use fbterm_config::ProfileKind;
use iced::widget::{column, container, row, text};
use iced::{Alignment, Element, Length, Padding, Task, window};

use crate::app::{App, Message};
use crate::keys::Action;
use crate::layout::PaneId;
use crate::tabs::{Pane, Tab, TabId, TabState};
use crate::theme::UI_FONT;
use crate::ui;

/// What bringing a pane back does: dial its SSH profile again, or start its local program
/// again. A tab is `Reconnect` when any of its dropped panes is an SSH profile's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Revive {
    Reconnect,
    Restart,
}

impl Revive {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Revive::Reconnect => "Reconnect",
            Revive::Restart => "Restart",
        }
    }
}

/// The session ended and can be started again.
pub(crate) fn ended(state: &TabState) -> bool {
    matches!(state, TabState::Disconnected(_) | TabState::Exited(_))
}

/// The line on the bar over a dropped pane, `None` while its session is live, starting or
/// not started yet.
pub(crate) fn bar_text(state: &TabState) -> Option<String> {
    match state {
        TabState::Disconnected(reason) if reason.is_empty() => Some("Disconnected".into()),
        // Connect failures already say so (`App::connect_failed`).
        TabState::Disconnected(reason) if reason.starts_with("could not") => {
            let mut chars = reason.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
        }
        TabState::Disconnected(reason) => Some(format!("Disconnected ({reason})")),
        TabState::Exited(Some(code)) if *code != 0 => {
            Some(format!("Process exited with code {code}"))
        }
        TabState::Exited(_) => Some("Process exited".into()),
        TabState::NotConnected | TabState::Connecting | TabState::Connected => None,
    }
}

impl App {
    /// What bringing `pane` back would do, if its session ended and its profile still exists
    /// (a deleted profile's pane has nothing to start again).
    pub(crate) fn pane_revive(&self, pane: &Pane) -> Option<Revive> {
        if !ended(&pane.state) {
            return None;
        }
        let profile = self.profiles.get(pane.profile)?;
        Some(match profile.kind {
            ProfileKind::Ssh(_) => Revive::Reconnect,
            ProfileKind::Local(_) => Revive::Restart,
        })
    }

    /// What the tab menu's item for `tab` says: `Reconnect` when any dropped pane is an SSH
    /// profile's, `Restart` when only local programs exited, `None` when nothing ended.
    pub(crate) fn revive_kind(&self, tab: &Tab) -> Option<Revive> {
        let mut kinds = tab.panes.iter().filter_map(|p| self.pane_revive(p));
        let first = kinds.next()?;
        Some(
            if first == Revive::Reconnect || kinds.any(|k| k == Revive::Reconnect) {
                Revive::Reconnect
            } else {
                Revive::Restart
            },
        )
    }

    /// Bring back every dropped pane of `tab` (the tab menu's item).
    pub(crate) fn revive_tab(&mut self, wid: window::Id, tab: TabId) -> Task<Message> {
        let Some(t) = self
            .windows
            .get(&wid)
            .and_then(|w| w.tabs.iter().find(|t| t.id == tab))
        else {
            return Task::none();
        };
        let ids: Vec<PaneId> = t
            .panes
            .iter()
            .filter(|p| self.pane_revive(p).is_some())
            .map(|p| p.id)
            .collect();
        Task::batch(ids.into_iter().map(|id| self.revive_pane(wid, id)))
    }

    /// The `Reconnect` shortcut: every dropped pane of the active tab.
    pub(crate) fn revive_active_tab(&mut self, wid: window::Id) -> Task<Message> {
        match self.windows.get(&wid).and_then(|w| w.active_tab()) {
            Some(tab) => self.revive_tab(wid, tab.id),
            None => Task::none(),
        }
    }

    /// Bring back one pane (its bar's button), only if its session ended.
    pub(crate) fn revive_pane(&mut self, wid: window::Id, id: PaneId) -> Task<Message> {
        let Some(pane) = self
            .windows
            .get(&wid)
            .and_then(|w| w.locate(id).map(|(t, p)| &w.tabs[t].panes[p]))
        else {
            return Task::none();
        };
        if self.pane_revive(pane).is_none() {
            return Task::none();
        }
        let profile = pane.profile;
        let task = self.start(wid, id);
        // Its tunnels come back once it connects (`connected`).
        self.sync_shell_tunnels(profile);
        task
    }

    /// The bar along the bottom of a dropped pane in window `wid`: why its session ended,
    /// the `Reconnect` chord and a `Reconnect` or `Restart` button. `None` while the session
    /// is live, starting or not started yet. It sits below the terminal, never over it, so the
    /// last lines of output (and the notice written into the terminal) stay readable.
    pub(crate) fn ended_bar<'a>(
        &self,
        wid: window::Id,
        pane: &Pane,
    ) -> Option<Element<'a, Message>> {
        let line = bar_text(&pane.state)?;
        let t = self.tokens;
        let mut content = row![
            ui::dot(crate::window::state_color(t, &pane.state), 7.0),
            // One line, clipped: bars side by side in a split keep one height, and the whole
            // reason is in the terminal's own notice above.
            container(
                text(line)
                    .font(UI_FONT)
                    .size(12.5)
                    .color(t.text_secondary)
                    .wrapping(text::Wrapping::None),
            )
            .width(Length::Fill)
            .clip(true),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if let Some(revive) = self.pane_revive(pane) {
            // The bar shows on the active tab only, where the chord brings this pane back.
            if let Some(chord) = self.bindings.label(Action::Reconnect) {
                content = content.push(text(chord).font(ui::MONO).size(12).color(t.text_muted));
            }
            content = content.push(ui::text_button(
                t,
                revive.title(),
                Some(Message::Revive(wid, pane.id)),
                ui::secondary,
            ));
        }
        let bar = container(content)
            .padding(Padding::from([6.0, 12.0]))
            .width(Length::Fill)
            .style(move |_| iced::widget::container::Style {
                background: Some(t.surface.into()),
                ..Default::default()
            });
        Some(column![ui::horizontal_rule(t, self.physical_scale(wid)), bar].into())
    }
}

#[cfg(test)]
mod tests {
    use fbterm_config::{AuthMethod, Profile, ProfileId, ProfileKind, SshProfile};
    use iced::{Point, window};

    use super::{Revive, bar_text};
    use crate::app::{App, Message};
    use crate::context_menu::{MenuItem, MenuMsg};
    use crate::keys::Action;
    use crate::layout::Side;
    use crate::restore_tests::{paths_with, profile};
    use crate::tabs::{Pane, Tab, TabState, WindowState};

    fn ssh(name: &str) -> Profile {
        Profile {
            icon: None,
            id: ProfileId::new(),
            name: name.into(),
            group: None,
            accent: None,
            accent_style: Default::default(),
            color_scheme: None,
            kind: ProfileKind::Ssh(SshProfile {
                host: "h.example.com".into(),
                port: 22,
                user: "root".into(),
                auth: AuthMethod::Agent,
                jump: vec![],
                tunnels: vec![],
            }),
        }
    }

    fn pane(p: &Profile, state: TabState) -> Pane {
        let mut pane = Pane::for_profile(p);
        pane.state = state;
        pane
    }

    fn tab_of(panes: Vec<Pane>) -> Tab {
        let mut panes = panes.into_iter();
        let first = panes.next().unwrap();
        let mut last = first.id;
        let mut tab = Tab::single(first);
        for p in panes {
            tab.layout.split(last, Side::Right, p.id);
            last = p.id;
            tab.panes.push(p);
        }
        tab
    }

    /// An app with one window holding `tabs`, the first one active.
    fn app_with(known: &[Profile], tabs: Vec<Tab>) -> (App, window::Id) {
        let (mut app, _) = App::build(paths_with(known), true);
        app.state_saving_enabled = false;
        let wid = window::Id::unique();
        let mut w = WindowState::new();
        for t in tabs {
            w.push(t);
        }
        w.active = 0;
        app.windows.clear();
        app.windows.insert(wid, w);
        app.sizes.insert(wid, (900.0, 600.0));
        (app, wid)
    }

    fn state(app: &App, wid: window::Id, tab: usize, pane: usize) -> TabState {
        app.windows[&wid].tabs[tab].panes[pane].state.clone()
    }

    #[test]
    fn the_bar_says_why_the_session_ended() {
        let d = |r: &str| bar_text(&TabState::Disconnected(r.into()));
        assert_eq!(d("").as_deref(), Some("Disconnected"));
        assert_eq!(
            d("connection reset").as_deref(),
            Some("Disconnected (connection reset)")
        );
        assert_eq!(
            d("could not connect: timed out").as_deref(),
            Some("Could not connect: timed out")
        );
        assert_eq!(
            bar_text(&TabState::Exited(Some(1))).as_deref(),
            Some("Process exited with code 1")
        );
        assert_eq!(
            bar_text(&TabState::Exited(Some(0))).as_deref(),
            Some("Process exited")
        );
        assert_eq!(
            bar_text(&TabState::Exited(None)).as_deref(),
            Some("Process exited")
        );
        for live in [
            TabState::Connected,
            TabState::Connecting,
            TabState::NotConnected,
        ] {
            assert_eq!(bar_text(&live), None, "{live:?}");
        }
    }

    #[test]
    fn a_tab_offers_reconnect_when_an_ssh_pane_dropped_and_restart_when_only_programs_exited() {
        let (s, l) = (ssh("prod"), profile("sh"));
        let (app, wid) = app_with(
            &[s.clone(), l.clone()],
            vec![
                tab_of(vec![
                    pane(&s, TabState::Connected),
                    pane(&s, TabState::Disconnected("eof".into())),
                ]),
                tab_of(vec![pane(&l, TabState::Exited(Some(0)))]),
                tab_of(vec![
                    pane(&l, TabState::Exited(None)),
                    pane(&s, TabState::Disconnected(String::new())),
                ]),
                tab_of(vec![
                    pane(&s, TabState::Connected),
                    pane(&l, TabState::Connecting),
                    pane(&l, TabState::NotConnected),
                ]),
            ],
        );
        let tabs = &app.windows[&wid].tabs;
        assert_eq!(app.revive_kind(&tabs[0]), Some(Revive::Reconnect));
        assert_eq!(app.revive_kind(&tabs[1]), Some(Revive::Restart));
        assert_eq!(app.revive_kind(&tabs[2]), Some(Revive::Reconnect));
        assert_eq!(app.revive_kind(&tabs[3]), None);
        // A pane's own bar names what that pane needs.
        assert_eq!(app.pane_revive(&tabs[2].panes[0]), Some(Revive::Restart));
        assert_eq!(app.pane_revive(&tabs[2].panes[1]), Some(Revive::Reconnect));
        assert_eq!(app.pane_revive(&tabs[3].panes[0]), None);
    }

    #[test]
    fn the_tab_menu_offers_reconnect_only_for_a_dropped_tab_with_the_chord_on_the_active_one() {
        let (s, l) = (ssh("prod"), profile("sh"));
        let (mut app, wid) = app_with(
            &[s.clone(), l.clone()],
            vec![
                tab_of(vec![pane(&s, TabState::Disconnected("eof".into()))]),
                tab_of(vec![pane(&s, TabState::Connected)]),
                tab_of(vec![pane(&l, TabState::Exited(Some(2)))]),
            ],
        );
        let chord = app.bindings.label(Action::Reconnect).unwrap();
        let _ = app.open_tab_menu(wid, 0, Point::ORIGIN);
        let m = app.context_menu.clone().unwrap();
        assert!(m.items.contains(&(MenuItem::Reconnect, true)));
        assert_eq!(
            m.chord_label(MenuItem::Reconnect).as_deref(),
            Some(chord.as_str())
        );
        let _ = app.open_tab_menu(wid, 1, Point::ORIGIN);
        let m = app.context_menu.clone().unwrap();
        assert!(
            !m.items
                .iter()
                .any(|(i, _)| matches!(i, MenuItem::Reconnect | MenuItem::Restart))
        );
        // A background tab: the chord acts on the active tab, so no hint.
        let _ = app.open_tab_menu(wid, 2, Point::ORIGIN);
        let m = app.context_menu.clone().unwrap();
        assert!(m.items.contains(&(MenuItem::Restart, true)));
        assert!(m.chord_label(MenuItem::Restart).is_none());
    }

    #[test]
    fn choosing_reconnect_restarts_every_dropped_pane_of_that_tab_only() {
        let s = ssh("prod");
        let (mut app, wid) = app_with(
            std::slice::from_ref(&s),
            vec![
                tab_of(vec![pane(&s, TabState::Connected)]),
                tab_of(vec![
                    pane(&s, TabState::Disconnected("eof".into())),
                    pane(&s, TabState::Connected),
                    pane(&s, TabState::Disconnected("eof".into())),
                ]),
                tab_of(vec![pane(&s, TabState::Disconnected("eof".into()))]),
            ],
        );
        let _ = app.open_tab_menu(wid, 1, Point::ORIGIN);
        let _ = app.update(Message::ContextMenu(
            wid,
            MenuMsg::Choose(MenuItem::Reconnect),
        ));
        assert_eq!(state(&app, wid, 1, 0), TabState::Connecting);
        assert_eq!(state(&app, wid, 1, 1), TabState::Connected);
        assert_eq!(state(&app, wid, 1, 2), TabState::Connecting);
        assert_eq!(state(&app, wid, 2, 0), TabState::Disconnected("eof".into()));
        assert!(app.context_menu.is_none());
    }

    #[test]
    fn the_reconnect_shortcut_acts_on_the_active_tab() {
        let s = ssh("prod");
        let (mut app, wid) = app_with(
            std::slice::from_ref(&s),
            vec![
                tab_of(vec![pane(&s, TabState::Connected)]),
                tab_of(vec![pane(&s, TabState::Disconnected("eof".into()))]),
            ],
        );
        // The active tab is connected: nothing happens anywhere.
        let _ = app.update(Message::Shortcut(wid, Action::Reconnect));
        assert_eq!(state(&app, wid, 0, 0), TabState::Connected);
        assert_eq!(state(&app, wid, 1, 0), TabState::Disconnected("eof".into()));
        app.windows.get_mut(&wid).unwrap().active = 1;
        let _ = app.update(Message::Shortcut(wid, Action::Reconnect));
        assert_eq!(state(&app, wid, 1, 0), TabState::Connecting);
    }

    #[test]
    fn the_bar_button_reconnects_its_own_pane_only() {
        let s = ssh("prod");
        let (mut app, wid) = app_with(
            std::slice::from_ref(&s),
            vec![tab_of(vec![
                pane(&s, TabState::Disconnected("eof".into())),
                pane(&s, TabState::Disconnected("eof".into())),
                pane(&s, TabState::Connected),
            ])],
        );
        let ids: Vec<_> = app.windows[&wid].tabs[0]
            .panes
            .iter()
            .map(|p| p.id)
            .collect();
        let _ = app.update(Message::Revive(wid, ids[1]));
        assert_eq!(state(&app, wid, 0, 0), TabState::Disconnected("eof".into()));
        assert_eq!(state(&app, wid, 0, 1), TabState::Connecting);
        // A live pane is never restarted by a stale click.
        let _ = app.update(Message::Revive(wid, ids[2]));
        assert_eq!(state(&app, wid, 0, 2), TabState::Connected);
    }
}
