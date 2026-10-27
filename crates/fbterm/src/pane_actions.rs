//! What the pane actions do: split, close, directional focus, per-pane zoom and maximize.

use fbterm_config::ProfileId;
use iced::{Rectangle, Size, Task, window};

use crate::app::{App, Message};
use crate::layout::{Dir, PaneId, Side};
use crate::tabs::{Pane, WindowState};

/// Smallest and largest font size a zoomed pane may reach, in points.
const ZOOM_RANGE: (f32, f32) = (6.0, 48.0);

/// How a zoom action changes the focused pane's text size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Zoom {
    In,
    Out,
    Reset,
}

impl App {
    /// Split the active tab's focused pane: a new pane of the same profile on `side` of it,
    /// focused. Refused (silently) when a pane would end up below the minimum size. Splitting a
    /// maximized pane shows the others again first.
    pub(crate) fn split_pane(&mut self, wid: window::Id, side: Side) -> Task<Message> {
        self.split_with(wid, side, None)
    }

    /// Split the focused pane with a new pane of `profile` on `side` of it (the palette's
    /// Shift+Enter).
    pub(crate) fn open_in_split(
        &mut self,
        wid: window::Id,
        profile: ProfileId,
        side: Side,
    ) -> Task<Message> {
        self.split_with(wid, side, Some(profile))
    }

    /// The shared split: the new pane copies the focused pane's profile unless `profile` is
    /// given.
    fn split_with(
        &mut self,
        wid: window::Id,
        side: Side,
        profile: Option<ProfileId>,
    ) -> Task<Message> {
        let area = self.areas.get(&wid).copied();
        let Some(tab) = self
            .windows
            .get_mut(&wid)
            .and_then(WindowState::active_tab_mut)
        else {
            return Task::none();
        };
        let focused = tab.focused;
        let new = match profile {
            Some(id) => {
                let Some(p) = self.profiles.get(id) else {
                    return Task::none();
                };
                Pane::for_profile(p)
            }
            None => {
                let from = tab.focused_pane();
                Pane {
                    accent_style: from.accent_style,
                    scheme: from.scheme.clone(),
                    ..Pane::new(from.profile, from.name.clone(), from.accent)
                }
            }
        };
        let id = new.id;
        let mut layout = tab.layout.clone();
        layout.split(focused, side, id);
        // The cell size is the focused pane's; unknown before the first layout (nothing to
        // check against then).
        if let (Some(area), Some(cell)) = (area, self.grids.get(&focused))
            && !layout.fits(
                Rectangle::with_size(area),
                Size::new(cell.cell_width, cell.cell_height),
            )
        {
            return Task::none();
        }
        let unhidden = tab.maximized.take().is_some();
        if let Some(id) = profile {
            self.recency.bump(id, crate::app::unix_now());
        }
        let Some(tab) = self
            .windows
            .get_mut(&wid)
            .and_then(WindowState::active_tab_mut)
        else {
            return Task::none();
        };
        tab.layout = layout;
        tab.panes.push(new);
        // Start it before it takes focus, so its grid comes from its own rectangle and the
        // focused pane's cell size.
        let started = self.start(wid, id);
        if let Some(tab) = self
            .windows
            .get_mut(&wid)
            .and_then(WindowState::active_tab_mut)
        {
            tab.focus(id);
        }
        self.save_state();
        let shown = if unhidden {
            self.shown(wid)
        } else {
            Task::none()
        };
        Task::batch([started, shown])
    }

    /// Close the active tab's focused pane; the last pane closes the tab. The pane's terminal
    /// goes through the release path of a closed tab: the memory goes back to the OS once the
    /// last reference to it drops.
    pub(crate) fn close_focused_pane(&mut self, wid: window::Id) -> Task<Message> {
        let Some(focused) = self
            .windows
            .get(&wid)
            .and_then(WindowState::active_tab)
            .map(|t| t.focused)
        else {
            return Task::none();
        };
        // Running transfers it would end are asked about first.
        if self.ask_before_closing(wid, &[focused], crate::prompts::Closing::Pane(focused)) {
            return Task::none();
        }
        self.close_pane(wid, focused)
    }

