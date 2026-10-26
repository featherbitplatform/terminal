//! The draw list built from the terminal grid, and drawing it.
//!
//! The draw list is built under the terminal lock (cheap: cells to runs) and drawn afterwards
//! without it. Cells are batched into runs: one quad per run of equal background, one text
//! primitive per run of equal foreground and font style. Wide characters and cells carrying
//! combining characters are drawn on their own so a fallback font cannot shift the rest of the row.

use fbterm_config::Rgb;
use feathercritty::grid::{Dimensions, RowReader};
use feathercritty::index::{Column, Line, Point};
use feathercritty::term::cell::{Cell, Flags, Hyperlink};
use feathercritty::vte::ansi::CursorShape;
use iced::advanced::renderer::{Quad, Renderer as _};
use iced::advanced::text::{self, Renderer as _};
use iced::{Border, Font, Pixels, Rectangle, Size};

use super::Highlight;
use super::colors::{cell_colors, resolve};
use crate::palette::ColorScheme;
use crate::{SearchMatch, Terminal};

/// A horizontal span of cells in one viewport row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub row: u16,
    pub col: u16,
    pub cells: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    pub span: Span,
    pub text: String,
    pub fg: Rgb,
    pub bold: bool,
    pub italic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Underline,
    DoubleUnderline,
    Strike,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMark {
    Hollow,
    Beam,
    Underline,
}

/// Everything needed to draw one frame of a terminal, in cell coordinates.
#[derive(Debug, Default)]
pub struct DrawList {
    pub background: Option<Rgb>,
    pub quads: Vec<(Span, Rgb)>,
    pub texts: Vec<TextRun>,
    pub lines: Vec<(Span, Rgb, LineKind)>,
    pub cursor: Option<(Span, Rgb, CursorMark)>,
}

/// Appends `cells` at `(row, col)` to the last span of `out` when it continues it with the same
/// key, otherwise starts a new span.
fn push_span<K: PartialEq>(out: &mut Vec<(Span, K)>, row: u16, col: u16, cells: u16, key: K) {
    if let Some((span, k)) = out.last_mut()
        && span.row == row
        && span.col + span.cells == col
        && *k == key
    {
        span.cells += cells;
        return;
    }
    out.push((Span { row, col, cells }, key));
}

/// Narrow characters drawn at their own cell rather than inside a run: anything outside ASCII may
/// come from a fallback font with a different advance, which would shift the rest of the run.
/// Box drawing and block elements (U+2500..=U+259F) stay in runs; the monospace fonts cover them
/// and they make up whole rows of TUI frames and bars.
fn standalone(c: char) -> bool {
    !c.is_ascii() && !('\u{2500}'..='\u{259f}').contains(&c)
}

/// A search match on one row of the view: columns `from..=to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Mark {
    from: usize,
    to: usize,
    current: bool,
}

/// The search matches in view, per row of the view: every match of the pattern on the visible
/// lines, plus the current match.
fn marks(term: &Terminal, highlight: &Highlight, rows: usize, cols: usize) -> Vec<Vec<Mark>> {
    let mut out = vec![Vec::new(); rows];
    let grid = term.term().grid();
    let offset = grid.display_offset() as i64;
    let first = term.stable_line(Line(-(offset as i32)));
    let mut add = |m: &SearchMatch, current: bool| {
        for stable in m.start.line..=m.end.line {
            let row = stable - first;
            if row < 0 || row >= rows as i64 {
                continue;
            }
            let from = if stable == m.start.line {
                usize::from(m.start.col)
            } else {
                0
            };
            let to = if stable == m.end.line {
                usize::from(m.end.col).min(cols - 1)
            } else {
                cols - 1
            };
            out[row as usize].push(Mark { from, to, current });
        }
    };
    for m in term.visible_matches(&highlight.pattern) {
        add(&m, false);
    }
    if let Some(m) = &highlight.current {
        add(m, true);
    }
    out
}

