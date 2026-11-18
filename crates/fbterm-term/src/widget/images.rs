//! Where pictures sit on the grid, and how they are drawn.
//!
//! The store ([`crate::images::ImageStore`]) hands over resampled copies whose size is stated in
//! PHYSICAL pixels; this module turns a placement's cell span, its pixel offsets inside the anchor
//! cell and its source rectangle into the rectangles iced draws, in LOGICAL pixels relative to the
//! grid's top-left corner ([`super::draw`] shifts them by the widget's own origin). Two rules decide
//! every number here:
//!
//! - **Never stretched.** A copy is drawn at its own size divided by the display scale, which maps
//!   one image pixel onto exactly one device pixel: the store resampled it to that many physical
//!   pixels for this cell size, so nothing scales it again.
//! - **Nothing spills.** Every draw is clipped to the intersection of its placement box and the pane
//!   ([`clip_rect`]), so an image cannot cover another pane's cells or leak outside the widget.
//!
//! Images shown through kitty Unicode placeholders (`U+10EEEE` cells, virtual placements) are
//! collected by [`VirtualTiles`], which rides along the grid row loop in [`super::draw::build`]:
//! each cell says which tile of which picture it shows ([`placeholder_tile`]), and the picture is
//! fitted into its span's cells once so all its cells agree on where it sits.

use std::collections::HashMap;
use std::sync::Arc;

use feathercritty::event::ImageKey;
use feathercritty::graphics::kitty::Rect;
use feathercritty::graphics::kitty_diacritics::{
    PLACEHOLDER_CHAR, decode_placeholder_cell, diacritic_index,
};
use feathercritty::term::cell::Cell;
use feathercritty::vte::ansi::Color as AnsiColor;
use iced::advanced::image::{FilterMethod, Handle, Image as IcedImage, Renderer as _};
use iced::advanced::renderer::Renderer as _;
use iced::{Point, Rectangle, Size};

use fbterm_config::Rgb;

use super::draw::{Span, TextRun};
use crate::Terminal;
use crate::images::{ImageStore, Lookup, ResampledImage};
use crate::palette::ColorScheme;

/// Alpha of the `[image W×H]` text that replaces an image the caps dropped (spec 2.6: the
/// placeholder is a hint about what was there, not content): the pane's foreground at this alpha,
/// never written into the grid, so it is neither selectable nor searchable.
pub const PLACEHOLDER_ALPHA: f32 = 0.55;

/// One store query's outcome, kept for the frame: `None` while pending or gone.
type CopyCache = HashMap<(ImageKey, (u32, u32)), Option<Arc<ResampledImage>>>;

/// Cell geometry image drawing converts with: logical cells for iced to draw into, and the display
/// scale that turns physical pixels (the unit placements and the store speak) into them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellGeometry {
    /// One text cell in logical pixels: what the widget draws text with, so images line up with it.
    pub cell: Size,
    /// Physical pixels per logical pixel of this window (1.0 when unknown).
    pub scale: f32,
}

impl CellGeometry {
    /// Geometry for one cell size and display scale.
    ///
    /// A scale that is not a positive finite number becomes 1.0 — the same fallback
    /// [`crate::GridSize`] uses when a display reports something bogus (see
    /// [`crate::grid_size_from_bounds`]). Without it a NaN would turn every box into `cols × rows`
    /// pixels and every bound into millions of logical ones, so images would stop lining up with the
    /// cells programs were told about. The widget builds this from the terminal's own grid, which is
    /// already sanitized; the guard here keeps every other caller honest too.
    pub fn new(cell: Size, scale: f32) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        Self { cell, scale }
    }

    /// One cell in physical pixels — the size programs are told and the store resamples by. The same
    /// rounding as [`crate::GridSize::physical_cell`], so a span a program asked for in cells and
    /// the box it is drawn in come from one number.
    fn physical_cell(&self) -> (u32, u32) {
        let whole = |v: f32| (v * self.scale).round().max(1.0).min(u32::MAX as f32) as u32;
        (whole(self.cell.width), whole(self.cell.height))
    }

    /// Physical pixels as logical ones.
    fn logical_of(&self, px: u32) -> f32 {
        (f64::from(px) / f64::from(self.scale.max(f32::EPSILON))) as f32
    }

    /// The box of `cols` × `rows` cells whose top-left cell is at grid-local `(col, row)`, in logical
    /// pixels relative to the grid origin. Coordinates may be negative (a placement partly scrolled
    /// off above the view) or past the pane; [`clip_rect`] is what cuts it to the screen.
    fn cell_box(&self, col: i64, row: i64, cols: u32, rows: u32) -> Rectangle {
        let at = |v: i64, unit: f32| (v as f64 * f64::from(unit)) as f32;
        let cells = |n: u32, unit: f32| f64::from(n) as f32 * unit;
        Rectangle::new(
            Point::new(at(col, self.cell.width), at(row, self.cell.height)),
            Size::new(cells(cols, self.cell.width), cells(rows, self.cell.height)),
        )
    }

    /// The pane's text area in the same coordinates: what every image is clipped to.
    fn pane(&self, cols: usize, rows: usize) -> Rectangle {
        self.cell_box(0, 0, cols as u32, rows as u32)
    }
}

/// The box a placement covers, in logical pixels relative to the grid origin: its `cols × rows` cell
/// span, moved by its `x_off`/`y_off` physical-pixel offsets inside the anchor cell.
///
/// `row_in_view` and `col` are where feathercritty says the placement's first cell is (the row is
/// negative when it starts above the top of the view).
///
/// The offset moves the box; it does not extend it. The result is cut back to the unshifted span, so
/// a picture never covers pixels past the cells its program was told about (finding M5): what comes
/// off is the sliver the offset would otherwise spill into the cell after the span. An offset beyond
/// the whole span leaves nothing inside it, and such a box overlaps nothing when drawn.
pub fn placement_rect(
    m: CellGeometry,
    row_in_view: i32,
    col: usize,
    cols: u32,
    rows: u32,
    x_off: u32,
    y_off: u32,
) -> Rectangle {
    let span = m.cell_box(col as i64, i64::from(row_in_view), cols, rows);
    let mut r = span;
    r.x += m.logical_of(x_off);
    r.y += m.logical_of(y_off);
    match clip_rect(r, span) {
        Some(cut) => cut,
        // Shifted clear of its own span: an empty box at the shifted place, which draws nothing.
        None => Rectangle::new(Point::new(r.x, r.y), Size::ZERO),
    }
}

/// The PHYSICAL pixel box of a `cols × rows` cell span: what to ask [`crate::images::ImageStore::get`]
/// for, so the copy it makes is exactly the size those cells occupy at the current font size and
/// display scale. Saturates rather than overflowing a hostile span.
pub fn placement_box_px(m: CellGeometry, cols: u32, rows: u32) -> (u32, u32) {
    let (cw, ch) = m.physical_cell();
    (cw.saturating_mul(cols), ch.saturating_mul(rows))
}

/// The PHYSICAL pixel box that draws a placement 1:1: the area of picture it shows, when its cell
/// span is exactly what that area needs at the current physical cell size.
///
/// A picture sent without a cell size gets the span `div_ceil` rounds up from its own pixels
/// (feathercritty's `span_for`), so this matches one: the copy asked for is the picture at its own
/// size, nothing is filtered, pixel art and sixel output stay sharp, and the store answers with the
/// source's own buffer (findings I1 and P4). It stops matching as soon as the font, line height or
/// display scale changes — then the copy fills the new span, which is the scaling the spec allows.
/// A span a program sized itself (`c`/`r`, OSC 1337 width or height) only matches by coincidence,
/// and resampling to fill it stays as it was. `None` says "no match": ask for the span.
fn natural_box_px(
    store: &ImageStore,
    owner: u64,
    key: ImageKey,
    src: Option<Rect>,
    cols: u32,
    rows: u32,
    m: CellGeometry,
) -> Option<(u32, u32)> {
    let (src_w, src_h) = store.source_size(owner, key)?;
    let (cw, ch) = m.physical_cell();
    // The area this placement shows, measured exactly as the store measures it.
    let (area_w, area_h) = crate::images::src_area(src, src_w, src_h);
    (area_w.div_ceil(cw) == cols && area_h.div_ceil(ch) == rows).then_some((area_w, area_h))
}

/// `r` cut to `pane`; `None` when they do not overlap at all. A rectangle with a zero axis overlaps
/// nothing: a placement cannot cover less than a pixel.
pub fn clip_rect(r: Rectangle, pane: Rectangle) -> Option<Rectangle> {
    let x0 = r.x.max(pane.x);
    let y0 = r.y.max(pane.y);
    let x1 = (r.x + r.width).min(pane.x + pane.width);
    let y1 = (r.y + r.height).min(pane.y + pane.height);
    if x1 > x0 && y1 > y0 {
        Some(Rectangle::new(
            Point::new(x0, y0),
            Size::new(x1 - x0, y1 - y0),
        ))
    } else {
        None
    }
}

