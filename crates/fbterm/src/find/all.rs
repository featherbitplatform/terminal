//! Find in all tabs: one query searched in every tab of every window, results grouped by tab.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fbterm_config::Rgb;
use fbterm_term::{
    Hit, SearchOutcome, SearchPattern, SearchQuery, SearchRequest, SharedTerminal, StablePoint,
};
use iced::window;

use crate::layout::PaneId;
use crate::tabs::TabId;

/// Matches listed per tab.
pub const PER_TAB: usize = 50;
/// Matches listed in all.
pub const TOTAL: usize = 500;

/// What the find-in-all-tabs box is asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindAllMsg {
    Open,
    Query(String),
    ToggleRegex,
    Up,
    Down,
    Submit,
    /// A click on a result (its position among the listed matches).
    Pick(usize),
    Close,
}

/// A pane to search: where it is, how it is labelled, and its terminal.
#[derive(Clone)]
pub struct Target {
    pub tab: TabId,
    pub pane: PaneId,
    pub window: window::Id,
    /// Window, tab, then pane position: the order groups are listed in.
    pub order: (usize, usize, usize),
    /// The tab's title.
    pub name: String,
    /// The pane's title, when its tab has more than one pane.
    pub pane_label: Option<String>,
    /// "Window 2" when there are several windows.
    pub window_label: Option<String>,
    pub accent: Option<Rgb>,
    pub terminal: SharedTerminal,
}

/// One pane's matches.
#[derive(Debug, Clone)]
pub struct Group {
    pub tab: TabId,
    pub pane: PaneId,
    pub window: window::Id,
    pub order: (usize, usize, usize),
    pub name: String,
    pub pane_label: Option<String>,
    pub window_label: Option<String>,
    pub accent: Option<Rgb>,
    /// Newest first, at most [`PER_TAB`].
    pub hits: Vec<Hit>,
    /// The pane has more matches than are listed.
    pub more: bool,
}

/// Where Enter on a result goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jump {
    pub window: window::Id,
    pub tab: TabId,
    pub pane: PaneId,
    pub at: StablePoint,
    pub query: String,
    pub regex: bool,
}

/// One pane's search, to run on a worker thread.
#[derive(Clone)]
pub struct TabSearch {
    pub seq: u64,
    pub pane: PaneId,
    pub terminal: SharedTerminal,
    pub request: SearchRequest,
    pub cancel: Arc<AtomicBool>,
}

pub struct FindAll {
    /// The window the box is open in.
    pub window: window::Id,
    pub query: String,
    pub regex: bool,
    pub input: iced::widget::Id,
    pub invalid: Option<fbterm_term::PatternError>,
    /// Search only this tab's panes (Find in this tab); `None` searches every tab.
    pub scope: Option<TabId>,
    seq: u64,
    cancel: Arc<AtomicBool>,
    /// Panes still being searched, with how to label their results.
    pending: HashMap<PaneId, Target>,
    /// In listing order.
    groups: Vec<Group>,
    /// The selected match, by pane and position in its group.
    selected: Option<(PaneId, usize)>,
}

impl FindAll {
    pub fn new(window: window::Id, query: impl Into<String>, regex: bool) -> Self {
        Self {
            window,
            query: query.into(),
            regex,
            input: iced::widget::Id::unique(),
            invalid: None,
            scope: None,
            seq: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            pending: HashMap::new(),
            groups: Vec::new(),
            selected: None,
        }
    }

    /// Search `targets` for the current query, dropping earlier results. Returns one search
    /// per pane.
    pub fn search(&mut self, targets: Vec<Target>) -> Vec<TabSearch> {
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.seq = super::next_seq();
        self.pending.clear();
        self.groups.clear();
        self.selected = None;
        let pattern = match SearchPattern::new(&SearchQuery::new(self.query.clone(), self.regex)) {
            Ok(p) => {
                self.invalid = None;
                p
            }
            Err(e) => {
                self.invalid = Some(e);
                None
            }
        };
        let Some(pattern) = pattern else {
            return Vec::new();
        };
        targets
            .into_iter()
            .map(|t| {
                let run = TabSearch {
                    seq: self.seq,
                    pane: t.pane,
                    terminal: t.terminal.clone(),
                    request: SearchRequest {
                        pattern: pattern.clone(),
                        below: None,
                        // One more than listed, to know there are more.
                        limit: PER_TAB + 1,
                        snippets: true,
                    },
                    cancel: self.cancel.clone(),
                };
                self.pending.insert(t.pane, t);
                run
            })
            .collect()
    }

