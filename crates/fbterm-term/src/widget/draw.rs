//! Color resolution and the draw list built from the terminal grid.
//!
//! The draw list is built under the terminal lock (cheap: cells to runs) and drawn afterwards
//! without it. Cells are batched into runs: one quad per run of equal background, one text
//! primitive per run of equal foreground and font style. Wide characters and cells carrying
//! combining characters are drawn on their own so a fallback font cannot shift the rest of the row.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell, Flags, Hyperlink};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb as VteRgb};
use fbterm_config::Rgb;
use iced::advanced::renderer::{Quad, Renderer as _};
use iced::advanced::text::{self, Renderer as _};
use iced::{Border, Font, Pixels, Rectangle, Size};

use crate::Terminal;
use crate::palette::{ColorScheme, blend};

/// How much of the foreground survives `DIM` (the rest is background).
const DIM_ALPHA: f32 = 0.66;

fn from_vte(c: VteRgb) -> Rgb {
    Rgb {
        r: c.r,
        g: c.g,
        b: c.b,
    }
}

fn named(n: NamedColor, scheme: &ColorScheme) -> Rgb {
    let idx = n as usize;
    match n {
        NamedColor::Foreground | NamedColor::BrightForeground => scheme.foreground,
        NamedColor::Background => scheme.background,
        NamedColor::Cursor => scheme.cursor,
        NamedColor::DimForeground => blend(scheme.foreground, scheme.background, DIM_ALPHA),
        _ if idx < 16 => scheme.ansi[idx],
        _ => {
            // DimBlack..=DimWhite
            let base = idx.saturating_sub(NamedColor::DimBlack as usize).min(7);
            blend(scheme.ansi[base], scheme.background, DIM_ALPHA)
        }
    }
}

fn resolve(color: Color, colors: &Colors, scheme: &ColorScheme) -> Rgb {
    match color {
        Color::Spec(c) => from_vte(c),
        Color::Indexed(i) => colors[i as usize]
            .map(from_vte)
            .unwrap_or_else(|| scheme.indexed(i as usize)),
        Color::Named(n) => colors[n].map(from_vte).unwrap_or_else(|| named(n, scheme)),
    }
}

/// Foreground and background of a cell after inverse, dim, hidden and selection.
pub fn cell_colors(
    cell: &Cell,
    colors: &Colors,
    scheme: &ColorScheme,
    selected: bool,
) -> (Rgb, Rgb) {
    let mut fg = resolve(cell.fg, colors, scheme);
    let mut bg = resolve(cell.bg, colors, scheme);
    if cell.flags.contains(Flags::DIM) {
        fg = blend(fg, bg, DIM_ALPHA);
    }
    if cell.flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.flags.contains(Flags::HIDDEN) {
        fg = bg;
    }
    if selected {
        bg = scheme.selection;
    }
    (fg, bg)
}

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