/// The part of a whole-image resample that a placement's `src` rectangle shows, in the copy's pixels.
///
/// The store always resamples the WHOLE image at the factor that fits `src` into the placement box
/// (so crops of one shape share one copy), and this maps the rectangle from source-image pixels onto
/// that copy. A zero `w` or `h` means "to the end" and a start past the edge is clamped back, exactly
/// as [`crate::images::ImageStore::get`] normalizes it: the drawer offsets the copy by `-x`/`-y`
/// logical pixels and clips it to the placement box, which leaves precisely this rectangle filling
/// that box.
pub fn crop_for_src(src: Option<Rect>, source: (u32, u32), copy: (u32, u32)) -> Rect {
    let Some(r) = src else {
        return Rect {
            x: 0,
            y: 0,
            w: copy.0,
            h: copy.1,
        };
    };
    let axis = |off: u32, len: u32, side: u32, out: u32| -> (u32, u32) {
        // The shown area, clamped to the image this copy came from (the store's `src_area` rule).
        let start = off.min(side.saturating_sub(1));
        let room = side.saturating_sub(start);
        let shown = if len == 0 {
            room.max(1)
        } else {
            len.min(room).max(1)
        };
        // The copy is the whole image at one factor, so source pixels map linearly onto it.
        let f = f64::from(out) / f64::from(side.max(1));
        let x = ((f64::from(start) * f).round() as u32).min(out.saturating_sub(1));
        let w = ((f64::from(shown) * f).round() as u32).clamp(1, out - x);
        (x, w)
    };
    let (x, w) = axis(r.x, r.w, source.0, copy.0);
    let (y, h) = axis(r.y, r.h, source.1, copy.1);
    Rect { x, y, w, h }
}

/// What a placement's picture is cut to: its box, the pane, and — when it shows a `src` rectangle —
/// as much of the box as that crop covers.
///
/// The copy holds the WHOLE image, shifted so the crop's first pixel sits at the box's top-left
/// corner. Whenever the box is bigger than the crop on either axis — its aspect differs from the
/// crop's, its span was rounded up to whole cells, [`crate::images::resample`] capped the copy —
/// pixels past the rectangle the program asked for would otherwise show inside the box: a sprite
/// sheet cropped to one tile would spill the neighbouring tiles into it. With no `src` the crop is
/// the whole copy and this third term cannot cut anything.
pub fn placement_clip(
    box_: Rectangle,
    crop: Rect,
    m: CellGeometry,
    pane: Rectangle,
) -> Option<Rectangle> {
    let shown = Rectangle::new(
        Point::new(box_.x, box_.y),
        Size::new(m.logical_of(crop.w), m.logical_of(crop.h)),
    );
    clip_rect(clip_rect(box_, pane)?, shown)
}

/// The rectangle a `copy`-pixel picture occupies inside `area`: its pixels at 1:1 physical scale
/// (`copy / scale` logical pixels) shrunk to fit with its aspect kept, centred in `area`.
///
/// This is the "fit to the placement, never stretch" rule for pictures that are not asked to fill
/// their box exactly — virtual placements, whose cells spell out one picture and read best centred.
fn fit_centred(area: Rectangle, copy: (u32, u32), scale: f32) -> Rectangle {
    let w = (f64::from(copy.0) / f64::from(scale.max(f32::EPSILON))) as f32;
    let h = (f64::from(copy.1) / f64::from(scale.max(f32::EPSILON))) as f32;
    // Never upscaled: the copy is already sized for this box, so anything below 1.0 here is a shrink.
    let fit = (area.width / w).min(area.height / h).min(1.0);
    let (w, h) = (w * fit, h * fit);
    Rectangle::new(
        Point::new(
            area.x + (area.width - w) / 2.0,
            area.y + (area.height - h) / 2.0,
        ),
        Size::new(w, h),
    )
}

/// Where one placeholder cell's tile of a virtual placement comes from.
///
/// The whole picture is fitted into the span's `cols × rows` cells at 1:1 physical scale and centred
/// in them (see [`fit_centred`]); this returns that rectangle — the bounds to draw with — and the
/// part of it lying under cell `(col_in_span, row_in_span)` — the clip that cuts the picture to this
/// one cell, so consecutive cells show consecutive tiles. `None` when the cell sits wholly outside
/// the picture: a centred picture letterboxes, and the outermost cells of an ill-matched span show
/// nothing of it.
pub fn virtual_tile(
    m: CellGeometry,
    anchor: (i64, i64),
    cols: u32,
    rows: u32,
    copy: (u32, u32),
    col_in_span: u32,
    row_in_span: u32,
) -> Option<(Rectangle, Rectangle)> {
    let fitted = fit_centred(m.cell_box(anchor.0, anchor.1, cols, rows), copy, m.scale);
    let cell = m.cell_box(
        anchor.0 + i64::from(col_in_span),
        anchor.1 + i64::from(row_in_span),
        1,
        1,
    );
    let clip = clip_rect(cell, fitted)?;
    Some((fitted, clip))
}

/// One `U+10EEEE` cell resolved to the tile it shows.
///
/// The image id comes from the cell's foreground colour (its most significant byte from a third
/// diacritic, or from the cell to the left where the protocol allows), the placement id from its
/// underline colour (`None` when that is absent or zero, meaning "any virtual placement of this
/// image"), and the row and column within the placement from the two required diacritics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placeholder {
    pub id: u32,
    pub pid: Option<u32>,
    pub row: u32,
    pub col: u32,
}

/// The number a colour encodes for the placeholder protocol: truecolor is the 24-bit value, an
/// indexed colour is its palette index, and a named colour encodes no number at all.
fn color_id(c: AnsiColor) -> Option<u32> {
    match c {
        AnsiColor::Spec(rgb) => {
            Some((u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b))
        }
        AnsiColor::Indexed(i) => Some(u32::from(i)),
        AnsiColor::Named(_) => None,
    }
}

/// What one `U+10EEEE` cell says, applying the protocol's inheritance rules with `prev`, the tile
/// decoded for the cell to its left (`None` at a row's first cell, or after any cell that is not a
/// placeholder or does not resolve).
///
/// A cell stating its row and column decodes on its own, through feathercritty's
/// [`decode_placeholder_cell`]. The protocol also lets cells omit diacritics, because every cell of
/// one picture carries the same colours: with none at all a cell takes the row, the column plus one
/// and the id's high byte from the left; with only the row mark it takes the column plus one and the
/// high byte; and even stating both it may omit the high byte when the left cell is its predecessor
/// in the same row. Cells whose colours differ from the left one inherit nothing, and a row's first
/// cell has nothing to inherit from, so those decode to `None` and stay ordinary text.
pub fn placeholder_tile(cell: &Cell, prev: Option<Placeholder>) -> Option<Placeholder> {
    if cell.c != PLACEHOLDER_CHAR {
        return None;
    }
    let low = color_id(cell.fg)?;
    let pid = cell
        .underline_color()
        .and_then(color_id)
        .filter(|p| *p != 0);
    // The colours the inheritance rules compare against the left cell — its foreground and underline,
    // which is exactly what `prev` carries.
    let inherits = prev.is_some_and(|p| p.id & 0x00FF_FFFF == low && p.pid == pid);
    let marks = cell.zerowidth().unwrap_or(&[]);
    match marks.len() {
        2 | 3 => {
            let (id, row, col) = decode_placeholder_cell(cell)?;
            // Same colours, same row, left cell is this one's predecessor: then a missing third mark
            // means the left cell's most significant id byte.
            let id = match prev {
                Some(p) if inherits && p.row == row && p.col + 1 == col => {
                    (p.id & 0xFF00_0000) | (id & 0x00FF_FFFF)
                }
                _ => id,
            };
            Some(Placeholder { id, pid, row, col })
        }
        // Only the row mark: column and high byte continue from the left cell of the same row.
        1 => {
            let row = diacritic_index(marks[0])? as u32;
            let p = prev.filter(|p| inherits && p.row == row)?;
            Some(Placeholder {
                id: (p.id & 0xFF00_0000) | low,
                pid,
                row,
                col: p.col + 1,
            })
        }
        // No marks at all: the whole position comes from the left cell.
        0 => {
            let p = prev.filter(|_| inherits)?;
            Some(Placeholder {
                id: (p.id & 0xFF00_0000) | low,
                pid,
                row: p.row,
                col: p.col + 1,
            })
        }
        // More marks than the protocol defines: not a placeholder this terminal understands.
        _ => None,
    }
}

/// One picture to draw, in logical pixels relative to the grid's top-left corner.
#[derive(Debug, Clone)]
pub struct ImageDraw {
    /// Where the picture goes: its own size at 1:1 physical scale, never stretched to `clip`.
    pub bounds: Rectangle,
    /// What to cut it to (its placement box ∩ the crop's extent ∩ the pane, or a run of cells).
    pub clip: Rectangle,
    /// The store's resampled copy: cloning this handle costs nothing and reuses one GPU texture.
    pub handle: Handle,
    /// What image this is, so a frame that is reused without asking the store can still report what
    /// it shows (see [`super::super::images::ImageStore::touch_drawn`]).
    pub key: ImageKey,
}

