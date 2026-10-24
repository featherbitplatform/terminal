//! Search: the find bar of a tab (this module, its view in `bar_view`) and the search over every
//! tab (`all`, `all_view`). The states are plain data, so their transitions are tested without a
//! GUI; `run` connects them to the app and runs the searches on worker threads.

pub mod all;
pub mod all_view;
pub mod bar_view;
mod run;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fbterm_term::{
    MATCH_CAP, SearchMatch, SearchOutcome, SearchPattern, SearchQuery, SearchRequest, StablePoint,
};

/// How long after a search the bar waits before searching again for new output.
pub const REFRESH_AFTER: std::time::Duration = std::time::Duration::from_millis(400);

/// What the find bar of a window's active tab is asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindMsg {
    /// Open the bar (or focus it and select its text when open).
    Open,
    Query(String),
    /// Text typed while the bar's field did not have focus: appended, and the field focused.
    Type(String),
    ToggleRegex,
    /// Next match up the scrollback (older).
    Next,
    /// Next match down the scrollback (newer).
    Previous,
    Close,
}

/// One search to run for a find bar.
#[derive(Debug, Clone)]
pub struct SearchRun {
    pub seq: u64,
    pub request: SearchRequest,
    pub cancel: Arc<AtomicBool>,
}

/// What a move to the next or previous match needs.
#[derive(Debug)]
pub enum Step {
    /// The current match changed (or there is nothing to move to).
    Done,
    /// The older matches are not loaded yet: run this, then the move completes.
    Fetch(SearchRun),
}

/// A tab's find bar.
#[derive(Debug)]
pub struct FindBar {
    pub query: String,
    pub regex: bool,
    pub input: iced::widget::Id,
    /// The query does not compile.
    pub invalid: bool,
    pattern: Option<SearchPattern>,
    /// Matches found, newest first.
    hits: Vec<SearchMatch>,
    /// More scrollback is left above this stable line (the search stopped at its cap).
    resume: Option<i64>,
    current: Option<usize>,
    seq: u64,
    /// The search in flight: its number and whether it continues an earlier one.
    pending: Option<(u64, bool)>,
    cancel: Arc<AtomicBool>,
    /// When the next fresh result arrives, the match at (or the first above) this point is
    /// made current.
    anchor: Option<StablePoint>,
    /// Move to the next older match when the pending continuation arrives.
    advance: bool,
    /// The terminal generation the results are from.
    pub generation: u64,
    pub alt_screen: bool,
    /// A refresh for new output is waiting for its timer.
    pub refresh_scheduled: bool,
    pub last_search: Option<std::time::Instant>,
}

impl FindBar {
    pub fn new(query: impl Into<String>, regex: bool) -> Self {
        Self {
            query: query.into(),
            regex,
            input: iced::widget::Id::unique(),
            invalid: false,
            pattern: None,
            hits: Vec::new(),
            resume: None,
            current: None,
            seq: 0,
            pending: None,
            cancel: Arc::new(AtomicBool::new(false)),
            anchor: None,
            advance: false,
            generation: 0,
            alt_screen: false,
            refresh_scheduled: false,
            last_search: None,
        }
    }

    pub fn pattern(&self) -> Option<&SearchPattern> {
        self.pattern.as_ref()
    }

    pub fn current_match(&self) -> Option<SearchMatch> {
        self.current.and_then(|i| self.hits.get(i).copied())
    }

    pub fn searching(&self) -> bool {
        self.pending.is_some()
    }

    /// Start a search of the whole terminal, newest line first. Cancels the one in flight.
    fn run(&mut self, below: Option<i64>, more: bool) -> Option<SearchRun> {
        let pattern = self.pattern.clone()?;
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.seq += 1;
        self.pending = Some((self.seq, more));
        Some(SearchRun {
            seq: self.seq,
            request: SearchRequest {
                pattern,
                below,
                limit: MATCH_CAP,
                snippets: false,
            },
            cancel: self.cancel.clone(),
        })
    }

    /// Compile the query and search again; the first match at or above `anchor` (the bottom
    /// of the view, or the match to keep) becomes current.
    fn restart(&mut self, anchor: Option<StablePoint>) -> Option<SearchRun> {
        self.anchor = anchor;
        self.advance = false;
        match SearchPattern::new(&SearchQuery::new(self.query.clone(), self.regex)) {
            Ok(pattern) => {
                self.invalid = false;
                self.pattern = pattern;
            }
            Err(_) => {
                self.invalid = true;
                self.pattern = None;
            }
        }
        if self.pattern.is_none() {
            self.cancel.store(true, Ordering::Relaxed);
            self.pending = None;
            self.hits.clear();
            self.current = None;
            self.resume = None;
            return None;
        }
        self.run(None, false)
    }

