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
use crate::tabs::{TabId, WindowState};

/// Run `run` for `tab` on a worker thread.
fn spawn(tab: TabId, terminal: SharedTerminal, run: SearchRun) -> Task<Message> {
    let seq = run.seq;
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                fbterm_term::search(&terminal, &run.request, &run.cancel)
            })
            .await
            .unwrap_or_default()
        },
        move |outcome| Message::Found { tab, seq, outcome },
    )
}

fn spawn_all(runs: Vec<TabSearch>) -> Task<Message> {
    Task::batch(runs.into_iter().map(|r| {
        let (seq, tab) = (r.seq, r.tab);
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    fbterm_term::search(&r.terminal, &r.request, &r.cancel)
                })
                .await
                .unwrap_or_default()
            },
            move |outcome| Message::FoundAll { seq, tab, outcome },
        )
    }))
}

/// Focus the bar's field with its text selected.
fn focus(input: &iced::widget::Id) -> Task<Message> {
    operation::focus(input.clone()).chain(operation::select_all(input.clone()))
}

impl App {
    /// The search highlight for a tab's bar: matches tinted violet, the current one in the
    /// accent.
    pub(crate) fn highlight(&self, bar: &FindBar) -> Option<fbterm_term::Highlight> {
        let to_rgb = |c: iced::Color| {
            let [r, g, b, _] = c.into_rgba8();
            fbterm_config::Rgb { r, g, b }
        };
        let accent = to_rgb(self.tokens.accent);
        Some(fbterm_term::Highlight {
            pattern: bar.pattern()?.clone(),
            current: bar.current_match(),
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
        let Some(tab) = self
            .windows
            .get_mut(&wid)
            .and_then(WindowState::active_tab_mut)
        else {
            return Task::none();
        };
        let Some(term) = tab.terminal().cloned() else {
            return Task::none();
        };
        let id = tab.id;
        let view_bottom = term.lock().view_bottom();
        if msg == FindMsg::Open {
            let bar = tab.find.get_or_insert_with(|| FindBar::new("", false));
            return focus(&bar.input);
        }
        let Some(bar) = tab.find.as_mut() else {
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
                tab.find = None;
                term.lock().scroll_to_bottom();
                None
            }
        };
        run.map_or(Task::none(), |r| spawn(id, term, r))
    }

    /// A tab's search finished.
    pub(crate) fn found(&mut self, id: TabId, seq: u64, outcome: SearchOutcome) -> Task<Message> {
        let Some(tab) = self.tab_mut(id) else {
            return Task::none();
        };
        let Some(term) = tab.terminal().cloned() else {
            return Task::none();
        };
        if let Some(bar) = tab.find.as_mut()
            && bar.apply(seq, outcome, Instant::now())
        {
            reveal(&term, bar);
        }
        Task::none()
    }

    /// Output arrived for the tab at `index` of `wid`, shown on screen: a find bar searches
    /// again (at most every [`REFRESH_AFTER`]) so its count and matches follow the output.
    pub(crate) fn find_output(&mut self, wid: window::Id, index: usize) -> Task<Message> {
        let Some(tab) = self
            .windows
            .get_mut(&wid)
            .and_then(|w| w.tabs.get_mut(index))
        else {
            return Task::none();
        };
        let Some(term) = tab.terminal().cloned() else {
            return Task::none();
        };
        let id = tab.id;
        let Some(bar) = tab.find.as_mut() else {
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
            return bar.refresh().map_or(Task::none(), |r| spawn(id, term, r));
        }
        // One timer per burst of output, not a loop: it fires once and searches.
        bar.refresh_scheduled = true;
        let wait = REFRESH_AFTER - since;
        Task::perform(async move { tokio::time::sleep(wait).await }, move |()| {
            Message::FindRefresh(id)
        })
    }

    pub(crate) fn find_refresh(&mut self, id: TabId) -> Task<Message> {
        let Some(tab) = self.tab_mut(id) else {
            return Task::none();
        };
        let Some(term) = tab.terminal().cloned() else {
            return Task::none();
        };
        let Some(bar) = tab.find.as_mut() else {
            return Task::none();
        };
        bar.refresh().map_or(Task::none(), |r| spawn(id, term, r))
    }

    /// The active tab of `wid` resized: wrapped lines moved, so its bar searches again.
    pub(crate) fn find_resized(&mut self, wid: window::Id) -> Task<Message> {
        let Some(tab) = self
            .windows
            .get_mut(&wid)
            .and_then(WindowState::active_tab_mut)
        else {
            return Task::none();
        };
        let Some(term) = tab.terminal().cloned() else {
            return Task::none();
        };
        let Some(bar) = tab.find.as_mut() else {
            return Task::none();
        };
        let view_bottom = term.lock().view_bottom();
        let q = bar.query.clone();
        let id = tab.id;
        bar.set_query(q, view_bottom)
            .map_or(Task::none(), |r| spawn(id, term, r))
    }

    /// Every tab of every window, with its terminal, in window then tab order.
    fn find_targets(&self) -> Vec<Target> {
        let several = self.windows.len() > 1;
        self.windows
            .iter()
            .enumerate()
            .flat_map(|(w, (wid, window))| {
                window.tabs.iter().enumerate().filter_map(move |(i, tab)| {
                    Some(Target {
                        tab: tab.id,
                        window: *wid,
                        order: (w, i),
                        name: tab.title.clone(),
                        window_label: several.then(|| format!("Window {}", w + 1)),
                        accent: tab.accent,
                        terminal: tab.terminal()?.clone(),
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
        if !self.windows.contains_key(&wid) {
            return Task::none();
        }
        self.close_find_all();
        self.palettes.remove(&wid);
        let mut state = FindAll::new(wid, query, false);
        let runs = state.search(self.find_targets());
        let input = state.input.clone();
        self.find_box = Some(state);
        let focus = operation::focus(input.clone()).chain(operation::move_cursor_to_end(input));
        Task::batch([focus, spawn_all(runs)])
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
        let targets = match msg {
            FindAllMsg::Query(_) | FindAllMsg::ToggleRegex => self.find_targets(),
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

    pub(crate) fn found_all(&mut self, seq: u64, tab: TabId, outcome: SearchOutcome) {
        if let Some(state) = self.find_box.as_mut() {
            state.apply(seq, tab, outcome);
        }
    }

    /// Go to a match from the box: focus its window, select its tab, close the box and open
    /// the tab's find bar on the same query with that match current.
    pub(crate) fn jump(&mut self, jump: Jump) -> Task<Message> {
        self.close_find_all();
        let Some(window) = self.windows.get_mut(&jump.window) else {
            return Task::none();
        };
        let Some(index) = window.find(jump.tab) else {
            return Task::none();
        };
        let selected = window.select(index);
        let tab = &mut window.tabs[index];
        let Some(term) = tab.terminal().cloned() else {
            return Task::none();
        };
        if let Some(old) = &tab.find {
            old.cancel();
        }
        let mut bar = FindBar::new(jump.query, jump.regex);
        let run = bar.jump_to(jump.at);
        let input = bar.input.clone();
        tab.find = Some(bar);
        let id = tab.id;
        self.focused = Some(jump.window);
        let shown = if selected {
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
        term.lock().reveal(&m);
    }
}

#[cfg(test)]
mod tests {
    use fbterm_config::{LocalProfile, Paths, Profile, ProfileId, ProfileKind};

    use super::*;
    use crate::tabs::Tab;

    fn app_with_tabs(n: usize) -> (App, window::Id, Vec<TabId>) {
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
            kind: ProfileKind::Local(LocalProfile {
                program: "/bin/sh".into(),
                args: vec![],
                cwd: None,
                env: Default::default(),
            }),
        };
        let mut ids = Vec::new();
        for i in 0..n {
            let mut tab = Tab::new(profile.id, format!("tab {i}"), None);
            let term = app.new_terminal(crate::app::DEFAULT_GRID);
            term.lock()
                .feed(format!("hello from {i}\r\nneedle {i}\r\n").as_bytes());
            tab.placeholder = Some(term);
            ids.push(tab.id);
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
        assert!(app.windows[&wid].tabs[0].find.is_some());
        assert!(app.windows[&wid].tabs[1].find.is_none());
        let _ = app.find(wid, FindMsg::Query("needle".into()));
        let bar = app.windows[&wid].tabs[0].find.as_ref().unwrap();
        assert!(bar.searching());
        let _ = app.find(wid, FindMsg::Close);
        assert!(app.windows[&wid].tabs[0].find.is_none());
        let _ = ids;
    }

    #[test]
    fn a_result_sets_the_count_and_next_moves() {
        let (mut app, wid, ids) = app_with_tabs(1);
        let _ = app.find(wid, FindMsg::Open);
        let _ = app.find(wid, FindMsg::Query("o".into()));
        let term = app.windows[&wid].tabs[0].terminal().unwrap().clone();
        let bar = app.windows[&wid].tabs[0].find.as_ref().unwrap();
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
        let seq = bar.seq;
        let _ = app.found(ids[0], seq, outcome);
        let bar = app.windows[&wid].tabs[0].find.as_ref().unwrap();
        // "hello from 0" has two, the prompt line none: newest first.
        assert_eq!(bar.counter(), "1 of 2");
        let _ = app.find(wid, FindMsg::Next);
        assert_eq!(
            app.windows[&wid].tabs[0].find.as_ref().unwrap().counter(),
            "2 of 2"
        );
        let _ = app.find(wid, FindMsg::Next);
        assert_eq!(
            app.windows[&wid].tabs[0].find.as_ref().unwrap().counter(),
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
        let targets = app.find_targets();
        // The box's first search.
        let seq = 1;
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
            app.found_all(seq, t.tab, out);
        }
        let state = app.find_box.as_ref().unwrap();
        assert_eq!(state.groups().len(), 1);
        assert_eq!(state.groups()[0].tab, ids[2]);
        let _ = app.find_all(wid, FindAllMsg::Submit);
        assert!(app.find_box.is_none(), "the box closes");
        let w = &app.windows[&wid];
        assert_eq!(w.active, 2, "the tab is selected");
        let bar = w.tabs[2].find.as_ref().expect("its bar opens");
        assert_eq!(bar.query, "needle 2");
        assert!(bar.searching(), "searching for the match to make current");
    }
}