    pub fn set_query(&mut self, query: String, targets: Vec<Target>) -> Vec<TabSearch> {
        self.query = query;
        self.search(targets)
    }

    pub fn toggle_regex(&mut self, targets: Vec<Target>) -> Vec<TabSearch> {
        self.regex = !self.regex;
        self.search(targets)
    }

    /// One pane's results arrived. Ignored when they belong to an earlier query.
    pub fn apply(&mut self, seq: u64, pane: PaneId, outcome: SearchOutcome) {
        if seq != self.seq || outcome.cancelled {
            return;
        }
        let Some(t) = self.pending.remove(&pane) else {
            return;
        };
        if outcome.hits.is_empty() {
            return;
        }
        let more = outcome.hits.len() > PER_TAB || outcome.resume.is_some();
        let mut hits = outcome.hits;
        hits.truncate(PER_TAB);
        let group = Group {
            tab: t.tab,
            pane,
            window: t.window,
            order: t.order,
            name: t.name,
            pane_label: t.pane_label,
            window_label: t.window_label,
            accent: t.accent,
            hits,
            more,
        };
        let at = self.groups.partition_point(|g| g.order < group.order);
        self.groups.insert(at, group);
        if self.selected.is_none() {
            self.selected = self
                .listed()
                .first()
                .copied()
                .map(|(g, h)| (self.groups[g].pane, h));
        }
    }