    /// Close `pane` of `wid`; the last pane of a tab closes the tab.
    pub(crate) fn close_pane(&mut self, wid: window::Id, pane: PaneId) -> Task<Message> {
        let Some(window) = self.windows.get_mut(&wid) else {
            return Task::none();
        };
        let Some((index, _)) = window.locate(pane) else {
            return Task::none();
        };
        let tab = &mut window.tabs[index];
        if tab.panes.len() < 2 {
            return self.close_tab(wid, index);
        }
        let Some(pane) = tab.close_pane(pane) else {
            return Task::none();
        };
        let freed = crate::memory::weak_terminals(std::iter::once(&pane));
        self.forget_panes(&[pane.id]);
        drop(pane);
        self.sync_all_shell_tunnels();
        self.save_state();
        self.release_closed(freed);
        // Takes the pending redraw of the panes now shown (a maximized pane that closed shows
        // the others again).
        self.shown(wid)
    }

    /// Focus `pane` in the active tab of `wid` (a click, or directional focus).
    pub(crate) fn focus_pane(&mut self, wid: window::Id, pane: PaneId) -> Task<Message> {
        let changed = self
            .windows
            .get_mut(&wid)
            .and_then(WindowState::active_tab_mut)
            .is_some_and(|tab| tab.focus(pane));
        if !changed {
            return Task::none();
        }
        self.save_state();
        self.shown(wid)
    }

    /// Move focus to the pane next to the focused one in `dir`. With a pane maximized there is
    /// nothing next to it.
    pub(crate) fn focus_direction(&mut self, wid: window::Id, dir: Dir) -> Task<Message> {
        let Some(area) = self.areas.get(&wid).copied() else {
            return Task::none();
        };
        let Some(tab) = self.windows.get(&wid).and_then(WindowState::active_tab) else {
            return Task::none();
        };
        let next =
            tab.shown_layout()
                .neighbour(Rectangle::with_size(area), tab.focused, dir, &tab.recent);
        match next {
            Some(pane) => self.focus_pane(wid, pane),
            None => Task::none(),
        }
    }

    /// Change the focused pane's text size by one point (or back to the Settings size); the
    /// other panes keep theirs.
    pub(crate) fn zoom_pane(&mut self, wid: window::Id, change: Zoom) -> Task<Message> {
        let size = self.settings.font_size;
        let Some(tab) = self
            .windows
            .get_mut(&wid)
            .and_then(WindowState::active_tab_mut)
        else {
            return Task::none();
        };
        let pane = tab.focused_pane_mut();
        let wanted = match change {
            Zoom::In => pane.zoom.saturating_add(1),
            Zoom::Out => pane.zoom.saturating_sub(1),
            Zoom::Reset => 0,
        };
        // Whole steps that keep the zoomed size inside the range.
        let lowest = (ZOOM_RANGE.0 - size).ceil().clamp(-127.0, 0.0) as i8;
        let highest = (ZOOM_RANGE.1 - size).floor().clamp(0.0, 127.0) as i8;
        let zoom = wanted.clamp(lowest, highest);
        if zoom == pane.zoom {
            return Task::none();
        }
        pane.zoom = zoom;
        self.save_state();
        Task::none()
    }

    /// Let the focused pane fill the tab, or give the tab back to all its panes.
    pub(crate) fn toggle_maximize(&mut self, wid: window::Id) -> Task<Message> {
        let Some(tab) = self
            .windows
            .get_mut(&wid)
            .and_then(WindowState::active_tab_mut)
        else {
            return Task::none();
        };
        if tab.panes.len() < 2 {
            return Task::none();
        }
        tab.maximized = match tab.maximized {
            Some(_) => None,
            None => Some(tab.focused),
        };
        self.save_state();
        // Panes that were hidden left their redraws pending: take them now.
        self.shown(wid)
    }
}

#[cfg(test)]
mod tests {
    use fbterm_config::Profile;
    use fbterm_term::GridSize;
    use iced::{Size, window};

    use crate::app::{App, Message};
    use crate::keys::Action;
    use crate::layout::{Axis, Layout, PaneId};
    use crate::restore_tests::{paths_with, profile};
    use crate::tabs::{Pane, Tab, WindowState};
    use crate::tiles::TilesMsg;

    const CELL: (f32, f32) = (9.0, 18.0);

    /// An app with one window holding one tab with one pane of profile `p`, laid out in an
    /// area of `area` logical pixels.
    fn app_with(p: &Profile, area: Size) -> (App, window::Id, PaneId) {
        let (mut app, _) = App::build(paths_with(std::slice::from_ref(p)), true);
        app.state_saving_enabled = false;
        let wid = window::Id::unique();
        let first = Pane::for_profile(p);
        let a = first.id;
        let mut w = WindowState::new();
        w.push(Tab::single(first));
        app.windows.clear();
        app.windows.insert(wid, w);
        let _ = app.update(Message::Tiles(wid, TilesMsg::Area(area)));
        app.grids.insert(a, grid(area));
        (app, wid, a)
    }

