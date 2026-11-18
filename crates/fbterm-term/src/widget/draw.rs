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
use iced::advanced::graphics::text::{Raw, Renderer as _};
use iced::advanced::renderer::{Quad, Renderer as _};
use iced::{Border, Font, Rectangle, Size};

use super::Highlight;
use super::cell_graphics;
use super::colors::{cell_colors, resolve};
use super::features::{FontFeatures, run_features};
use super::images::{self, ImageLayer};
use super::shaped::{ShapeCache, ShapeKey};
use crate::images::ImageStore;
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
    /// A cell drawn on its own (wide, a grapheme cluster, or a standalone symbol): no ligature
    /// can form in it.
    pub alone: bool,
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
    /// Runs of block and box-drawing characters, drawn as cell-filling shapes instead of glyphs so
    /// their edges meet (see [`cell_graphics`]). Each is one character in one colour; the run exists
    /// only to draw a row of them as fewer, longer quads.
    pub cell_graphics: Vec<(Span, char, Rgb)>,
    /// Pictures in view and the `[image W×H]` text standing in for the ones that are gone (see
    /// [`ImageLayer`]). Empty when this terminal shows no images.
    pub images: ImageLayer,
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
/// Block and box-drawing characters never reach this: shapes draw them (see [`cell_graphics`]), and
/// only the rounded corners and diagonals of that block, which stay glyphs, are kept in runs — the
/// monospace fonts cover them and they make up whole rows of TUI frames.
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
///
/// `store` and `metrics` are the image store and the cell geometry being drawn with: pictures are
/// resolved here, under the same lock, because that is where the view is read (see [`images`]).
#[allow(clippy::too_many_arguments)] // the frame's whole context: view, style, hover, images
pub fn build(
    reader: &mut RowReader<Cell>,
    term: &Terminal,
    scheme: &ColorScheme,
    focused: bool,
    hover: Option<&Hyperlink>,
    highlight: Option<&Highlight>,
    store: &ImageStore,
    metrics: images::CellGeometry,
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
        images: images::resolve(store, term, metrics, scheme),
        ..DrawList::default()
    };
    // Pictures shown through `U+10EEEE` cells are collected as the rows below are walked: a cell
    // says which tile of which picture it shows, and its glyph is skipped.
    let mut virtuals = images::VirtualTiles::new(store, term, metrics);
    let mut quads: Vec<(Span, Rgb)> = Vec::new();
    let mut graphics: Vec<(Span, char, Rgb)> = Vec::new();
    let mut lines: Vec<(Span, (Rgb, LineKind))> = Vec::new();
    let mut strikes: Vec<(Span, (Rgb, LineKind))> = Vec::new();
    let marks = highlight.map(|h| (h, marks(term, h, rows, cols)));
    for y in 0..rows {
        let row_u16 = y as u16;
        let line = Line(y as i32 - offset as i32);
        let row = reader.row(grid, line);
        if let Some(v) = &mut virtuals {
            v.begin_row();
        }
        let mut run: Option<TextRun> = None;
        // What a run must share besides foreground and font style: a ligature never spans a
        // change of background, decoration, selection, match or the cursor cell.
        let mut run_key = None;
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
            // One scan of the row's matches gives both answers.
            let (in_match, current) = marks.as_ref().map_or((false, false), |(_, m)| {
                m[y].iter()
                    .filter(|m| (m.from..=m.to).contains(&x))
                    .fold((false, false), |(_, cur), m| (true, cur || m.current))
            });
            if let Some((h, _)) = &marks {
                if current {
                    bg = h.current_bg;
                    fg = h.current_fg;
                } else if in_match && !selected {
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

            let decoration = flags & (Flags::ALL_UNDERLINES | Flags::STRIKEOUT);
            let key = (bg, decoration, hovered, selected, in_match, is_cursor_cell);

            // Glyphs.
            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            if let Some(v) = &mut virtuals
                && v.visit(row_u16, col, cell)
            {
                // A kitty placeholder cell: a tile of its picture is drawn instead of this glyph.
                flush(&mut run, &mut list.texts);
                continue;
            }
            let bold = flags.contains(Flags::BOLD);
            let italic = flags.contains(Flags::ITALIC);
            let zerowidth = cell.zerowidth();
            // Block and box-drawing characters are shapes that fill their cells rather than glyphs
            // (see [`cell_graphics`]): they leave the text runs entirely, and a run of one character
            // in one colour becomes a single entry, drawn as one long quad per shape piece. A wide
            // cell or one carrying a combining mark keeps the font path: its glyph spans two cells or
            // belongs to a cluster.
            if !flags.contains(Flags::WIDE_CHAR)
                && zerowidth.is_none()
                && cell_graphics::shapes(cell.c).is_some()
            {
                flush(&mut run, &mut list.texts);
                match graphics.last_mut() {
                    // The same character in the same colour next door is the same shape: one entry
                    // for the run, so a row of them costs one quad per shape piece, not one per cell.
                    Some((span, c, last_fg))
                        if span.row == row_u16
                            && span.col + span.cells == col
                            && *c == cell.c
                            && *last_fg == fg =>
                    {
                        span.cells += 1;
                    }
                    _ => graphics.push((
                        Span {
                            row: row_u16,
                            col,
                            cells: 1,
                        },
                        cell.c,
                        fg,
                    )),
                }
                continue;
            }
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
                    alone: true,
                });
                continue;
            }
            let c = if cell.c == '\t' { ' ' } else { cell.c };
            match &mut run {
                // A blank continues any run: it draws nothing.
                Some(r)
                    if c == ' '
                        || (r.fg == fg
                            && r.bold == bold
                            && r.italic == italic
                            && run_key == Some(key)) =>
                {
                    r.text.push(c);
                    r.span.cells += 1;
                }
                _ if c == ' ' => flush(&mut run, &mut list.texts),
                _ => {
                    flush(&mut run, &mut list.texts);
                    run_key = Some(key);
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
                        alone: false,
                    });
                }
            }
        }
        flush(&mut run, &mut list.texts);
        if let Some(v) = &mut virtuals {
            v.end_row();
        }
    }
    if let Some(v) = virtuals {
        v.finish(&mut list.images);
    }
    list.quads = quads;
    list.cell_graphics = graphics;
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
#[derive(Debug, Clone)]
pub struct Metrics {
    pub cell: Size,
    pub font: Font,
    pub font_size: f32,
    pub features: FontFeatures,
    /// Physical pixels per logical pixel of the window being drawn (1.0 when unknown). Cell graphics
    /// need it to put their edges on whole physical pixels; text does not, because iced snaps that
    /// itself (see [`cell_graphics`]).
    pub scale: f32,
}

