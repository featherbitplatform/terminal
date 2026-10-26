//! Find bar and find-in-all-tabs messages, and the searches behind them. Searches run on
//! tokio's blocking pool (one task per tab), lock their terminal one chunk at a time, and
//! report back with a sequence number so a result for an older query is dropped.

use std::time::Instant;

use fbterm_term::{SearchOutcome, SharedTerminal, blend};
use iced::widget::operation;
use iced::{Task, window};

use super::all::{FindAll, FindAllMsg, Jump, TabSearch, Target};
use super::{FindBar, FindMsg, REFRESH_AFTER, SearchRun, Step};
use crate::app::{App, Message};
use crate::layout::PaneId;
use crate::tabs::{Tab, TabId, WindowState};

/// Run `run` for `pane` on a worker thread.
fn spawn(pane: PaneId, terminal: SharedTerminal, run: SearchRun) -> Task<Message> {
    let seq = run.seq;
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                fbterm_term::search(&terminal, &run.request, &run.cancel)
            })
            .await
            .unwrap_or_default()
        },
        move |outcome| Message::Found { pane, seq, outcome },
    )
}

fn spawn_all(runs: Vec<TabSearch>) -> Task<Message> {
    Task::batch(runs.into_iter().map(|r| {
        let (seq, pane) = (r.seq, r.pane);
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    fbterm_term::search(&r.terminal, &r.request, &r.cancel)
                })
                .await
                .unwrap_or_default()
            },
            move |outcome| Message::FoundAll { seq, pane, outcome },
        )
    }))
}

/// Focus the bar's field with its text selected.
fn focus(input: &iced::widget::Id) -> Task<Message> {
    operation::focus(input.clone()).chain(operation::select_all(input.clone()))
}

impl App {
    /// The search highlight for a pane's bar: matches tinted violet, the current one in the
    /// accent.
    /// `alt`: the terminal shows the alternate screen now; the current match is only drawn when
    /// it was found on the same screen.
    pub(crate) fn highlight(&self, bar: &FindBar, alt: bool) -> Option<fbterm_term::Highlight> {
        let to_rgb = |c: iced::Color| {
            let [r, g, b, _] = c.into_rgba8();
            fbterm_config::Rgb { r, g, b }
        };
        let accent = to_rgb(self.tokens.accent);
        Some(fbterm_term::Highlight {
            pattern: bar.pattern()?.clone(),
            current: bar.current_match().filter(|_| bar.alt_screen == alt),
            match_bg: blend(accent, self.style.scheme.background, 0.38),
            current_bg: accent,
            current_fg: fbterm_config::Rgb {
                r: 255,
                g: 255,
                b: 255,
            },
        })
    }

    pub(crate) fn find(&mut self, wid: window::Id, msg: FindMsg) -> Task<Message> {
        // The bar and the box are not open together.
        if msg == FindMsg::Open {
            self.close_find_all();
            self.palettes.remove(&wid);
        }
        let Some(pane) = self
            .windows
            .get_mut(&wid)
            .and_then(WindowState::active_tab_mut)
            .map(Tab::focused_pane_mut)
        else {
            return Task::none();
        };
        let Some(term) = pane.terminal().cloned() else {
            return Task::none();
        };
        let id = pane.id;
        let view_bottom = term.lock().view_bottom();
        if msg == FindMsg::Open {
            let bar = pane.find.get_or_insert_with(|| FindBar::new("", false));
            return focus(&bar.input);
        }
        let Some(bar) = pane.find.as_mut() else {
            return Task::none();
        };
        let run = match msg {
            FindMsg::Open => None,
            FindMsg::Query(q) => bar.set_query(q, view_bottom),
            FindMsg::Type(text) => {
                let q = format!("{}{}", bar.query, text);
                let run = bar.set_query(q, view_bottom);
                let input = bar.input.clone();
                let refocus =
                    operation::focus(input.clone()).chain(operation::move_cursor_to_end(input));
                return Task::batch([refocus, run.map_or(Task::none(), |r| spawn(id, term, r))]);
            }
            FindMsg::ToggleRegex => bar.toggle_regex(view_bottom),
            FindMsg::Next => match bar.next() {
                Step::Fetch(run) => Some(run),
                Step::Done => {
                    reveal(&term, bar);
                    None
                }
            },
            FindMsg::Previous => {
                bar.previous();
                reveal(&term, bar);
                None
            }
            FindMsg::Close => {
                bar.cancel();
                pane.find = None;
                term.lock().scroll_to_bottom();
                None
            }
        };
        run.map_or(Task::none(), |r| spawn(id, term, r))
    }