/// Builds the draw list. Call with the terminal locked; it only reads.
///
/// `reader` reads the rows in view. Keep one with the widget's draw state across frames: rows
/// on the screen are read in place, and only rows above it (the view scrolled back into the
/// history) go through the reader's scratch row, at most one screen of them per frame.
pub fn build(
    reader: &mut RowReader<Cell>,
    term: &Terminal,
    scheme: &ColorScheme,
    focused: bool,
    hover: Option<&Hyperlink>,
    highlight: Option<&Highlight>,
) -> DrawList {
    let t = term.term();
    let grid = t.grid();
    let colors = t.colors();
    let rows = grid.screen_lines();
    let cols = grid.columns();
    let offset = grid.display_offset();
    let selection = term.selection();
    let content = t.renderable_content();
    let cursor = content.cursor;
    let cursor_row = cursor.point.line.0 + offset as i32;
    let cursor_visible =
        cursor.shape != CursorShape::Hidden && (0..rows as i32).contains(&cursor_row);
    let block_cursor = cursor_visible && focused && cursor.shape == CursorShape::Block;

    let mut list = DrawList {
        background: Some(scheme.background),
        ..DrawList::default()
    };
    let mut quads: Vec<(Span, Rgb)> = Vec::new();
    let mut lines: Vec<(Span, (Rgb, LineKind))> = Vec::new();
    let mut strikes: Vec<(Span, (Rgb, LineKind))> = Vec::new();
    let marks = highlight.map(|h| (h, marks(term, h, rows, cols)));
    for y in 0..rows {
        let row_u16 = y as u16;
        let line = Line(y as i32 - offset as i32);
        let row = reader.row(grid, line);
        let mut run: Option<TextRun> = None;
        let flush = |run: &mut Option<TextRun>, texts: &mut Vec<TextRun>| {
            if let Some(mut r) = run.take() {
                let trimmed = r.text.trim_end_matches(' ').len();
                if trimmed > 0 {
                    // Trailing blanks are one byte and one cell each.
                    r.span.cells -= (r.text.len() - trimmed) as u16;
                    r.text.truncate(trimmed);
                    texts.push(r);
                }
            }
        };
        for x in 0..cols {
            let cell = &row[Column(x)];
            let col = x as u16;
            let selected = selection.is_some_and(|s| s.contains(Point::new(line, Column(x))));
            let (mut fg, mut bg) = cell_colors(cell, colors, scheme, selected);
            // Search matches: the current one strongest, then the selection, then the others.
            if let Some((h, marks)) = &marks {
                let mut hit = marks[y].iter().filter(|m| (m.from..=m.to).contains(&x));
                let current = hit.clone().any(|m| m.current);
                if current {
                    bg = h.current_bg;
                    fg = h.current_fg;
                } else if hit.next().is_some() && !selected {
                    bg = h.match_bg;
                }
            }
            let is_cursor_cell = cursor_visible
                && y as i32 == cursor_row
                && (x == cursor.point.column.0
                    || (x == cursor.point.column.0 + 1
                        && cell.flags.contains(Flags::WIDE_CHAR_SPACER)));
            if block_cursor && is_cursor_cell {
                bg = scheme.cursor;
                fg = scheme.background;
            }
            if bg != scheme.background {
                push_span(&mut quads, row_u16, col, 1, bg);
            }

            // Decorations.
            let flags = cell.flags;
            let deco_color = cell
                .underline_color()
                .map(|c| resolve(c, colors, scheme))
                .unwrap_or(fg);
            let hovered = hover.is_some() && cell.hyperlink().as_ref() == hover;
            if flags.contains(Flags::DOUBLE_UNDERLINE) {
                push_span(
                    &mut lines,
                    row_u16,
                    col,
                    1,
                    (deco_color, LineKind::DoubleUnderline),
                );
            } else if hovered || flags.intersects(Flags::ALL_UNDERLINES) {
                push_span(
                    &mut lines,
                    row_u16,
                    col,
                    1,
                    (deco_color, LineKind::Underline),
                );
            }
            if flags.contains(Flags::STRIKEOUT) {
                push_span(&mut strikes, row_u16, col, 1, (fg, LineKind::Strike));
            }

            // Glyphs.
            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let bold = flags.contains(Flags::BOLD);
            let italic = flags.contains(Flags::ITALIC);
            let zerowidth = cell.zerowidth();
            if flags.contains(Flags::WIDE_CHAR) || zerowidth.is_some() || standalone(cell.c) {
                flush(&mut run, &mut list.texts);
                let mut text = String::from(cell.c);
                text.extend(zerowidth.into_iter().flatten());
                let cells = if flags.contains(Flags::WIDE_CHAR) {
                    2
                } else {
                    1
                };
                list.texts.push(TextRun {
                    span: Span {
                        row: row_u16,
                        col,
                        cells,
                    },
                    text,
                    fg,
                    bold,
                    italic,
                });
                continue;
            }
            let c = if cell.c == '\t' { ' ' } else { cell.c };
            match &mut run {
                // A blank continues any run: it draws nothing.
                Some(r) if c == ' ' || (r.fg == fg && r.bold == bold && r.italic == italic) => {
                    r.text.push(c);
                    r.span.cells += 1;
                }
                _ if c == ' ' => flush(&mut run, &mut list.texts),
                _ => {
                    flush(&mut run, &mut list.texts);
                    run = Some(TextRun {
                        span: Span {
                            row: row_u16,
                            col,
                            cells: 1,
                        },
                        text: String::from(c),
                        fg,
                        bold,
                        italic,
                    });
                }
            }
        }
        flush(&mut run, &mut list.texts);
    }
    list.quads = quads;
    list.lines = lines
        .into_iter()
        .chain(strikes)
        .map(|(s, (c, k))| (s, c, k))
        .collect();

    if cursor_visible && !block_cursor {
        let x = cursor.point.column.0;
        let wide = grid
            .cell(Point::new(cursor.point.line, Column(x)))
            .flags
            .contains(Flags::WIDE_CHAR);
        let mark = match cursor.shape {
            _ if !focused => CursorMark::Hollow,
            CursorShape::Beam => CursorMark::Beam,
            CursorShape::Underline => CursorMark::Underline,
            _ => CursorMark::Hollow,
        };
        let span = Span {
            row: cursor_row as u16,
            col: x as u16,
            cells: if wide { 2 } else { 1 },
        };
        list.cursor = Some((span, scheme.cursor, mark));
    }
    list
}