fn fill(renderer: &mut iced::Renderer, bounds: Rectangle, c: Rgb) {
    fill_color(renderer, bounds, color(c));
}

/// A quad in already-computed bounds, filled with `c`. Cell graphics come from physical pixels
/// divided by the scale, so their corners land where iced's own snapping would put them anyway.
fn fill_color(renderer: &mut iced::Renderer, bounds: Rectangle, c: iced::Color) {
    renderer.fill_quad(
        Quad {
            bounds,
            snap: true,
            ..Quad::default()
        },
        c,
    );
}

fn is_han(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3134F)
}

/// The color emoji font that ships with the system.
#[cfg(target_os = "macos")]
const EMOJI_FONT: &str = "Apple Color Emoji";
#[cfg(target_os = "windows")]
const EMOJI_FONT: &str = "Segoe UI Emoji";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const EMOJI_FONT: &str = "Noto Color Emoji";

/// Font for a run. The text stack's default Han fallback on macOS names a font that is not always
/// installed as a file, so ideographs would render blank; point them at one that ships with the OS.
/// A wide cluster asking for emoji presentation (VS16) is drawn with the color emoji font: the
/// terminal font's own glyph for the base (such as a text-style heart) would otherwise win. A
/// narrow one (without mode 2027 the cluster keeps one cell) keeps the terminal font, which fits.
fn run_font(base: Font, run: &TextRun) -> Font {
    let mut font = base;
    if run.span.cells == 2 && run.text.contains('\u{FE0F}') {
        return Font::with_name(EMOJI_FONT);
    }
    if cfg!(target_os = "macos")
        && run.span.cells == 2
        && run.text.chars().next().is_some_and(is_han)
    {
        font = Font::with_name("Hiragino Sans GB");
    }
    font
}

/// Shapes `runs` with their OpenType features and hands them to the renderer as shaped buffers,
/// each in the color `color_of` gives it. The cache keeps a buffer alive until the frame after its
/// last use, so a run that does not change is shaped once however often the pane redraws.
fn draw_runs(
    renderer: &mut iced::Renderer,
    runs: &[TextRun],
    bounds: Rectangle,
    m: &Metrics,
    shapes: &mut ShapeCache,
    color_of: impl Fn(&TextRun) -> iced::Color,
) {
    let (cw, ch) = (m.cell.width, m.cell.height);
    for run in runs {
        let mut font = run_font(m.font, run);
        if run.bold {
            font.weight = iced::font::Weight::Bold;
        }
        if run.italic {
            font.style = iced::font::Style::Italic;
        }
        let detected = if m.features.ligatures && !run.alone {
            shapes.sets(font)
        } else {
            std::sync::Arc::from([])
        };
        let features = run_features(&m.features, &detected, run.alone);
        let r = Rectangle {
            x: bounds.x + f32::from(run.span.col) * cw,
            y: bounds.y + f32::from(run.span.row) * ch,
            width: f32::from(run.span.cells) * cw,
            height: ch,
        };
        let key = ShapeKey::new(
            &run.text,
            font,
            m.font_size,
            ch,
            r.width + cw,
            features,
            !run.alone,
        );
        let buffer = shapes.buffer(key);
        renderer.fill_raw(Raw {
            buffer: std::sync::Arc::downgrade(&buffer),
            position: iced::Point::new(r.x, r.y),
            color: color_of(run),
            clip_bounds: bounds,
        });
    }
}

