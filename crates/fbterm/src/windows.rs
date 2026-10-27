//! Opening, restoring and closing windows and tabs, quitting, and saving the layout; merging a
//! tab into another and turning a tile into a tab (by drag or from the palette).

use std::time::Duration;

use fbterm_config::{ProfileId, SavedWindow};
use iced::{Task, window};

use crate::app::{App, Message, unix_now};
use crate::chrome::Chrome;
use crate::layout::{Drop, Layout, PaneId};
use crate::prompts::Closing;
use crate::restore;
use crate::tabs::{self, Pane, Tab, TabId, TabState, WindowState};

/// How long a carried tab hovers over another tab's button before that tab is shown.
pub(crate) const HOVER_SWITCH: Duration = Duration::from_millis(500);

/// What is being carried across a window: a tab picked up from the tab bar, or a tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Carry {
    Tab(TabId),
    Tile(PaneId),
}

/// What the tab bar and the tiles report about a drag that crosses between them.
#[derive(Debug, Clone, PartialEq)]
pub enum DragMsg {
    /// A tab button was pressed and moved past the drag threshold: it is carried.
    StartTab(TabId),
    /// The carried tab is over this tab button (`None`: over none).
    OverTab(Option<usize>),
    /// A hover over a tab button (its generation) lasted `HOVER_SWITCH`.
    HoverElapsed(u64),
    /// The carried tab was dropped on the content of the shown tab.
    DropTab { tab: TabId, at: Drop },
    /// A tile is being carried (`None`: no longer).
    Tile(Option<PaneId>),
    /// A carried tile was dropped on the tab bar: a tab of its own at `index`.
    TileToTab { pane: PaneId, index: usize },
    /// The drag ended (released, Escape, the pointer or focus left).
    End,
}

impl App {
    /// A new terminal window of `size` (logical pixels), with no tabs yet.
    pub(crate) fn new_window(&mut self, size: (f32, f32)) -> (window::Id, Task<Message>) {
        let chrome = Chrome::current();
        let backdrop = chrome.backdrop(
            self.settings.background_opacity,
            self.settings.background_blur,
        );
        let (id, open) = crate::chrome::open_window_as(
            chrome,
            window::Settings {
                size: iced::Size::new(size.0, size.1),
                min_size: Some(iced::Size::new(360.0, 200.0)),
                exit_on_close_request: false,
                transparent: backdrop.transparent,
                blur: backdrop.blur,
                ..window::Settings::default()
            },
        );
        self.windows.insert(
            id,
            WindowState {
                transparent: backdrop.transparent,
                ..WindowState::new()
            },
        );
        self.sizes.insert(id, size);
        (id, open.map(Message::WindowOpened))
    }

    /// Reopen the saved windows with their tabs not connected, then connect the active tab of
    /// each; the others connect when first selected (see [`App::shown`]). With nothing to
    /// restore, the default single window opens.
    pub(crate) fn restore_windows(&mut self, plan: &[SavedWindow]) -> Task<Message> {
        if plan.is_empty() {
            return self.open_window(self.default_profile());
        }
        let mut tasks = Vec::new();
        for saved in plan {
            let (wid, open) = self.new_window(saved.size);
            let window = self.windows.get_mut(&wid).expect("just inserted");
            for tab in &saved.tabs {
                if let Some(tab) = restore::build_tab(tab, &self.profiles) {
                    window.tabs.push(tab);
                }
            }
            window.active = saved.active.min(window.tabs.len().saturating_sub(1));
            tracing::info!(
                tabs = window.tabs.len(),
                active = window.active,
                "restoring window"
            );
            tasks.push(open);
            tasks.push(self.shown(wid));
        }
        Task::batch(tasks)
    }

    pub fn open_window(&mut self, profile: Option<ProfileId>) -> Task<Message> {
        let (id, open) = self.new_window(restore::DEFAULT_SIZE);
        let start = match profile {
            Some(p) => self.open_tab(id, p),
            None => Task::none(),
        };
        open.chain(start)
    }

    pub fn open_tab(&mut self, wid: window::Id, profile: ProfileId) -> Task<Message> {
        let Some(p) = self.profiles.get(profile) else {
            return Task::none();
        };
        let tab = Tab::single(Pane::for_profile(p));
        self.recency.bump(profile, unix_now());
        let id = tab.focused;
        let Some(window) = self.windows.get_mut(&wid) else {
            return Task::none();
        };
        window.push(tab);
        self.save_state();
        self.start(wid, id)
    }

    /// The tab's close button: a tab whose closing would end running transfers asks first
    /// (`Dialog::Transfers`), then a tab with several live panes (`Dialog::CloseTab`);
    /// anything else closes at once. Bulk closes call `close_tab` directly.
    pub(crate) fn close_tab_asking(&mut self, wid: window::Id, index: usize) -> Task<Message> {
        let Some(tab) = self.windows.get(&wid).and_then(|w| w.tabs.get(index)) else {
            return Task::none();
        };
        let (id, panes): (TabId, Vec<PaneId>) = (tab.id, tab.panes.iter().map(|p| p.id).collect());
        if self.ask_before_closing(wid, &panes, Closing::Tab(id)) {
            return Task::none();
        }
        let Some(tab) = self.windows.get(&wid).and_then(|w| w.tabs.get(index)) else {
            return Task::none();
        };
        if tab.live_panes() < 2 {
            return self.close_tab(wid, index);
        }
        let (id, panes) = (tab.id, tab.panes.len());
        if !self.prompts.has_close(wid, id) {
            self.prompts.push(
                wid,
                None,
                crate::prompts::Dialog::CloseTab { tab: id, panes },
            );
        }
        Task::none()
    }