/// Builds the draw list. Call with the terminal locked; it only reads.
pub fn build(
    term: &Terminal,
    scheme: &ColorScheme,
    focused: bool,
    hover: Option<&Hyperlink>,
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
    for y in 0..rows {
        let row_u16 = y as u16;
        let line = Line(y as i32 - offset as i32);
        let row = &grid[line];
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
            if flags.contains(Flags::WIDE_CHAR) || zerowidth.is_some() {
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
        let wide = grid[cursor.point.line][Column(x)]
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
) {
    let (cw, ch) = (m.cell.width, m.cell.height);
    let rect = |s: Span| Rectangle {
        x: bounds.x + f32::from(s.col) * cw,
        y: bounds.y + f32::from(s.row) * ch,
        width: f32::from(s.cells) * cw,
        height: ch,
    };
    if let Some(bg) = list.background {
        fill(renderer, bounds, bg);
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
    use alacritty_terminal::term::cell::Flags;
    use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb as VteRgb};

    fn scheme() -> ColorScheme {
        ColorScheme::graphite()
    }

    fn red_on_green() -> Cell {
        Cell {
            fg: Color::Named(NamedColor::Red),
            bg: Color::Named(NamedColor::Green),
            ..Cell::default()
        }
    }

    #[test]
    fn cell_colors_default_cell_uses_scheme() {
        let s = scheme();
        let (fg, bg) = cell_colors(&Cell::default(), &Colors::default(), &s, false);
        assert_eq!((fg, bg), (s.foreground, s.background));
    }

    #[test]
    fn cell_colors_resolves_named_indexed_and_truecolor() {
        let s = scheme();
        let cell = Cell {
            fg: Color::Indexed(196),
            bg: Color::Spec(VteRgb { r: 1, g: 2, b: 3 }),
            ..Cell::default()
        };
        let (fg, bg) = cell_colors(&cell, &Colors::default(), &s, false);
        assert_eq!(fg, s.indexed(196));
        assert_eq!(bg, Rgb { r: 1, g: 2, b: 3 });
        let (fg, bg) = cell_colors(&red_on_green(), &Colors::default(), &s, false);
        assert_eq!((fg, bg), (s.ansi[1], s.ansi[2]));
    }

    #[test]
    fn cell_colors_honors_osc_overrides() {
        let s = scheme();
        let mut colors = Colors::default();
        colors[NamedColor::Red] = Some(VteRgb { r: 9, g: 9, b: 9 });
        let (fg, _) = cell_colors(&red_on_green(), &colors, &s, false);
        assert_eq!(fg, Rgb { r: 9, g: 9, b: 9 });
    }

    #[test]
    fn cell_colors_inverse_swaps_fg_and_bg() {
        let s = scheme();
        let mut cell = red_on_green();
        cell.flags = Flags::INVERSE;
        let (fg, bg) = cell_colors(&cell, &Colors::default(), &s, false);
        assert_eq!((fg, bg), (s.ansi[2], s.ansi[1]));
    }

    #[test]
    fn cell_colors_dim_moves_fg_towards_bg() {
        let s = scheme();
        let cell = Cell {
            flags: Flags::DIM,
            ..Cell::default()
        };
        let (fg, bg) = cell_colors(&cell, &Colors::default(), &s, false);
        assert_eq!(bg, s.background);
        assert_ne!(fg, s.foreground);
        assert!(fg.r < s.foreground.r && fg.r > s.background.r);
    }

    #[test]
    fn cell_colors_hidden_draws_fg_as_bg() {
        let s = scheme();
        let mut cell = red_on_green();
        cell.flags = Flags::HIDDEN;
        let (fg, bg) = cell_colors(&cell, &Colors::default(), &s, false);
        assert_eq!(fg, bg);
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
        let list = build(&t, &scheme(), true, None);
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
    fn build_unfocused_cursor_is_hollow() {
        let t = term_with(b"x");
        let list = build(&t, &scheme(), false, None);
        assert_eq!(list.cursor.map(|c| c.2), Some(CursorMark::Hollow));
        assert!(list.quads.is_empty());
    }

    #[test]
    fn build_draws_wide_chars_on_their_own() {
        let t = term_with("a日b".as_bytes());
        let list = build(&t, &scheme(), true, None);
        let texts: Vec<_> = list
            .texts
            .iter()
            .map(|r| (r.span.col, r.span.cells, r.text.as_str()))
            .collect();
        assert_eq!(texts, vec![(0, 1, "a"), (1, 2, "日"), (3, 1, "b")]);
    }

    #[test]
    fn build_underlines_hovered_link_only() {
        let t = term_with(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ x");
        let none = build(&t, &scheme(), true, None);
        assert!(none.lines.is_empty());
        let link = t.term().grid()[Line(0)][Column(0)].hyperlink().unwrap();
        let hovered = build(&t, &scheme(), true, Some(&link));
        assert_eq!(hovered.lines.len(), 1);
        assert_eq!(hovered.lines[0].0.cells, 4);
    }

    #[test]
    fn build_marks_selection_background() {
        let mut t = term_with(b"hello");
        let mut sel = crate::Selection::start(Point::new(Line(0), Column(1)));
        sel.update(Point::new(Line(0), Column(3)));
        t.set_selection(Some(sel));
        let list = build(&t, &scheme(), false, None);
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
    fn cell_colors_selected_cell_uses_selection_color() {
        let s = scheme();
        let (fg, bg) = cell_colors(&red_on_green(), &Colors::default(), &s, true);
        assert_eq!((fg, bg), (s.ansi[1], s.selection));
        let mut inv = red_on_green();
        inv.flags = Flags::INVERSE;
        let (_, bg) = cell_colors(&inv, &Colors::default(), &s, true);
        assert_eq!(bg, s.selection);
    }
}