    /// The number of the current search (see [`super::next_seq`]).
    #[cfg(test)]
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Whether any pane is still being searched.
    pub fn searching(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// The listed matches as (group, match) positions, in order, at most [`TOTAL`].
    pub fn listed(&self) -> Vec<(usize, usize)> {
        self.groups
            .iter()
            .enumerate()
            .flat_map(|(g, group)| (0..group.hits.len()).map(move |h| (g, h)))
            .take(TOTAL)
            .collect()
    }

    /// Matches found in all (listed or not).
    pub fn total(&self) -> usize {
        self.groups.iter().map(|g| g.hits.len()).sum()
    }

    /// Position of the selected match among [`Self::listed`].
    pub fn selected_position(&self) -> Option<usize> {
        let (pane, h) = self.selected?;
        self.listed()
            .iter()
            .position(|&(g, hit)| self.groups[g].pane == pane && hit == h)
    }

    fn select_position(&mut self, position: usize) {
        if let Some(&(g, h)) = self.listed().get(position) {
            self.selected = Some((self.groups[g].pane, h));
        }
    }

    pub fn up(&mut self) {
        let n = self.listed().len();
        if n > 0 {
            let p = self.selected_position().unwrap_or(0);
            self.select_position((p + n - 1) % n);
        }
    }

    pub fn down(&mut self) {
        let n = self.listed().len();
        if n > 0 {
            let p = self.selected_position().map_or(0, |p| (p + 1) % n);
            self.select_position(p);
        }
    }

    /// Where the match at `position` (or the selected one) is.
    pub fn jump(&self, position: Option<usize>) -> Option<Jump> {
        let position = position.or_else(|| self.selected_position())?;
        let &(g, h) = self.listed().get(position)?;
        let group = &self.groups[g];
        Some(Jump {
            window: group.window,
            tab: group.tab,
            pane: group.pane,
            at: group.hits[h].range.start,
            query: self.query.clone(),
            regex: self.regex,
        })
    }

    /// Stop the searches in flight (the box is closing).
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// The status line under the field, if any: errors, progress and empty results.
    pub fn status(&self) -> Option<String> {
        if let Some(e) = self.invalid {
            return Some(e.to_string());
        }
        if self.query.is_empty() {
            return Some(if self.scope.is_some() {
                "Type to search this tab".into()
            } else {
                "Type to search every tab".into()
            });
        }
        if self.groups.is_empty() {
            return Some(if self.searching() {
                "Searching".into()
            } else {
                "No matches".into()
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use fbterm_term::{GridSize, SearchMatch, Terminal};
    use parking_lot::Mutex;

    use super::*;

    fn terminal() -> SharedTerminal {
        Arc::new(Mutex::new(Terminal::new(
            GridSize {
                cols: 20,
                rows: 5,
                cell_width: 8.0,
                cell_height: 16.0,
            },
            100,
        )))
    }

    fn target(window: window::Id, order: (usize, usize, usize), name: &str) -> Target {
        Target {
            tab: TabId::next(),
            pane: PaneId::next(),
            window,
            order,
            name: name.into(),
            pane_label: None,
            window_label: None,
            accent: None,
            terminal: terminal(),
        }
    }

    fn hits(n: usize) -> SearchOutcome {
        SearchOutcome {
            hits: (0..n)
                .map(|i| Hit {
                    range: SearchMatch {
                        start: StablePoint {
                            line: 100 - i as i64,
                            col: 0,
                        },
                        end: StablePoint {
                            line: 100 - i as i64,
                            col: 1,
                        },
                    },
                    snippet: None,
                })
                .collect(),
            ..SearchOutcome::default()
        }
    }

    #[test]
    fn results_stream_in_and_list_in_tab_order() {
        let w = window::Id::unique();
        let mut box_ = FindAll::new(w, "", false);
        let targets = vec![
            target(w, (0, 0, 0), "a"),
            target(w, (0, 1, 0), "b"),
            target(w, (0, 2, 0), "c"),
        ];
        let ids: Vec<PaneId> = targets.iter().map(|t| t.pane).collect();
        let runs = box_.set_query("x".into(), targets);
        assert_eq!(runs.len(), 3);
        assert!(runs.iter().all(|r| r.request.snippets));
        assert_eq!(box_.status().as_deref(), Some("Searching"));
        let seq = runs[0].seq;
        box_.apply(seq, ids[2], hits(2));
        assert_eq!(box_.status(), None);
        box_.apply(seq, ids[1], hits(0));
        box_.apply(seq, ids[0], hits(1));
        assert!(!box_.searching());
        let names: Vec<&str> = box_.groups().iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["a", "c"], "tabs without matches are left out");
        // The first result to arrive was selected and stays selected.
        assert_eq!(box_.selected_position(), Some(1));
    }

    #[test]
    fn per_tab_and_total_caps() {
        let w = window::Id::unique();
        let mut box_ = FindAll::new(w, "", false);
        let targets: Vec<Target> = (0..12).map(|i| target(w, (0, i, 0), "t")).collect();
        let ids: Vec<PaneId> = targets.iter().map(|t| t.pane).collect();
        let runs = box_.set_query("x".into(), targets);
        assert_eq!(runs[0].request.limit, PER_TAB + 1);
        for id in &ids {
            box_.apply(runs[0].seq, *id, hits(PER_TAB + 1));
        }
        assert!(
            box_.groups()
                .iter()
                .all(|g| g.hits.len() == PER_TAB && g.more)
        );
        assert_eq!(box_.total(), 12 * PER_TAB);
        assert_eq!(box_.listed().len(), TOTAL);
    }

    #[test]
    fn selection_wraps_and_a_new_query_drops_old_results() {
        let w = window::Id::unique();
        let mut box_ = FindAll::new(w, "", false);
        let targets = vec![target(w, (0, 0, 0), "a")];
        let id = targets[0].pane;
        let runs = box_.set_query("x".into(), targets.clone());
        box_.apply(runs[0].seq, id, hits(3));
        box_.up();
        assert_eq!(box_.selected_position(), Some(2));
        box_.down();
        assert_eq!(box_.selected_position(), Some(0));
        let jump = box_.jump(None).unwrap();
        assert_eq!(jump.pane, id);
        assert_eq!(jump.at.line, 100);
        assert_eq!(jump.query, "x");
        // A newer query: results of the old one are ignored.
        let newer = box_.set_query("xy".into(), targets);
        assert!(runs[0].cancel.load(Ordering::Relaxed));
        box_.apply(runs[0].seq, id, hits(3));
        assert!(box_.groups().is_empty());
        box_.apply(newer[0].seq, id, hits(1));
        assert_eq!(box_.total(), 1);
    }

    #[test]
    fn invalid_and_empty_queries_search_nothing() {
        let w = window::Id::unique();
        let mut box_ = FindAll::new(w, "", true);
        assert!(
            box_.set_query("(".into(), vec![target(w, (0, 0, 0), "a")])
                .is_empty()
        );
        assert_eq!(box_.status().as_deref(), Some("Invalid pattern"));
        assert!(
            box_.set_query(String::new(), vec![target(w, (0, 0, 0), "a")])
                .is_empty()
        );
        assert_eq!(box_.status().as_deref(), Some("Type to search every tab"));
        assert_eq!(box_.toggle_regex(vec![]).len(), 0);
        assert!(!box_.regex);
    }
}