fn color(c: Rgb) -> iced::Color {
    iced::Color::from_rgb8(c.r, c.g, c.b)
}

/// The fill of the default background at `opacity`; `None` when it is not drawn (fully clear).
/// Everything else in the list is drawn opaque with [`color`].
fn background_fill(list: &DrawList, opacity: f32) -> Option<iced::Color> {
    list.background
        .filter(|_| opacity > 0.0)
        .map(|bg| color(bg).scale_alpha(opacity.min(1.0)))
}

/// Font metrics used to place cells.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub cell: Size,
    pub font: Font,
    pub font_size: f32,
}

fn fill(renderer: &mut iced::Renderer, bounds: Rectangle, c: Rgb) {
    renderer.fill_quad(
        Quad {
            bounds,
            snap: true,
            ..Quad::default()
        },
        color(c),
    );
}

fn is_han(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3134F)
}

/// Font for a run. The text stack's default Han fallback on macOS names a font that is not always
/// installed as a file, so ideographs would render blank; point them at one that ships with the OS.
fn run_font(base: Font, run: &TextRun) -> Font {
    let mut font = base;
    if cfg!(target_os = "macos")
        && run.span.cells == 2
        && run.text.chars().next().is_some_and(is_han)
    {
        font = Font::with_name("Hiragino Sans GB");
    }
    font
}