    /// Close the tab at `index` (the user agreed to close it), unless running transfers would
    /// end with it: then that is asked first.
    pub(crate) fn close_tab_if_no_transfers(
        &mut self,
        wid: window::Id,
        index: usize,
    ) -> Task<Message> {
        let Some(tab) = self.windows.get(&wid).and_then(|w| w.tabs.get(index)) else {
            return Task::none();
        };
        let (id, panes): (TabId, Vec<PaneId>) = (tab.id, tab.panes.iter().map(|p| p.id).collect());
        if self.ask_before_closing(wid, &panes, Closing::Tab(id)) {
            return Task::none();
        }
        self.close_tab(wid, index)
    }

    pub(crate) fn close_tab(&mut self, wid: window::Id, index: usize) -> Task<Message> {
        let Some(window) = self.windows.get_mut(&wid) else {
            return Task::none();
        };
        let Some(closed) = window.close(index) else {
            return Task::none();
        };
        let freed = crate::memory::weak_terminals(closed.tab.panes.iter());
        let gone: Vec<_> = closed.tab.panes.iter().map(|p| p.id).collect();
        self.forget_panes(&gone);
        drop(closed.tab);
        self.sync_all_shell_tunnels();
        self.save_state();
        self.release_closed(freed);
        if closed.window_empty {
            self.close_window(wid)
        } else {
            self.shown(wid)
        }
    }

    /// A tab or window the user closed freed `terminals`: request the memory release once none
    /// is held any more. The session task may still hold its reference for a moment (or, with a
    /// background job on the PTY, for long), so a held terminal waits in `pending_release` for
    /// `SessionEvent::Released` (see `release_swept`); nothing polls. Only user closes come
    /// here, never output or the end of a program.
    pub(crate) fn release_closed(&mut self, terminals: Vec<crate::memory::WeakTerminal>) {
        if terminals.is_empty() {
            return;
        }
        if terminals.iter().all(|t| t.strong_count() == 0) {
            crate::memory::release_freed();
        } else {
            self.pending_release.push(terminals);
        }
    }

    /// A closed session task let go of its terminal: request the release if that freed the
    /// last terminal of a tab the user closed (one request however many groups finished).
    pub(crate) fn release_swept(&mut self) {
        let before = self.pending_release.len();
        self.pending_release
            .retain(|g| g.iter().any(|t| t.strong_count() > 0));
        if self.pending_release.len() < before {
            crate::memory::release_freed();
        }
    }

    /// The active tab of `wid` was just shown (selected, created, or its window focused): take
    /// the redraws its sessions left pending while hidden (the redraw follows this update), and
    /// start every pane that never was (restored tabs connect on first focus).
    pub(crate) fn shown(&mut self, wid: window::Id) -> Task<Message> {
        let Some(window) = self.windows.get(&wid) else {
            return Task::none();
        };
        tabs::on_shown(window);
        let waiting: Vec<_> = window
            .active_tab()
            .map(|t| {
                t.panes
                    .iter()
                    .filter(|p| p.state == TabState::NotConnected)
                    .map(|p| p.id)
                    .collect()
            })
            .unwrap_or_default();
        Task::batch(
            waiting
                .into_iter()
                .map(|id| self.start(wid, id))
                .collect::<Vec<_>>(),
        )
    }

    /// Write `state.toml`: windows, tabs, selection, sizes and recency. Does nothing for debug
    /// runs (see `state_saving_enabled`) and while `profiles.toml` is unreadable (the tabs would
    /// look orphaned; the file keeps the layout until the profiles load again).
    pub(crate) fn save_state(&self) {
        if !self.state_saving_enabled || self.quitting || self.profiles_error.is_some() {
            return;
        }
        let mut state = restore::snapshot(&self.windows, &self.sizes, &self.recency);
        state.file_panel_width = self.file_panels.saved_width();
        state.file_panel_show_hidden = self.file_panels.saved_show_hidden();
        if let Err(e) = state.save(&self.paths.state()) {
            tracing::warn!(error = %e, "could not save the session state");
        }
    }

    /// Close a window on purpose (its `Closed` event then is not an OS teardown).
    pub(crate) fn close_window(&mut self, id: window::Id) -> Task<Message> {
        self.closing.insert(id);
        window::close(id)
    }

