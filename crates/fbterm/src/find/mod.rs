//! Search: the find bar of a tab (this module, its view in `bar_view`) and the search over every
//! tab (`all`, `all_view`). The states are plain data, so their transitions are tested without a
//! GUI; `run` connects them to the app and runs the searches on worker threads.

pub mod all;
pub mod all_view;
pub mod bar_view;
mod run;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use fbterm_term::{
    MATCH_CAP, PatternError, SearchMatch, SearchOutcome, SearchPattern, SearchQuery, SearchRequest,
    StablePoint,
};

/// Most matches a bar keeps loaded; moving past the oldest of them wraps to the newest.
pub const HITS_CAP: usize = 10_000;

/// A search number unique in the process, so a result can never be taken for another bar's
/// or box's search (a bar closed and opened again starts over, its numbers do not).
pub fn next_seq() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// What a bar's search in flight is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A new query (or after a resize): replaces the matches.
    Fresh,
    /// The same query again for new output: keeps the current match and the older matches
    /// loaded past the newest [`MATCH_CAP`].
    Refresh,
    /// Older matches past the loaded ones, appended.
    More,
}

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
    pub invalid: Option<PatternError>,
    pattern: Option<SearchPattern>,
    /// Matches found, newest first.
    hits: Vec<SearchMatch>,
    /// More scrollback is left above this stable line (the search stopped at its cap).
    resume: Option<i64>,
    current: Option<usize>,
    /// The search in flight: its number and what it is for.
    pending: Option<(u64, Kind)>,
    cancel: Arc<AtomicBool>,
    /// When the next fresh result arrives, the match at (or the first above) this point is
    /// made current.
    anchor: Option<StablePoint>,
    /// Move to the next older match when the pending continuation arrives.
    advance: bool,
    /// The terminal generation the results are from.
    pub generation: u64,
    pub alt_screen: bool,
    /// Columns of the terminal the matches were found at (0: none yet).
    pub cols: u16,
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
            invalid: None,
            pattern: None,
            hits: Vec::new(),
            resume: None,
            current: None,
            pending: None,
            cancel: Arc::new(AtomicBool::new(false)),
            anchor: None,
            advance: false,
            generation: 0,
            alt_screen: false,
            cols: 0,
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

    /// The sequence number of the search in flight (tests of other modules drive results by it).
    #[cfg(test)]
    pub(crate) fn pending_seq(&self) -> Option<u64> {
        self.pending.map(|(seq, _)| seq)
    }

    pub fn searching(&self) -> bool {
        self.pending.is_some()
    }

    /// Start a search of the whole terminal, newest line first. Cancels the one in flight.
    fn run(&mut self, below: Option<i64>, kind: Kind) -> Option<SearchRun> {
        let pattern = self.pattern.clone()?;
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        let seq = next_seq();
        self.pending = Some((seq, kind));
        Some(SearchRun {
            seq,
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
    fn restart(&mut self, anchor: Option<StablePoint>, kind: Kind) -> Option<SearchRun> {
        self.anchor = anchor;
        self.advance = false;
        match SearchPattern::new(&SearchQuery::new(self.query.clone(), self.regex)) {
            Ok(pattern) => {
                self.invalid = None;
                self.pattern = pattern;
            }
            Err(e) => {
                self.invalid = Some(e);
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
        self.run(None, kind)
    }

    fn bottom(view_bottom: i64) -> StablePoint {
        StablePoint {
            line: view_bottom,
            col: u16::MAX,
        }
    }

    /// The query changed; `view_bottom` is the stable line at the bottom of the view.
    pub fn set_query(&mut self, query: String, view_bottom: i64) -> Option<SearchRun> {
        self.query = query;
        self.restart(Some(Self::bottom(view_bottom)), Kind::Fresh)
    }

    pub fn toggle_regex(&mut self, view_bottom: i64) -> Option<SearchRun> {
        self.regex = !self.regex;
        let q = std::mem::take(&mut self.query);
        self.set_query(q, view_bottom)
    }

    /// Search again for output that arrived since, keeping the current match. `alt` is
    /// whether the terminal shows the alternate screen now: after a switch the old positions
    /// mean nothing, so the search starts over from the bottom of the view.
    pub fn refresh(&mut self, alt: bool, view_bottom: i64) -> Option<SearchRun> {
        self.refresh_scheduled = false;
        if self.pending.is_some() {
            return None;
        }
        if alt != self.alt_screen {
            self.hits.clear();
            self.current = None;
            self.resume = None;
            return self.restart(Some(Self::bottom(view_bottom)), Kind::Fresh);
        }
        let anchor = self.current_match().map(|m| m.start).or(self.anchor);
        self.restart(anchor, Kind::Refresh)
    }

    /// The terminal was resized to `cols` columns. Rows alone keep positions, so the current
    /// match (or the one a pending jump is for) stays; new columns reflow wrapped lines, so the
    /// matches are dropped until the new search arrives, which starts from the view's bottom.
    pub fn resized(&mut self, cols: u16, view_bottom: i64) -> Option<SearchRun> {
        if self.cols != 0 && cols != self.cols {
            self.hits.clear();
            self.current = None;
            self.resume = None;
            return self.restart(Some(Self::bottom(view_bottom)), Kind::Fresh);
        }
        let pending_anchor = self.pending.and(self.anchor);
        let anchor = pending_anchor
            .or_else(|| self.current_match().map(|m| m.start))
            .unwrap_or(Self::bottom(view_bottom));
        self.restart(Some(anchor), Kind::Fresh)
    }

    /// Make the match at `point` current once the search started for it arrives.
    pub fn jump_to(&mut self, point: StablePoint) -> Option<SearchRun> {
        self.restart(Some(point), Kind::Fresh)
    }

    /// Keep at most [`HITS_CAP`] matches loaded.
    fn cap_hits(&mut self) {
        if self.hits.len() > HITS_CAP {
            self.hits.truncate(HITS_CAP);
            self.resume = self.hits.last().map(|m| m.start.line);
            self.current = self.current.map(|i| i.min(HITS_CAP - 1));
        }
    }

    /// Whether results from `generation` (alternate screen `alt`) are out of date.
    pub fn stale(&self, generation: u64, alt: bool) -> bool {
        self.pattern.is_some() && (generation != self.generation || alt != self.alt_screen)
    }

    /// Apply a finished search. Returns whether the current match changed (the view should
    /// scroll to it).
    pub fn apply(&mut self, seq: u64, outcome: SearchOutcome, now: std::time::Instant) -> bool {
        let Some((pending, kind)) = self.pending else {
            return false;
        };
        if pending != seq || outcome.cancelled {
            return false;
        }
        self.pending = None;
        self.last_search = Some(now);
        self.alt_screen = outcome.alt_screen;
        self.cols = outcome.cols;
        let found: Vec<SearchMatch> = outcome.hits.into_iter().map(|h| h.range).collect();
        if kind == Kind::More {
            self.resume = outcome.resume;
            let had = self.hits.len();
            self.hits.extend(found);
            self.cap_hits();
            if std::mem::take(&mut self.advance) {
                self.current = Some(if self.hits.len() > had { had } else { 0 });
                return true;
            }
            return false;
        }
        self.generation = outcome.generation;
        let before = self.current_match();
        let old = std::mem::replace(&mut self.hits, found);
        let old_resume = std::mem::replace(&mut self.resume, outcome.resume);
        // A refresh stopped at its cap: the older matches loaded before stay, so the current
        // match (and everything past the newest thousand) is kept.
        if kind == Kind::Refresh
            && let Some(edge) = outcome.resume
        {
            let older: Vec<SearchMatch> = old.into_iter().filter(|m| m.start.line < edge).collect();
            if !older.is_empty() {
                self.hits.extend(older);
                self.resume = old_resume;
            }
        }
        self.cap_hits();
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
            Some(_) if self.resume.is_some() && n < HITS_CAP => {
                if self.pending.is_none() {
                    let below = self.resume;
                    if let Some(run) = self.run(below, Kind::More) {
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

    /// "3 of 41", "3 of 1000+", "No matches", "Invalid pattern", "Query too long", or nothing
    /// (no query yet, or the first search still running).
    pub fn counter(&self) -> String {
        if let Some(e) = self.invalid {
            return e.to_string();
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
        let again = bar.refresh(false, 100);
        assert!(!bar.apply(
            seq(&again),
            outcome(&[50, 30, 20, 10], None),
            Instant::now()
        ));
        assert_eq!(bar.current_match(), Some(m(20, 0)));
        assert_eq!(bar.counter(), "3 of 4");
    }

    fn lines(from: i64, to: i64) -> Vec<i64> {
        (to..=from).rev().collect()
    }

    #[test]
    fn a_refresh_past_the_cap_keeps_the_older_matches_and_the_current_one() {
        let mut bar = FindBar::new("", false);
        // 3000 matches, one per line 1..=3000; the newest thousand first.
        let run = bar.set_query("x".into(), 5000);
        bar.apply(
            seq(&run),
            outcome(&lines(3000, 2001), Some(2001)),
            Instant::now(),
        );
        for _ in 0..1500 {
            if let Step::Fetch(more) = bar.next() {
                let below = more.request.below.unwrap();
                let got = outcome(
                    &lines(below - 1, (below - 1000).max(1)),
                    (below > 1001).then_some(below - 1000),
                );
                bar.apply(more.seq, got, Instant::now());
            }
        }
        let current = bar.current_match().unwrap();
        assert_eq!(current.start.line, 1500);
        // Output streams in: ten new matching lines, the refresh again stops at a thousand.
        for round in 1..=3 {
            let newest = 3000 + 10 * round;
            let again = bar.refresh(false, newest);
            assert!(!bar.apply(
                seq(&again),
                outcome(&lines(newest, newest - 999), Some(newest - 999)),
                Instant::now()
            ));
            assert_eq!(bar.current_match(), Some(current), "round {round}");
            let expected = format!("{} of {}+", 1501 + 10 * round, 2000 + 10 * round);
            assert_eq!(bar.counter(), expected);
        }
    }

    #[test]
    fn at_most_hits_cap_matches_are_loaded() {
        let mut bar = FindBar::new("", false);
        let run = bar.set_query("x".into(), 100_000);
        bar.apply(
            seq(&run),
            outcome(&lines(20_000, 19_001), Some(19_001)),
            Instant::now(),
        );
        loop {
            match bar.next() {
                Step::Fetch(more) => {
                    let below = more.request.below.unwrap();
                    bar.apply(
                        more.seq,
                        outcome(&lines(below - 1, below - 1000), Some(below - 1000)),
                        Instant::now(),
                    );
                }
                Step::Done if bar.current == Some(0) => break,
                Step::Done => {}
            }
        }
        assert_eq!(bar.hits.len(), HITS_CAP, "wrapped to the newest at the cap");
    }

    #[test]
    fn a_resize_keeps_the_match_unless_the_width_changes() {
        let mut bar = FindBar::new("", false);
        let run = bar.set_query("x".into(), 100);
        let mut first = outcome(&[30, 20, 10], None);
        first.cols = 80;
        bar.apply(seq(&run), first, Instant::now());
        bar.next();
        // Rows only: the current match is the anchor.
        let again = bar.resized(80, 120);
        let mut same = outcome(&[30, 20, 10], None);
        same.cols = 80;
        bar.apply(seq(&again), same, Instant::now());
        assert_eq!(bar.current_match(), Some(m(20, 0)));
        // New width: nothing current until the new result, then from the view's bottom.
        let again = bar.resized(100, 25);
        assert_eq!(bar.current_match(), None);
        let mut wide = outcome(&[30, 21, 11], None);
        wide.cols = 100;
        bar.apply(seq(&again), wide, Instant::now());
        assert_eq!(bar.current_match(), Some(m(21, 0)));
        // A pending jump keeps its target through a resize of the rows.
        let jump = bar.jump_to(StablePoint { line: 11, col: 0 });
        drop(jump);
        let again = bar.resized(100, 25);
        let mut res = outcome(&[30, 21, 11], None);
        res.cols = 100;
        bar.apply(seq(&again), res, Instant::now());
        assert_eq!(bar.current_match(), Some(m(11, 0)));
    }

    #[test]
    fn switching_screens_starts_over() {
        let mut bar = FindBar::new("", false);
        let run = bar.set_query("x".into(), 100);
        bar.apply(seq(&run), outcome(&[30, 20], None), Instant::now());
        bar.next();
        let again = bar.refresh(true, 5);
        assert_eq!(bar.current_match(), None);
        let mut alt = outcome(&[4, 2], None);
        alt.alt_screen = true;
        bar.apply(seq(&again), alt, Instant::now());
        assert_eq!(bar.current_match(), Some(m(4, 0)));
    }

    #[test]
    fn search_numbers_are_unique_across_bars() {
        let mut a = FindBar::new("", false);
        let ra = a.set_query("x".into(), 0);
        let mut b = FindBar::new("", false);
        let rb = b.set_query("x".into(), 0);
        assert_ne!(seq(&ra), seq(&rb));
        // A result for the old bar's search is not taken by the new one.
        assert!(!b.apply(seq(&ra), outcome(&[1], None), Instant::now()));
    }

    #[test]
    fn an_over_long_query_says_so() {
        let mut bar = FindBar::new("", false);
        assert!(
            bar.set_query("y".repeat(fbterm_term::MAX_QUERY_CHARS + 1), 0)
                .is_none()
        );
        assert_eq!(bar.counter(), "Query too long");
    }

    #[test]
    fn a_jump_makes_that_match_current() {
        let mut bar = FindBar::new("x", false);
        let run = bar.jump_to(StablePoint { line: 20, col: 0 });
        bar.apply(seq(&run), outcome(&[30, 20, 10], None), Instant::now());
        assert_eq!(bar.current_match(), Some(m(20, 0)));
    }
}