/// Issues the renderer primitives for `list` inside `bounds`.
/// What must be painted above the z < 0 pictures: decorations, the cursor, the grid text and the
/// dimmed `[image W×H]` lines. `render` calls this inside one layer opened after every under-text
/// picture, which is what keeps it above them (see the note there).
fn draw_text_group(
    renderer: &mut iced::Renderer,
    list: &DrawList,
    bounds: Rectangle,
    m: &Metrics,
    shapes: &mut ShapeCache,
) {
    let (cw, ch) = (m.cell.width, m.cell.height);
    let rect = |s: Span| Rectangle {
        x: bounds.x + f32::from(s.col) * cw,
        y: bounds.y + f32::from(s.row) * ch,
        width: f32::from(s.cells) * cw,
        height: ch,
    };
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
    // Block and box-drawing characters: cell-filling shapes whose edges are the grid's own, worked
    // out in physical pixels so neighbours agree on them (see [`cell_graphics`]). They go where their
    // glyphs used to, under the text.
    if !list.cell_graphics.is_empty() {
        let g = cell_graphics::Geometry::new(bounds, m.cell, m.scale);
        for &(span, c, fg) in &list.cell_graphics {
            let Some(shapes) = cell_graphics::shapes(c) else {
                continue;
            };
            for shape in shapes {
                let paint = color(fg).scale_alpha(shape.alpha());
                g.cover(
                    shape,
                    u32::from(span.col),
                    u32::from(span.row),
                    u32::from(span.cells),
                    |r| fill_color(renderer, r, paint),
                );
            }
        }
    }
    // Grid text is shaped here, with the run's OpenType features, and handed to the renderer as
    // shaped buffers; the cache keeps them alive until the frame after their last use. Placeholder
    // text (`[image W×H]`) goes through the same path, only dimmer (spec 2.6).
    shapes.end_frame();
    draw_runs(renderer, &list.texts, bounds, m, shapes, |run| {
        color(run.fg)
    });
    if !list.images.placeholders.is_empty() {
        draw_runs(
            renderer,
            &list.images.placeholders,
            bounds,
            m,
            shapes,
            |run| color(run.fg).scale_alpha(images::PLACEHOLDER_ALPHA),
        );
    }
}

/// Which extra layers one frame's pictures force, and nothing more (see [`render`]'s comment: the
/// order is fixed by layer creation; a layer that buys nothing just costs iced a prepare pass).
#[derive(Debug, PartialEq, Eq)]
struct LayerPlan {
    /// Text over the under-text pictures: needed exactly when there is one to go under.
    lift_text: bool,
    /// The accent in its own top layer: needed whenever this frame opens ANY picture layer — both
    /// z < 0 and z >= 0 pictures paint after the base layer, so either kind could hide the border.
    lift_accent: bool,
}

/// With no pictures at all both are false and `render` issues exactly what it issued before images
/// existed: everything in the window's base layer, one text batch for every pane.
fn layer_plan(under_text: usize, over_text: usize) -> LayerPlan {
    let pictures = under_text > 0 || over_text > 0;
    LayerPlan {
        lift_text: under_text > 0,
        lift_accent: pictures,
    }
}