    fn grid(area: Size) -> GridSize {
        GridSize {
            cols: (area.width / CELL.0) as u16,
            rows: (area.height / CELL.1) as u16,
            cell_width: CELL.0,
            cell_height: CELL.1,
        }
    }

    fn big() -> Size {
        Size::new(984.0, 600.0)
    }

    fn act(app: &mut App, wid: window::Id, action: Action) {
        let _ = app.update(Message::Shortcut(wid, action));
    }

    fn tab(app: &App, wid: window::Id) -> &Tab {
        app.windows[&wid].active_tab().unwrap()
    }

    fn font(app: &App, wid: window::Id, pane: PaneId) -> f32 {
        let t = tab(app, wid);
        app.pane_style(wid, t, t.pane(pane).unwrap()).font_size
    }

    #[test]
    fn shift_enter_opens_profile_in_split() {
        use crate::palette::PaletteMsg;
        let p = profile("a");
        let other = profile("b");
        let (mut app, _) = App::build(paths_with(&[p.clone(), other.clone()]), true);
        app.state_saving_enabled = false;
        let wid = window::Id::unique();
        let first = Pane::for_profile(&p);
        let a = first.id;
        let mut w = WindowState::new();
        w.push(Tab::single(first));
        app.windows.clear();
        app.windows.insert(wid, w);
        let _ = app.update(Message::Tiles(wid, TilesMsg::Area(big())));
        app.grids.insert(a, grid(big()));
        let _ = app.open_palette(wid, "b".into());
        let _ = app.update(Message::Palette(wid, PaletteMsg::Submit { split: true }));
        let t = tab(&app, wid);
        assert_eq!(app.windows[&wid].tabs.len(), 1, "no new tab");
        assert_eq!(t.panes.len(), 2);
        assert_eq!(t.layout.panes()[0], a, "to the right of the focused pane");
        assert_eq!(t.focused_pane().profile, other.id);
        assert!(!app.palettes.contains_key(&wid));
        // Plain Enter still opens a tab.
        let _ = app.open_palette(wid, "b".into());
        let _ = app.update(Message::Palette(wid, PaletteMsg::Submit { split: false }));
        assert_eq!(app.windows[&wid].tabs.len(), 2);
    }

    #[test]
    fn shift_enter_refuses_a_split_below_the_minimum_size() {
        use crate::palette::PaletteMsg;
        let p = profile("a");
        let other = profile("b");
        let (mut app, _) = App::build(paths_with(&[p.clone(), other.clone()]), true);
        app.state_saving_enabled = false;
        let wid = window::Id::unique();
        let first = Pane::for_profile(&p);
        let a = first.id;
        let mut w = WindowState::new();
        w.push(Tab::single(first));
        app.windows.clear();
        app.windows.insert(wid, w);
        let tiny = Size::new(40.0, 40.0);
        let _ = app.update(Message::Tiles(wid, TilesMsg::Area(tiny)));
        app.grids.insert(a, grid(tiny));
        let _ = app.open_palette(wid, "b".into());
        let _ = app.update(Message::Palette(wid, PaletteMsg::Submit { split: true }));
        assert_eq!(tab(&app, wid).panes.len(), 1);
        assert_eq!(app.windows[&wid].tabs.len(), 1);
    }

    #[test]
    fn split_right_adds_pane_of_same_profile() {
        let p = profile("a");
        let (mut app, wid, a) = app_with(&p, big());
        act(&mut app, wid, Action::SplitRight);
        let t = tab(&app, wid);
        assert_eq!(t.panes.len(), 2);
        let b = t.focused;
        assert_ne!(b, a, "the new pane takes focus");
        assert_eq!(t.pane(b).unwrap().profile, p.id);
        assert_eq!(t.layout.panes(), vec![a, b]);
        assert!(matches!(
            t.layout,
            Layout::Split {
                axis: Axis::Row,
                ..
            }
        ));
        assert_eq!(t.recent, vec![b, a]);
        assert!(
            t.pane(b).unwrap().terminal().is_some(),
            "its session started"
        );

        act(&mut app, wid, Action::SplitDown);
        let t = tab(&app, wid);
        assert_eq!(t.panes.len(), 3);
        let c = t.focused;
        assert_eq!(t.layout.panes(), vec![a, b, c]);
        assert!(
            matches!(&t.layout, Layout::Split { second, .. }
                if matches!(**second, Layout::Split { axis: Axis::Column, .. })),
            "the new pane is below the focused one"
        );
    }