    /// How windows close and the app quits, as far as `state.toml` is concerned:
    /// - Quit (Cmd+Q / Ctrl+Shift+Q / palette) saves once while every window still exists, then
    ///   closes them all and exits; nothing saves after that, so every window comes back.
    /// - The close button on the last terminal window is Quit. On any other terminal window it
    ///   drops that window from the layout. Closing the last tab of the last window leaves no
    ///   tabs to save, so the next launch opens the default window.
    /// - macOS: while transfers run, `Cmd+Q` is taken from the app menu (`App::sync_quit_key`),
    ///   so the key reaches `quit_asking` like any shortcut and asks first. Otherwise, and when
    ///   Quit is chosen in the app menu with the mouse, it is `terminate:`: winit closes the
    ///   windows without a close request and the process ends; windows closing unasked count as a quit, and
    ///   state.toml is already current because it is saved on every layout change.
    pub(crate) fn quit(&mut self) -> Task<Message> {
        if self.quitting {
            return Task::none();
        }
        // Settings changes still waiting for their quiet period are written now.
        self.write_pending_settings();
        self.save_state();
        self.quitting = true;
        let ids: Vec<_> = self
            .windows
            .keys()
            .copied()
            .chain(self.editor_window())
            .chain(self.settings_window())
            .chain(self.schemes_window())
            .chain(self.tunnels_window())
            .chain(self.archive_window())
            .collect();
        self.closing.extend(ids.iter().copied());
        Task::batch(ids.into_iter().map(window::close)).chain(iced::exit())
    }

    /// The OS close button: other windows close; a terminal window asks first when closing
    /// it would end running transfers; the last terminal window quits (asking the same).
    pub(crate) fn close_requested(&mut self, id: window::Id) -> Task<Message> {
        if self.settings_window() == Some(id) {
            self.write_pending_settings();
        }
        if self.editor_window() == Some(id)
            || self.settings_window() == Some(id)
            || self.schemes_window() == Some(id)
            || self.tunnels_window() == Some(id)
            || self.archive_window() == Some(id)
        {
            self.close_window(id)
        } else if self.windows.len() > 1 {
            if self.ask_before_closing_window(id) {
                return Task::none();
            }
            self.close_window(id)
        } else {
            self.quit_asking()
        }
    }

    pub(crate) fn window_resized(&mut self, id: window::Id, size: iced::Size) -> Task<Message> {
        if !self.windows.contains_key(&id) {
            return Task::none();
        }
        self.sizes.insert(id, (size.width, size.height));
        // Dragging a window edge resizes continuously: save once it settles.
        self.save_state_soon()
    }

    /// Whether an open terminal window was created opaque: a background opacity below 1
    /// shows in it only after a restart (never on macOS, where every window is transparent).
    pub(crate) fn opaque_window_open(&self) -> bool {
        self.windows.values().any(|w| !w.transparent)
    }

    /// What is carried in `wid`, if anything.
    pub(crate) fn carry_in(&self, wid: window::Id) -> Option<Carry> {
        self.carry.filter(|(w, _)| *w == wid).map(|(_, c)| c)
    }

    pub(crate) fn drag_msg(&mut self, wid: window::Id, msg: DragMsg) -> Task<Message> {
        match msg {
            DragMsg::StartTab(tab) => {
                self.carry = Some((wid, Carry::Tab(tab)));
                self.hover = None;
                Task::none()
            }
            DragMsg::OverTab(over) => {
                if !matches!(self.carry_in(wid), Some(Carry::Tab(_))) {
                    return Task::none();
                }
                let Some(index) = over else {
                    self.hover = None;
                    return Task::none();
                };
                self.hover_generation += 1;
                let generation = self.hover_generation;
                self.hover = Some((wid, index, generation));
                Task::perform(
                    async { tokio::time::sleep(HOVER_SWITCH).await },
                    move |()| Message::Drag(wid, DragMsg::HoverElapsed(generation)),
                )
            }
            DragMsg::HoverElapsed(generation) => match self.hover {
                Some((w, index, g))
                    if w == wid
                        && g == generation
                        && matches!(self.carry_in(wid), Some(Carry::Tab(_))) =>
                {
                    self.hover = None;
                    self.with_window(wid, |w| w.select(index))
                }
                _ => Task::none(),
            },
            DragMsg::DropTab { tab, at } => {
                self.end_carry();
                let target = self
                    .windows
                    .get(&wid)
                    .and_then(WindowState::active_tab)
                    .map(|t| t.id);
                match target {
                    Some(target) => self.merge_tab(wid, tab, target, at),
                    None => Task::none(),
                }
            }
            DragMsg::Tile(pane) => {
                match pane {
                    Some(pane) => self.carry = Some((wid, Carry::Tile(pane))),
                    None if matches!(self.carry_in(wid), Some(Carry::Tile(_))) => {
                        self.end_carry();
                    }
                    None => {}
                }
                Task::none()
            }
            DragMsg::TileToTab { pane, index } => {
                self.end_carry();
                self.tile_to_tab(wid, pane, index)
            }
            DragMsg::End => {
                self.end_carry();
                Task::none()
            }
        }
    }

    /// Whether `layout` keeps every pane of `wid` at least `MIN_COLS` x `MIN_ROWS` cells in the
    /// window's pane area, with the cell size of pane `cell_of` (the tab's focused pane, as
    /// splits check). Before the first layout nothing is known and anything goes, as for splits.
    pub(crate) fn fits_in(&self, wid: window::Id, cell_of: PaneId, layout: &Layout) -> bool {
        match (self.areas.get(&wid), self.grids.get(&cell_of)) {
            (Some(area), Some(g)) => layout.fits(
                iced::Rectangle::with_size(*area),
                iced::Size::new(g.cell_width, g.cell_height),
            ),
            _ => true,
        }
    }