/// Issues the renderer primitives for `list` inside `bounds`.
pub fn render(
    list: &DrawList,
    renderer: &mut iced::Renderer,
    bounds: Rectangle,
    m: Metrics,
    accent: Option<Rgb>,
    background_opacity: f32,
) {
    let (cw, ch) = (m.cell.width, m.cell.height);
    let rect = |s: Span| Rectangle {
        x: bounds.x + f32::from(s.col) * cw,
        y: bounds.y + f32::from(s.row) * ch,
        width: f32::from(s.cells) * cw,
        height: ch,
    };
    if let Some(bg) = background_fill(list, background_opacity) {
        renderer.fill_quad(
            Quad {
                bounds,
                snap: true,
                ..Quad::default()
            },
            bg,
        );
    }
    for (span, c) in &list.quads {
        fill(renderer, rect(*span), *c);
    }
    let thin = (ch / 16.0).round().max(1.0);
    for (span, c, kind) in &list.lines {
        let r = rect(*span);
        let mut line = |y: f32| {
            fill(
                renderer,
                Rectangle {
                    y,
                    height: thin,
                    ..r
                },
                *c,
            );
        };
        match kind {
            LineKind::Underline => line(r.y + ch - 2.0 * thin),
            LineKind::DoubleUnderline => {
                line(r.y + ch - thin);
                line(r.y + ch - 3.0 * thin);
            }
            LineKind::Strike => line(r.y + (ch * 0.55).round()),
        }
    }
    if let Some((span, c, mark)) = list.cursor {
        let r = rect(span);
        let w = (cw * 0.12).round().max(1.0).max(thin);
        match mark {
            CursorMark::Beam => fill(renderer, Rectangle { width: w, ..r }, c),
            CursorMark::Underline => fill(
                renderer,
                Rectangle {
                    y: r.y + ch - w,
                    height: w,
                    ..r
                },
                c,
            ),
            CursorMark::Hollow => renderer.fill_quad(
                Quad {
                    bounds: r,
                    border: Border {
                        color: color(c),
                        width: 1.0,
                        radius: 0.0.into(),
                    },
                    snap: true,
                    ..Quad::default()
                },
                iced::Color::TRANSPARENT,
            ),
        }
    }
    for run in &list.texts {
        let mut font = run_font(m.font, run);
        if run.bold {
            font.weight = iced::font::Weight::Bold;
        }
        if run.italic {
            font.style = iced::font::Style::Italic;
        }
        let r = rect(run.span);
        renderer.fill_text(
            text::Text {
                content: run.text.clone(),
                bounds: Size::new(r.width + cw, ch),
                size: Pixels(m.font_size),
                line_height: text::LineHeight::Absolute(Pixels(ch)),
                font,
                align_x: text::Alignment::Left,
                align_y: iced::alignment::Vertical::Top,
                shaping: text::Shaping::Advanced,
                wrapping: text::Wrapping::None,
            },
            iced::Point::new(r.x, r.y),
            color(run.fg),
            bounds,
        );
    }
    if let Some(a) = accent {
        renderer.fill_quad(
            Quad {
                bounds,
                border: Border {
                    color: color(a),
                    width: 1.0,
                    radius: 0.0.into(),
                },
                snap: true,
                ..Quad::default()
            },
            iced::Color::TRANSPARENT,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheme() -> ColorScheme {
        ColorScheme::graphite()
    }

    fn term_with(bytes: &[u8]) -> Terminal {
        let mut t = Terminal::new(
            crate::GridSize {
                cols: 20,
                rows: 4,
                cell_width: 8.0,
                cell_height: 16.0,
            },
            100,
        );
        t.feed(bytes);
        t
    }

    #[test]
    fn build_batches_runs_by_style() {
        let t = term_with(b"ab \x1b[31mcd\x1b[0m  \x1b[1mef");
        let list = build(&mut RowReader::new(), &t, &scheme(), true, None, None);
        let texts: Vec<_> = list
            .texts
            .iter()
            .map(|r| (r.span.col, r.text.as_str()))
            .collect();
        // "ab " continues into nothing (blank), red "cd  " trims, bold "ef".
        assert_eq!(texts, vec![(0, "ab"), (3, "cd"), (7, "ef")]);
        assert!(list.texts[2].bold);
        // Focused block cursor sits after "ef": a cursor-colored quad, no outline.
        assert!(
            list.quads
                .iter()
                .any(|(s, c)| s.col == 9 && *c == scheme().cursor)
        );
        assert!(list.cursor.is_none());
    }

    #[test]
    fn only_the_default_background_is_translucent() {
        // Green background on "ab", then default cells.
        let t = term_with(b"\x1b[42mab\x1b[0m cd");
        let list = build(&mut RowReader::new(), &t, &scheme(), true, None, None);
        let bg = background_fill(&list, 0.85).expect("drawn");
        assert!((bg.a - 0.85).abs() < f32::EPSILON);
        assert_eq!(
            iced::Color { a: 1.0, ..bg },
            color(scheme().background),
            "same color, only the alpha changes"
        );
        // The explicit background (and the cursor) are quads, drawn opaque.
        let green = list
            .quads
            .iter()
            .find(|(s, _)| s.col == 0)
            .expect("explicit background quad");
        assert_eq!(green.0.cells, 2);
        assert_eq!(color(green.1).a, 1.0);
        assert!(list.quads.iter().all(|(_, c)| *c != scheme().background));
        assert!(list.texts.iter().all(|r| color(r.fg).a == 1.0));
        // Opaque by default, and not drawn at all at zero.
        assert_eq!(background_fill(&list, 1.0).map(|c| c.a), Some(1.0));
        assert_eq!(background_fill(&list, 0.0), None);
    }

    #[test]
    fn build_unfocused_cursor_is_hollow() {
        let t = term_with(b"x");
        let list = build(&mut RowReader::new(), &t, &scheme(), false, None, None);
        assert_eq!(list.cursor.map(|c| c.2), Some(CursorMark::Hollow));
        assert!(list.quads.is_empty());
    }

    #[test]
    fn build_draws_wide_chars_on_their_own() {
        let t = term_with("a日b".as_bytes());
        let list = build(&mut RowReader::new(), &t, &scheme(), true, None, None);
        let texts: Vec<_> = list
            .texts
            .iter()
            .map(|r| (r.span.col, r.span.cells, r.text.as_str()))
            .collect();
        assert_eq!(texts, vec![(0, 1, "a"), (1, 2, "日"), (3, 1, "b")]);
    }

    #[test]
    fn non_ascii_narrow_char_breaks_run() {
        let t = term_with("a❯b █▓─x".as_bytes());
        let list = build(&mut RowReader::new(), &t, &scheme(), true, None, None);
        let texts: Vec<_> = list
            .texts
            .iter()
            .map(|r| (r.span.col, r.span.cells, r.text.as_str()))
            .collect();
        assert_eq!(texts, vec![(0, 1, "a"), (1, 1, "❯"), (2, 6, "b █▓─x")]);
    }

    #[test]
    fn build_underlines_hovered_link_only() {
        let t = term_with(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ x");
        let none = build(&mut RowReader::new(), &t, &scheme(), true, None, None);
        assert!(none.lines.is_empty());
        let link = t.term().grid().screen()[Line(0)][Column(0)]
            .hyperlink()
            .unwrap();
        let hovered = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            true,
            Some(&link),
            None,
        );
        assert_eq!(hovered.lines.len(), 1);
        assert_eq!(hovered.lines[0].0.cells, 4);
    }

    #[test]
    fn build_marks_selection_background() {
        let mut t = term_with(b"hello");
        let mut sel = crate::Selection::start(Point::new(Line(0), Column(1)));
        sel.update(Point::new(Line(0), Column(3)));
        t.set_selection(Some(sel));
        let list = build(&mut RowReader::new(), &t, &scheme(), false, None, None);
        assert!(list.quads.contains(&(
            Span {
                row: 0,
                col: 1,
                cells: 3
            },
            scheme().selection
        )));
    }

    #[test]
    fn build_tints_matches_and_marks_the_current_one() {
        let t = term_with(b"ab ab\r\nab");
        let pattern = crate::SearchPattern::new(&crate::SearchQuery::new("ab", false))
            .unwrap()
            .unwrap();
        let all = t.visible_matches(&pattern);
        assert_eq!(all.len(), 3);
        let (tint, strong, text) = (
            Rgb { r: 1, g: 1, b: 1 },
            Rgb { r: 2, g: 2, b: 2 },
            Rgb { r: 3, g: 3, b: 3 },
        );
        let h = Highlight {
            pattern,
            current: Some(all[1]),
            match_bg: tint,
            current_bg: strong,
            current_fg: text,
        };
        let list = build(&mut RowReader::new(), &t, &scheme(), false, None, Some(&h));
        let quads: Vec<_> = list
            .quads
            .iter()
            .map(|(s, c)| (s.row, s.col, s.cells, *c))
            .collect();
        assert!(quads.contains(&(0, 0, 2, tint)));
        assert!(quads.contains(&(0, 3, 2, strong)));
        assert!(quads.contains(&(1, 0, 2, tint)));
        let current_text = list
            .texts
            .iter()
            .find(|r| r.span.row == 0 && r.span.col == 3);
        assert_eq!(current_text.map(|r| r.fg), Some(text));
        // Without a highlight nothing is tinted.
        let plain = build(&mut RowReader::new(), &t, &scheme(), false, None, None);
        assert!(plain.quads.iter().all(|(_, c)| *c != tint && *c != strong));
    }
}
