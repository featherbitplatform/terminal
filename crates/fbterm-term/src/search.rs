//! Scrollback search: a query (plain text or a regular expression, smart case) matched against
//! a terminal's whole scrollback and screen.
//!
//! The search walks the grid from the newest line upward in chunks of [`CHUNK_ROWS`] rows and
//! locks the terminal for one chunk at a time, so output keeps flowing and frames keep drawing
//! while a long history is searched (run it off the UI thread with [`search`]). Positions are
//! [`StablePoint`]s, whose line numbers do not change while output scrolls the text upward, so
//! matches found a moment ago still point at the same text.
//!
//! Wrapped rows are searched as one line, so a match can span a soft wrap. Matches never span a
//! real line break.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use regex::{Regex, RegexBuilder};

use crate::{SharedTerminal, Terminal};

/// Matches a search collects before it stops; the count then reads "1000+".
pub const MATCH_CAP: usize = 1000;

/// Rows searched per lock of the terminal.
pub const CHUNK_ROWS: usize = 1000;

/// Characters of context kept before a match in a [`Snippet`].
const SNIPPET_BEFORE: usize = 32;
/// A match ending within this many characters of the line start keeps the line from its
/// start (it fits a results row as it is).
const SNIPPET_FITS: usize = 96;
/// Most characters in a [`Snippet`].
const SNIPPET_LEN: usize = 120;
/// Most rows one wrapped line is followed for while finding the matches on screen.
const MAX_WRAP_ROWS: i32 = 64;

/// What the user typed: plain text (default) or a regular expression.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct SearchQuery {
    pub text: String,
    pub regex: bool,
}

impl SearchQuery {
    pub fn new(text: impl Into<String>, regex: bool) -> Self {
        Self {
            text: text.into(),
            regex,
        }
    }
}

/// The query is not a valid regular expression (or is too large to compile).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternError;

impl std::fmt::Display for PatternError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Invalid pattern")
    }
}

impl std::error::Error for PatternError {}

/// A compiled query. Two patterns are equal when their queries are.
#[derive(Debug, Clone)]
pub struct SearchPattern {
    query: SearchQuery,
    re: Regex,
}

impl PartialEq for SearchPattern {
    fn eq(&self, other: &Self) -> bool {
        self.query == other.query
    }
}

/// Smart case: whether `text` asks for a case-sensitive search (it has an uppercase letter).
/// In a regular expression the letter after a backslash is syntax (`\S`, `\W`), not text.
pub fn is_case_sensitive(text: &str, regex: bool) -> bool {
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if regex && c == '\\' {
            chars.next();
            continue;
        }
        if c.is_uppercase() {
            return true;
        }
    }
    false
}

impl SearchPattern {
    /// Compile `query`: plain text is matched literally; case is ignored unless the query has
    /// an uppercase letter. `Ok(None)` for an empty query.
    pub fn new(query: &SearchQuery) -> Result<Option<Self>, PatternError> {
        if query.text.is_empty() {
            return Ok(None);
        }
        let source = if query.regex {
            query.text.clone()
        } else {
            regex::escape(&query.text)
        };
        let re = RegexBuilder::new(&source)
            .case_insensitive(!is_case_sensitive(&query.text, query.regex))
            .size_limit(1 << 20)
            .dfa_size_limit(4 << 20)
            .build()
            .map_err(|_| PatternError)?;
        Ok(Some(Self {
            query: query.clone(),
            re,
        }))
    }

    pub fn query(&self) -> &SearchQuery {
        &self.query
    }

    /// Non-empty matches in `text`, as byte ranges, left to right.
    pub fn find_iter<'a>(&'a self, text: &'a str) -> impl Iterator<Item = Range<usize>> + 'a {
        self.re
            .find_iter(text)
            .filter(|m| !m.is_empty())
            .map(|m| m.range())
    }
}

/// A cell position whose line number stays the same while output scrolls the text upward:
/// `line` counts from the first line the terminal ever had (see [`Terminal::stable_line`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StablePoint {
    pub line: i64,
    pub col: u16,
}