    /// The layout tab `target` of `wid` would have with tab `source` merged in at `at`, if
    /// every pane would still fit.
    pub(crate) fn merged_layout(
        &self,
        wid: window::Id,
        source: TabId,
        target: TabId,
        at: Drop,
    ) -> Option<Layout> {
        let w = self.windows.get(&wid)?;
        if source == target {
            return None;
        }
        let (source, target) = (&w.tabs[w.find(source)?], &w.tabs[w.find(target)?]);
        let mut merged = target.layout.clone();
        merged.insert(source.layout.clone(), at);
        self.fits_in(wid, target.focused, &merged).then_some(merged)
    }

    /// The layout `pane`'s tab would have with it moved to `at`, if that changes anything and
    /// every pane would still fit.
    pub(crate) fn moved_layout(&self, wid: window::Id, pane: PaneId, at: Drop) -> Option<Layout> {
        let w = self.windows.get(&wid)?;
        let tab = &w.tabs[w.locate(pane)?.0];
        let mut moved = tab.layout.clone();
        if !moved.move_pane(pane, at) || moved == tab.layout {
            return None;
        }
        self.fits_in(wid, tab.focused, &moved).then_some(moved)
    }

    fn end_carry(&mut self) {
        self.carry = None;
        self.hover = None;
    }

    /// Merge tab `source` into tab `target` of `wid`: the source's whole layout (its
    /// arrangement kept) lands at `at`, its panes move over with their sessions, the source
    /// leaves the bar and focus goes to its focused pane. Nothing closes, so nothing is
    /// released. Merging a tab into itself (or a missing one) does nothing.
    pub(crate) fn merge_tab(
        &mut self,
        wid: window::Id,
        source: TabId,
        target: TabId,
        at: Drop,
    ) -> Task<Message> {
        // Refused when it would squeeze a pane below the minimum (or is no merge at all).
        let Some(merged) = self.merged_layout(wid, source, target, at) else {
            return Task::none();
        };
        let Some(window) = self.windows.get_mut(&wid) else {
            return Task::none();
        };
        // Taken out of the bar, not closed: its panes live on in the target.
        let Some(moved) = window.find(source).and_then(|i| window.close(i)) else {
            return Task::none();
        };
        let moved = moved.tab;
        let Some(index) = window.find(target) else {
            return Task::none();
        };
        let tab = &mut window.tabs[index];
        tab.maximized = None;
        tab.layout = merged;
        tab.panes.extend(moved.panes);
        tab.recent.extend(moved.recent);
        tab.focus(moved.focused);
        window.active = index;
        self.save_state();
        // The panes now on screen take their pending redraws; one never started starts.
        self.shown(wid)
    }

    /// Move the tile `pane` out of its tab into a tab of its own at `index` of `wid`'s bar,
    /// shown. The pane keeps its session. A tab's only pane stays where it is.
    pub(crate) fn tile_to_tab(
        &mut self,
        wid: window::Id,
        pane: PaneId,
        index: usize,
    ) -> Task<Message> {
        let Some(window) = self.windows.get_mut(&wid) else {
            return Task::none();
        };
        let Some((t, _)) = window.locate(pane) else {
            return Task::none();
        };
        // Not a pane close: nothing is released, the pane goes on in its new tab.
        let Some(moved) = window.tabs[t].close_pane(pane) else {
            return Task::none();
        };
        let index = index.min(window.tabs.len());
        window.tabs.insert(index, Tab::single(moved));
        window.active = index;
        self.save_state();
        self.shown(wid)
    }

    /// Move the tile `pane` to `at` within its tab (the palette's "Move tile…"), showing the
    /// whole layout.
    pub(crate) fn move_tile(&mut self, wid: window::Id, pane: PaneId, at: Drop) -> Task<Message> {
        let Some(moved) = self.moved_layout(wid, pane, at) else {
            return Task::none();
        };
        let Some(window) = self.windows.get_mut(&wid) else {
            return Task::none();
        };
        let Some((t, _)) = window.locate(pane) else {
            return Task::none();
        };
        let tab = &mut window.tabs[t];
        tab.layout = moved;
        tab.maximized = None;
        tab.focus(pane);
        self.save_state();
        self.shown(wid)
    }

    /// Save after 500 ms without another request (window edge and divider drags).
    pub(crate) fn save_state_soon(&mut self) -> Task<Message> {
        self.resize_count += 1;
        let n = self.resize_count;
        Task::perform(
            async { tokio::time::sleep(Duration::from_millis(500)).await },
            move |()| Message::SaveState(n),
        )
    }
}

#[cfg(test)]
mod tests {
    use fbterm_config::Profile;
    use iced::window;

    use crate::app::{App, Message};
    use crate::keys::Action;
    use crate::layout::{Axis, Drop, Layout, PaneId, Side};
    use crate::palette::{PaletteItem, PaletteMsg};
    use crate::restore_tests::{paths_with, profile};
    use crate::tabs::{Pane, Tab, TabId, WindowState};

    fn two_panes(first: Pane, second: Pane) -> Tab {
        let (a, b) = (first.id, second.id);
        let mut tab = Tab::single(first);
        tab.layout.split(a, Side::Right, b);
        tab.panes.push(second);
        tab
    }