    /// A pane's search finished.
    pub(crate) fn found(&mut self, id: PaneId, seq: u64, outcome: SearchOutcome) -> Task<Message> {
        let Some((_, pane)) = self.pane_mut(id) else {
            return Task::none();
        };
        let Some(term) = pane.terminal().cloned() else {
            return Task::none();
        };
        if let Some(bar) = pane.find.as_mut()
            && bar.apply(seq, outcome, Instant::now())
        {
            reveal(&term, bar);
        }
        Task::none()
    }

    /// Output arrived for `pane`, shown on screen: a find bar searches again (at most every
    /// [`REFRESH_AFTER`]) so its count and matches follow the output.
    pub(crate) fn find_output(&mut self, pane: PaneId) -> Task<Message> {
        let Some((_, pane)) = self.pane_mut(pane) else {
            return Task::none();
        };
        let Some(term) = pane.terminal().cloned() else {
            return Task::none();
        };
        let id = pane.id;
        let Some(bar) = pane.find.as_mut() else {
            return Task::none();
        };
        if bar.searching() || bar.refresh_scheduled {
            return Task::none();
        }
        let (generation, alt) = {
            let t = term.lock();
            (
                t.generation(),
                t.mode().contains(fbterm_term::TermMode::ALT_SCREEN),
            )
        };
        if !bar.stale(generation, alt) {
            return Task::none();
        }
        let since = bar.last_search.map_or(REFRESH_AFTER, |t| t.elapsed());
        if since >= REFRESH_AFTER {
            let view_bottom = term.lock().view_bottom();
            return bar
                .refresh(alt, view_bottom)
                .map_or(Task::none(), |r| spawn(id, term, r));
        }
        // One timer per burst of output, not a loop: it fires once and searches.
        bar.refresh_scheduled = true;
        let wait = REFRESH_AFTER - since;
        Task::perform(async move { tokio::time::sleep(wait).await }, move |()| {
            Message::FindRefresh(id)
        })
    }

    pub(crate) fn find_refresh(&mut self, id: PaneId) -> Task<Message> {
        let Some((_, pane)) = self.pane_mut(id) else {
            return Task::none();
        };
        let Some(term) = pane.terminal().cloned() else {
            return Task::none();
        };
        let Some(bar) = pane.find.as_mut() else {
            return Task::none();
        };
        let (alt, view_bottom) = {
            let t = term.lock();
            (
                t.mode().contains(fbterm_term::TermMode::ALT_SCREEN),
                t.view_bottom(),
            )
        };
        bar.refresh(alt, view_bottom)
            .map_or(Task::none(), |r| spawn(id, term, r))
    }

    /// The focused pane of `wid`'s active tab resized: its bar searches again, keeping its
    /// match when only the rows changed (see [`FindBar::resized`]).
    pub(crate) fn find_resized(&mut self, pane: PaneId) -> Task<Message> {
        let Some((_, pane)) = self.pane_mut(pane) else {
            return Task::none();
        };
        let Some(term) = pane.terminal().cloned() else {
            return Task::none();
        };
        let Some(bar) = pane.find.as_mut() else {
            return Task::none();
        };
        let (cols, view_bottom) = {
            let t = term.lock();
            (t.size().cols, t.view_bottom())
        };
        let id = pane.id;
        bar.resized(cols, view_bottom)
            .map_or(Task::none(), |r| spawn(id, term, r))
    }