/// Everything a frame draws that is not text, resolved from the terminal under its lock.
#[derive(Debug, Default)]
pub struct ImageLayer {
    /// Placements with `z < 0`, and the placeholder tiles that replace `U+10EEEE` cells (they stand
    /// in for glyphs of their own rows): after cell backgrounds, before the text.
    pub under_text: Vec<ImageDraw>,
    /// Placements with `z >= 0`: after the text and cursor.
    pub over_text: Vec<ImageDraw>,
    /// `[image W×H]` lines to draw in [`PLACEHOLDER_ALPHA`]: placements the caps evicted, and rows
    /// whose image scrolled out of the view for good.
    pub placeholders: Vec<TextRun>,
    /// Something asked for is still on its way: a decode or a resample the store queued this frame.
    /// The widget rebuilds its draw list (asking again) instead of caching a frame that draws less
    /// than it can, and the store's `wake` hook is what makes that redraw happen.
    pub pending: bool,
}

impl ImageLayer {
    /// The images this layer's pictures show: what a reused frame reports to the store so its LRU
    /// follows what is on screen (see [`ImageStore::touch_drawn`](crate::images::ImageStore::touch_drawn)).
    pub fn drawn_keys(&self) -> impl Iterator<Item = ImageKey> + '_ {
        self.under_text
            .iter()
            .chain(self.over_text.iter())
            .map(|d| d.key)
    }
}

/// The `[image W×H]` text of an evicted placement or a scrolled-off row, as a text run at its grid
/// position (`alone`: drawn on its own, so no ligature forms with the cells around it).
fn placeholder_run(row: i64, col: usize, text: String, fg: Rgb) -> TextRun {
    let u16_of = |v: i64| u16::try_from(v.max(0)).unwrap_or(u16::MAX);
    TextRun {
        span: Span {
            row: u16_of(row),
            col: u16_of(col as i64),
            cells: u16::try_from(text.chars().count()).unwrap_or(u16::MAX),
        },
        text,
        fg,
        bold: false,
        italic: false,
        alone: true,
    }
}

/// Resolves the placements and placeholder lines in view. Call with the terminal locked: it only
/// reads, and asking the store for a copy is cheap (a size not cached yet is queued on the worker
/// and answers [`Lookup::Pending`]; nothing is resampled here).
///
/// Returns an empty layer when this terminal shows no images.
pub fn resolve(
    store: &ImageStore,
    term: &Terminal,
    m: CellGeometry,
    scheme: &ColorScheme,
) -> ImageLayer {
    let mut out = ImageLayer::default();
    if !term.images_enabled() {
        return out;
    }
    let size = term.size();
    let pane = m.pane(usize::from(size.cols), usize::from(size.rows));
    let owner = term.image_owner();
    let t = term.term();

    // In the order feathercritty returns them: it is already paint order.
    for p in t.visible_placements() {
        // Clipped away entirely: nothing to draw, and no reason to ask a copy for it. (The real clip
        // is cut further by the crop below, once there is a copy to know the crop from.)
        let box_ = placement_rect(m, p.row_in_view, p.col, p.cols, p.rows, p.x_off, p.y_off);
        if clip_rect(box_, pane).is_none() {
            continue;
        }
        // A picture whose span is its own pixels at this cell size asks for exactly those pixels and
        // draws unscaled; a span the program sized, or one no longer matching after a cell-size
        // change, asks for the span and resamples into it as before (finding I1). A placement the
        // protocol asks to fill takes neither branch: it wants the whole span covered, so the copy is
        // the span's own box even where that stretches the picture.
        let box_px = if p.stretch {
            placement_box_px(m, p.cols, p.rows)
        } else {
            natural_box_px(store, owner, p.key, p.src, p.cols, p.rows, m)
                .unwrap_or_else(|| placement_box_px(m, p.cols, p.rows))
        };
        match store.get(owner, p.key, box_px, p.src, p.stretch) {
            Lookup::Ready(res) => {
                let crop = crop_for_src(p.src, res.source, (res.width, res.height));
                // The copy is the whole image; shifting it by the crop's offset puts the part this
                // placement shows in its box. The clip cuts away the rest — including everything
                // outside the crop, which would otherwise spill into a box larger than it ([`placement_clip`]).
                let bounds = Rectangle::new(
                    Point::new(box_.x - m.logical_of(crop.x), box_.y - m.logical_of(crop.y)),
                    Size::new(m.logical_of(res.width), m.logical_of(res.height)),
                );
                let Some(clip) = placement_clip(box_, crop, m, pane) else {
                    continue;
                };
                let draw = ImageDraw {
                    bounds,
                    clip,
                    handle: res.handle.clone(),
                    key: p.key,
                };
                if p.z < 0 {
                    out.under_text.push(draw);
                } else {
                    out.over_text.push(draw);
                }
            }
            // Gone for good (or this size provably cannot be made beside it): say what was there,
            // over the first row of where it sat.
            Lookup::Evicted { width, height } => {
                let row = i64::from(p.row_in_view).max(0);
                if row < i64::from(size.rows) {
                    out.placeholders.push(placeholder_run(
                        row,
                        p.col,
                        format!("[image {width}\u{d7}{height}]"),
                        scheme.foreground,
                    ));
                }
            }
            // Still coming: draw nothing now, but say so — the frame is rebuilt (and asks again) as
            // soon as the copy lands, which the store's `wake` hook reports without any timer.
            Lookup::Pending => out.pending = true,
            // Never known, or released: nothing to draw and no reason to wait.
            Lookup::Unknown => {}
        }
    }

    for ph in t.visible_placeholders() {
        if ph.row_in_view < 0 || ph.row_in_view >= i32::from(size.rows) {
            continue;
        }
        out.placeholders.push(placeholder_run(
            i64::from(ph.row_in_view),
            ph.col,
            ph.text,
            scheme.foreground,
        ));
    }
    out
}

/// One virtual placement seen in this frame: what it refers to, resolved once however many of its
/// cells are on screen. Where its picture sits is NOT kept here — every cell computes it from
/// itself (see [`VirtualTiles::tile`]), so the same image shown at two places draws twice.
struct Virtual {
    key: ImageKey,
    cols: u32,
    rows: u32,
}

/// A run of consecutive cells sharing one picture, merged so a row of an image costs one draw.
struct Run {
    row: u16,
    col: u16,
    cells: u32,
    picture: Rectangle,
    handle: Handle,
    key: ImageKey,
}

/// Collects the pictures shown through `U+10EEEE` cells while the grid's rows are walked, and turns
/// them into tile draws. Call with the terminal locked.
///
/// Ask it about every cell of a row in order between [`VirtualTiles::begin_row`] and
/// [`VirtualTiles::end_row`]; when it answers `true` the cell is a placeholder whose glyph must not
/// be drawn as text, because its tile is (or will be) drawn instead.
pub struct VirtualTiles<'a> {
    store: &'a ImageStore,
    term: &'a Terminal,
    m: CellGeometry,
    pane: Rectangle,
    /// `(image id, placement id)` → what it refers to, resolved once per frame.
    virtuals: HashMap<(u32, Option<u32>), Virtual>,
    /// One store query per distinct `(key, box)`: the span a copy is made for.
    /// A copy per distinct `(key, target size)`, `None` when the store has nothing for it yet.
    copies: CopyCache,
    /// The cell to the left, as far as the inheritance rules got it.
    prev: Option<Placeholder>,
    run: Option<Run>,
    out: Vec<ImageDraw>,
    /// A copy a tile of this frame is waiting on (see [`ImageLayer::pending`]).
    pending: bool,
}

impl<'a> VirtualTiles<'a> {
    /// A resolver for one frame, or `None` when this terminal shows no images (then placeholder
    /// cells are drawn as the text they are).
    pub fn new(store: &'a ImageStore, term: &'a Terminal, m: CellGeometry) -> Option<Self> {
        if !term.images_enabled() {
            return None;
        }
        let size = term.size();
        Some(Self {
            store,
            term,
            m,
            pane: m.pane(usize::from(size.cols), usize::from(size.rows)),
            virtuals: HashMap::new(),
            copies: HashMap::new(),
            prev: None,
            run: None,
            out: Vec::new(),
            pending: false,
        })
    }

    /// Starts a row: the inheritance rules only look leftwards within one row.
    pub fn begin_row(&mut self) {
        self.prev = None;
    }

    /// Ends a row: flushes its last merged run of tiles.
    pub fn end_row(&mut self) {
        self.flush_run();
    }

    /// Whether `cell` at grid-local `(row, col)` is a placeholder whose glyph to skip.
    ///
    /// Every `U+10EEEE` cell answers `true` while images are on: the character is private use, so
    /// shaping it would draw a missing-glyph box whether or not the tile behind it can be resolved.
    pub fn visit(&mut self, row: u16, col: u16, cell: &Cell) -> bool {
        if cell.c != PLACEHOLDER_CHAR {
            self.prev = None;
            return false;
        }
        let Some(tile) = placeholder_tile(cell, self.prev) else {
            // A placeholder this terminal cannot resolve (unknown id, or colours that break the
            // inheritance chain): it draws no tile, and nothing to its right may inherit from it.
            // Its glyph is still skipped — a private-use character is not worth a tofu box.
            self.prev = None;
            return true;
        };
        self.prev = Some(tile);
        self.tile(tile, row, col);
        true
    }

    /// This frame's tiles, added to the layer's under-text group: they stand in for glyphs, and real
    /// text drawn over them stays legible.
    pub fn finish(mut self, out: &mut ImageLayer) {
        self.flush_run();
        out.under_text.append(&mut self.out);
        out.pending |= self.pending;
    }