    fn named(p: &Profile, title: &str) -> Pane {
        let mut pane = Pane::for_profile(p);
        pane.title = title.into();
        pane
    }

    /// One window: tab 0 holds `a1 | a2` (a2 focused), tab 1 holds `b`; tab 1 is active.
    fn app_with_tabs(p: &Profile) -> (App, window::Id, [PaneId; 3], [TabId; 2]) {
        let (mut app, _) = App::build(paths_with(std::slice::from_ref(p)), true);
        app.state_saving_enabled = false;
        let wid = window::Id::unique();
        let (a1, a2, b) = (named(p, "build"), named(p, "logs"), named(p, "shell"));
        let ids = [a1.id, a2.id, b.id];
        let mut first = two_panes(a1, a2);
        first.focus(ids[1]);
        let second = Tab::single(b);
        let tabs = [first.id, second.id];
        let mut w = WindowState::new();
        w.push(first);
        w.push(second);
        app.windows.clear();
        app.windows.insert(wid, w);
        (app, wid, ids, tabs)
    }

    fn labels(app: &App, wid: window::Id) -> Vec<String> {
        let state = &app.palettes[&wid];
        (0..)
            .map_while(|i| state.at(i))
            .map(|item| match item {
                PaletteItem::Choice { label, .. }
                | PaletteItem::Action { label, .. }
                | PaletteItem::Tunnel { label, .. } => label.clone(),
                PaletteItem::Profile { name, .. } => name.clone(),
            })
            .collect()
    }

    #[test]
    fn merge_tab_inserts_whole_layout() {
        let p = profile("a");
        let (mut app, wid, [a1, a2, b], [ta, tb]) = app_with_tabs(&p);
        let _ = app.merge_tab(wid, ta, tb, Drop::Edge(Side::Right));
        let w = &app.windows[&wid];
        assert_eq!(w.tabs.len(), 1, "the merged tab is gone");
        let tab = &w.tabs[0];
        assert_eq!(tab.id, tb);
        assert_eq!(w.active, 0);
        // b on the left, the whole a1 | a2 split (its arrangement kept) on the right.
        let Layout::Split {
            axis: Axis::Row,
            first,
            second,
            ..
        } = &tab.layout
        else {
            panic!("a row: {:?}", tab.layout);
        };
        assert_eq!(**first, Layout::Pane(b));
        assert!(matches!(
            &**second,
            Layout::Split { axis: Axis::Row, first, second, .. }
                if **first == Layout::Pane(a1) && **second == Layout::Pane(a2)
        ));
        assert_eq!(tab.panes.len(), 3);
        assert_eq!(
            tab.focused, a2,
            "focus goes to the merged tab's focused pane"
        );

        // Into a tile's edge: that tile is split.
        let (mut app, wid, [a1, a2, b], [ta, tb]) = app_with_tabs(&p);
        let _ = app.merge_tab(wid, tb, ta, Drop::Tile(a1, Side::Bottom));
        let tab = &app.windows[&wid].tabs[0];
        assert_eq!(tab.id, ta);
        assert_eq!(tab.layout.panes(), vec![a1, b, a2]);
        assert_eq!(tab.focused, b);
    }

    #[test]
    fn tile_to_tab_creates_tab_at_index() {
        let p = profile("a");
        let (mut app, wid, [a1, a2, b], [ta, tb]) = app_with_tabs(&p);
        let _ = app.tile_to_tab(wid, a2, 0);
        let w = &app.windows[&wid];
        assert_eq!(w.tabs.len(), 3);
        assert_eq!(w.tabs[0].layout, Layout::Pane(a2));
        assert_eq!(w.tabs[0].panes.len(), 1);
        assert_eq!(w.tabs[0].panes[0].id, a2, "the same pane, moved");
        assert_eq!(w.active, 0, "the new tab is shown");
        assert_eq!(w.tabs[1].id, ta);
        assert_eq!(w.tabs[1].layout, Layout::Pane(a1));
        assert_eq!(w.tabs[1].focused, a1);
        assert_eq!(w.tabs[2].id, tb);
        assert_eq!(w.tabs[2].layout, Layout::Pane(b));

        // At the end of the bar (the palette's "Move tile to new tab").
        let (mut app, wid, [_, a2, _], _) = app_with_tabs(&p);
        let _ = app.update(Message::Shortcut(wid, Action::SelectTab(1)));
        let _ = app.update(Message::Shortcut(wid, Action::TileToTab));
        let w = &app.windows[&wid];
        assert_eq!(w.tabs.len(), 3);
        assert_eq!(w.tabs[2].layout, Layout::Pane(a2));
        assert_eq!(w.active, 2);
    }