    /// Every pane of every window with a terminal, in window, tab, then pane reading order;
    /// with `scope`, only the panes of that tab.
    pub(crate) fn find_targets(&self, scope: Option<TabId>) -> Vec<Target> {
        let several = self.windows.len() > 1;
        self.windows
            .iter()
            .enumerate()
            .flat_map(|(w, (wid, window))| {
                let tabs = window
                    .tabs
                    .iter()
                    .enumerate()
                    .filter(move |(_, tab)| scope.is_none_or(|s| s == tab.id));
                tabs.flat_map(move |(i, tab)| {
                    let split = tab.panes.len() > 1;
                    tab.layout
                        .panes()
                        .into_iter()
                        .enumerate()
                        .filter_map(move |(p, id)| {
                            let pane = tab.pane(id)?;
                            Some(Target {
                                tab: tab.id,
                                pane: id,
                                window: *wid,
                                order: (w, i, p),
                                name: tab.title().to_owned(),
                                pane_label: split.then(|| pane.title.clone()),
                                window_label: several.then(|| format!("Window {}", w + 1)),
                                accent: pane.accent,
                                terminal: pane.terminal()?.clone(),
                            })
                        })
                })
            })
            .collect()
    }

    pub(crate) fn close_find_all(&mut self) {
        if let Some(state) = self.find_box.take() {
            state.cancel();
        }
    }

    /// Open the box in `wid` with `query` and search at once.
    pub(crate) fn open_find_all(&mut self, wid: window::Id, query: String) -> Task<Message> {
        self.open_find_all_in(wid, query, None)
    }

    /// Like [`Self::open_find_all`], searching only the panes of tab `scope` when given.
    pub(crate) fn open_find_all_in(
        &mut self,
        wid: window::Id,
        query: String,
        scope: Option<TabId>,
    ) -> Task<Message> {
        if !self.windows.contains_key(&wid) {
            return Task::none();
        }
        self.close_find_all();
        self.palettes.remove(&wid);
        let mut state = FindAll::new(wid, query, false);
        state.scope = scope;
        let runs = state.search(self.find_targets(scope));
        let input = state.input.clone();
        self.find_box = Some(state);
        let focus = operation::focus(input.clone()).chain(operation::move_cursor_to_end(input));
        Task::batch([focus, spawn_all(runs)])
    }

    /// Find in this tab: the box, searching only the active tab's panes. Again closes it.
    pub(crate) fn find_in_tab(&mut self, wid: window::Id) -> Task<Message> {
        let Some(tab) = self.windows.get(&wid).and_then(WindowState::active_tab) else {
            return Task::none();
        };
        let scope = Some(tab.id);
        if self
            .find_box
            .as_ref()
            .is_some_and(|s| s.window == wid && s.scope == scope)
        {
            self.close_find_all();
            return Task::none();
        }
        self.open_find_all_in(wid, String::new(), scope)
    }

    pub(crate) fn find_all(&mut self, wid: window::Id, msg: FindAllMsg) -> Task<Message> {
        if msg == FindAllMsg::Open {
            // Opening again closes it, like the palette.
            if self.find_box.as_ref().is_some_and(|s| s.window == wid) {
                self.close_find_all();
                return Task::none();
            }
            return self.open_find_all(wid, String::new());
        }
        let scope = self.find_box.as_ref().and_then(|s| s.scope);
        // The tab a scoped box searches is gone: close the box.
        if scope.is_some_and(|id| {
            !self
                .windows
                .values()
                .any(|w| w.tabs.iter().any(|t| t.id == id))
        }) {
            self.close_find_all();
            return Task::none();
        }
        let targets = match msg {
            FindAllMsg::Query(_) | FindAllMsg::ToggleRegex => self.find_targets(scope),
            _ => Vec::new(),
        };
        let Some(state) = self.find_box.as_mut().filter(|s| s.window == wid) else {
            return Task::none();
        };
        match msg {
            FindAllMsg::Open => Task::none(),
            FindAllMsg::Query(q) => spawn_all(state.set_query(q, targets)),
            FindAllMsg::ToggleRegex => spawn_all(state.toggle_regex(targets)),
            FindAllMsg::Up => {
                state.up();
                Task::none()
            }
            FindAllMsg::Down => {
                state.down();
                Task::none()
            }
            FindAllMsg::Close => {
                self.close_find_all();
                Task::none()
            }
            FindAllMsg::Submit => match state.jump(None) {
                Some(jump) => self.jump(jump),
                None => Task::none(),
            },
            FindAllMsg::Pick(position) => match state.jump(Some(position)) {
                Some(jump) => self.jump(jump),
                None => Task::none(),
            },
        }
    }