/// A match from `start` to `end`, both inclusive (a wide character's end covers both cells).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SearchMatch {
    pub start: StablePoint,
    pub end: StablePoint,
}

/// The line a match is on, trimmed around it, for a results list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub text: String,
    /// Byte range of the match in `text`.
    pub highlight: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub range: SearchMatch,
    /// Only when asked for (see [`SearchRequest::snippets`]).
    pub snippet: Option<Snippet>,
}

/// One search over a terminal.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub pattern: SearchPattern,
    /// Search only lines above this stable line (continuing a search that stopped at its
    /// limit); `None` starts at the newest line.
    pub below: Option<i64>,
    /// Stop after this many hits (whole lines at a time, so a few more can come back).
    pub limit: usize,
    pub snippets: bool,
}

/// What a search found, newest match first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchOutcome {
    pub hits: Vec<Hit>,
    /// The search stopped at its limit with history left above this stable line.
    pub resume: Option<i64>,
    /// The search was cancelled before it finished; the hits are partial.
    pub cancelled: bool,
    /// The terminal showed the alternate screen (which has no scrollback).
    pub alt_screen: bool,
    /// The terminal's [`Terminal::generation`] when the search finished.
    pub generation: u64,
}

/// One logical line (rows joined across soft wraps) as text, with where each character sits.
struct LineText {
    text: String,
    /// Per character: byte offset in `text`, cell index from the first row's column 0, and
    /// width in cells.
    cells: Vec<(usize, u32, u8)>,
    /// Stable line of the first row.
    top: i64,
    cols: usize,
}

impl LineText {
    fn point(&self, cell: u32) -> StablePoint {
        let cols = self.cols.max(1) as u32;
        StablePoint {
            line: self.top + i64::from(cell / cols),
            col: (cell % cols) as u16,
        }
    }

    fn to_match(&self, bytes: &Range<usize>) -> Option<SearchMatch> {
        let first = self.cells.partition_point(|c| c.0 < bytes.start);
        let last = self
            .cells
            .partition_point(|c| c.0 < bytes.end)
            .checked_sub(1)?;
        let (_, start, _) = *self.cells.get(first)?;
        let (_, end, width) = *self.cells.get(last)?;
        Some(SearchMatch {
            start: self.point(start),
            end: self.point(end + u32::from(width.max(1)) - 1),
        })
    }
}

/// `text` trimmed to the match at `range` with some context before it, marked with an
/// ellipsis where cut.
pub fn snippet(text: &str, range: Range<usize>) -> Snippet {
    let lead = text.len() - text.trim_start().len();
    let start = range.start.max(lead);
    // Context before the match: the whole line when the match ends early enough to fit,
    // otherwise SNIPPET_BEFORE characters.
    let mut from = start;
    if text[lead..range.end.max(lead)].chars().count() <= SNIPPET_FITS {
        from = lead;
    } else {
        for (n, (i, _)) in text[lead..start].char_indices().rev().enumerate() {
            if n == SNIPPET_BEFORE {
                break;
            }
            from = lead + i;
        }
    }
    let mut out = String::new();
    if from > lead {
        out.push('\u{2026}');
    }
    let offset = out.len() as isize - from as isize;
    let mut end = text.len();
    for (n, (i, _)) in text[from..].char_indices().enumerate() {
        if n == SNIPPET_LEN {
            end = from + i;
            break;
        }
    }
    // Always keep the whole match (up to the limit above).
    out.push_str(&text[from..end]);
    let trimmed = end < text.len();
    if trimmed {
        out.push('\u{2026}');
    }
    let shift = |b: usize| (b as isize + offset) as usize;
    let hl_end = range.end.min(end);
    Snippet {
        highlight: shift(start)..shift(hl_end.max(start)),
        text: out,
    }
}

impl Terminal {
    /// The stable line number of grid line `line` (see [`StablePoint`]).
    pub fn stable_line(&self, line: Line) -> i64 {
        self.term.grid().scrolled_lines() as i64 + i64::from(line.0)
    }