    #[test]
    fn merge_into_self_is_noop() {
        let p = profile("a");
        let (mut app, wid, [_, _, b], [ta, tb]) = app_with_tabs(&p);
        let before: Vec<Layout> = app.windows[&wid]
            .tabs
            .iter()
            .map(|t| t.layout.clone())
            .collect();
        let _ = app.merge_tab(wid, ta, ta, Drop::Edge(Side::Left));
        let _ = app.merge_tab(wid, tb, tb, Drop::Edge(Side::Left));
        // A single-pane tab's tile cannot leave it: the tab would just be recreated.
        let _ = app.tile_to_tab(wid, b, 0);
        let _ = app.tile_to_tab(wid, b, 2);
        let w = &app.windows[&wid];
        let after: Vec<Layout> = w.tabs.iter().map(|t| t.layout.clone()).collect();
        assert_eq!(after, before);
        assert_eq!(w.tabs[0].id, ta);
        assert_eq!(w.tabs[1].id, tb);
        // The drag path refuses the same.
        let _ = app.update(Message::Drag(
            wid,
            crate::windows::DragMsg::DropTab {
                tab: tb,
                at: Drop::Edge(Side::Left),
            },
        ));
        assert_eq!(app.windows[&wid].tabs.len(), 2);
    }

    #[test]
    fn a_carried_tab_hovering_a_tab_shows_it_after_a_while() {
        use crate::windows::DragMsg;
        let p = profile("a");
        let (mut app, wid, [_, a2, b], [ta, tb]) = app_with_tabs(&p);
        let drag = |app: &mut App, m| {
            let _ = app.update(Message::Drag(wid, m));
        };
        drag(&mut app, DragMsg::StartTab(tb));
        drag(&mut app, DragMsg::OverTab(Some(0)));
        let first = app.hover_generation;
        // Moved on before the time was up: that hover's timer does nothing.
        drag(&mut app, DragMsg::OverTab(None));
        drag(&mut app, DragMsg::HoverElapsed(first));
        assert_eq!(app.windows[&wid].active, 1);
        // A drag that ended: its pending hover does nothing.
        drag(&mut app, DragMsg::OverTab(Some(0)));
        let ended = app.hover_generation;
        drag(&mut app, DragMsg::End);
        drag(&mut app, DragMsg::HoverElapsed(ended));
        assert_eq!(app.windows[&wid].active, 1);
        drag(&mut app, DragMsg::StartTab(tb));
        drag(&mut app, DragMsg::OverTab(Some(0)));
        let second = app.hover_generation;
        drag(&mut app, DragMsg::HoverElapsed(second));
        assert_eq!(app.windows[&wid].active, 0, "tab 0 is shown");
        // Dropped on its content: merged there.
        drag(
            &mut app,
            DragMsg::DropTab {
                tab: tb,
                at: Drop::Tile(a2, Side::Bottom),
            },
        );
        let w = &app.windows[&wid];
        assert_eq!(w.tabs.len(), 1);
        assert_eq!(w.tabs[0].id, ta);
        assert!(w.tabs[0].layout.contains(b));
        assert_eq!(app.carry, None);

        // A tile dropped on the bar becomes a tab there.
        drag(&mut app, DragMsg::Tile(Some(b)));
        assert!(app.carry.is_some());
        drag(&mut app, DragMsg::TileToTab { pane: b, index: 0 });
        let w = &app.windows[&wid];
        assert_eq!(w.tabs.len(), 2);
        assert_eq!(w.tabs[0].layout, Layout::Pane(b));
        drag(&mut app, DragMsg::Tile(None));
        assert_eq!(app.carry, None);
    }

    /// A pane area of `w` x `h` logical pixels and a 9 x 18 px cell for every pane: a pane
    /// needs 180 x 90 px (20 x 5 cells).
    fn small(app: &mut App, wid: window::Id, w: f32, h: f32) {
        let _ = app.update(Message::Tiles(
            wid,
            crate::tiles::TilesMsg::Area(iced::Size::new(w, h)),
        ));
        let panes: Vec<PaneId> = app.windows[&wid]
            .tabs
            .iter()
            .flat_map(|t| t.panes.iter().map(|p| p.id))
            .collect();
        for p in panes {
            app.grids.insert(
                p,
                fbterm_term::GridSize {
                    cols: 20,
                    rows: 5,
                    cell_width: 9.0,
                    cell_height: 18.0,
                },
            );
        }
    }

    #[test]
    fn merge_refused_when_it_would_not_fit() {
        let p = profile("a");
        let (mut app, wid, [_, _, b], [ta, tb]) = app_with_tabs(&p);
        small(&mut app, wid, 400.0, 300.0);
        // a1 | a2 beside b would leave a1 and a2 about 100 px wide each.
        let _ = app.merge_tab(wid, ta, tb, Drop::Edge(Side::Right));
        let _ = app.update(Message::Drag(
            wid,
            crate::windows::DragMsg::DropTab {
                tab: ta,
                at: Drop::Tile(b, Side::Left),
            },
        ));
        let w = &app.windows[&wid];
        assert_eq!(w.tabs.len(), 2, "nothing merged");
        assert_eq!(w.tabs[1].layout, Layout::Pane(b));
        // Stacked under b they fit (400 x 150 each side).
        let _ = app.merge_tab(wid, ta, tb, Drop::Edge(Side::Bottom));
        assert_eq!(app.windows[&wid].tabs.len(), 1);
    }