    pub(crate) fn found_all(&mut self, seq: u64, pane: PaneId, outcome: SearchOutcome) {
        if let Some(state) = self.find_box.as_mut() {
            state.apply(seq, pane, outcome);
        }
    }

    /// Go to a match from the box: focus its window, select its tab and pane, close the box
    /// and open the pane's find bar on the same query with that match current.
    pub(crate) fn jump(&mut self, jump: Jump) -> Task<Message> {
        self.close_find_all();
        let Some(window) = self.windows.get_mut(&jump.window) else {
            return Task::none();
        };
        let Some((index, p)) = window.locate(jump.pane) else {
            return Task::none();
        };
        let selected = window.select(index);
        let moved = window.tabs[index].focus(jump.pane);
        let pane = &mut window.tabs[index].panes[p];
        let Some(term) = pane.terminal().cloned() else {
            return Task::none();
        };
        if let Some(old) = &pane.find {
            old.cancel();
        }
        let mut bar = FindBar::new(jump.query, jump.regex);
        let run = bar.jump_to(jump.at);
        let input = bar.input.clone();
        pane.find = Some(bar);
        let id = pane.id;
        self.focused = Some(jump.window);
        let shown = if selected || moved {
            self.save_state();
            self.shown(jump.window)
        } else {
            Task::none()
        };
        Task::batch([
            window::gain_focus(jump.window),
            shown,
            focus(&input),
            run.map_or(Task::none(), |r| spawn(id, term, r)),
        ])
    }
}

/// Scroll the terminal so the bar's current match is in view.
fn reveal(term: &SharedTerminal, bar: &FindBar) {
    if let Some(m) = bar.current_match() {
        let mut t = term.lock();
        let cover = super::bar_view::cover(t.size());
        t.reveal(&m, Some(cover));
    }
}

#[cfg(test)]
mod tests {
    use fbterm_config::{LocalProfile, Paths, Profile, ProfileId, ProfileKind};

    use super::*;
    use crate::tabs::{Pane, Tab, TabId};