    #[test]
    fn a_split_that_would_squeeze_a_pane_is_refused() {
        let p = profile("a");
        // 20 columns of 9 px do not fit twice into 300 px.
        let (mut app, wid, _) = app_with(&p, Size::new(300.0, 600.0));
        act(&mut app, wid, Action::SplitRight);
        assert_eq!(tab(&app, wid).panes.len(), 1);
        // Nor five rows of 18 px twice into 150 px.
        let (mut app, wid, _) = app_with(&p, Size::new(984.0, 150.0));
        act(&mut app, wid, Action::SplitDown);
        assert_eq!(tab(&app, wid).panes.len(), 1);
    }

    #[test]
    fn equalize_refused_when_it_would_not_fit() {
        use crate::tiles::TilesMsg;
        let p = profile("a");
        let (mut app, wid, a) = app_with(&p, big());
        // a | (b | c)
        act(&mut app, wid, Action::SplitRight);
        act(&mut app, wid, Action::SplitRight);
        let focused = tab(&app, wid).focused;
        app.grids.insert(focused, grid(big()));
        let ratio = |app: &App| match &tab(app, wid).layout {
            Layout::Split { ratio, .. } => *ratio,
            Layout::Pane(_) => panic!("split"),
        };
        let _ = a;
        // Narrow window; the outer divider sits left, so b | c just fit.
        let _ = app.update(Message::Tiles(wid, TilesMsg::Area(Size::new(700.0, 600.0))));
        let _ = app.update(Message::Tiles(
            wid,
            TilesMsg::DragDivider {
                path: vec![],
                ratio: 0.4,
            },
        ));
        assert_eq!(ratio(&app), 0.4);
        // Halving would leave b | c 350 px, below two 20 column panes (360 px).
        let _ = app.update(Message::Tiles(wid, TilesMsg::Equalize(vec![])));
        assert_eq!(ratio(&app), 0.4, "equalize keeps the old ratio");
        // Where it fits, equalize works.
        let _ = app.update(Message::Tiles(wid, TilesMsg::Area(Size::new(984.0, 600.0))));
        let _ = app.update(Message::Tiles(wid, TilesMsg::Equalize(vec![])));
        assert_eq!(ratio(&app), 0.5);
    }

    #[test]
    fn close_pane_keeps_tab_until_last() {
        let p = profile("a");
        let (mut app, wid, a) = app_with(&p, big());
        let other = Tab::single(Pane::for_profile(&p));
        app.windows.get_mut(&wid).unwrap().tabs.push(other);
        app.windows.get_mut(&wid).unwrap().active = 0;
        act(&mut app, wid, Action::SplitRight);
        let b = tab(&app, wid).focused;
        act(&mut app, wid, Action::CloseTab);
        let w = &app.windows[&wid];
        assert_eq!(w.tabs.len(), 2, "the tab stays");
        assert_eq!(w.tabs[0].panes.len(), 1);
        assert_eq!(w.tabs[0].focused, a);
        assert_eq!(w.tabs[0].layout, Layout::Pane(a));
        assert!(!app.grids.contains_key(&b), "its size is forgotten");
        // The last pane closes the tab.
        act(&mut app, wid, Action::CloseTab);
        let w = &app.windows[&wid];
        assert_eq!(w.tabs.len(), 1);
        assert_ne!(w.tabs[0].focused, a);
    }

    #[test]
    fn focus_moves_by_direction() {
        let p = profile("a");
        let (mut app, wid, a) = app_with(&p, big());
        // a | (b over c), c focused.
        act(&mut app, wid, Action::SplitRight);
        let b = tab(&app, wid).focused;
        act(&mut app, wid, Action::SplitDown);
        let c = tab(&app, wid).focused;
        let focused = |app: &App| tab(app, wid).focused;
        act(&mut app, wid, Action::FocusUp);
        assert_eq!(focused(&app), b);
        act(&mut app, wid, Action::FocusLeft);
        assert_eq!(focused(&app), a);
        act(&mut app, wid, Action::FocusLeft);
        assert_eq!(focused(&app), a, "nothing further left");
        // b and c are equally near: the one focused more recently wins.
        act(&mut app, wid, Action::FocusRight);
        assert_eq!(focused(&app), b);
        act(&mut app, wid, Action::FocusDown);
        assert_eq!(focused(&app), c);
        assert_eq!(tab(&app, wid).recent[..2], [c, b]);
        act(&mut app, wid, Action::FocusDown);
        assert_eq!(focused(&app), c, "nothing further down");
    }