    #[test]
    fn tile_move_refused_when_it_would_not_fit() {
        let p = profile("a");
        let (mut app, wid, [a1, a2, _], _) = app_with_tabs(&p);
        let _ = app.update(Message::Shortcut(wid, Action::SelectTab(1)));
        small(&mut app, wid, 400.0, 150.0);
        let before = app.windows[&wid].tabs[0].layout.clone();
        // Stacked, each would get 75 px of the 90 px a pane needs.
        let _ = app.update(Message::Tiles(
            wid,
            crate::tiles::TilesMsg::DropTile {
                pane: a2,
                at: Drop::Tile(a1, Side::Bottom),
            },
        ));
        let _ = app.move_tile(wid, a2, Drop::Edge(Side::Top));
        assert_eq!(app.windows[&wid].tabs[0].layout, before);
        // Side by side the other way round fits.
        let _ = app.move_tile(wid, a2, Drop::Edge(Side::Left));
        assert_eq!(app.windows[&wid].tabs[0].layout.panes(), vec![a2, a1]);
    }

    #[test]
    fn palette_lists_only_choices_that_fit() {
        let p = profile("a");
        let (mut app, wid, _, _) = app_with_tabs(&p);
        // Shown without starting (a start would retitle the panes after their profile).
        app.windows.get_mut(&wid).unwrap().active = 0;
        small(&mut app, wid, 400.0, 150.0);
        let _ = app.update(Message::Shortcut(wid, Action::MoveTile));
        assert_eq!(
            labels(&app, wid),
            ["Next to build: left", "Whole column on the left"]
        );
        let _ = app.update(Message::Palette(wid, PaletteMsg::Close));
        // "shell" fits nowhere in "build | logs" here: no tab to merge into is listed.
        app.windows.get_mut(&wid).unwrap().active = 1;
        let _ = app.update(Message::Shortcut(wid, Action::MergeTab));
        assert!(app.palettes.is_empty());
        // In a taller area it fits stacked only.
        small(&mut app, wid, 400.0, 300.0);
        let _ = app.update(Message::Shortcut(wid, Action::MergeTab));
        assert_eq!(labels(&app, wid), ["logs"]);
        let _ = app.update(Message::Palette(wid, PaletteMsg::Pick(0)));
        assert_eq!(
            labels(&app, wid),
            [
                "Next to build: above",
                "Next to build: below",
                "Next to logs: above",
                "Next to logs: below",
                "Whole row on top",
                "Whole row on bottom",
            ]
        );
    }

    /// Tab 0 (`a1 | a2`) shown with a2 maximized, never started; the state file in use.
    fn maximized_target(p: &Profile) -> (App, window::Id, [PaneId; 3], [TabId; 2]) {
        let (mut app, wid, ids, tabs) = app_with_tabs(p);
        let w = app.windows.get_mut(&wid).unwrap();
        w.active = 0;
        w.tabs[0].maximized = Some(ids[1]);
        app.state_saving_enabled = true;
        (app, wid, ids, tabs)
    }

    #[test]
    fn cancelled_tab_drag_leaves_target_maximized_and_unsaved() {
        use crate::tabs::TabState;
        use crate::windows::DragMsg;
        let p = profile("a");
        let (mut app, wid, [a1, a2, _], [_, tb]) = maximized_target(&p);
        let state = app.paths.state();
        let before = std::fs::read_to_string(&state).ok();
        for m in [DragMsg::StartTab(tb), DragMsg::OverTab(None), DragMsg::End] {
            let _ = app.update(Message::Drag(wid, m));
        }
        let tab = &app.windows[&wid].tabs[0];
        assert_eq!(tab.maximized, Some(a2), "still maximized");
        assert_eq!(
            tab.pane(a1).unwrap().state,
            TabState::NotConnected,
            "the hidden pane was not started"
        );
        assert_eq!(
            std::fs::read_to_string(&state).ok(),
            before,
            "nothing saved"
        );

        // A hover switch onto a maximized tab shows it as it is (selected like a click on its
        // button: that runs `shown()` as any selection does).
        let (mut app, wid, [_, a2, _], [_, tb]) = maximized_target(&p);
        app.windows.get_mut(&wid).unwrap().active = 1;
        let _ = app.update(Message::Drag(wid, DragMsg::StartTab(tb)));
        let _ = app.update(Message::Drag(wid, DragMsg::OverTab(Some(0))));
        let generation = app.hover_generation;
        let _ = app.update(Message::Drag(wid, DragMsg::HoverElapsed(generation)));
        let _ = app.update(Message::Drag(wid, DragMsg::End));
        let tab = &app.windows[&wid].tabs[0];
        assert_eq!(app.windows[&wid].active, 0);
        assert_eq!(
            tab.maximized,
            Some(a2),
            "still maximized after the drag ended"
        );
    }