    /// Adds cell `(row, col)`'s tile of the picture `tile` names, merging it into a run of
    /// consecutive cells sharing that picture.
    fn tile(&mut self, tile: Placeholder, row: u16, col: u16) {
        let id = (tile.id, tile.pid);
        if !self.virtuals.contains_key(&id) {
            let Some((key, cols, rows)) = self.term.term().kitty_virtual_image(tile.id, tile.pid)
            else {
                self.flush_run();
                return; // no such virtual placement: this cell has nothing to show
            };
            self.virtuals.insert(id, Virtual { key, cols, rows });
        }
        let (key, cols, rows) = {
            let v = &self.virtuals[&id];
            (v.key, v.cols, v.rows)
        };
        // A virtual placement carries no stretch flag: kitty has none, placeholders keep the ratio, so
        // its copy is always fitted into the span and then mapped cell by cell onto it.
        let box_px = placement_box_px(self.m, cols, rows);
        if !self.copies.contains_key(&(key, box_px)) {
            let copy = match self
                .store
                .get(self.term.image_owner(), key, box_px, None, false)
            {
                Lookup::Ready(res) => Some(res),
                // Pending: the pane redraws when the copy lands (see [`ImageLayer::pending`]).
                // Unknown or evicted: a virtual placement's cells are text already, so per spec 5
                // they get no extra placeholder.
                Lookup::Pending => {
                    self.pending = true;
                    None
                }
                Lookup::Unknown | Lookup::Evicted { .. } => None,
            };
            self.copies.insert((key, box_px), copy);
        }
        let Some(copy) = self.copies[&(key, box_px)].clone() else {
            self.flush_run();
            return;
        };
        // The protocol defines this cell as "tile (row, col) of the image at THIS cell", so the
        // picture is pinned to this cell: cells of one copy agree on where it sits because they do
        // the same arithmetic, and cells of a second copy of the same `(id, pid)` — the same picture
        // printed twice on one screen — land where their own cells do.
        let anchor = (
            i64::from(col) - i64::from(tile.col),
            i64::from(row) - i64::from(tile.row),
        );
        let Some((picture, part)) = virtual_tile(
            self.m,
            anchor,
            cols,
            rows,
            (copy.width, copy.height),
            tile.col,
            tile.row,
        ) else {
            // This cell lies outside the centred picture: a gap, so the run ends here.
            self.flush_run();
            return;
        };
        if clip_rect(part, self.pane).is_none() {
            self.flush_run(); // the tile is off-screen too
            return;
        }
        match &mut self.run {
            Some(r)
                if r.key == key
                    && r.picture == picture
                    && r.row == row
                    && u64::from(r.col) + u64::from(r.cells) == u64::from(col) =>
            {
                r.cells += 1;
            }
            _ => {
                self.flush_run();
                self.run = Some(Run {
                    row,
                    col,
                    cells: 1,
                    picture,
                    handle: copy.handle.clone(),
                    key,
                });
            }
        }
    }

    /// Draws the merged run as one picture clipped to its cells.
    fn flush_run(&mut self) {
        let Some(run) = self.run.take() else { return };
        let cells = self
            .m
            .cell_box(i64::from(run.col), i64::from(run.row), run.cells, 1);
        let Some(cells) = clip_rect(cells, self.pane) else {
            return;
        };
        let Some(clip) = clip_rect(cells, run.picture) else {
            return;
        };
        self.out.push(ImageDraw {
            bounds: run.picture,
            clip,
            handle: run.handle,
            key: run.key,
        });
    }
}