    #[test]
    fn zoom_changes_only_focused_pane() {
        let p = profile("a");
        let (mut app, wid, a) = app_with(&p, big());
        act(&mut app, wid, Action::SplitRight);
        let b = tab(&app, wid).focused;
        assert_eq!(app.settings.font_size, 13.0);
        assert_eq!((font(&app, wid, a), font(&app, wid, b)), (13.0, 13.0));
        act(&mut app, wid, Action::ZoomIn);
        assert_eq!((font(&app, wid, a), font(&app, wid, b)), (13.0, 14.0));
        act(&mut app, wid, Action::ZoomIn);
        act(&mut app, wid, Action::ZoomOut);
        assert_eq!((font(&app, wid, a), font(&app, wid, b)), (13.0, 14.0));
        act(&mut app, wid, Action::ZoomReset);
        assert_eq!((font(&app, wid, a), font(&app, wid, b)), (13.0, 13.0));
        assert_eq!(tab(&app, wid).pane(b).unwrap().zoom, 0);
        // The ends of the range hold.
        for _ in 0..100 {
            act(&mut app, wid, Action::ZoomIn);
        }
        assert_eq!(font(&app, wid, b), 48.0);
        for _ in 0..100 {
            act(&mut app, wid, Action::ZoomOut);
        }
        assert_eq!(font(&app, wid, b), 6.0);
        act(&mut app, wid, Action::ZoomReset);
        assert_eq!(font(&app, wid, b), 13.0);
    }

    #[test]
    fn maximize_toggles_visible_panes() {
        let p = profile("a");
        let (mut app, wid, a) = app_with(&p, big());
        // One pane: nothing to maximize.
        act(&mut app, wid, Action::MaximizePane);
        assert_eq!(tab(&app, wid).maximized, None);
        act(&mut app, wid, Action::SplitRight);
        let b = tab(&app, wid).focused;
        assert_eq!(tab(&app, wid).visible_panes(), vec![a, b]);
        act(&mut app, wid, Action::MaximizePane);
        assert_eq!(tab(&app, wid).visible_panes(), vec![b]);
        // Moving focus around a maximized pane does nothing.
        act(&mut app, wid, Action::FocusLeft);
        assert_eq!(tab(&app, wid).focused, b);
        act(&mut app, wid, Action::MaximizePane);
        assert_eq!(tab(&app, wid).visible_panes(), vec![a, b]);
        // A split un-maximizes first.
        act(&mut app, wid, Action::MaximizePane);
        act(&mut app, wid, Action::SplitDown);
        assert_eq!(tab(&app, wid).maximized, None);
        assert_eq!(tab(&app, wid).visible_panes().len(), 3);
    }

    #[test]
    fn rearranging_needs_something_to_rearrange() {
        let p = profile("a");
        let (mut app, wid, _) = app_with(&p, big());
        // One tab of one pane: no other tab to merge into, no tile to move.
        let before = tab(&app, wid).layout.clone();
        for a in [Action::MergeTab, Action::MoveTile, Action::TileToTab] {
            act(&mut app, wid, a);
        }
        assert_eq!(tab(&app, wid).layout, before);
        assert_eq!(app.windows[&wid].tabs.len(), 1);
        assert!(app.palettes.is_empty());
        assert!(app.find_box.is_none());
    }

    #[test]
    fn closing_pane_requests_release_after_last_reference() {
        let p = profile("a");
        let (mut app, wid, _) = app_with(&p, big());
        act(&mut app, wid, Action::SplitRight);
        let b = tab(&app, wid).focused;
        let held = tab(&app, wid).pane(b).unwrap().terminal().cloned().unwrap();
        let before = crate::memory::requests();
        act(&mut app, wid, Action::CloseTab);
        assert_eq!(
            crate::memory::requests(),
            before,
            "something still holds the terminal"
        );
        assert_eq!(app.pending_release.len(), 1, "waits for the session task");
        let released = || {
            Message::Session(
                fbterm_session::SessionId::next(),
                fbterm_session::SessionEvent::Released,
            )
        };
        let _ = app.update(released());
        assert_eq!(
            crate::memory::requests(),
            before,
            "still held: keeps waiting"
        );
        drop(held);
        let _ = app.update(released());
        assert_eq!(crate::memory::requests(), before + 1);
        assert!(app.pending_release.is_empty());
        let _ = app.update(released());
        assert_eq!(crate::memory::requests(), before + 1, "no second request");

        // With nothing else holding it, closing requests the release at once.
        act(&mut app, wid, Action::SplitDown);
        let before = crate::memory::requests();
        act(&mut app, wid, Action::CloseTab);
        assert_eq!(crate::memory::requests(), before + 1);
    }
}