    #[test]
    fn tab_drag_preview_uses_full_layout_of_maximized_target() {
        let p = profile("a");
        let (mut app, wid, [a1, a2, b], [ta, tb]) = maximized_target(&p);
        app.state_saving_enabled = false;
        let area = iced::Rectangle::with_size(iced::Size::new(1000.0, 600.0));
        let _ = app.update(Message::Tiles(
            wid,
            crate::tiles::TilesMsg::Area(area.size()),
        ));
        let (full, carried) = {
            let w = &app.windows[&wid];
            (w.tabs[0].layout.clone(), w.tabs[1].layout.clone())
        };
        // Over the left part of the window, where a1 sits in the whole layout (a2 fills it on
        // screen): next to a1.
        let (at, preview) = crate::tiles::incoming_target(
            &full,
            area,
            &carried,
            iced::Point::new(250.0, 500.0),
            None,
        )
        .unwrap();
        assert_eq!(at, Drop::Tile(a1, Side::Bottom));
        let _ = app.merge_tab(wid, tb, ta, at);
        let tab = &app.windows[&wid].tabs[0];
        let landed = tab
            .layout
            .rects(area)
            .into_iter()
            .find(|(id, _)| *id == b)
            .map(|(_, r)| r)
            .unwrap();
        assert!(
            (landed.x - preview.x).abs() <= 1.0
                && (landed.y - preview.y).abs() <= 1.0
                && (landed.width - preview.width).abs() <= 1.0
                && (landed.height - preview.height).abs() <= 1.0,
            "preview {preview:?}, landed {landed:?}"
        );
        assert!(tab.layout.contains(a2));
    }

    #[test]
    fn merge_into_maximized_tab_unmaximizes_and_starts_panes() {
        use crate::tabs::TabState;
        let p = profile("a");
        let (mut app, wid, [a1, _, b], [ta, tb]) = maximized_target(&p);
        app.state_saving_enabled = false;
        let _ = app.merge_tab(wid, tb, ta, Drop::Edge(Side::Right));
        let tab = &app.windows[&wid].tabs[0];
        assert_eq!(tab.maximized, None);
        assert_eq!(tab.visible_panes().len(), 3);
        for pane in [a1, b] {
            assert_ne!(tab.pane(pane).unwrap().state, TabState::NotConnected);
        }
    }

    #[test]
    fn hover_switch_starts_the_shown_tabs_panes() {
        use crate::tabs::TabState;
        use crate::windows::DragMsg;
        let p = profile("a");
        let (mut app, wid, _, [_, tb]) = app_with_tabs(&p);
        assert!(
            app.windows[&wid].tabs[0]
                .panes
                .iter()
                .all(|p| p.state == TabState::NotConnected),
            "restored, never shown"
        );
        let _ = app.update(Message::Drag(wid, DragMsg::StartTab(tb)));
        let _ = app.update(Message::Drag(wid, DragMsg::OverTab(Some(0))));
        let generation = app.hover_generation;
        let _ = app.update(Message::Drag(wid, DragMsg::HoverElapsed(generation)));
        let w = &app.windows[&wid];
        assert_eq!(w.active, 0);
        assert!(
            w.tabs[0]
                .panes
                .iter()
                .all(|p| p.state != TabState::NotConnected),
            "both panes of the tab shown started"
        );
    }

    #[test]
    fn palette_merge_lists_drop_choices() {
        let p = profile("a");
        let (mut app, wid, [a1, a2, b], [ta, _]) = app_with_tabs(&p);
        // From tab "shell": merge it into the other tab.
        let _ = app.update(Message::Shortcut(wid, Action::MergeTab));
        assert_eq!(labels(&app, wid), ["logs"], "the other tab, by its title");
        let _ = app.update(Message::Palette(wid, PaletteMsg::Pick(0)));
        assert_eq!(
            labels(&app, wid),
            [
                "Next to build: left",
                "Next to build: right",
                "Next to build: above",
                "Next to build: below",
                "Next to logs: left",
                "Next to logs: right",
                "Next to logs: above",
                "Next to logs: below",
                "Whole column on the left",
                "Whole column on the right",
                "Whole row on top",
                "Whole row on bottom",
            ]
        );
        // Typing filters the choices.
        let _ = app.update(Message::Palette(
            wid,
            PaletteMsg::Query("logs below".into()),
        ));
        assert_eq!(labels(&app, wid), ["Next to logs: below"]);
        let _ = app.update(Message::Palette(wid, PaletteMsg::Submit { split: false }));
        assert!(app.palettes.is_empty());
        let w = &app.windows[&wid];
        assert_eq!(w.tabs.len(), 1);
        assert_eq!(w.tabs[0].id, ta);
        assert_eq!(w.tabs[0].layout.panes(), vec![a1, a2, b]);
        assert_eq!(w.tabs[0].focused, b);

        // "Move tile…": the focused pane's choices in its own tab, without no-op moves.
        let _ = app.update(Message::Shortcut(wid, Action::MoveTile));
        let listed = labels(&app, wid);
        assert!(!listed.iter().any(|l| l.contains("shell")), "{listed:?}");
        assert!(
            !listed.contains(&"Next to logs: below".to_owned()),
            "already there: {listed:?}"
        );
        assert!(listed.contains(&"Whole column on the left".to_owned()));
        let at = listed
            .iter()
            .position(|l| l == "Whole column on the left")
            .unwrap();
        let _ = app.update(Message::Palette(wid, PaletteMsg::Pick(at)));
        let tab = &app.windows[&wid].tabs[0];
        assert_eq!(tab.layout.panes(), vec![b, a1, a2]);
        assert!(app.palettes.is_empty());

        // The launcher still lists profiles and actions afterwards.
        let _ = app.update(Message::Shortcut(wid, Action::Palette));
        assert!(labels(&app, wid).contains(&"Merge tab into\u{2026}".to_owned()));
    }
}
