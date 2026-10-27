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
            for c in from..=to {
                let cell = &row[Column(c)];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                seg.push(cell.c);
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
}