    /// The grid line showing stable line `stable`, if it is still in the scrollback or on screen.
    pub fn grid_line(&self, stable: i64) -> Option<Line> {
        let grid = self.term.grid();
        let line = stable - grid.scrolled_lines() as i64;
        let line = i32::try_from(line).ok()?;
        (grid.topmost_line().0..=grid.bottommost_line().0)
            .contains(&line)
            .then_some(Line(line))
    }

    /// The stable line at the bottom of the view.
    pub fn view_bottom(&self) -> i64 {
        let grid = self.term.grid();
        let bottom = grid.screen_lines() as i32 - 1 - grid.display_offset() as i32;
        self.stable_line(Line(bottom))
    }

    /// Whether row `line` continues on the next row (soft wrap).
    fn wraps(&self, line: i32) -> bool {
        let grid = self.term.grid();
        let last = grid.columns() - 1;
        grid[Line(line)][Column(last)]
            .flags
            .contains(Flags::WRAPLINE)
    }

    fn line_text(&self, first: i32, last: i32) -> LineText {
        let grid = self.term.grid();
        let cols = grid.columns();
        let mut text = String::new();
        let mut cells = Vec::new();
        for (k, line) in (first..=last).enumerate() {
            let row = &grid[Line(line)];
            for c in 0..cols {
                let cell = &row[Column(c)];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                    2
                } else {
                    1
                };
                cells.push((text.len(), (k * cols + c) as u32, width));
                text.push(if cell.c == '\t' { ' ' } else { cell.c });
                if let Some(zw) = cell.zerowidth() {
                    text.extend(zw.iter());
                }
            }
        }
        while text.ends_with(' ') {
            text.pop();
            if cells.last().is_some_and(|c| c.0 == text.len()) {
                cells.pop();
            }
        }
        LineText {
            text,
            cells,
            top: self.stable_line(Line(first)),
            cols,
        }
    }

    /// Matches on the logical line of rows `first..=last`, left to right.
    fn line_hits(
        &self,
        pattern: &SearchPattern,
        first: i32,
        last: i32,
        snippets: bool,
    ) -> Vec<Hit> {
        let line = self.line_text(first, last);
        pattern
            .find_iter(&line.text)
            .filter_map(|bytes| {
                let range = line.to_match(&bytes)?;
                let snippet = snippets.then(|| snippet(&line.text, bytes));
                Some(Hit { range, snippet })
            })
            .collect()
    }

    /// One chunk of a search: about `rows` rows ending above `below` (or at the newest line),
    /// newest match first, stopping after whole lines once `limit` hits are found. Returns the
    /// hits and the stable line to continue above, if any history is left.
    fn search_chunk(
        &self,
        req: &SearchRequest,
        below: Option<i64>,
        rows: usize,
        limit: usize,
    ) -> (Vec<Hit>, Option<i64>) {
        let grid = self.term.grid();
        let top = grid.topmost_line().0;
        let scrolled = grid.scrolled_lines() as i64;
        let last = match below {
            None => grid.bottommost_line().0,
            Some(s) => match i32::try_from(s - scrolled - 1) {
                Ok(l) => l.min(grid.bottommost_line().0),
                Err(_) => return (Vec::new(), None),
            },
        };
        if last < top {
            return (Vec::new(), None);
        }
        let mut first = (last - rows.max(1) as i32 + 1).max(top);
        while first > top && self.wraps(first - 1) {
            first -= 1;
        }
        let mut hits = Vec::new();
        let mut end = last;
        while end >= first {
            let mut start = end;
            while start > first && self.wraps(start - 1) {
                start -= 1;
            }
            let mut line = self.line_hits(&req.pattern, start, end, req.snippets);
            line.reverse();
            hits.extend(line);
            end = start - 1;
            if hits.len() >= limit {
                return (hits, (start > top).then(|| self.stable_line(Line(start))));
            }
        }
        (hits, (first > top).then(|| self.stable_line(Line(first))))
    }

    /// Matches on the lines in view (whole logical lines, so a wrapped match half off screen
    /// still counts), top to bottom.
    pub fn visible_matches(&self, pattern: &SearchPattern) -> Vec<SearchMatch> {
        let grid = self.term.grid();
        let offset = grid.display_offset() as i32;
        let (top, bottom) = (grid.topmost_line().0, grid.bottommost_line().0);
        let mut first = -offset;
        let mut last = grid.screen_lines() as i32 - 1 - offset;
        let floor = (first - MAX_WRAP_ROWS).max(top);
        while first > floor && self.wraps(first - 1) {
            first -= 1;
        }
        let ceiling = (last + MAX_WRAP_ROWS).min(bottom);
        while last < ceiling && self.wraps(last) {
            last += 1;
        }
        let mut out = Vec::new();
        let mut start = first;
        while start <= last {
            let mut end = start;
            while end < last && self.wraps(end) {
                end += 1;
            }
            out.extend(
                self.line_hits(pattern, start, end, false)
                    .into_iter()
                    .map(|h| h.range),
            );
            start = end + 1;
        }
        out
    }

    /// Scroll so `m` is in view: nothing when it already is, otherwise its line lands near the
    /// middle of the view. Returns whether the view moved.
    pub fn reveal(&mut self, m: &SearchMatch) -> bool {
        let Some(line) = self.grid_line(m.start.line) else {
            return false;
        };
        let end = self.grid_line(m.end.line).unwrap_or(line);
        let grid = self.term.grid();
        let rows = grid.screen_lines() as i32;
        let offset = grid.display_offset() as i32;
        let view_top = -offset;
        if line.0 >= view_top && end.0 < view_top + rows {
            return false;
        }
        let history = grid.history_size() as i32;
        let target = (rows / 2 - line.0).clamp(0, history);
        self.term.scroll_display(Scroll::Delta(target - offset));
        self.generation += 1;
        true
    }
}

