//! Text selection over the emulator grid.

use feathercritty::grid::{Dimensions, RowReader};
use feathercritty::index::{Column, Line, Point};
use feathercritty::term::cell::Flags;

use crate::Terminal;

/// A linear selection between two grid points (inclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    anchor: Point,
    head: Point,
}

impl Selection {
    pub fn start(point: Point) -> Self {
        Self {
            anchor: point,
            head: point,
        }
    }

    /// Columns `first..=last` of one grid line (`0` is the top line of the screen, negative lines
    /// are history).
    pub fn on_line(line: i32, first: usize, last: usize) -> Self {
        let mut sel = Self::start(Point::new(Line(line), Column(first)));
        sel.update(Point::new(Line(line), Column(last)));
        sel
    }

    pub fn update(&mut self, point: Point) {
        self.head = point;
    }

    /// First and last selected points, in grid order.
    pub fn bounds(&self) -> (Point, Point) {
        self.ordered()
    }

    fn ordered(&self) -> (Point, Point) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub fn contains(&self, point: Point) -> bool {
        let (a, b) = self.ordered();
        point >= a && point <= b
    }

    /// Selected text: trailing spaces trimmed per line, lines joined with `\n`, wrapped lines
    /// joined without a newline.
    pub fn text(&self, term: &Terminal) -> String {
        let grid = term.term().grid();
        let (start, end) = self.ordered();
        let top = grid.topmost_line();
        let bottom = grid.bottommost_line();
        let last_col = grid.columns() - 1;
        let first = start.line.max(top);
        let last = end.line.min(bottom);
        let mut reader = RowReader::new();
        let mut out = String::new();
        let mut line = first;
        while line <= last {
            let from = if line == start.line {
                start.column.0
            } else {
                0
            };
            let to = if line == end.line {
                end.column.0.min(last_col)
            } else {
                last_col
            };
            let wrapped = grid.wraps(line);
            let row = reader.row(grid, line);
            let mut seg = String::new();
            // A selection starting on a wide character's second column copies the character.
            let from = if from > 0 && row[Column(from)].flags.contains(Flags::WIDE_CHAR_SPACER) {
                from - 1
            } else {
                from
            };
            for c in from..=to {
                let cell = &row[Column(c)];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                seg.push(cell.c);
                // Combining marks and the rest of a grapheme cluster live with the cell.
                seg.extend(cell.zerowidth().into_iter().flatten());
            }
            if wrapped && to == last_col {
                out.push_str(&seg);
            } else {
                out.push_str(seg.trim_end_matches(' '));
                if line != last {
                    out.push('\n');
                }
            }
            line += 1;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GridSize, Terminal};
    use feathercritty::index::{Column, Line, Point};

    fn p(l: i32, c: usize) -> Point {
        Point::new(Line(l), Column(c))
    }
    fn term() -> Terminal {
        Terminal::new(
            GridSize {
                cols: 10,
                rows: 5,
                cell_width: 8.0,
                cell_height: 16.0,
                scale: 1.0,
            },
            100,
        )
    }

    #[test]
    fn selection_text_spans_lines_and_trims_trailing_spaces() {
        let mut t = term();
        t.feed(b"hello\r\nworld");
        let mut s = Selection::start(p(0, 0));
        s.update(p(1, 9));
        assert_eq!(s.text(&t), "hello\nworld");
        assert!(s.contains(p(0, 3)));
        assert!(!s.contains(p(2, 0)));
    }
    #[test]
    fn joined_pieces_are_copied_with_their_cell() {
        let mut t = term();
        t.feed("\x1b[?2027hab👨\u{200D}👩\u{200D}👧c e\u{301}".as_bytes());
        let s = Selection::on_line(0, 0, 9);
        assert_eq!(s.text(&t), "ab👨\u{200D}👩\u{200D}👧c e\u{301}");
    }

    #[test]
    fn a_selection_starting_on_a_spacer_copies_its_wide_character() {
        let mut t = term();
        t.feed("ab界c".as_bytes());
        assert_eq!(Selection::on_line(0, 3, 4).text(&t), "界c");
    }

    #[test]
    fn wrapped_lines_join_without_newline() {
        let mut t = term();
        t.feed(b"abcdefghijklm");
        let mut s = Selection::start(p(0, 0));
        s.update(p(1, 9));
        assert_eq!(s.text(&t), "abcdefghijklm");
    }
    #[test]
    fn reversed_selection_is_normalized() {
        let mut t = term();
        t.feed(b"hello");
        let mut s = Selection::start(p(0, 3));
        s.update(p(0, 1));
        assert_eq!(s.text(&t), "ell");
    }
    #[test]
    fn on_line_selects_columns_of_one_line() {
        let mut t = term();
        t.feed(b"one\r\nab word");
        assert_eq!(Selection::on_line(1, 3, 6).text(&t), "word");
    }

    // Compression-on variants: the same text, written into the oldest history rows and then
    // compressed (`cold_tests::pair`), copies exactly as with compression off.

    /// The text `select` (given the oldest history line) copies from both terminals of the
    /// pair; asserts they match and returns it.
    fn cold_text(before: &[u8], select: impl Fn(i32) -> Selection) -> String {
        let (off, on) = crate::cold_tests::pair(10, 5, before, b"");
        let top = on.term().grid().topmost_line().0;
        assert!(top < -1000, "the text is deep in history");
        let (a, b) = (select(top).text(&off), select(top).text(&on));
        assert_eq!(a, b, "differs with compression on");
        b
    }

    #[test]
    fn selection_text_spans_lines_and_trims_trailing_spaces_compressed() {
        let text = cold_text(b"hello\r\nworld\r\n", |top| {
            let mut s = Selection::start(p(top, 0));
            s.update(p(top + 1, 9));
            s
        });
        assert_eq!(text, "hello\nworld");
    }

    #[test]
    fn joined_pieces_are_copied_with_their_cell_compressed() {
        let text = cold_text(
            "\x1b[?2027hab👨\u{200D}👩\u{200D}👧c e\u{301}\r\n".as_bytes(),
            |top| Selection::on_line(top, 0, 9),
        );
        assert_eq!(text, "ab👨\u{200D}👩\u{200D}👧c e\u{301}");
    }

    #[test]
    fn a_selection_starting_on_a_spacer_copies_its_wide_character_compressed() {
        let text = cold_text("ab界c\r\n".as_bytes(), |top| Selection::on_line(top, 3, 4));
        assert_eq!(text, "界c");
    }

    #[test]
    fn wrapped_lines_join_without_newline_compressed() {
        let text = cold_text(b"abcdefghijklm\r\n", |top| {
            let mut s = Selection::start(p(top, 0));
            s.update(p(top + 1, 9));
            s
        });
        assert_eq!(text, "abcdefghijklm");
    }

    #[test]
    fn reversed_selection_is_normalized_compressed() {
        let text = cold_text(b"hello\r\n", |top| {
            let mut s = Selection::start(p(top, 3));
            s.update(p(top, 1));
            s
        });
        assert_eq!(text, "ell");
    }

    #[test]
    fn on_line_selects_columns_of_one_line_compressed() {
        let text = cold_text(b"one\r\nab word\r\n", |top| {
            Selection::on_line(top + 1, 3, 6)
        });
        assert_eq!(text, "word");
    }
}