    /// The query changed; `view_bottom` is the stable line at the bottom of the view.
    pub fn set_query(&mut self, query: String, view_bottom: i64) -> Option<SearchRun> {
        self.query = query;
        self.restart(Some(StablePoint {
            line: view_bottom,
            col: u16::MAX,
        }))
    }

    pub fn toggle_regex(&mut self, view_bottom: i64) -> Option<SearchRun> {
        self.regex = !self.regex;
        let q = std::mem::take(&mut self.query);
        self.set_query(q, view_bottom)
    }

    /// Search again for output that arrived since, keeping the current match.
    pub fn refresh(&mut self) -> Option<SearchRun> {
        self.refresh_scheduled = false;
        if self.pending.is_some() {
            return None;
        }
        let anchor = self.current_match().map(|m| m.start).or(self.anchor);
        self.restart(anchor)
    }

    /// Make the match at `point` current once the search started for it arrives.
    pub fn jump_to(&mut self, point: StablePoint) -> Option<SearchRun> {
        self.restart(Some(point))
    }

    /// Whether results from `generation` (alternate screen `alt`) are out of date.
    pub fn stale(&self, generation: u64, alt: bool) -> bool {
        self.pattern.is_some() && (generation != self.generation || alt != self.alt_screen)
    }

    /// Apply a finished search. Returns whether the current match changed (the view should
    /// scroll to it).
    pub fn apply(&mut self, seq: u64, outcome: SearchOutcome, now: std::time::Instant) -> bool {
        let Some((pending, more)) = self.pending else {
            return false;
        };
        if pending != seq || outcome.cancelled {
            return false;
        }
        self.pending = None;
        self.last_search = Some(now);
        self.alt_screen = outcome.alt_screen;
        self.resume = outcome.resume;
        let found: Vec<SearchMatch> = outcome.hits.into_iter().map(|h| h.range).collect();
        if more {
            let had = self.hits.len();
            self.hits.extend(found);
            if std::mem::take(&mut self.advance) {
                self.current = Some(if self.hits.len() > had { had } else { 0 });
                return true;
            }
            return false;
        }
        self.generation = outcome.generation;
        let before = self.current_match();
        self.hits = found;
        self.current = match self.anchor.take() {
            _ if self.hits.is_empty() => None,
            // Newest first: the first match starting at or above the anchor.
            Some(a) => Some(
                self.hits
                    .iter()
                    .position(|m| m.start <= a)
                    .unwrap_or(self.hits.len() - 1),
            ),
            None => Some(0),
        };
        self.current_match() != before
    }

    /// Move up the scrollback to the next older match, wrapping to the newest.
    pub fn next(&mut self) -> Step {
        let n = self.hits.len();
        if n == 0 {
            return Step::Done;
        }
        match self.current {
            Some(i) if i + 1 < n => self.current = Some(i + 1),
            Some(_) if self.resume.is_some() => {
                if self.pending.is_none() {
                    let below = self.resume;
                    if let Some(run) = self.run(below, true) {
                        self.advance = true;
                        return Step::Fetch(run);
                    }
                }
            }
            Some(_) | None => self.current = Some(0),
        }
        Step::Done
    }

    /// Move down the scrollback to the next newer match, wrapping to the oldest found.
    pub fn previous(&mut self) {
        let n = self.hits.len();
        if n == 0 {
            return;
        }
        self.current = Some(match self.current {
            Some(0) | None => n - 1,
            Some(i) => i - 1,
        });
    }

    /// "3 of 41", "3 of 1000+", "No matches", "Invalid pattern", or nothing (no query yet, or
    /// the first search still running).
    pub fn counter(&self) -> String {
        if self.invalid {
            return "Invalid pattern".into();
        }
        if self.pattern.is_none() || (self.hits.is_empty() && self.pending.is_some()) {
            return String::new();
        }
        match self.current {
            None => "No matches".into(),
            Some(i) => format!(
                "{} of {}{}",
                i + 1,
                self.hits.len(),
                if self.resume.is_some() { "+" } else { "" }
            ),
        }
    }

    /// Stop the search in flight (the bar is closing).
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use fbterm_term::Hit;

    use super::*;

    fn m(line: i64, col: u16) -> SearchMatch {
        SearchMatch {
            start: StablePoint { line, col },
            end: StablePoint { line, col: col + 2 },
        }
    }

    fn outcome(lines: &[i64], resume: Option<i64>) -> SearchOutcome {
        SearchOutcome {
            hits: lines
                .iter()
                .map(|&l| Hit {
                    range: m(l, 0),
                    snippet: None,
                })
                .collect(),
            resume,
            generation: 7,
            ..SearchOutcome::default()
        }
    }

    fn seq(run: &Option<SearchRun>) -> u64 {
        run.as_ref().expect("a search starts").seq
    }