/// Search `term` for `req`, locking it one chunk at a time. Checks `cancel` between chunks.
/// Meant for a worker thread: a full 10,000-line scrollback takes a few milliseconds.
pub fn search(term: &SharedTerminal, req: &SearchRequest, cancel: &AtomicBool) -> SearchOutcome {
    let mut out = SearchOutcome::default();
    let mut below = req.below;
    loop {
        if cancel.load(Ordering::Relaxed) {
            out.cancelled = true;
            break;
        }
        let t = term.lock();
        let left = req.limit.saturating_sub(out.hits.len()).max(1);
        let (hits, next) = t.search_chunk(req, below, CHUNK_ROWS, left);
        out.alt_screen = t.mode().contains(crate::TermMode::ALT_SCREEN);
        out.generation = t.generation();
        drop(t);
        out.hits.extend(hits);
        match next {
            Some(n) if out.hits.len() < req.limit => below = Some(n),
            Some(n) => {
                out.resume = Some(n);
                break;
            }
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::Mutex;

    use super::*;
    use crate::GridSize;

    fn term(cols: u16, rows: u16, history: usize) -> Terminal {
        Terminal::new(
            GridSize {
                cols,
                rows,
                cell_width: 8.0,
                cell_height: 16.0,
            },
            history,
        )
    }

    fn pattern(text: &str, regex: bool) -> SearchPattern {
        SearchPattern::new(&SearchQuery::new(text, regex))
            .unwrap()
            .unwrap()
    }

    fn req(text: &str, regex: bool) -> SearchRequest {
        SearchRequest {
            pattern: pattern(text, regex),
            below: None,
            limit: MATCH_CAP,
            snippets: true,
        }
    }

    fn run(t: Terminal, r: &SearchRequest) -> SearchOutcome {
        search(&Arc::new(Mutex::new(t)), r, &AtomicBool::new(false))
    }

    fn sp(line: i64, col: u16) -> StablePoint {
        StablePoint { line, col }
    }

    #[test]
    fn smart_case() {
        assert!(!is_case_sensitive("error", false));
        assert!(is_case_sensitive("Error", false));
        assert!(
            !is_case_sensitive(r"\S+\d", true),
            "escapes are not letters"
        );
        assert!(is_case_sensitive(r"\sE", true));
        let p = pattern("error", false);
        assert_eq!(p.find_iter("ERROR Error error").count(), 3);
        let p = pattern("Error", false);
        assert_eq!(p.find_iter("ERROR Error error").count(), 1);
    }

    #[test]
    fn plain_text_is_literal_and_regex_is_not() {
        let p = pattern("a.c (x)", false);
        assert_eq!(
            p.find_iter("abc (x) a.c (x)").collect::<Vec<_>>(),
            vec![(8..15)]
        );
        let p = pattern("a.c", true);
        assert_eq!(p.find_iter("abc a.c").count(), 2);
    }

    #[test]
    fn invalid_regex_is_an_error_not_a_panic() {
        let e = SearchPattern::new(&SearchQuery::new("(unclosed", true)).unwrap_err();
        assert_eq!(e.to_string(), "Invalid pattern");
        assert!(SearchPattern::new(&SearchQuery::new("(unclosed", false)).is_ok());
        assert!(
            SearchPattern::new(&SearchQuery::new("", true))
                .unwrap()
                .is_none()
        );
        // Far too large to compile within the limits.
        assert!(
            SearchPattern::new(&SearchQuery::new("\\w{1000}\\w{1000}\\w{1000}", true)).is_err()
        );
    }

    #[test]
    fn empty_matches_are_skipped() {
        let p = pattern("x*", true);
        assert_eq!(p.find_iter("abxxc").collect::<Vec<_>>(), vec![(2..4)]);
    }

    #[test]
    fn finds_matches_in_scrollback_newest_first() {
        let mut t = term(20, 5, 1000);
        for i in 0..50 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        let out = run(t, &req("line 1", false));
        // line 1, 10..=19
        assert_eq!(out.hits.len(), 11);
        let lines: Vec<i64> = out.hits.iter().map(|h| h.range.start.line).collect();
        assert!(lines.windows(2).all(|w| w[0] > w[1]), "newest first");
        assert_eq!(out.hits[0].snippet.as_ref().unwrap().text, "line 19");
        assert_eq!(
            out.hits.last().unwrap().snippet.as_ref().unwrap().text,
            "line 1"
        );
        assert_eq!(out.resume, None);
    }

    #[test]
    fn stable_points_survive_scrolling() {
        let shared: SharedTerminal = Arc::new(Mutex::new(term(20, 5, 1000)));
        shared.lock().feed(b"needle\r\n");
        let r = req("needle", false);
        let before = search(&shared, &r, &AtomicBool::new(false));
        for i in 0..30 {
            shared.lock().feed(format!("other {i}\r\n").as_bytes());
        }
        let after = search(&shared, &r, &AtomicBool::new(false));
        assert_eq!(before.hits[0].range, after.hits[0].range);
        let t = shared.lock();
        let line = t.grid_line(after.hits[0].range.start.line).unwrap();
        assert_eq!(t.term().grid()[line][Column(0)].c, 'n');
    }

    #[test]
    fn stable_points_survive_a_full_history() {
        let shared: SharedTerminal = Arc::new(Mutex::new(term(20, 5, 20)));
        for i in 0..40 {
            shared.lock().feed(format!("x{i}\r\n").as_bytes());
        }
        shared.lock().feed(b"needle\r\n");
        let r = req("needle", false);
        let before = search(&shared, &r, &AtomicBool::new(false));
        for i in 0..10 {
            shared.lock().feed(format!("y{i}\r\n").as_bytes());
        }
        let after = search(&shared, &r, &AtomicBool::new(false));
        assert_eq!(before.hits[0].range, after.hits[0].range);
        // Scrolled out of the history entirely: gone.
        for i in 0..40 {
            shared.lock().feed(format!("z{i}\r\n").as_bytes());
        }
        assert!(search(&shared, &r, &AtomicBool::new(false)).hits.is_empty());
        assert_eq!(
            shared.lock().grid_line(before.hits[0].range.start.line),
            None
        );
    }

    #[test]
    fn matches_span_soft_wraps_but_not_line_breaks() {
        let mut t = term(10, 5, 100);
        t.feed(b"0123456789abc\r\n");
        t.feed(b"xyz\r\nabc");
        let top = t.stable_line(Line(0));
        let out = run(t, &req("89ab", false));
        assert_eq!(out.hits.len(), 1);
        assert_eq!(
            out.hits[0].range,
            SearchMatch {
                start: sp(top, 8),
                end: sp(top + 1, 1)
            }
        );
        let mut t = term(10, 5, 100);
        t.feed(b"xyz\r\nabc");
        assert!(run(t, &req("zab", false)).hits.is_empty());
    }

    #[test]
    fn wide_characters_cover_both_cells() {
        let mut t = term(20, 5, 100);
        t.feed("ab界c".as_bytes());
        let top = t.stable_line(Line(0));
        let out = run(t, &req("界c", false));
        assert_eq!(
            out.hits[0].range,
            SearchMatch {
                start: sp(top, 2),
                end: sp(top, 4)
            }
        );
        let mut t = term(20, 5, 100);
        t.feed("ab界".as_bytes());
        let out = run(t, &req("界", false));
        assert_eq!(out.hits[0].range.end, sp(top, 3));
    }

    #[test]
    fn stops_at_the_limit_and_resumes_above() {
        let mut t = term(20, 5, 5000);
        for i in 0..3000 {
            t.feed(format!("hit {i}\r\n").as_bytes());
        }
        let shared: SharedTerminal = Arc::new(Mutex::new(t));
        let mut r = req("hit", false);
        let first = search(&shared, &r, &AtomicBool::new(false));
        assert_eq!(first.hits.len(), MATCH_CAP);
        let resume = first.resume.expect("more history above");
        r.below = Some(resume);
        let second = search(&shared, &r, &AtomicBool::new(false));
        assert_eq!(second.hits.len(), MATCH_CAP);
        assert!(second.hits[0].range.start.line < first.hits.last().unwrap().range.start.line);
        r.below = second.resume;
        let third = search(&shared, &r, &AtomicBool::new(false));
        assert_eq!(third.hits.len(), 1000);
        assert_eq!(third.resume, None);
        assert_eq!(
            third.hits.last().unwrap().snippet.as_ref().unwrap().text,
            "hit 0"
        );
    }

    #[test]
    fn a_cancelled_search_stops() {
        let mut t = term(20, 5, 100);
        t.feed(b"hit");
        let out = search(
            &Arc::new(Mutex::new(t)),
            &req("hit", false),
            &AtomicBool::new(true),
        );
        assert!(out.cancelled);
        assert!(out.hits.is_empty());
    }

    #[test]
    fn visible_matches_and_reveal() {
        let mut t = term(20, 5, 1000);
        t.feed(b"target\r\n");
        for i in 0..100 {
            t.feed(format!("filler {i}\r\n").as_bytes());
        }
        let p = pattern("target", false);
        assert!(t.visible_matches(&p).is_empty());
        let shared: SharedTerminal = Arc::new(Mutex::new(t));
        let out = search(&shared, &req("target", false), &AtomicBool::new(false));
        let mut t = shared.lock();
        assert!(t.reveal(&out.hits[0].range));
        assert_eq!(t.visible_matches(&p), vec![out.hits[0].range]);
        assert!(!t.reveal(&out.hits[0].range), "already in view");
        assert!(t.view_bottom() > out.hits[0].range.start.line);
    }

    #[test]
    fn snippets_trim_around_the_match() {
        let s = snippet("   hello world", 9..14);
        assert_eq!(s.text, "hello world");
        assert_eq!(&s.text[s.highlight.clone()], "world");
        // Early in a long line: from the start.
        let line = format!("ab needle {}", "c".repeat(200));
        let s = snippet(&line, 3..9);
        assert!(s.text.starts_with("ab needle"));
        assert_eq!(&s.text[s.highlight.clone()], "needle");
        let long = format!("{}needle{}", "a".repeat(100), "b".repeat(200));
        let s = snippet(&long, 100..106);
        assert!(s.text.starts_with('\u{2026}'));
        assert!(s.text.ends_with('\u{2026}'));
        assert_eq!(&s.text[s.highlight.clone()], "needle");
        assert_eq!(s.text.chars().count(), SNIPPET_LEN + 2);
    }
}