    fn app_with_tabs(n: usize) -> (App, window::Id, Vec<PaneId>) {
        let dir = std::env::temp_dir().join(format!(
            "fbterm-find-test-{}-{}",
            std::process::id(),
            TabId::next().0
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let (mut app, _) = App::build(Paths { config_dir: dir }, true);
        app.state_saving_enabled = false;
        let wid = window::Id::unique();
        let mut w = WindowState::new();
        let profile = Profile {
            id: ProfileId::new(),
            name: "sh".into(),
            group: None,
            accent: None,
            accent_style: Default::default(),
            kind: ProfileKind::Local(LocalProfile {
                program: "/bin/sh".into(),
                args: vec![],
                cwd: None,
                env: Default::default(),
            }),
        };
        let mut ids = Vec::new();
        for i in 0..n {
            let mut tab = Tab::single(Pane::new(profile.id, format!("tab {i}"), None));
            let term = app.new_terminal(crate::app::DEFAULT_GRID);
            term.lock()
                .feed(format!("hello from {i}\r\nneedle {i}\r\n").as_bytes());
            tab.panes[0].placeholder = Some(term);
            ids.push(tab.focused);
            w.push(tab);
        }
        w.active = 0;
        app.windows.clear();
        app.windows.insert(wid, w);
        (app, wid, ids)
    }

    #[test]
    fn the_bar_opens_on_the_active_tab_and_closes() {
        let (mut app, wid, ids) = app_with_tabs(2);
        let _ = app.find(wid, FindMsg::Open);
        assert!(app.windows[&wid].tabs[0].focused_pane().find.is_some());
        assert!(app.windows[&wid].tabs[1].focused_pane().find.is_none());
        let _ = app.find(wid, FindMsg::Query("needle".into()));
        let bar = app.windows[&wid].tabs[0]
            .focused_pane()
            .find
            .as_ref()
            .unwrap();
        assert!(bar.searching());
        let _ = app.find(wid, FindMsg::Close);
        assert!(app.windows[&wid].tabs[0].focused_pane().find.is_none());
        let _ = ids;
    }

    #[test]
    fn a_result_sets_the_count_and_next_moves() {
        let (mut app, wid, ids) = app_with_tabs(1);
        let _ = app.find(wid, FindMsg::Open);
        let _ = app.find(wid, FindMsg::Query("o".into()));
        let term = app.windows[&wid].tabs[0].terminal().unwrap().clone();
        let bar = app.windows[&wid].tabs[0]
            .focused_pane()
            .find
            .as_ref()
            .unwrap();
        let req = bar.pattern().cloned().unwrap();
        let outcome = fbterm_term::search(
            &term,
            &fbterm_term::SearchRequest {
                pattern: req,
                below: None,
                limit: fbterm_term::MATCH_CAP,
                snippets: false,
            },
            &std::sync::atomic::AtomicBool::new(false),
        );
        let seq = bar.pending.expect("searching").0;
        let _ = app.found(ids[0], seq, outcome);
        let bar = app.windows[&wid].tabs[0]
            .focused_pane()
            .find
            .as_ref()
            .unwrap();
        // "hello from 0" has two, the prompt line none: newest first.
        assert_eq!(bar.counter(), "1 of 2");
        let _ = app.find(wid, FindMsg::Next);
        assert_eq!(
            app.windows[&wid].tabs[0]
                .focused_pane()
                .find
                .as_ref()
                .unwrap()
                .counter(),
            "2 of 2"
        );
        let _ = app.find(wid, FindMsg::Next);
        assert_eq!(
            app.windows[&wid].tabs[0]
                .focused_pane()
                .find
                .as_ref()
                .unwrap()
                .counter(),
            "1 of 2"
        );
    }

    #[test]
    fn the_bar_and_the_box_exclude_each_other() {
        let (mut app, wid, _) = app_with_tabs(2);
        let _ = app.find(wid, FindMsg::Open);
        let _ = app.find_all(wid, FindAllMsg::Open);
        assert!(app.find_box.is_some());
        let _ = app.find(wid, FindMsg::Open);
        assert!(app.find_box.is_none());
        let _ = app.find_all(wid, FindAllMsg::Open);
        let _ = app.find_all(wid, FindAllMsg::Open);
        assert!(app.find_box.is_none(), "the chord toggles the box");
    }

    #[test]
    fn jumping_from_the_box_selects_the_tab_and_opens_its_bar() {
        let (mut app, wid, ids) = app_with_tabs(3);
        let _ = app.open_find_all(wid, "needle 2".into());
        assert!(app.find_box.as_ref().unwrap().searching());
        // Deliver every tab's result as the worker threads would.
        let targets = app.find_targets(None);
        let seq = app.find_box.as_ref().unwrap().seq();
        for t in targets {
            let req = fbterm_term::SearchRequest {
                pattern: fbterm_term::SearchPattern::new(&fbterm_term::SearchQuery::new(
                    "needle 2", false,
                ))
                .unwrap()
                .unwrap(),
                below: None,
                limit: super::super::all::PER_TAB + 1,
                snippets: true,
            };
            let out = fbterm_term::search(
                &t.terminal,
                &req,
                &std::sync::atomic::AtomicBool::new(false),
            );
            app.found_all(seq, t.pane, out);
        }
        let state = app.find_box.as_ref().unwrap();
        assert_eq!(state.groups().len(), 1);
        assert_eq!(state.groups()[0].pane, ids[2]);
        let _ = app.find_all(wid, FindAllMsg::Submit);
        assert!(app.find_box.is_none(), "the box closes");
        let w = &app.windows[&wid];
        assert_eq!(w.active, 2, "the tab is selected");
        let bar = w.tabs[2]
            .focused_pane()
            .find
            .as_ref()
            .expect("its bar opens");
        assert_eq!(bar.query, "needle 2");
        assert!(bar.searching(), "searching for the match to make current");
    }

    /// One window with one tab of two panes (each with its own text) and one single-pane tab.
    fn app_with_split() -> (App, window::Id, Vec<PaneId>) {
        let (mut app, wid, ids) = app_with_tabs(2);
        let w = app.windows.get_mut(&wid).unwrap();
        let tab = &mut w.tabs[0];
        let mut second = Pane::new(tab.focused_pane().profile, String::from("right"), None);
        let term = app.new_terminal(crate::app::DEFAULT_GRID);
        term.lock().feed(b"needle right\r\n");
        second.placeholder = Some(term);
        let id = second.id;
        let w = app.windows.get_mut(&wid).unwrap();
        let tab = &mut w.tabs[0];
        tab.layout
            .split(tab.focused, crate::layout::Side::Right, id);
        tab.panes.push(second);
        (app, wid, vec![ids[0], id, ids[1]])
    }

    fn deliver(app: &mut App, query: &str) {
        let seq = app.find_box.as_ref().unwrap().seq();
        for t in app.find_targets(app.find_box.as_ref().unwrap().scope) {
            let req = fbterm_term::SearchRequest {
                pattern: fbterm_term::SearchPattern::new(&fbterm_term::SearchQuery::new(
                    query, false,
                ))
                .unwrap()
                .unwrap(),
                below: None,
                limit: super::super::all::PER_TAB + 1,
                snippets: true,
            };
            let out = fbterm_term::search(
                &t.terminal,
                &req,
                &std::sync::atomic::AtomicBool::new(false),
            );
            app.found_all(seq, t.pane, out);
        }
    }

    #[test]
    fn find_all_lists_each_pane() {
        let (mut app, wid, ids) = app_with_split();
        let _ = app.open_find_all(wid, "needle".into());
        deliver(&mut app, "needle");
        let groups = app.find_box.as_ref().unwrap().groups();
        let panes: Vec<PaneId> = groups.iter().map(|g| g.pane).collect();
        assert_eq!(panes, ids, "one group per pane, in reading order");
        assert!(groups[0].pane_label.is_some() && groups[1].pane_label.is_some());
        assert_eq!(groups[0].name, groups[1].name, "the tab name is shared");
        assert!(
            groups[2].pane_label.is_none(),
            "a single-pane tab has no pane label"
        );
    }

    #[test]
    fn find_in_tab_searches_only_its_panes() {
        let (mut app, wid, ids) = app_with_split();
        let _ = app.update(Message::Shortcut(wid, crate::keys::Action::FindInTab));
        let state = app.find_box.as_ref().expect("the box opens");
        assert_eq!(state.scope, Some(app.windows[&wid].tabs[0].id));
        let _ = app.find_all(wid, FindAllMsg::Query("needle".into()));
        assert_eq!(
            app.find_box.as_ref().unwrap().scope,
            Some(app.windows[&wid].tabs[0].id)
        );
        deliver(&mut app, "needle");
        let panes: Vec<PaneId> = app
            .find_box
            .as_ref()
            .unwrap()
            .groups()
            .iter()
            .map(|g| g.pane)
            .collect();
        assert_eq!(panes, ids[..2], "the other tab is left out");
    }

    #[test]
    fn jump_focuses_the_pane() {
        let (mut app, wid, ids) = app_with_split();
        // Look at the other tab, with the first tab's focus on its left pane.
        app.windows.get_mut(&wid).unwrap().active = 1;
        let jump = Jump {
            window: wid,
            tab: app.windows[&wid].tabs[0].id,
            pane: ids[1],
            at: fbterm_term::StablePoint { line: 0, col: 0 },
            query: "needle".into(),
            regex: false,
        };
        let _ = app.jump(jump);
        let w = &app.windows[&wid];
        assert_eq!(w.active, 0);
        assert_eq!(w.tabs[0].focused, ids[1]);
        assert!(w.tabs[0].pane(ids[1]).unwrap().find.is_some());
        assert!(w.tabs[0].pane(ids[0]).unwrap().find.is_none());
    }

    #[test]
    fn jump_to_closed_pane_is_ignored() {
        let (mut app, wid, ids) = app_with_split();
        let before = app.windows[&wid].active;
        let jump = Jump {
            window: wid,
            tab: app.windows[&wid].tabs[0].id,
            pane: PaneId::next(),
            at: fbterm_term::StablePoint { line: 0, col: 0 },
            query: "needle".into(),
            regex: false,
        };
        let _ = app.jump(jump);
        let w = &app.windows[&wid];
        assert_eq!(w.active, before);
        assert_eq!(w.tabs[0].focused, ids[0]);
        assert!(
            w.tabs
                .iter()
                .all(|t| t.panes.iter().all(|p| p.find.is_none()))
        );
    }

    /// Both panes of the split tab get a bar with its own query, and its result delivered.
    fn two_bars(app: &mut App, wid: window::Id, ids: &[PaneId]) {
        for (id, q) in [(ids[0], "needle"), (ids[1], "right")] {
            app.windows.get_mut(&wid).unwrap().tabs[0].focus(id);
            let _ = app.find(wid, FindMsg::Open);
            let _ = app.find(wid, FindMsg::Query(q.into()));
            let pane = app.windows[&wid].tabs[0].pane(id).unwrap();
            let bar = pane.find.as_ref().unwrap();
            let seq = bar.pending.expect("searching").0;
            let outcome = fbterm_term::search(
                pane.terminal().unwrap(),
                &fbterm_term::SearchRequest {
                    pattern: bar.pattern().cloned().unwrap(),
                    below: None,
                    limit: fbterm_term::MATCH_CAP,
                    snippets: false,
                },
                &std::sync::atomic::AtomicBool::new(false),
            );
            let _ = app.found(id, seq, outcome);
        }
    }

    fn bar(app: &App, wid: window::Id, id: PaneId) -> &FindBar {
        app.windows[&wid].tabs[0]
            .pane(id)
            .unwrap()
            .find
            .as_ref()
            .unwrap()
    }

    #[test]
    fn non_focused_pane_keeps_its_find_results() {
        let (mut app, wid, ids) = app_with_split();
        two_bars(&mut app, wid, &ids);
        let (a, b) = (ids[0], ids[1]);
        assert_eq!(bar(&app, wid, a).counter(), "1 of 1");
        let a_counter = bar(&app, wid, a).counter();
        assert_eq!(bar(&app, wid, b).counter(), "1 of 1");
        // Output in B (focus is on B): only B's bar searches again.
        let term = app.windows[&wid].tabs[0]
            .pane(b)
            .unwrap()
            .terminal()
            .unwrap()
            .clone();
        term.lock().feed(b"right again\r\n");
        let _ = app.find_output(b);
        assert!(bar(&app, wid, b).searching() || bar(&app, wid, b).refresh_scheduled);
        assert!(!bar(&app, wid, a).searching());
        assert!(!bar(&app, wid, a).refresh_scheduled);
        assert_eq!(bar(&app, wid, a).counter(), a_counter);
        assert_eq!(bar(&app, wid, a).query, "needle");
        assert_eq!(bar(&app, wid, b).query, "right");
        // Output for A while B has focus refreshes A's bar, not B's.
        let _ = app.find_output(a);
        assert!(!bar(&app, wid, a).searching(), "A saw no new output");
    }

    #[test]
    fn resize_refreshes_only_that_panes_bar() {
        let (mut app, wid, ids) = app_with_split();
        two_bars(&mut app, wid, &ids);
        let (a, b) = (ids[0], ids[1]);
        assert!(!bar(&app, wid, a).searching() && !bar(&app, wid, b).searching());
        let term = app.windows[&wid].tabs[0]
            .pane(b)
            .unwrap()
            .terminal()
            .unwrap()
            .clone();
        let mut size = term.lock().size();
        size.cols += 7;
        term.lock().resize(size);
        let _ = app.find_resized(b);
        assert!(bar(&app, wid, b).searching());
        assert!(!bar(&app, wid, a).searching());
    }

    #[test]
    fn a_scoped_box_closes_when_its_tab_closes() {
        let (mut app, wid, _) = app_with_split();
        let _ = app.find_in_tab(wid);
        assert!(app.find_box.is_some());
        let _ = app.close_tab(wid, 0);
        let _ = app.find_all(wid, FindAllMsg::Query("x".into()));
        assert!(app.find_box.is_none());
    }
}