    #[test]
    fn typing_searches_and_counts_from_the_newest_match_in_view() {
        let mut bar = FindBar::new("", false);
        assert_eq!(bar.counter(), "");
        let run = bar.set_query("err".into(), 30);
        assert!(bar.searching());
        assert_eq!(bar.counter(), "", "nothing yet while the first search runs");
        assert!(bar.apply(seq(&run), outcome(&[40, 30, 20, 10], None), Instant::now()));
        // The view ends at line 30: the newest match at or above it is current.
        assert_eq!(bar.current_match(), Some(m(30, 0)));
        assert_eq!(bar.counter(), "2 of 4");
        assert_eq!(bar.generation, 7);
        assert!(!bar.stale(7, false));
        assert!(bar.stale(8, false));
    }

    #[test]
    fn next_and_previous_wrap_around() {
        let mut bar = FindBar::new("", false);
        let run = bar.set_query("x".into(), 100);
        bar.apply(seq(&run), outcome(&[3, 2, 1], None), Instant::now());
        assert_eq!(bar.counter(), "1 of 3");
        assert!(matches!(bar.next(), Step::Done));
        assert!(matches!(bar.next(), Step::Done));
        assert_eq!(bar.counter(), "3 of 3");
        assert!(matches!(bar.next(), Step::Done));
        assert_eq!(bar.counter(), "1 of 3", "wraps to the newest");
        bar.previous();
        assert_eq!(bar.counter(), "3 of 3", "wraps to the oldest");
        bar.previous();
        assert_eq!(bar.current_match(), Some(m(2, 0)));
    }

    #[test]
    fn stale_results_are_dropped() {
        let mut bar = FindBar::new("", false);
        let first = bar.set_query("a".into(), 0);
        let second = bar.set_query("ab".into(), 0);
        assert!(!bar.apply(seq(&first), outcome(&[1], None), Instant::now()));
        assert_eq!(bar.counter(), "");
        assert!(first.unwrap().cancel.load(Ordering::Relaxed), "superseded");
        bar.apply(seq(&second), outcome(&[], None), Instant::now());
        assert_eq!(bar.counter(), "No matches");
    }

    #[test]
    fn invalid_and_empty_queries() {
        let mut bar = FindBar::new("", true);
        assert!(bar.set_query("(".into(), 0).is_none());
        assert_eq!(bar.counter(), "Invalid pattern");
        assert!(bar.set_query(String::new(), 0).is_none());
        assert_eq!(bar.counter(), "");
        // The same text is fine as plain text.
        assert!(bar.toggle_regex(0).is_none(), "still empty");
        assert!(!bar.regex);
        assert!(bar.set_query("(".into(), 0).is_some());
        assert_eq!(bar.counter(), "");
    }

    #[test]
    fn regex_toggle_searches_again() {
        let mut bar = FindBar::new("", false);
        bar.set_query("a.c".into(), 0);
        let run = bar.toggle_regex(0).expect("searches again");
        assert!(bar.regex);
        assert!(run.request.pattern.query().regex);
        assert_eq!(bar.query, "a.c");
    }

    #[test]
    fn capped_results_load_more_when_moving_past_the_oldest() {
        let mut bar = FindBar::new("", false);
        let run = bar.set_query("x".into(), 100);
        bar.apply(seq(&run), outcome(&[9, 8], Some(8)), Instant::now());
        assert_eq!(bar.counter(), "1 of 2+");
        assert!(matches!(bar.next(), Step::Done));
        let Step::Fetch(more) = bar.next() else {
            panic!("older matches are fetched");
        };
        assert_eq!(more.request.below, Some(8));
        assert!(bar.apply(more.seq, outcome(&[5, 4], None), Instant::now()));
        assert_eq!(bar.counter(), "3 of 4");
        assert_eq!(bar.current_match(), Some(m(5, 0)));
    }

    #[test]
    fn a_refresh_keeps_the_current_match() {
        let mut bar = FindBar::new("", false);
        let run = bar.set_query("x".into(), 100);
        bar.apply(seq(&run), outcome(&[30, 20, 10], None), Instant::now());
        bar.next();
        assert_eq!(bar.current_match(), Some(m(20, 0)));
        // New output added a newer match; the current one stays.
        let again = bar.refresh();
        assert!(!bar.apply(
            seq(&again),
            outcome(&[50, 30, 20, 10], None),
            Instant::now()
        ));
        assert_eq!(bar.current_match(), Some(m(20, 0)));
        assert_eq!(bar.counter(), "3 of 4");
    }

    #[test]
    fn a_jump_makes_that_match_current() {
        let mut bar = FindBar::new("x", false);
        let run = bar.jump_to(StablePoint { line: 20, col: 0 });
        bar.apply(seq(&run), outcome(&[30, 20, 10], None), Instant::now());
        assert_eq!(bar.current_match(), Some(m(20, 0)));
    }
}