/// Issues the primitives for `draws`, shifted to a widget whose grid starts at `at`.
///
/// Each picture goes in its own layer so the clip is a real scissor rectangle, and with
/// [`FilterMethod::Nearest`] because the copy already holds exactly the pixels this box needs: any
/// filtering here would only blur it.
pub fn draw_pictures(renderer: &mut iced::Renderer, at: Point, draws: &[ImageDraw]) {
    for d in draws {
        let shift = |r: Rectangle| Rectangle::new(Point::new(at.x + r.x, at.y + r.y), r.size());
        let (clip, bounds) = (shift(d.clip), shift(d.bounds));
        renderer.with_layer(clip, |r| {
            r.draw_image(
                IcedImage::new(d.handle.clone())
                    .filter_method(FilterMethod::Nearest)
                    .snap(true),
                bounds,
                clip,
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use feathercritty::event::ImageData;
    use feathercritty::graphics::kitty_diacritics::ROW_COLUMN_DIACRITICS;
    use feathercritty::grid::RowReader;
    use feathercritty::index::{Column, Line};
    use feathercritty::vte::ansi::Rgb as AnsiRgb;

    /// Cells of 8×16 logical px at `scale`.
    fn geometry(scale: f32) -> CellGeometry {
        CellGeometry::new(Size::new(8.0, 16.0), scale)
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rectangle {
        Rectangle::new(Point::new(x, y), Size::new(w, h))
    }

    /// The diacritic encoding `n`.
    fn mark(n: usize) -> char {
        ROW_COLUMN_DIACRITICS[n]
    }

    /// A placeholder cell for the image whose id's low 24 bits are `id`, with these diacritics.
    fn placeholder(id: u32, marks: &[char]) -> Cell {
        let mut c = Cell {
            c: PLACEHOLDER_CHAR,
            fg: AnsiColor::Spec(AnsiRgb {
                r: (id >> 16) as u8,
                g: (id >> 8) as u8,
                b: id as u8,
            }),
            ..Cell::default()
        };
        for m in marks {
            c.push_zerowidth(*m);
        }
        c
    }

    #[test]
    fn rescale_keeps_cells() {
        // A 4x2 placement covers exactly 4 cells by 2 whatever the cell size or display scale: the
        // physical box asked of the store is the span at the physical cell, and what comes back —
        // drawn at copy px / scale — is that same span in logical px.
        for (scale, cell) in [
            (1.0f32, (8.0, 16.0)),
            (2.0, (8.0, 16.0)),
            (1.25, (7.5, 15.5)),
        ] {
            let m = CellGeometry::new(Size::new(cell.0, cell.1), scale);
            let r = placement_rect(m, 3, 5, 4, 2, 0, 0);
            assert_eq!((r.x, r.y), (5.0 * cell.0, 3.0 * cell.1));
            assert_eq!((r.width, r.height), (4.0 * cell.0, 2.0 * cell.1));

            let cw = (cell.0 * scale).round().max(1.0);
            let ch = (cell.1 * scale).round().max(1.0);
            assert_eq!(m.physical_cell(), (cw as u32, ch as u32));
            let (bw, bh) = placement_box_px(m, 4, 2);
            assert_eq!((bw, bh), (cw as u32 * 4, ch as u32 * 2));

            // A copy of that many physical pixels, drawn at copy px / scale, is exactly the box the
            // span occupies — no slack for an implementation that dropped a column or a row: the only
            // difference from `cols × cell` logical px is the rounding of each cell to whole physical
            // pixels, half a cell per axis.
            let drawn = Size::new(m.logical_of(bw), m.logical_of(bh));
            let eps = 1e-4f32;
            let exact = |copy: u32| (f64::from(copy) / f64::from(scale)) as f32;
            assert!(
                (drawn.width - exact(bw)).abs() <= eps,
                "the copy is drawn at its own size over the scale, got {}",
                drawn.width
            );
            assert!(
                (drawn.height - exact(bh)).abs() <= eps,
                "the copy is drawn at its own size over the scale, got {}",
                drawn.height
            );
            // Which leaves only the rounding of each cell to whole physical pixels: half a cell per
            // axis, times the span. Anything coarser (a dropped column) fails here.
            assert!(
                (drawn.width - r.width).abs() <= f64::from(4.0 * (0.5 / scale)) as f32 + eps,
                "4 columns of rounded physical cells, back to logical px"
            );
            assert!(
                (drawn.height - r.height).abs() <= f64::from(2.0 * (0.5 / scale)) as f32 + eps,
                "2 rows of rounded physical cells, back to logical px"
            );
        }
    }

    /// A display that reports a scale of NaN (or zero, or infinity) must not move pictures off the
    /// grid: [`crate::GridSize`] already falls back to 1.0 for what it tells programs, and the
    /// widget builds this geometry from those numbers — the guard here is for every caller. Without
    /// it `NaN × cell` would poison every box, and a scale of 0 would ask the store for boxes of
    /// `cols × rows` pixels while drawing them at millions of logical ones.
    #[test]
    fn a_bogus_scale_falls_back_to_one() {
        let one = geometry(1.0);
        for bogus in [f32::NAN, 0.0, -1.0, f32::INFINITY] {
            let m = CellGeometry::new(Size::new(8.0, 16.0), bogus);
            assert_eq!(m.scale, 1.0, "{bogus} is not a usable scale");
            assert_eq!(
                placement_box_px(m, 4, 2),
                placement_box_px(one, 4, 2),
                "the copy asked of the store follows the fallback"
            );
            assert_eq!(
                placement_rect(m, 0, 0, 4, 2, 3, 5),
                placement_rect(one, 0, 0, 4, 2, 3, 5),
                "so does where it is drawn"
            );
        }
    }

    #[test]
    fn placement_offsets_are_physical_pixels() {
        let m = geometry(2.0);
        let r = placement_rect(m, 0, 0, 1, 1, 10, 6);
        assert_eq!((r.x, r.y), (5.0, 3.0), "10 and 6 physical px at 2x");
    }

    #[test]
    fn placement_rects_clip_to_pane() {
        let m = geometry(1.0);
        let pane = m.pane(10, 5); // 80 x 80 logical px
        // Scrolled off above: the placement starts two rows up, and two of its rows show.
        let above = placement_rect(m, -2, 1, 3, 4, 0, 0);
        assert_eq!(above.y, -32.0);
        assert_eq!(
            clip_rect(above, pane),
            Some(rect(8.0, 0.0, 24.0, 32.0)),
            "cut to the view"
        );
        // Hanging off the right edge and the bottom.
        let corner = placement_rect(m, 4, 9, 3, 3, 0, 0);
        assert_eq!(clip_rect(corner, pane), Some(rect(72.0, 64.0, 8.0, 16.0)));
        // Wholly outside: no draw at all.
        assert_eq!(clip_rect(placement_rect(m, -3, 0, 2, 3, 0, 0), pane), None);
        assert_eq!(clip_rect(placement_rect(m, 5, 0, 2, 1, 0, 0), pane), None);
        // Fully inside survives unchanged.
        let inside = placement_rect(m, 1, 1, 2, 2, 0, 0);
        assert_eq!(clip_rect(inside, pane), Some(inside));
    }

    const MB: usize = 1024 * 1024;

    /// A 160x96 opaque PNG (256 bytes: solid RGBA, one filter-0 scanline per row, compressed) as the
    /// base64 body an OSC 1337 sequence carries. Built once so the sizing tests can ask for the same
    /// picture under both aspect rules.
    const PNG_160X96_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAKAAAABgCAYAAACaJ3mZAAAAx0lEQVR42u3SMREAAAjEsJeOFJzCgAiWDDHQa9I18EYEDIgBwYAYEAyIAcGAGBAMiAHBgBgQDIgBwYAYEAyIAcGAGBAMiAHBgBgQDIgBwYAYEAyIAcGAGBADioABMSAYEAOCATEgGBADggExIBgQA4IBMSAYEAOCATEgGBADggExIBgQA4IBMSAYEAOCATEgGBADYkAwIAYEA2JAMCAGBANiQDAgBgQDYkAwIAYEA2JAMCAGBANiQDAgBgQDYkAwIAYEA2JAOAs1+EtyH6AijAAAAABJRU5ErkJggg==";

    /// One OSC 1337 inline transmission of [`PNG_160X96_B64`] sized `width` cells by `height` cells,
    /// asking (`preserve=false`) or not asking (`preserve=true`) to fill that box.
    fn inline_1337(width: u32, height: u32, preserve: bool) -> String {
        format!(
            "\x1b]1337;File=inline=1;width={width};height={height};preserveAspectRatio={}:{PNG_160X96_B64}\x07",
            if preserve { 1 } else { 0 },
        )
    }

    /// A terminal of 20 columns by 6 rows whose physical cell is `cell` square pixels, with one inline
    /// picture displayed at `width` × `height` cells starting on row 1, plus the store it submits to.
    fn inline_term(cell: u32, width: u32, height: u32, preserve: bool) -> (ImageStore, Terminal) {
        let store = ImageStore::with_caps(8 * MB, 32 * MB);
        let mut term = Terminal::new(
            crate::GridSize {
                cols: 20,
                rows: 6,
                cell_width: cell as f32,
                cell_height: cell as f32,
                scale: 1.0,
            },
            100,
        );
        term.set_cell_pixel_size(cell, cell);
        term.feed(format!("\x1b[1;1H{}", inline_1337(width, height, preserve)).as_bytes());
        for event in term.take_events() {
            if let crate::TermEvent::ImageReceived { key, data } = event {
                store.submit(term.image_owner(), key, data);
            }
        }
        (store, term)
    }

    /// Drive an inline picture to the point where its placement draws, and return that layer. A
    /// container payload (unlike kitty's raw RGBA, which `submit` installs at once) is only *queued* by
    /// submit, so the first resolve finds a decode running and has no copy to ask for: pump the decode,
    /// resolve to queue the resample, pump again, resolve to draw it.
    fn draw_after_decode(
        store: &ImageStore,
        term: &Terminal,
        m: CellGeometry,
        scheme: &ColorScheme,
    ) -> ImageLayer {
        store.run_jobs_until_idle(); // the payload's decode
        assert!(
            resolve(store, term, m, scheme).pending,
            "the copy is asked for, never made on the draw path"
        );
        store.run_jobs_until_idle(); // the copy itself
        resolve(store, term, m, scheme)
    }

    /// The one case a protocol asks for a stretch: OSC 1337 with `preserveAspectRatio=0` and both axes
    /// given as sizes. The copy is made at the span's own physical box (10 columns by 2 rows of 20x20
    /// px cells, 200x40) and drawn over the whole span, so the picture's 160x96 ratio is ignored and
    /// nothing is letterboxed (owner decision, 2026-11-18).
    #[test]
    fn an_inline_picture_that_asks_to_fill_fills_the_span() {
        let (store, term) = inline_term(20, 10, 2, false);
        let m = CellGeometry::new(Size::new(20.0, 20.0), 1.0);
        let layer = draw_after_decode(&store, &term, m, &ColorScheme::graphite());
        let draws: Vec<&ImageDraw> = layer.under_text.iter().chain(&layer.over_text).collect();
        assert_eq!(draws.len(), 1, "the picture draws once");
        assert_eq!(
            draws[0].bounds,
            rect(0.0, 0.0, 200.0, 40.0),
            "10 columns by 2 rows of 20x20 px cells, covered whole"
        );
        assert_eq!(
            draws[0].clip,
            rect(0.0, 0.0, 200.0, 40.0),
            "the clip is the span too: no letterbox bar is cut away"
        );
    }

    /// The same request with the default `preserveAspectRatio=1` keeps today's behavior: fitted into
    /// the span and anchored top-left, so 160x96 in a 200x40 box draws 67 wide by 40 tall.
    #[test]
    fn an_inline_picture_that_keeps_its_ratio_is_letterboxed() {
        let (store, term) = inline_term(20, 10, 2, true);
        let m = CellGeometry::new(Size::new(20.0, 20.0), 1.0);
        let layer = draw_after_decode(&store, &term, m, &ColorScheme::graphite());
        let draws: Vec<&ImageDraw> = layer.under_text.iter().chain(&layer.over_text).collect();
        assert_eq!(draws.len(), 1, "the picture draws once");
        assert_eq!(
            draws[0].bounds,
            rect(0.0, 0.0, 67.0, 40.0),
            "160x96 fitted into 200x40 keeps its ratio: 67 wide"
        );
    }

    /// kitty `c` together with `r` is NOT a stretch request: the protocol says "when both are specified
    /// the image is letterboxed/pillarboxed to prevent distortion", and feathercritty reports
    /// `stretch = false` for every kitty placement. Regression guard for that ruling, on the same
    /// 160x96-into-200x40 geometry as the fill test above.
    #[test]
    fn a_kitty_placement_with_cells_and_rows_stays_letterboxed() {
        let store = ImageStore::with_caps(8 * MB, 32 * MB);
        let mut term = Terminal::new(
            crate::GridSize {
                cols: 20,
                rows: 6,
                cell_width: 20.0,
                cell_height: 20.0,
                scale: 1.0,
            },
            100,
        );
        term.set_cell_pixel_size(20, 20);
        // A 160x96 opaque RGBA frame (61440 bytes) as id 7, then displayed over 10 columns by 2 rows.
        let pixels = "/wAA//8AAP//AAD/".repeat(5120);
        term.feed(format!("\x1b_Ga=t,f=32,s=160,v=96,i=7;{pixels}\x1b\\").as_bytes());
        for event in term.take_events() {
            if let crate::TermEvent::ImageReceived { key, data } = event {
                store.submit(term.image_owner(), key, data);
            }
        }
        term.feed(b"\x1b[1;1H\x1b_Ga=p,i=7,c=10,r=2;\x1b\\");
        term.take_events(); // the OK replies: not what this is about
        let m = CellGeometry::new(Size::new(20.0, 20.0), 1.0);
        let scheme = ColorScheme::graphite();
        assert!(
            resolve(&store, &term, m, &scheme).pending,
            "the copy is asked for"
        );
        store.run_jobs_until_idle();
        let layer = resolve(&store, &term, m, &scheme);
        let draws: Vec<&ImageDraw> = layer.under_text.iter().chain(&layer.over_text).collect();
        assert_eq!(draws.len(), 1, "the placement draws once");
        assert_eq!(
            draws[0].bounds,
            rect(0.0, 0.0, 67.0, 40.0),
            "kitty keeps the ratio: 160x96 in a 10x2 span is 67 wide by 40 tall"
        );
    }

    /// A stretched picture follows its span whatever the cells have become: after the physical cell
    /// changes under it the copy is re-made at the new box (10 columns of 10 px by 2 rows of 40 px), so
    /// a font, line-height or scale change cannot leave letterbox bars where the protocol asked for a fill.
    #[test]
    fn a_stretched_picture_still_fills_its_span_after_the_cell_size_changes() {
        let (store, mut term) = inline_term(20, 10, 2, false);
        let scheme = ColorScheme::graphite();
        let m = CellGeometry::new(Size::new(20.0, 20.0), 1.0);
        let first = draw_after_decode(&store, &term, m, &scheme);
        let draws: Vec<&ImageDraw> = first.under_text.iter().chain(&first.over_text).collect();
        assert_eq!(draws.len(), 1, "the picture draws once");
        assert_eq!(
            draws[0].bounds,
            rect(0.0, 0.0, 200.0, 40.0),
            "the 20x20 px span"
        );

        // Taller, narrower cells: the same 10x2 cell span is now 100 px by 80 px.
        term.set_cell_pixel_size(10, 40);
        let m = CellGeometry::new(Size::new(10.0, 40.0), 1.0);
        assert!(
            resolve(&store, &term, m, &scheme).pending,
            "the new box is a copy the store has not made"
        );
        store.run_jobs_until_idle();
        let second = resolve(&store, &term, m, &scheme);
        let draws: Vec<&ImageDraw> = second.under_text.iter().chain(&second.over_text).collect();
        assert_eq!(draws.len(), 1, "still one picture");
        assert_eq!(
            draws[0].bounds,
            rect(0.0, 0.0, 100.0, 80.0),
            "and it fills the new span whole"
        );
    }

    /// An `x_off`/`y_off` shift moves a placement inside its cells; it must not carry the box past
    /// the span the program was given (finding M5). Cells here are 8×16 logical px.
    #[test]
    fn an_offset_never_spills_past_the_span() {
        let m = geometry(1.0);
        // A 2×1 span (16×16 px) pushed right 5 px and down 3: it still ends at the span's far edge.
        let r = placement_rect(m, 0, 0, 2, 1, 5, 3);
        assert_eq!((r.x, r.y), (5.0, 3.0), "the offset moves it");
        assert_eq!(
            (r.width, r.height),
            (11.0, 13.0),
            "and the box stops at x=16 and y=16, where its cells do, instead of spilling to 21×19"
        );
        // No offset: the span exactly, as before.
        assert_eq!(
            placement_rect(m, 0, 0, 2, 1, 0, 0),
            rect(0.0, 0.0, 16.0, 16.0),
        );
        // Shifted clear of its own span (a hostile offset larger than the cells): nothing is left of
        // the placement, so it draws nothing rather than covering a neighbour's cells.
        let gone = placement_rect(m, 0, 0, 1, 1, 40, 0);
        assert_eq!(clip_rect(gone, m.pane(10, 5)), None, "past its own cell");
    }

    #[test]
    fn crops_map_onto_the_shared_copy() {
        // No src: the whole copy.
        assert_eq!(
            crop_for_src(None, (64, 32), (128, 64)),
            Rect {
                x: 0,
                y: 0,
                w: 128,
                h: 64
            }
        );
        // The copy is the whole image at the factor that fits the src area into the box. A 16x8 crop
        // of a 64x32 image shown in a box twice that area means a 128x64 copy, and this crop is the
        // doubled rectangle under the doubled offset.
        assert_eq!(
            crop_for_src(
                Some(Rect {
                    x: 10,
                    y: 4,
                    w: 16,
                    h: 8
                }),
                (64, 32),
                (128, 64)
            ),
            Rect {
                x: 20,
                y: 8,
                w: 32,
                h: 16
            }
        );
        // A zero side means "to the end", clamped to the image.
        assert_eq!(
            crop_for_src(
                Some(Rect {
                    x: 60,
                    y: 30,
                    w: 0,
                    h: 0
                }),
                (64, 32),
                (128, 64)
            ),
            Rect {
                x: 120,
                y: 60,
                w: 8,
                h: 4
            }
        );
        // A start past the edge is pulled back inside rather than wrapping.
        assert_eq!(
            crop_for_src(
                Some(Rect {
                    x: 100,
                    y: 50,
                    w: 20,
                    h: 20
                }),
                (64, 32),
                (128, 64)
            ),
            Rect {
                x: 126,
                y: 62,
                w: 2,
                h: 2
            }
        );
        // Shrinking copies scale the same way (here a half).
        assert_eq!(
            crop_for_src(
                Some(Rect {
                    x: 0,
                    y: 8,
                    w: 64,
                    h: 16
                }),
                (64, 32),
                (32, 16)
            ),
            Rect {
                x: 0,
                y: 4,
                w: 32,
                h: 8
            }
        );
        // Two placements cropping the same shape of one image get crops of the same size, which is
        // what makes them share the store's copy.
        let a = crop_for_src(
            Some(Rect {
                x: 0,
                y: 0,
                w: 16,
                h: 8,
            }),
            (64, 32),
            (128, 64),
        );
        let b = crop_for_src(
            Some(Rect {
                x: 32,
                y: 16,
                w: 16,
                h: 8,
            }),
            (64, 32),
            (128, 64),
        );
        assert_eq!((a.w, a.h), (b.w, b.h));
    }

    #[test]
    fn virtual_placeholder_tiles() {
        let m = geometry(1.0);
        // A square copy in a span twice as tall as wide: centred, each of the 4 cells shows its own
        // quarter of it.
        let mut tiles = Vec::new();
        for row in 0..2 {
            for col in 0..2 {
                let (picture, clip) = virtual_tile(m, (2, 1), 2, 2, (16, 16), col, row).unwrap();
                assert_eq!(
                    picture,
                    rect(16.0, 24.0, 16.0, 16.0),
                    "centred in the 16x32 span"
                );
                tiles.push((clip.x, clip.y, clip.width, clip.height));
            }
        }
        assert_eq!(
            tiles,
            vec![
                (16.0, 24.0, 8.0, 8.0),
                (24.0, 24.0, 8.0, 8.0),
                (16.0, 32.0, 8.0, 8.0),
                (24.0, 32.0, 8.0, 8.0),
            ],
            "consecutive cells show consecutive tiles of one picture"
        );

        // A wide copy in a one-column span shrinks to the column's width and sits in the middle row;
        // the rows above and below it hold nothing of the picture.
        let (picture, middle) = virtual_tile(m, (0, 1), 1, 3, (24, 6), 0, 1).unwrap();
        assert_eq!(picture, rect(0.0, 39.0, 8.0, 2.0));
        assert_eq!(middle, picture, "the whole picture is in this row");
        assert_eq!(virtual_tile(m, (0, 1), 1, 3, (24, 6), 0, 0), None);
        assert_eq!(virtual_tile(m, (0, 1), 1, 3, (24, 6), 0, 2), None);

        // At 2x the copy's pixels are half as many logical px: tiles keep their cell geometry.
        let (picture, clip) = virtual_tile(geometry(2.0), (0, 0), 2, 1, (32, 16), 1, 0).unwrap();
        assert_eq!(picture, rect(0.0, 4.0, 16.0, 8.0));
        assert_eq!(clip, rect(8.0, 4.0, 8.0, 8.0));
    }

    /// A `src` rectangle cuts the picture to its own extent, not to the box around it: the copy holds
    /// the whole image, so a box bigger than the crop — a different aspect, a span rounded up to whole
    /// cells, a copy the caps shrank — would otherwise show pixels the program never asked for.
    #[test]
    fn a_crop_shows_only_its_own_pixels() {
        let m = geometry(1.0);
        let pane = m.pane(10, 5);
        // A 16x16 tile of a 64x32 sprite sheet, in a box two cells tall (16x32 physical px): the fit
        // factor is 1, so the copy is the whole sheet and the crop stays 16x16. The lower half of the
        // box must show nothing — not the next row of sprites.
        let crop = crop_for_src(
            Some(Rect {
                x: 0,
                y: 0,
                w: 16,
                h: 16,
            }),
            (64, 32),
            (64, 32),
        );
        let tall = placement_rect(m, 0, 0, 2, 2, 0, 0);
        assert_eq!((tall.width, tall.height), (16.0, 32.0));
        assert_eq!(
            placement_clip(tall, crop, m, pane),
            Some(rect(0.0, 0.0, 16.0, 16.0)),
            "a box taller than the crop clips to the crop"
        );

        // The same tile in a box four cells wide (32x16): the right half shows nothing either.
        let wide = placement_rect(m, 0, 0, 4, 1, 0, 0);
        assert_eq!(
            placement_clip(wide, crop, m, pane),
            Some(rect(0.0, 0.0, 16.0, 16.0)),
            "a box wider than the crop clips to the crop"
        );

        // A crop partway into the sheet keeps its own size, and a scrolled-off placement is still cut
        // to both the pane and the crop.
        let middle = crop_for_src(
            Some(Rect {
                x: 16,
                y: 16,
                w: 16,
                h: 8,
            }),
            (64, 32),
            (64, 32),
        );
        assert_eq!((middle.w, middle.h), (16, 8));
        // The same tile scrolled a row off the top, its `y_off` pulling it down by 12 px: only the
        // last 4 px of the crop are in view — cut to the pane AND to the crop at once.
        let scrolled = placement_rect(m, -1, 0, 2, 2, 0, 12);
        assert_eq!(
            placement_clip(scrolled, middle, m, pane),
            Some(rect(0.0, 0.0, 16.0, 4.0)),
            "pane and crop together"
        );

        // With no `src` the crop is the whole copy, so this can cut nothing: the box stands as is.
        let whole = crop_for_src(None, (64, 32), (64, 32));
        assert_eq!(whole.w, 64);
        assert_eq!(
            placement_clip(tall, whole, m, pane),
            clip_rect(tall, pane),
            "an uncropped placement is unchanged"
        );
    }

    /// The protocol defines each placeholder cell as "tile (row, col) of the image AT THIS CELL", so
    /// a picture printed twice on one screen — the same `(id, pid)` in two places, which is how a TUI
    /// redraws it and how scrollback keeps an earlier copy — draws at both. Each cell computes its
    /// picture from its own position, and cells of different copies never merge into one run.
    #[test]
    fn one_image_printed_twice_draws_at_both_places() {
        let store = ImageStore::with_caps(64 * KB, 128 * KB);
        let mut term = Terminal::new(
            crate::GridSize {
                cols: 20,
                rows: 6,
                cell_width: 8.0,
                cell_height: 16.0,
                scale: 1.0,
            },
            100,
        );
        // A 2x2 RGBA picture transmitted as id 7 (16 bytes of opaque white), then displayed as a
        // virtual placement (`U=1`) covering 2 cells by 2. Its placeholder cells are what a program
        // prints; the emulator only records the id and span.
        term.feed(b"\x1b_Ga=t,f=32,s=2,v=2,i=7;/////////////////////w==\x1b\\");
        let mut key = None;
        for event in term.take_events() {
            if let crate::TermEvent::ImageReceived { key: k, data } = event {
                assert!(matches!(
                    data,
                    ImageData::Rgba {
                        width: 2,
                        height: 2,
                        ..
                    }
                ));
                store.submit(term.image_owner(), k, data);
                key = Some(k);
            }
        }
        let key = key.expect("the transmission reaches the store");
        term.feed(b"\x1b_Ga=p,i=7,U=1,c=2,r=2\x1b\\");
        term.take_events(); // the OK reply: not what this is about

        // The same two tiles printed on row 0 and again on row 2, colours naming image id 7.
        let print = |term: &mut Terminal, row: u16| {
            term.feed(format!("\x1b[{};1H\x1b[0m\x1b[38;2;0;0;7m", row + 1).as_bytes());
            for col in 0..2u32 {
                term.feed(
                    format!("{}{}{}", PLACEHOLDER_CHAR, mark(0), mark(col as usize)).as_bytes(),
                );
            }
        };
        print(&mut term, 0);
        print(&mut term, 2);

        // The span's copy is queued by the first pass and ready for the second.
        let draw = |term: &Terminal| {
            let mut layer = ImageLayer::default();
            let mut vt = VirtualTiles::new(&store, term, geometry(1.0)).expect("images are on");
            let grid = term.term().grid();
            let rows = usize::from(term.size().rows);
            let cols = usize::from(term.size().cols);
            let mut reader = RowReader::new();
            for y in 0..rows {
                vt.begin_row();
                let line = Line(y as i32 - grid.display_offset() as i32);
                let cells = reader.row(grid, line);
                for x in 0..cols {
                    vt.visit(y as u16, x as u16, &cells[Column(x)]);
                }
                vt.end_row();
            }
            vt.finish(&mut layer);
            layer
        };
        let first = draw(&term);
        assert!(first.pending, "the 2x2 copy is not made yet");
        store.run_jobs_until_idle();
        let layer = draw(&term);

        // A 2x2 picture in a 16x32 px span fits to 16x16 and centres: rows 8..24 of the first copy's
        // cells, rows 40..56 of the second's. Two draws, one per copy — not one and a blank.
        assert_eq!(layer.under_text.len(), 2, "both copies draw");
        assert_eq!(layer.under_text[0].bounds, rect(0.0, 8.0, 16.0, 16.0));
        assert_eq!(layer.under_text[0].clip, rect(0.0, 8.0, 16.0, 8.0));
        assert_eq!(layer.under_text[1].bounds, rect(0.0, 40.0, 16.0, 16.0));
        assert_eq!(
            layer.under_text[1].clip,
            rect(0.0, 40.0, 16.0, 8.0),
            "the second copy's clip covers its own cells"
        );
        assert_eq!(layer.under_text[0].key, key);
        assert_eq!(layer.under_text[1].key, key);
    }

    /// A picture sent with no cell size is drawn at its own pixels (finding I1). An 800×600 sixel in
    /// 9×18 px cells covers 89×34 cells — a span of 801×612 px, because the span rounds up to whole
    /// cells — and it is drawn 800×600: the copy is the picture itself, so no filter runs and the
    /// store gains no second frame (finding P4). The remainder of the last cell on each axis stays
    /// empty.
    #[test]
    fn an_unsized_sixel_draws_at_its_own_pixels() {
        let source = 800 * 600 * 4; // 1.92 MB of RGBA8
        let store = ImageStore::with_caps(8 * 1024 * 1024, 16 * 1024 * 1024);
        let mut term = Terminal::new(
            crate::GridSize {
                cols: 90,
                rows: 36,
                cell_width: 9.0,
                cell_height: 18.0,
                scale: 1.0,
            },
            100,
        );
        term.set_cell_pixel_size(9, 18);
        // The raster attributes declare an 800×600 canvas; one red pixel is painted in it. A sixel
        // arrives decoded and unsized: its span comes from those pixels, not from the program.
        term.feed(b"\x1bPq\"1;1;800;600#0;2;100;0;0#0~\x1b\\");
        let mut key = None;
        for event in term.take_events() {
            if let crate::TermEvent::ImageReceived { key: k, data } = event {
                assert!(
                    matches!(
                        data,
                        ImageData::Rgba {
                            width: 800,
                            height: 600,
                            ..
                        }
                    ),
                    "the raster attributes set the canvas size"
                );
                store.submit(term.image_owner(), k, data);
                key = Some(k);
            }
        }
        assert!(key.is_some(), "the sixel reaches the store");
        let m = CellGeometry::new(Size::new(9.0, 18.0), 1.0);
        let scheme = ColorScheme::graphite();

        let first = resolve(&store, &term, m, &scheme);
        assert!(
            first.pending,
            "the copy is asked for, never made on the draw path"
        );
        store.run_jobs_until_idle();
        let layer = resolve(&store, &term, m, &scheme);
        let draws: Vec<&ImageDraw> = layer.under_text.iter().chain(&layer.over_text).collect();
        assert_eq!(draws.len(), 1, "the picture draws once");
        assert_eq!(
            draws[0].bounds,
            rect(0.0, 0.0, 800.0, 600.0),
            "its own pixels, unscaled: not the 801×612 its cells round up to"
        );
        assert_eq!(
            draws[0].clip,
            rect(0.0, 0.0, 800.0, 600.0),
            "the clip is cut to what the copy covers, so the span's last partial cell on each axis \
             stays empty"
        );
        assert_eq!(
            store.owner_bytes(term.image_owner()),
            source,
            "no second frame: the 1:1 copy is the source's own buffer"
        );
    }

    /// The same rule at the size a sixel of a small picture makes (finding I1): a 4×12 px red band in
    /// 8×16 px cells covers one cell, and it draws 4×12 — not stretched to fill the 8×16 cell, which
    /// is what blurred it before.
    #[test]
    fn a_four_by_twelve_sixel_draws_at_its_own_pixels() {
        let store = ImageStore::with_caps(64 * KB, 128 * KB);
        let mut term = Terminal::new(
            crate::GridSize {
                cols: 20,
                rows: 6,
                cell_width: 8.0,
                cell_height: 16.0,
                scale: 1.0,
            },
            100,
        );
        term.set_cell_pixel_size(8, 16);
        // A red 4×12 px sixel: four painted columns, then a second band of four.
        term.feed(b"\x1bPq#0;2;100;0;0#0~~~~-~~~~\x1b\\");
        for event in term.take_events() {
            if let crate::TermEvent::ImageReceived { key, data } = event {
                store.submit(term.image_owner(), key, data);
            }
        }
        let m = geometry(1.0);
        let scheme = ColorScheme::graphite();
        assert!(
            resolve(&store, &term, m, &scheme).pending,
            "the 4×12 copy is asked for first"
        );
        store.run_jobs_until_idle();
        let layer = resolve(&store, &term, m, &scheme);
        let draws: Vec<&ImageDraw> = layer.under_text.iter().chain(&layer.over_text).collect();
        assert_eq!(draws.len(), 1);
        assert_eq!(
            draws[0].bounds,
            rect(0.0, 0.0, 4.0, 12.0),
            "the picture's own pixels, not the 8×16 cell it rounds up to"
        );
        assert_eq!(
            draws[0].clip,
            rect(0.0, 0.0, 4.0, 12.0),
            "cut to what the picture covers: the rest of its cell stays empty"
        );
        assert_eq!(
            store.owner_bytes(term.image_owner()),
            4 * 12 * 4,
            "and no second frame for it"
        );
    }

    /// The other half of the rule (finding I1): a span the program sized itself still resamples. The
    /// same 4×12 picture asked for as 3 cells by 1 (24×16 px) is fitted to that box — 5×16, aspect
    /// kept — and that copy costs its own bytes, because it is another picture from the source.
    #[test]
    fn a_program_sized_span_still_resamples() {
        let store = ImageStore::with_caps(64 * KB, 128 * KB);
        let mut term = Terminal::new(
            crate::GridSize {
                cols: 20,
                rows: 6,
                cell_width: 8.0,
                cell_height: 16.0,
                scale: 1.0,
            },
            100,
        );
        term.set_cell_pixel_size(8, 16);
        // A 4×12 red picture transmitted as id 7 (192 bytes of opaque red RGBA: sixteen copies of the
        // 16-character base64 block this pixel pattern makes every 12 bytes), not displayed yet.
        let pixels = "/wAA//8AAP//AAD/".repeat(16);
        term.feed(format!("\x1b_Ga=t,f=32,s=4,v=12,i=7;{pixels}\x1b\\").as_bytes());
        let mut key = None;
        for event in term.take_events() {
            if let crate::TermEvent::ImageReceived { key: k, data } = event {
                store.submit(term.image_owner(), k, data);
                key = Some(k);
            }
        }
        assert!(key.is_some(), "the picture reaches the store");
        // Two placements of it: one sized by its pixels (no `c`/`r`), one sized by the program as 3
        // cells by 1, on row 2.
        term.feed(b"\x1b[1;1H\x1b_Ga=p,i=7;\x1b\\");
        term.feed(b"\x1b[3;1H\x1b_Ga=p,i=7,c=3,r=1;\x1b\\");
        term.take_events(); // the OK replies: not what this is about

        let m = geometry(1.0);
        let scheme = ColorScheme::graphite();
        assert!(resolve(&store, &term, m, &scheme).pending);
        store.run_jobs_until_idle();
        let layer = resolve(&store, &term, m, &scheme);
        let draws: Vec<&ImageDraw> = layer.under_text.iter().chain(&layer.over_text).collect();
        assert_eq!(draws.len(), 2, "both placements draw");
        let own = draws
            .iter()
            .find(|d| d.bounds.width == 4.0)
            .expect("the unsized placement draws at its own pixels");
        assert_eq!(own.bounds, rect(0.0, 0.0, 4.0, 12.0));
        let fitted = draws
            .iter()
            .find(|d| d.bounds.width == 5.0)
            .expect("the program-sized span gets a fitted copy");
        assert_eq!(
            fitted.bounds,
            rect(0.0, 32.0, 5.0, 16.0),
            "4×12 aspect-fitted into 24×16 px: 5 wide by 16 tall"
        );
        assert_eq!(
            store.owner_bytes(term.image_owner()),
            4 * 12 * 4 + 5 * 16 * 4,
            "the source frame plus the fitted copy: only the 1:1 alias was free"
        );
    }

    /// A `U+10EEEE` cell whose tile cannot be resolved still does not go to text shaping: it is a
    /// private-use character, so shaping it draws a missing-glyph box where the picture should be.
    #[test]
    fn an_unresolvable_placeholder_cell_is_not_shaped() {
        let store = ImageStore::with_caps(64 * KB, 128 * KB);
        let term = Terminal::new(
            crate::GridSize {
                cols: 20,
                rows: 6,
                cell_width: 8.0,
                cell_height: 16.0,
                scale: 1.0,
            },
            100,
        );
        let mut vt = VirtualTiles::new(&store, &term, geometry(1.0)).expect("images are on");
        // An id nothing refers to: no tile to draw, and the glyph stays skipped. (This case passed
        // before finding 7 was fixed — it resolves to a tile; what the fix is about is the three
        // below, where `placeholder_tile` itself says no.)
        assert!(
            vt.visit(0, 0, &placeholder(99, &[mark(0), mark(0)])),
            "an unresolved placeholder skips its glyph too"
        );
        // A row's first cell with no diacritics: nothing to continue from the left.
        vt.begin_row();
        assert!(
            vt.visit(1, 0, &placeholder(7, &[])),
            "a 0-mark cell at a row start skips its glyph too"
        );
        // A named-colour foreground encodes no image id: unresolvable by definition.
        vt.begin_row();
        let named = Cell {
            c: PLACEHOLDER_CHAR,
            fg: AnsiColor::Named(feathercritty::vte::ansi::NamedColor::Red),
            ..Cell::default()
        };
        assert!(
            vt.visit(2, 0, &named),
            "a named-colour placeholder skips its glyph too"
        );
        // A broken colour chain: a 1-mark cell whose colours differ from the cell on its left has
        // nothing to continue, and it must leave `prev` reset so the 0-mark cell after it does not
        // inherit either. (The reset itself is asserted on the pure decoder, next.)
        let left = placeholder(7, &[mark(0), mark(0)]);
        let prev_tile = placeholder_tile(&left, None).expect("the left cell decodes");
        assert!(
            placeholder_tile(&placeholder(8, &[mark(0)]), Some(prev_tile)).is_none(),
            "different colours break the chain"
        );
        vt.begin_row();
        assert!(vt.visit(3, 0, &left));
        assert!(
            vt.visit(3, 1, &placeholder(8, &[mark(0)])),
            "a broken-chain cell skips its glyph too"
        );
        assert!(
            vt.visit(3, 2, &placeholder(8, &[])),
            "and so does the 0-mark cell after it: nothing was inherited"
        );
        // A cell that is not a placeholder at all keeps going to text.
        let plain = Cell {
            c: 'x',
            ..Cell::default()
        };
        assert!(!vt.visit(0, 1, &plain));
        let mut layer = ImageLayer::default();
        vt.finish(&mut layer);
        assert!(
            layer.under_text.is_empty(),
            "an id nothing refers to draws no tile"
        );
        assert!(!layer.pending, "and it waits for nothing");
    }

    /// KiB, for store caps in these tests.
    const KB: usize = 1024;

    #[test]
    fn placeholder_cells_decode_their_tile() {
        // A cell stating row 1 and column 0 for image 3.
        let c = placeholder(3, &[mark(1), mark(0)]);
        assert_eq!(
            placeholder_tile(&c, None),
            Some(Placeholder {
                id: 3,
                pid: None,
                row: 1,
                col: 0
            })
        );
        // A third diacritic adds the most significant id byte.
        let c = placeholder(3, &[mark(0), mark(2), mark(1)]);
        assert_eq!(
            placeholder_tile(&c, None).map(|p| (p.id, p.row, p.col)),
            Some((0x0100_0003, 0, 2))
        );
        // Not a placeholder cell at all.
        let plain = Cell {
            c: 'x',
            ..Cell::default()
        };
        assert_eq!(placeholder_tile(&plain, None), None);
    }

    #[test]
    fn placeholder_cells_inherit_from_the_left() {
        // The protocol's example: only the first cell of a row states its position.
        let first = placeholder(42, &[mark(1), mark(0)]);
        let prev = placeholder_tile(&first, None).expect("the first cell decodes");
        assert_eq!((prev.row, prev.col), (1, 0));

        // No diacritics at all: row from the left, column one further.
        let next = placeholder(42, &[]);
        let got = placeholder_tile(&next, Some(prev)).expect("inherits");
        assert_eq!((got.id, got.row, got.col), (42, 1, 1));

        // Only the row mark: the column still continues from the left.
        let marked = placeholder(42, &[mark(1)]);
        let got = placeholder_tile(&marked, Some(prev)).expect("inherits the column");
        assert_eq!((got.row, got.col), (1, 1));

        // A different row than the left cell's: nothing to inherit.
        let other_row = placeholder(42, &[mark(2)]);
        assert_eq!(placeholder_tile(&other_row, Some(prev)), None);

        // Different colours break the chain, even with no marks of its own.
        let other_id = placeholder(43, &[]);
        assert_eq!(placeholder_tile(&other_id, Some(prev)), None);

        // Stating row and column but not the high byte: it comes from the predecessor.
        let wide_first = placeholder(0x0100_0042, &[mark(0), mark(0), mark(1)]);
        let wide_prev = placeholder_tile(&wide_first, None).expect("three marks");
        assert_eq!(wide_prev.id, 0x0100_0042);
        let wide_next = placeholder(0x42, &[mark(0), mark(1)]);
        let got = placeholder_tile(&wide_next, Some(wide_prev)).expect("inherits the high byte");
        assert_eq!((got.id, got.row, got.col), (0x0100_0042, 0, 1));

        // ...but only for the immediate predecessor in the same row.
        let gap = placeholder(0x42, &[mark(0), mark(5)]);
        assert_eq!(
            placeholder_tile(&gap, Some(wide_prev)).map(|p| p.id),
            Some(0x42),
            "no inheritance across a gap"
        );

        // A cell naming its own placement id through the underline colour keeps it.
        let mut underlined = placeholder(7, &[mark(0), mark(0)]);
        underlined.set_underline_color(Some(AnsiColor::Spec(AnsiRgb { r: 0, g: 0, b: 9 })));
        assert_eq!(
            placeholder_tile(&underlined, None).map(|p| (p.id, p.pid)),
            Some((7, Some(9)))
        );
    }

    #[test]
    fn placeholder_text_is_the_exact_format_and_never_written_into_the_grid() {
        let run = placeholder_run(
            2,
            3,
            "[image 64\u{d7}48]".to_string(),
            Rgb { r: 1, g: 2, b: 3 },
        );
        assert_eq!((run.span.row, run.span.col), (2, 3));
        assert_eq!(run.span.cells, 13, "one cell per character");
        assert_eq!(run.text, "[image 64\u{d7}48]");
        assert!(!run.bold && !run.italic && run.alone);
        assert_eq!(run.fg, Rgb { r: 1, g: 2, b: 3 });
    }
}