pub fn render(
    list: &DrawList,
    renderer: &mut iced::Renderer,
    bounds: Rectangle,
    m: Metrics,
    accent: Option<Rgb>,
    background_opacity: f32,
    shapes: &mut ShapeCache,
) {
    let (cw, ch) = (m.cell.width, m.cell.height);
    // Pictures are laid out relative to the grid's top-left corner; this shifts them here.
    let at = iced::Point::new(bounds.x, bounds.y);
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
    // Paint order on screen is the ORDER LAYERS ARE CREATED, not the order primitives are issued:
    // `push_clip` opens a new layer above the current one and `pop_clip` returns to it
    // (`iced_graphics-0.14.0/src/layer.rs`), and a renderer paints layers in index order — each
    // one's quads, then its images, then its text (`iced_wgpu-0.14.0/src/lib.rs`). Everything
    // issued above went into the base layer, so it sits under every picture opened from here on.
    // The two layers that lift something over the pictures are opened ONLY when this frame has a
    // picture that makes the lift necessary ([`layer_plan`]): with none, each pane's text stays in
    // the window's one base-layer batch, as it was before images existed.

    let plan = layer_plan(list.images.under_text.len(), list.images.over_text.len());
    // z < 0 pictures (and the tiles replacing `U+10EEEE` cells), each in its own clipped layer:
    // over the cell backgrounds, under the text that follows.
    images::draw_pictures(renderer, at, &list.images.under_text);
    // Decorations, cursor, glyphs and placeholder text share ONE layer opened after all of them —
    // only then: a background image may cover a cell's fill, but it must not cover what is written
    // there, and with no under-text picture the base layer already keeps that order.
    if plan.lift_text {
        renderer.with_layer(bounds, |r| draw_text_group(r, list, bounds, &m, shapes));
    } else {
        draw_text_group(renderer, list, bounds, &m, shapes);
    }
    // Pictures with `z >= 0` sit over the text: their cells' glyphs are already drawn.
    images::draw_pictures(renderer, at, &list.images.over_text);
    // The pane accent in a layer of its own, opened last, when this frame has any picture at all:
    // both z < 0 and z >= 0 pictures paint after the base layer, and either could cover the border
    // that says which pane is focused (their cells start where the border does). With no pictures
    // there is nothing to stay under, and the base layer draws it after everything else.
    if let Some(a) = accent {
        let border = Quad {
            bounds,
            border: Border {
                color: color(a),
                width: 1.0,
                radius: 0.0.into(),
            },
            snap: true,
            ..Quad::default()
        };
        if plan.lift_accent {
            renderer.with_layer(bounds, |r| r.fill_quad(border, iced::Color::TRANSPARENT));
        } else {
            renderer.fill_quad(border, iced::Color::TRANSPARENT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheme() -> ColorScheme {
        ColorScheme::graphite()
    }

    /// The app-wide store the widget asks for pictures from. These tests feed text only, so nothing
    /// is ever looked up in it; a leaked private store would race with other tests over the global.
    fn store() -> &'static ImageStore {
        crate::images::store()
    }

    /// The cell geometry of `term_with`'s terminals: 8x16 logical px at scale 1.
    fn metrics() -> images::CellGeometry {
        images::CellGeometry::new(Size::new(8.0, 16.0), 1.0)
    }

    fn term_with(bytes: &[u8]) -> Terminal {
        let mut t = Terminal::new(
            crate::GridSize {
                cols: 20,
                rows: 4,
                cell_width: 8.0,
                cell_height: 16.0,
                scale: 1.0,
            },
            100,
        );
        t.feed(bytes);
        t
    }

    #[test]
    fn emoji_presentation_draws_with_the_color_emoji_font() {
        let t = term_with("\x1b[?2027h❤\u{FE0F} ❤ 日".as_bytes());
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            true,
            None,
            None,
            store(),
            metrics(),
        );
        let font_of = |text: &str| {
            let run = list.texts.iter().find(|r| r.text == text).expect(text);
            run_font(Font::MONOSPACE, run).family
        };
        assert_eq!(font_of("❤\u{FE0F}"), iced::font::Family::Name(EMOJI_FONT));
        assert_eq!(
            font_of("❤"),
            Font::MONOSPACE.family,
            "text presentation keeps the font"
        );

        // Without the mode the cluster has one cell: a color emoji would spill over the next.
        let t = term_with("❤\u{FE0F}x".as_bytes());
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            true,
            None,
            None,
            store(),
            metrics(),
        );
        let run = list.texts.iter().find(|r| r.text == "❤\u{FE0F}").unwrap();
        assert_eq!(run.span.cells, 1);
        assert_eq!(
            run_font(Font::MONOSPACE, run).family,
            Font::MONOSPACE.family
        );
    }

    #[test]
    fn build_batches_runs_by_style() {
        let t = term_with(b"ab \x1b[31mcd\x1b[0m  \x1b[1mef");
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            true,
            None,
            None,
            store(),
            metrics(),
        );
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
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            true,
            None,
            None,
            store(),
            metrics(),
        );
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
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            false,
            None,
            None,
            store(),
            metrics(),
        );
        assert_eq!(list.cursor.map(|c| c.2), Some(CursorMark::Hollow));
        assert!(list.quads.is_empty());
    }

    #[test]
    fn build_draws_wide_chars_on_their_own() {
        let t = term_with("a日b".as_bytes());
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            true,
            None,
            None,
            store(),
            metrics(),
        );
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
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            true,
            None,
            None,
            store(),
            metrics(),
        );
        let texts: Vec<_> = list
            .texts
            .iter()
            .map(|r| (r.span.col, r.span.cells, r.text.as_str()))
            .collect();
        // The block and line characters are not text any more: shapes draw them (see `cell_graphics`),
        // each character a run of its own because they differ.
        let graphics: Vec<_> = list
            .cell_graphics
            .iter()
            .map(|(s, c, _)| (s.col, s.cells, *c))
            .collect();
        assert_eq!(
            texts,
            vec![(0, 1, "a"), (1, 1, "❯"), (2, 1, "b"), (7, 1, "x")]
        );
        assert_eq!(graphics, vec![(4, 1, '█'), (5, 1, '▓'), (6, 1, '─')]);
    }

    /// Helper for the shape tests: the cell-graphics batch as (row, col, cells, character).
    fn shape_runs(list: &DrawList) -> Vec<(u16, u16, u16, char)> {
        list.cell_graphics
            .iter()
            .map(|(s, c, _)| (s.row, s.col, s.cells, *c))
            .collect()
    }

    /// A run of one shape character is one entry; the text around it keeps its own runs.
    #[test]
    fn block_runs_leave_the_text_for_shape_runs() {
        let t = term_with("ab██cd".as_bytes());
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            true,
            None,
            None,
            store(),
            metrics(),
        );
        let texts: Vec<_> = list
            .texts
            .iter()
            .map(|r| (r.span.col, r.span.cells, r.text.as_str()))
            .collect();
        assert_eq!(texts, vec![(0, 2, "ab"), (4, 2, "cd")]);
        assert_eq!(shape_runs(&list), vec![(0, 2, 2, '█')]);
    }

    #[test]
    fn a_row_of_one_line_character_merges_into_one_run() {
        let t = term_with("┌────┐".as_bytes());
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            true,
            None,
            None,
            store(),
            metrics(),
        );
        assert!(list.texts.is_empty(), "the whole row is shapes");
        // The four dashes share one entry; the corners differ, so they stand on their own.
        assert_eq!(
            shape_runs(&list),
            vec![(0, 0, 1, '┌'), (0, 1, 4, '─'), (0, 5, 1, '┐')]
        );
    }

    #[test]
    fn shapes_get_the_same_colour_as_text() {
        let build_row = |bytes: &[u8]| {
            let t = term_with(bytes);
            build(
                &mut RowReader::new(),
                &t,
                &scheme(),
                true,
                None,
                None,
                store(),
                metrics(),
            )
        };
        // DIM: the block blends exactly like the dim letter next to it.
        let list = build_row("\x1b[2ma█\x1b[0m".as_bytes());
        assert_eq!(list.texts.len(), 1);
        let (span, c, fg) = list.cell_graphics[0];
        assert_eq!((span.col, span.cells, c), (1, 1, '█'));
        assert_eq!(fg, list.texts[0].fg, "DIM resolves the same way for both");
        // INVERSE: the block takes the cell's background as its colour, like a glyph would.
        let list = build_row("\x1b[7ma█\x1b[0m".as_bytes());
        assert_eq!(list.cell_graphics[0].2, scheme().background);
        // A search match tints the background only, so the shape keeps its colour and the run split.
        let list = build_row("██".as_bytes());
        assert_eq!(shape_runs(&list), vec![(0, 0, 2, '█')]);
    }

    /// A selected block takes the selection the same way text does. Selection changes only the
    /// background here (see `colors::cell_colors`), so the shape keeps the cell's foreground and
    /// the selected cells get a selection-coloured quad under it.
    #[test]
    fn a_selected_block_sits_on_the_selection_and_keeps_its_colour() {
        let mut t = term_with("a██b".as_bytes());
        let mut sel = crate::Selection::start(Point::new(Line(0), Column(1)));
        sel.update(Point::new(Line(0), Column(2)));
        t.set_selection(Some(sel));
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            false,
            None,
            None,
            store(),
            metrics(),
        );
        assert_eq!(shape_runs(&list), vec![(0, 1, 2, '█')]);
        assert_eq!(
            list.cell_graphics[0].2,
            scheme().foreground,
            "the shape is drawn in the colour its glyph would have used"
        );
        assert!(
            list.quads.contains(&(
                Span {
                    row: 0,
                    col: 1,
                    cells: 2
                },
                scheme().selection
            )),
            "the selected cells get the selection background under the shape"
        );
    }

    #[test]
    fn block_under_a_block_cursor_uses_the_cursor_text_colour() {
        // Cursor back onto the first block of "ab██cd".
        let t = term_with("ab██cd\x1b[4D".as_bytes());
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            true,
            None,
            None,
            store(),
            metrics(),
        );
        assert!(
            list.quads
                .iter()
                .any(|(s, c)| s.col == 2 && *c == scheme().cursor),
            "the cursor cell gets the cursor background"
        );
        // The covered block takes the cursor's text colour; its neighbour keeps its own, so they are
        // two runs and not one.
        assert_eq!(
            list.cell_graphics
                .iter()
                .map(|(s, c, fg)| (s.col, s.cells, *c, *fg))
                .collect::<Vec<_>>(),
            vec![
                (2, 1, '█', scheme().background),
                (3, 1, '█', scheme().foreground)
            ]
        );
    }

    #[test]
    fn build_underlines_hovered_link_only() {
        let t = term_with(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ x");
        let none = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            true,
            None,
            None,
            store(),
            metrics(),
        );
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
            store(),
            metrics(),
        );
        assert_eq!(hovered.lines.len(), 1);
        assert_eq!(hovered.lines[0].0.cells, 4);
    }

    /// The text of each run on row 0, in order.
    fn row0(list: &DrawList) -> Vec<&str> {
        list.texts
            .iter()
            .filter(|r| r.span.row == 0)
            .map(|r| r.text.as_str())
            .collect()
    }

    #[test]
    fn runs_split_at_the_cursor() {
        let t = term_with(b"a->b\x1b[1;3H");
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            true,
            None,
            None,
            store(),
            metrics(),
        );
        assert_eq!(row0(&list), ["a-", ">", "b"]);
        // An unfocused (hollow) cursor keeps the cell's colors, and still splits.
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            false,
            None,
            None,
            store(),
            metrics(),
        );
        assert_eq!(row0(&list), ["a-", ">", "b"]);
    }

    #[test]
    fn runs_split_at_selection_edges() {
        let mut t = term_with(b"a->b");
        t.set_selection(Some(crate::Selection::on_line(0, 2, 3)));
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            false,
            None,
            None,
            store(),
            metrics(),
        );
        assert_eq!(row0(&list), ["a-", ">b"]);
    }

    #[test]
    fn runs_split_at_match_edges() {
        let t = term_with(b"a->b");
        let pattern = crate::SearchPattern::new(&crate::SearchQuery::new(">", false))
            .unwrap()
            .unwrap();
        let h = Highlight {
            pattern,
            current: None,
            match_bg: Rgb { r: 1, g: 1, b: 1 },
            current_bg: Rgb { r: 2, g: 2, b: 2 },
            current_fg: Rgb { r: 3, g: 3, b: 3 },
        };
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            false,
            None,
            Some(&h),
            store(),
            metrics(),
        );
        assert_eq!(row0(&list), ["a-", ">", "b"]);
    }

    #[test]
    fn runs_split_on_background_and_underline() {
        let t = term_with(b"\x1b[41m-\x1b[0m>");
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            false,
            None,
            None,
            store(),
            metrics(),
        );
        assert_eq!(row0(&list), ["-", ">"]);
        let t = term_with(b"\x1b[4m-\x1b[0m>");
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            false,
            None,
            None,
            store(),
            metrics(),
        );
        assert_eq!(row0(&list), ["-", ">"]);
    }

    #[test]
    fn alone_runs_are_marked() {
        let t = term_with("日 e\u{301} ->".as_bytes());
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            false,
            None,
            None,
            store(),
            metrics(),
        );
        let alone: Vec<(&str, bool)> = list
            .texts
            .iter()
            .map(|r| (r.text.as_str(), r.alone))
            .collect();
        assert_eq!(alone, [("日", true), ("e\u{301}", true), ("->", false)]);
    }

    #[test]
    fn a_ligature_at_the_last_cells_stays_in_its_run() {
        let mut t = Terminal::new(
            crate::GridSize {
                cols: 4,
                rows: 3,
                cell_width: 8.0,
                cell_height: 16.0,
                scale: 1.0,
            },
            100,
        );
        t.feed(b"ab->\r\n");
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            false,
            None,
            None,
            store(),
            metrics(),
        );
        let run = list.texts.iter().find(|r| r.span.row == 0).unwrap();
        assert_eq!(run.text, "ab->");
        assert_eq!((run.span.col, run.span.cells), (0, 4));
    }

    #[test]
    fn build_marks_selection_background() {
        let mut t = term_with(b"hello");
        let mut sel = crate::Selection::start(Point::new(Line(0), Column(1)));
        sel.update(Point::new(Line(0), Column(3)));
        t.set_selection(Some(sel));
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            false,
            None,
            None,
            store(),
            metrics(),
        );
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
        let list = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            false,
            None,
            Some(&h),
            store(),
            metrics(),
        );
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
        let plain = build(
            &mut RowReader::new(),
            &t,
            &scheme(),
            false,
            None,
            None,
            store(),
            metrics(),
        );
        assert!(plain.quads.iter().all(|(_, c)| *c != tint && *c != strong));
    }

    // Compression-on variants: the same content, written into the oldest history rows and
    // compressed (`cold_tests::pair`), viewed with the view scrolled to the top, builds the same
    // draw list as with compression off.

    /// Both terminals of the pair (`cols` x `rows`), scrolled to the top of the history, after
    /// `prepare` (given the oldest line); asserts that `draw` builds the same list from each and
    /// returns the compressed one's.
    fn cold_list(
        cols: u16,
        rows: u16,
        before: &[u8],
        prepare: impl Fn(&mut Terminal, i32),
        draw: impl Fn(&Terminal) -> DrawList,
    ) -> DrawList {
        let (mut off, mut on) = crate::cold_tests::pair(cols, rows, before, b"");
        let top = on.term().grid().topmost_line().0;
        assert!(top < -1000, "the content is deep in history");
        for t in [&mut off, &mut on] {
            t.scroll_to_top();
            prepare(t, top);
        }
        let (a, b) = (draw(&off), draw(&on));
        assert_eq!(
            format!("{a:?}"),
            format!("{b:?}"),
            "differs with compression on"
        );
        b
    }

    fn cold(before: &[u8]) -> DrawList {
        cold_list(
            20,
            4,
            before,
            |_, _| {},
            |t| {
                build(
                    &mut RowReader::new(),
                    t,
                    &scheme(),
                    true,
                    None,
                    None,
                    store(),
                    metrics(),
                )
            },
        )
    }

    #[test]
    fn build_batches_runs_by_style_compressed() {
        let list = cold(b"ab \x1b[31mcd\x1b[0m  \x1b[1mef\r\n");
        let texts: Vec<_> = list
            .texts
            .iter()
            .filter(|r| r.span.row == 0)
            .map(|r| (r.span.col, r.text.as_str(), r.bold))
            .collect();
        assert_eq!(
            texts,
            vec![(0, "ab", false), (3, "cd", false), (7, "ef", true)]
        );
        assert!(list.cursor.is_none(), "the cursor is below the view");
    }

    #[test]
    fn explicit_backgrounds_are_quads_compressed() {
        let list = cold(b"\x1b[42mab\x1b[0m cd\r\n");
        let green = list.quads.iter().find(|(s, _)| s.row == 0 && s.col == 0);
        assert_eq!(green.map(|(s, _)| s.cells), Some(2));
    }

    #[test]
    fn wide_and_non_ascii_characters_compressed() {
        let list = cold("a日b\r\na❯b █▓─x\r\n".as_bytes());
        let texts: Vec<_> = list
            .texts
            .iter()
            .map(|r| (r.span.row, r.span.col, r.span.cells, r.text.as_str()))
            .collect();
        assert_eq!(
            texts[..6],
            [
                (0, 0, 1, "a"),
                (0, 1, 2, "日"),
                (0, 3, 1, "b"),
                (1, 0, 1, "a"),
                (1, 1, 1, "❯"),
                (1, 2, 1, "b")
            ]
        );
        // The same through the cold path: shapes come out as their own batch, one run per character.
        let graphics: Vec<_> = list
            .cell_graphics
            .iter()
            .map(|(s, c, _)| (s.row, s.col, s.cells, *c))
            .collect();
        assert_eq!(graphics, [(1, 4, 1, '█'), (1, 5, 1, '▓'), (1, 6, 1, '─')]);
    }

    #[test]
    fn emoji_presentation_draws_with_the_color_emoji_font_compressed() {
        let list = cold("\x1b[?2027h❤\u{FE0F} ❤ 日\r\n".as_bytes());
        let run = list.texts.iter().find(|r| r.text == "❤\u{FE0F}").unwrap();
        assert_eq!(
            run_font(Font::MONOSPACE, run).family,
            iced::font::Family::Name(EMOJI_FONT)
        );
    }

    #[test]
    fn build_underlines_hovered_link_only_compressed() {
        let bytes = b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ x\r\n";
        let none = cold(bytes);
        assert!(none.lines.is_empty());
        let hovered = cold_list(
            20,
            4,
            bytes,
            |_, _| {},
            |t| {
                let top = t.term().grid().topmost_line();
                let link = t
                    .term()
                    .grid()
                    .cell(Point::new(top, Column(0)))
                    .hyperlink()
                    .unwrap();
                assert_eq!(link.uri(), "https://example.com");
                build(
                    &mut RowReader::new(),
                    t,
                    &scheme(),
                    true,
                    Some(&link),
                    None,
                    store(),
                    metrics(),
                )
            },
        );
        assert_eq!(hovered.lines.len(), 1);
        assert_eq!(hovered.lines[0].0.cells, 4);
    }

    #[test]
    fn runs_split_at_selection_and_match_edges_compressed() {
        let list = cold_list(
            20,
            4,
            b"a->b\r\n",
            |t, top| t.set_selection(Some(crate::Selection::on_line(top, 2, 3))),
            |t| {
                build(
                    &mut RowReader::new(),
                    t,
                    &scheme(),
                    false,
                    None,
                    None,
                    store(),
                    metrics(),
                )
            },
        );
        assert_eq!(row0(&list), ["a-", ">b"]);
        let list = cold_list(
            20,
            4,
            b"a->b\r\n",
            |_, _| {},
            |t| {
                let pattern = crate::SearchPattern::new(&crate::SearchQuery::new(">", false))
                    .unwrap()
                    .unwrap();
                let h = Highlight {
                    pattern,
                    current: None,
                    match_bg: Rgb { r: 1, g: 1, b: 1 },
                    current_bg: Rgb { r: 2, g: 2, b: 2 },
                    current_fg: Rgb { r: 3, g: 3, b: 3 },
                };
                build(
                    &mut RowReader::new(),
                    t,
                    &scheme(),
                    false,
                    None,
                    Some(&h),
                    store(),
                    metrics(),
                )
            },
        );
        assert_eq!(row0(&list), ["a-", ">", "b"]);
    }

    #[test]
    fn runs_split_on_background_and_underline_and_alone_runs_compressed() {
        assert_eq!(row0(&cold(b"\x1b[41m-\x1b[0m>\r\n")), ["-", ">"]);
        assert_eq!(row0(&cold(b"\x1b[4m-\x1b[0m>\r\n")), ["-", ">"]);
        let list = cold("日 e\u{301} ->\r\n".as_bytes());
        let alone: Vec<(&str, bool)> = list
            .texts
            .iter()
            .filter(|r| r.span.row == 0)
            .map(|r| (r.text.as_str(), r.alone))
            .collect();
        assert_eq!(alone, [("日", true), ("e\u{301}", true), ("->", false)]);
    }

    #[test]
    fn a_ligature_at_the_last_cells_stays_in_its_run_compressed() {
        let list = cold_list(
            4,
            3,
            b"ab->\r\n",
            |_, _| {},
            |t| {
                build(
                    &mut RowReader::new(),
                    t,
                    &scheme(),
                    false,
                    None,
                    None,
                    store(),
                    metrics(),
                )
            },
        );
        let run = list.texts.iter().find(|r| r.span.row == 0).unwrap();
        assert_eq!(run.text, "ab->");
        assert_eq!((run.span.col, run.span.cells), (0, 4));
    }

    #[test]
    fn build_marks_selection_and_tints_matches_compressed() {
        let list = cold_list(
            20,
            4,
            b"hello\r\n",
            |t, top| {
                let mut sel = crate::Selection::start(Point::new(Line(top), Column(1)));
                sel.update(Point::new(Line(top), Column(3)));
                t.set_selection(Some(sel));
            },
            |t| {
                build(
                    &mut RowReader::new(),
                    t,
                    &scheme(),
                    false,
                    None,
                    None,
                    store(),
                    metrics(),
                )
            },
        );
        assert!(list.quads.contains(&(
            Span {
                row: 0,
                col: 1,
                cells: 3
            },
            scheme().selection
        )));
        let (tint, strong) = (Rgb { r: 1, g: 1, b: 1 }, Rgb { r: 2, g: 2, b: 2 });
        let list = cold_list(
            20,
            4,
            b"ab ab\r\nab\r\n",
            |_, _| {},
            |t| {
                let pattern = crate::SearchPattern::new(&crate::SearchQuery::new("ab", false))
                    .unwrap()
                    .unwrap();
                let all = t.visible_matches(&pattern);
                assert_eq!(all.len(), 3);
                let h = Highlight {
                    pattern,
                    current: Some(all[1]),
                    match_bg: tint,
                    current_bg: strong,
                    current_fg: Rgb { r: 3, g: 3, b: 3 },
                };
                build(
                    &mut RowReader::new(),
                    t,
                    &scheme(),
                    false,
                    None,
                    Some(&h),
                    store(),
                    metrics(),
                )
            },
        );
        let quads: Vec<_> = list
            .quads
            .iter()
            .map(|(s, c)| (s.row, s.col, s.cells, *c))
            .collect();
        assert!(quads.contains(&(0, 0, 2, tint)));
        assert!(quads.contains(&(1, 0, 2, tint)));
        assert_eq!(quads.iter().filter(|q| q.3 == strong).count(), 1);
    }

    /// The four shapes a frame's picture counts can take, and the layers each one needs: text is
    /// lifted only when something is under it, the accent whenever any picture exists (both kinds
    /// paint after the base layer); with no pictures at all `render` must issue exactly what it did
    /// before images existed (everything in the base layer).
    #[test]
    fn layers_are_opened_only_when_pictures_force_them() {
        assert_eq!(
            layer_plan(0, 0),
            LayerPlan {
                lift_text: false,
                lift_accent: false,
            },
            "a text-only pane pays for no extra layers"
        );
        assert_eq!(
            layer_plan(2, 0),
            LayerPlan {
                lift_text: true,
                lift_accent: true,
            },
            "background pictures force the text above them — and they paint after the base layer \
             too, starting where the border does, so the accent must be lifted over them as well"
        );
        assert_eq!(
            layer_plan(0, 1),
            LayerPlan {
                lift_text: false,
                lift_accent: true,
            },
            "an over-text picture threatens the border; nothing needs lifting under"
        );
        assert_eq!(
            layer_plan(1, 1),
            LayerPlan {
                lift_text: true,
                lift_accent: true,
            },
            "both kinds, both lifts"
        );
    }
}
