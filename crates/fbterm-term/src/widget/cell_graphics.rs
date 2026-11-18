//! Block and box-drawing characters drawn as shapes that fill their cells, not as font glyphs.
//!
//! A font cuts these characters to its own design, and that design is not the terminal's cell:
//! Menlo's full block is about 0.87 of a line tall, so a bar built from `█` shows a seam at every
//! row, and its lines sit wherever the font put them, so a frame built from `─` and `│` shows a gap
//! or a jog wherever two glyphs fail to meet. Drawing them here instead fixes both, because every
//! edge such a shape has is the edge of a cell, computed for the whole grid from one origin and one
//! cell size: neighbours agree on that edge to the pixel, so shapes meet with no seam and no
//! overlap, at any scale and any (fractional) cell size.
//!
//! Two rules keep the arithmetic exact:
//!
//! - A cell boundary is where iced's quad shader puts the left edge of that column's background:
//!   `round((origin + column × cell_width) × scale + 0.001)`, in the shader's f32 order and with its
//!   slack — never a sum of rounded cell widths. Cell `n`'s right boundary is by construction cell
//!   `n + 1`'s left one, so neighbours agree however the rounding falls out. That background quad's
//!   own right edge scales the width and adds it instead, a different f32 order: it agrees with this
//!   except at extreme coordinates (a simulation finds a pixel of disagreement only past some 9400
//!   physical pixels at scale 3).
//! - Everything inside a cell is a fraction of that cell measured between two of its edges and
//!   rounded once, and a line's thickness is a whole number of physical pixels. Lines are centred
//!   on their cell's centre line and reach the cell edges, so joints land on the shared edge.
//!
//! Quads are handed back to iced in logical pixels as `physical ÷ scale`. That division cannot always
//! be exact, so the shader re-derives the pixel by the rule above — which is why [`Geometry::px`]
//! follows that rule rather than rounding the physical position itself: a shape starts where its own
//! cell's background starts and ends where the next cell's background starts.
//!
//! Rounded corners (U+256D..=U+2570) and diagonals (U+2571..=U+2573) stay with the font: they are
//! curves and slants, and a quad cannot stand in for one.

use iced::{Rectangle, Size};

/// How many parts a cell is divided into wherever [`Shape`] stores a fraction: enough for eighths
/// (blocks), quarters, thirds and halves (line dashes) to all be whole numbers of parts.
const PARTS: u32 = 48;
const PARTS_U8: u8 = PARTS as u8;

/// The thickness of one arm of a line character, in light-line thicknesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    /// One line.
    Light,
    /// Two lines wide.
    Heavy,
    /// Two light lines with a light-width gap between them, as the double box lines are drawn.
    Double,
}

impl Weight {
    /// How many light thicknesses this covers, gap included.
    const fn bands(self) -> i64 {
        match self {
            Weight::Light => 1,
            Weight::Heavy => 2,
            Weight::Double => 3,
        }
    }
}

/// The dash pattern along a line's axis. `Solid` is one unbroken run; the rest repeat across the
/// cell so that a row of them keeps the same rhythm at the cell boundaries, which the font's own
/// glyphs do not (their periods are chosen in font units, not cells).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dash {
    Solid,
    Double,
    Triple,
    Quad,
}

impl Dash {
    /// The length of one dash-and-gap period, in parts.
    const fn period(self) -> u32 {
        match self {
            Dash::Solid => PARTS,
            Dash::Double => 24,
            Dash::Triple => 16,
            Dash::Quad => 12,
        }
    }

    /// How much of a period is inked, as fractions that match the reference monospace's ratios:
    /// 7/8 for double dashes, 5/8 for triple, and just over half for quadruple. [`Geometry`] rounds
    /// each dash to whole pixels and keeps at least one pixel for the gap.
    const fn on(self) -> u32 {
        match self {
            Dash::Solid => PARTS,
            Dash::Double => 21,
            Dash::Triple => 10,
            Dash::Quad => 7,
        }
    }
}

/// One quad-covered piece of a covered character, in cell-relative terms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    /// Padding: [`Entry`] holds room for two shapes and most characters need one.
    Empty,
    /// A box from (`x0`, `y0`) to (`x1`, `y1`) in parts of the cell, filled at `alpha` of the
    /// cell's foreground. Blocks use 1.0; the shades use a quarter, half and three quarters.
    Fill {
        x0: u8,
        y0: u8,
        x1: u8,
        y1: u8,
        alpha: f32,
    },
    /// Line arms out of the cell's centre, in the order up, down, left, right. A missing arm stops
    /// at the centre line; arms that meet do so inside the crossing region, so the joint is solid.
    Lines {
        up: Option<Weight>,
        down: Option<Weight>,
        left: Option<Weight>,
        right: Option<Weight>,
        dash: Dash,
    },
}

impl Shape {
    /// Whether this shape covers a cell from its left edge to its right edge, which is what lets one
    /// quad stand for a whole merged run of the character. Dashes repeat per cell instead.
    pub fn full_width(&self) -> bool {
        match *self {
            Shape::Empty => false,
            Shape::Fill { x0, x1, .. } => x0 == 0 && x1 == PARTS_U8,
            Shape::Lines {
                ref up,
                ref down,
                ref left,
                ref right,
                dash,
            } => {
                dash == Dash::Solid
                    && up.is_none()
                    && down.is_none()
                    && left.is_some()
                    && left == right
            }
        }
    }

    /// The fill's opacity of the cell's foreground; 1.0 for lines, which are always opaque.
    pub fn alpha(&self) -> f32 {
        match self {
            Shape::Empty => 0.0,
            Shape::Fill { alpha, .. } => *alpha,
            Shape::Lines { .. } => 1.0,
        }
    }
}

/// One character's entry in [`TABLE`]: up to two shapes, since a three-quadrant character is one
/// half of the cell plus the leftover quadrant.
#[derive(Debug, Clone, Copy)]
struct Entry {
    shapes: [Shape; 2],
    len: u8,
}

impl Entry {
    const NONE: Entry = Entry {
        shapes: [Shape::Empty; 2],
        len: 0,
    };

    const fn one(s: Shape) -> Entry {
        Entry {
            shapes: [s, Shape::Empty],
            len: 1,
        }
    }

    const fn two(a: Shape, b: Shape) -> Entry {
        Entry {
            shapes: [a, b],
            len: 2,
        }
    }
}

const fn fill(x0: u8, y0: u8, x1: u8, y1: u8) -> Shape {
    Shape::Fill {
        x0,
        y0,
        x1,
        y1,
        alpha: 1.0,
    }
}

const fn shade(alpha: f32) -> Shape {
    Shape::Fill {
        x0: 0,
        y0: 0,
        x1: PARTS_U8,
        y1: PARTS_U8,
        alpha,
    }
}

const fn lines(
    up: Option<Weight>,
    down: Option<Weight>,
    left: Option<Weight>,
    right: Option<Weight>,
) -> Shape {
    Shape::Lines {
        up,
        down,
        left,
        right,
        dash: Dash::Solid,
    }
}

const fn dashed(
    up: Option<Weight>,
    down: Option<Weight>,
    left: Option<Weight>,
    right: Option<Weight>,
    dash: Dash,
) -> Shape {
    Shape::Lines {
        up,
        down,
        left,
        right,
        dash,
    }
}

const FIRST: u32 = 0x2500;
const LAST: u32 = 0x259F;
const COUNT: usize = (LAST - FIRST + 1) as usize;

/// Every character this module draws, by code point. The arm weights and dash families are the
/// characters' own names read literally — `UP HEAVY AND RIGHT DOWN LIGHT` really is a heavy arm up,
/// a light one down and a light one right — which was checked against a real monospace's outlines
/// for all 117 line characters: they agree everywhere. Blocks and shades follow their names the same
/// way (`LOWER SEVEN EIGHTHS BLOCK` fills the bottom seven eighths of the cell).
static TABLE: [Entry; COUNT] = {
    let mut table = [Entry::NONE; COUNT];
    let mut i = 0;
    while i < COUNT {
        table[i] = entry(FIRST + i as u32);
        i += 1;
    }
    table
};

const fn entry(cp: u32) -> Entry {
    match cp {
        0x2500 => Entry::one(lines(None, None, Some(Weight::Light), Some(Weight::Light))), /* ─  arms:--LL */

        0x2501 => Entry::one(lines(None, None, Some(Weight::Heavy), Some(Weight::Heavy))), /* ━  arms:--HH */

        0x2502 => Entry::one(lines(Some(Weight::Light), Some(Weight::Light), None, None)), /* │  arms:LL-- */

        0x2503 => Entry::one(lines(Some(Weight::Heavy), Some(Weight::Heavy), None, None)), /* ┃  arms:HH-- */

        0x2504 => Entry::one(dashed(
            None,
            None,
            Some(Weight::Light),
            Some(Weight::Light),
            Dash::Triple,
        )), /* ┄  arms:--LL */

        0x2505 => Entry::one(dashed(
            None,
            None,
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            Dash::Triple,
        )), /* ┅  arms:--HH */

        0x2506 => Entry::one(dashed(
            Some(Weight::Light),
            Some(Weight::Light),
            None,
            None,
            Dash::Triple,
        )), /* ┆  arms:LL-- */

        0x2507 => Entry::one(dashed(
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            None,
            None,
            Dash::Triple,
        )), /* ┇  arms:HH-- */

        0x2508 => Entry::one(dashed(
            None,
            None,
            Some(Weight::Light),
            Some(Weight::Light),
            Dash::Quad,
        )), /* ┈  arms:--LL */

        0x2509 => Entry::one(dashed(
            None,
            None,
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            Dash::Quad,
        )), /* ┉  arms:--HH */

        0x250A => Entry::one(dashed(
            Some(Weight::Light),
            Some(Weight::Light),
            None,
            None,
            Dash::Quad,
        )), /* ┊  arms:LL-- */

        0x250B => Entry::one(dashed(
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            None,
            None,
            Dash::Quad,
        )), /* ┋  arms:HH-- */

        0x250C => Entry::one(lines(None, Some(Weight::Light), None, Some(Weight::Light))), /* ┌  arms:-L-L */

        0x250D => Entry::one(lines(None, Some(Weight::Light), None, Some(Weight::Heavy))), /* ┍  arms:-L-H */

        0x250E => Entry::one(lines(None, Some(Weight::Heavy), None, Some(Weight::Light))), /* ┎  arms:-H-L */

        0x250F => Entry::one(lines(None, Some(Weight::Heavy), None, Some(Weight::Heavy))), /* ┏  arms:-H-H */

        0x2510 => Entry::one(lines(None, Some(Weight::Light), Some(Weight::Light), None)), /* ┐  arms:-LL- */

        0x2511 => Entry::one(lines(None, Some(Weight::Light), Some(Weight::Heavy), None)), /* ┑  arms:-LH- */

        0x2512 => Entry::one(lines(None, Some(Weight::Heavy), Some(Weight::Light), None)), /* ┒  arms:-HL- */

        0x2513 => Entry::one(lines(None, Some(Weight::Heavy), Some(Weight::Heavy), None)), /* ┓  arms:-HH- */

        0x2514 => Entry::one(lines(Some(Weight::Light), None, None, Some(Weight::Light))), /* └  arms:L--L */

        0x2515 => Entry::one(lines(Some(Weight::Light), None, None, Some(Weight::Heavy))), /* ┕  arms:L--H */

        0x2516 => Entry::one(lines(Some(Weight::Heavy), None, None, Some(Weight::Light))), /* ┖  arms:H--L */

        0x2517 => Entry::one(lines(Some(Weight::Heavy), None, None, Some(Weight::Heavy))), /* ┗  arms:H--H */

        0x2518 => Entry::one(lines(Some(Weight::Light), None, Some(Weight::Light), None)), /* ┘  arms:L-L- */

        0x2519 => Entry::one(lines(Some(Weight::Light), None, Some(Weight::Heavy), None)), /* ┙  arms:L-H- */

        0x251A => Entry::one(lines(Some(Weight::Heavy), None, Some(Weight::Light), None)), /* ┚  arms:H-L- */

        0x251B => Entry::one(lines(Some(Weight::Heavy), None, Some(Weight::Heavy), None)), /* ┛  arms:H-H- */

        0x251C => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Light),
            None,
            Some(Weight::Light),
        )), /* ├  arms:LL-L */

        0x251D => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Light),
            None,
            Some(Weight::Heavy),
        )), /* ┝  arms:LL-H */

        0x251E => Entry::one(lines(
            Some(Weight::Heavy),
            Some(Weight::Light),
            None,
            Some(Weight::Light),
        )), /* ┞  arms:HL-L */

        0x251F => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Heavy),
            None,
            Some(Weight::Light),
        )), /* ┟  arms:LH-L */

        0x2520 => Entry::one(lines(
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            None,
            Some(Weight::Light),
        )), /* ┠  arms:HH-L */

        0x2521 => Entry::one(lines(
            Some(Weight::Heavy),
            Some(Weight::Light),
            None,
            Some(Weight::Heavy),
        )), /* ┡  arms:HL-H */

        0x2522 => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Heavy),
            None,
            Some(Weight::Heavy),
        )), /* ┢  arms:LH-H */

        0x2523 => Entry::one(lines(
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            None,
            Some(Weight::Heavy),
        )), /* ┣  arms:HH-H */

        0x2524 => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Light),
            Some(Weight::Light),
            None,
        )), /* ┤  arms:LLL- */

        0x2525 => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Light),
            Some(Weight::Heavy),
            None,
        )), /* ┥  arms:LLH- */

        0x2526 => Entry::one(lines(
            Some(Weight::Heavy),
            Some(Weight::Light),
            Some(Weight::Light),
            None,
        )), /* ┦  arms:HLL- */

        0x2527 => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Heavy),
            Some(Weight::Light),
            None,
        )), /* ┧  arms:LHL- */

        0x2528 => Entry::one(lines(
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            Some(Weight::Light),
            None,
        )), /* ┨  arms:HHL- */

        0x2529 => Entry::one(lines(
            Some(Weight::Heavy),
            Some(Weight::Light),
            Some(Weight::Heavy),
            None,
        )), /* ┩  arms:HLH- */

        0x252A => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            None,
        )), /* ┪  arms:LHH- */

        0x252B => Entry::one(lines(
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            None,
        )), /* ┫  arms:HHH- */

        0x252C => Entry::one(lines(
            None,
            Some(Weight::Light),
            Some(Weight::Light),
            Some(Weight::Light),
        )), /* ┬  arms:-LLL */

        0x252D => Entry::one(lines(
            None,
            Some(Weight::Light),
            Some(Weight::Heavy),
            Some(Weight::Light),
        )), /* ┭  arms:-LHL */

        0x252E => Entry::one(lines(
            None,
            Some(Weight::Light),
            Some(Weight::Light),
            Some(Weight::Heavy),
        )), /* ┮  arms:-LLH */

        0x252F => Entry::one(lines(
            None,
            Some(Weight::Light),
            Some(Weight::Heavy),
            Some(Weight::Heavy),
        )), /* ┯  arms:-LHH */

        0x2530 => Entry::one(lines(
            None,
            Some(Weight::Heavy),
            Some(Weight::Light),
            Some(Weight::Light),
        )), /* ┰  arms:-HLL */

        0x2531 => Entry::one(lines(
            None,
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            Some(Weight::Light),
        )), /* ┱  arms:-HHL */

        0x2532 => Entry::one(lines(
            None,
            Some(Weight::Heavy),
            Some(Weight::Light),
            Some(Weight::Heavy),
        )), /* ┲  arms:-HLH */

        0x2533 => Entry::one(lines(
            None,
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            Some(Weight::Heavy),
        )), /* ┳  arms:-HHH */

        0x2534 => Entry::one(lines(
            Some(Weight::Light),
            None,
            Some(Weight::Light),
            Some(Weight::Light),
        )), /* ┴  arms:L-LL */

        0x2535 => Entry::one(lines(
            Some(Weight::Light),
            None,
            Some(Weight::Heavy),
            Some(Weight::Light),
        )), /* ┵  arms:L-HL */

        0x2536 => Entry::one(lines(
            Some(Weight::Light),
            None,
            Some(Weight::Light),
            Some(Weight::Heavy),
        )), /* ┶  arms:L-LH */

        0x2537 => Entry::one(lines(
            Some(Weight::Light),
            None,
            Some(Weight::Heavy),
            Some(Weight::Heavy),
        )), /* ┷  arms:L-HH */

        0x2538 => Entry::one(lines(
            Some(Weight::Heavy),
            None,
            Some(Weight::Light),
            Some(Weight::Light),
        )), /* ┸  arms:H-LL */

        0x2539 => Entry::one(lines(
            Some(Weight::Heavy),
            None,
            Some(Weight::Heavy),
            Some(Weight::Light),
        )), /* ┹  arms:H-HL */

        0x253A => Entry::one(lines(
            Some(Weight::Heavy),
            None,
            Some(Weight::Light),
            Some(Weight::Heavy),
        )), /* ┺  arms:H-LH */

        0x253B => Entry::one(lines(
            Some(Weight::Heavy),
            None,
            Some(Weight::Heavy),
            Some(Weight::Heavy),
        )), /* ┻  arms:H-HH */

        0x253C => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Light),
            Some(Weight::Light),
            Some(Weight::Light),
        )), /* ┼  arms:LLLL */

        0x253D => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Light),
            Some(Weight::Heavy),
            Some(Weight::Light),
        )), /* ┽  arms:LLHL */

        0x253E => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Light),
            Some(Weight::Light),
            Some(Weight::Heavy),
        )), /* ┾  arms:LLLH */

        0x253F => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Light),
            Some(Weight::Heavy),
            Some(Weight::Heavy),
        )), /* ┿  arms:LLHH */

        0x2540 => Entry::one(lines(
            Some(Weight::Heavy),
            Some(Weight::Light),
            Some(Weight::Light),
            Some(Weight::Light),
        )), /* ╀  arms:HLLL */

        0x2541 => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Heavy),
            Some(Weight::Light),
            Some(Weight::Light),
        )), /* ╁  arms:LHLL */

        0x2542 => Entry::one(lines(
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            Some(Weight::Light),
            Some(Weight::Light),
        )), /* ╂  arms:HHLL */

        0x2543 => Entry::one(lines(
            Some(Weight::Heavy),
            Some(Weight::Light),
            Some(Weight::Heavy),
            Some(Weight::Light),
        )), /* ╃  arms:HLHL */

        0x2544 => Entry::one(lines(
            Some(Weight::Heavy),
            Some(Weight::Light),
            Some(Weight::Light),
            Some(Weight::Heavy),
        )), /* ╄  arms:HLLH */

        0x2545 => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            Some(Weight::Light),
        )), /* ╅  arms:LHHL */

        0x2546 => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Heavy),
            Some(Weight::Light),
            Some(Weight::Heavy),
        )), /* ╆  arms:LHLH */

        0x2547 => Entry::one(lines(
            Some(Weight::Heavy),
            Some(Weight::Light),
            Some(Weight::Heavy),
            Some(Weight::Heavy),
        )), /* ╇  arms:HLHH */

        0x2548 => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            Some(Weight::Heavy),
        )), /* ╈  arms:LHHH */

        0x2549 => Entry::one(lines(
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            Some(Weight::Light),
        )), /* ╉  arms:HHHL */

        0x254A => Entry::one(lines(
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            Some(Weight::Light),
            Some(Weight::Heavy),
        )), /* ╊  arms:HHLH */

        0x254B => Entry::one(lines(
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            Some(Weight::Heavy),
        )), /* ╋  arms:HHHH */

        0x254C => Entry::one(dashed(
            None,
            None,
            Some(Weight::Light),
            Some(Weight::Light),
            Dash::Double,
        )), /* ╌  arms:--LL */

        0x254D => Entry::one(dashed(
            None,
            None,
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            Dash::Double,
        )), /* ╍  arms:--HH */

        0x254E => Entry::one(dashed(
            Some(Weight::Light),
            Some(Weight::Light),
            None,
            None,
            Dash::Double,
        )), /* ╎  arms:LL-- */

        0x254F => Entry::one(dashed(
            Some(Weight::Heavy),
            Some(Weight::Heavy),
            None,
            None,
            Dash::Double,
        )), /* ╏  arms:HH-- */

        0x2550 => Entry::one(lines(
            None,
            None,
            Some(Weight::Double),
            Some(Weight::Double),
        )), /* ═  arms:--DD */

        0x2551 => Entry::one(lines(
            Some(Weight::Double),
            Some(Weight::Double),
            None,
            None,
        )), /* ║  arms:DD-- */

        0x2552 => Entry::one(lines(None, Some(Weight::Light), None, Some(Weight::Double))), /* ╒  arms:-L-D */

        0x2553 => Entry::one(lines(None, Some(Weight::Double), None, Some(Weight::Light))), /* ╓  arms:-D-L */

        0x2554 => Entry::one(lines(
            None,
            Some(Weight::Double),
            None,
            Some(Weight::Double),
        )), /* ╔  arms:-D-D */

        0x2555 => Entry::one(lines(None, Some(Weight::Light), Some(Weight::Double), None)), /* ╕  arms:-LD- */

        0x2556 => Entry::one(lines(None, Some(Weight::Double), Some(Weight::Light), None)), /* ╖  arms:-DL- */

        0x2557 => Entry::one(lines(
            None,
            Some(Weight::Double),
            Some(Weight::Double),
            None,
        )), /* ╗  arms:-DD- */

        0x2558 => Entry::one(lines(Some(Weight::Light), None, None, Some(Weight::Double))), /* ╘  arms:L--D */

        0x2559 => Entry::one(lines(Some(Weight::Double), None, None, Some(Weight::Light))), /* ╙  arms:D--L */

        0x255A => Entry::one(lines(
            Some(Weight::Double),
            None,
            None,
            Some(Weight::Double),
        )), /* ╚  arms:D--D */

        0x255B => Entry::one(lines(Some(Weight::Light), None, Some(Weight::Double), None)), /* ╛  arms:L-D- */

        0x255C => Entry::one(lines(Some(Weight::Double), None, Some(Weight::Light), None)), /* ╜  arms:D-L- */

        0x255D => Entry::one(lines(
            Some(Weight::Double),
            None,
            Some(Weight::Double),
            None,
        )), /* ╝  arms:D-D- */

        0x255E => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Light),
            None,
            Some(Weight::Double),
        )), /* ╞  arms:LL-D */

        0x255F => Entry::one(lines(
            Some(Weight::Double),
            Some(Weight::Double),
            None,
            Some(Weight::Light),
        )), /* ╟  arms:DD-L */

        0x2560 => Entry::one(lines(
            Some(Weight::Double),
            Some(Weight::Double),
            None,
            Some(Weight::Double),
        )), /* ╠  arms:DD-D */

        0x2561 => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Light),
            Some(Weight::Double),
            None,
        )), /* ╡  arms:LLD- */

        0x2562 => Entry::one(lines(
            Some(Weight::Double),
            Some(Weight::Double),
            Some(Weight::Light),
            None,
        )), /* ╢  arms:DDL- */

        0x2563 => Entry::one(lines(
            Some(Weight::Double),
            Some(Weight::Double),
            Some(Weight::Double),
            None,
        )), /* ╣  arms:DDD- */

        0x2564 => Entry::one(lines(
            None,
            Some(Weight::Light),
            Some(Weight::Double),
            Some(Weight::Double),
        )), /* ╤  arms:-LDD */

        0x2565 => Entry::one(lines(
            None,
            Some(Weight::Double),
            Some(Weight::Light),
            Some(Weight::Light),
        )), /* ╥  arms:-DLL */

        0x2566 => Entry::one(lines(
            None,
            Some(Weight::Double),
            Some(Weight::Double),
            Some(Weight::Double),
        )), /* ╦  arms:-DDD */

        0x2567 => Entry::one(lines(
            Some(Weight::Light),
            None,
            Some(Weight::Double),
            Some(Weight::Double),
        )), /* ╧  arms:L-DD */

        0x2568 => Entry::one(lines(
            Some(Weight::Double),
            None,
            Some(Weight::Light),
            Some(Weight::Light),
        )), /* ╨  arms:D-LL */

        0x2569 => Entry::one(lines(
            Some(Weight::Double),
            None,
            Some(Weight::Double),
            Some(Weight::Double),
        )), /* ╩  arms:D-DD */

        0x256A => Entry::one(lines(
            Some(Weight::Light),
            Some(Weight::Light),
            Some(Weight::Double),
            Some(Weight::Double),
        )), /* ╪  arms:LLDD */

        0x256B => Entry::one(lines(
            Some(Weight::Double),
            Some(Weight::Double),
            Some(Weight::Light),
            Some(Weight::Light),
        )), /* ╫  arms:DDLL */

        0x256C => Entry::one(lines(
            Some(Weight::Double),
            Some(Weight::Double),
            Some(Weight::Double),
            Some(Weight::Double),
        )), /* ╬  arms:DDDD */

        0x256D => Entry::NONE, /* light arc down and right stays with the font */

        0x256E => Entry::NONE, /* light arc down and left stays with the font */

        0x256F => Entry::NONE, /* light arc up and left stays with the font */

        0x2570 => Entry::NONE, /* light arc up and right stays with the font */

        0x2571 => Entry::NONE, /* light diagonal upper right to lower left stays with the font */

        0x2572 => Entry::NONE, /* light diagonal upper left to lower right stays with the font */

        0x2573 => Entry::NONE, /* light diagonal cross stays with the font */

        0x2574 => Entry::one(lines(None, None, Some(Weight::Light), None)), /* ╴  arms:--L- */

        0x2575 => Entry::one(lines(Some(Weight::Light), None, None, None)), /* ╵  arms:L--- */

        0x2576 => Entry::one(lines(None, None, None, Some(Weight::Light))), /* ╶  arms:---L */

        0x2577 => Entry::one(lines(None, Some(Weight::Light), None, None)), /* ╷  arms:-L-- */

        0x2578 => Entry::one(lines(None, None, Some(Weight::Heavy), None)), /* ╸  arms:--H- */

        0x2579 => Entry::one(lines(Some(Weight::Heavy), None, None, None)), /* ╹  arms:H--- */

        0x257A => Entry::one(lines(None, None, None, Some(Weight::Heavy))), /* ╺  arms:---H */

        0x257B => Entry::one(lines(None, Some(Weight::Heavy), None, None)), /* ╻  arms:-H-- */

        0x257C => Entry::one(lines(None, None, Some(Weight::Light), Some(Weight::Heavy))), /* ╼  arms:--LH */

        0x257D => Entry::one(lines(Some(Weight::Light), Some(Weight::Heavy), None, None)), /* ╽  arms:LH-- */

        0x257E => Entry::one(lines(None, None, Some(Weight::Heavy), Some(Weight::Light))), /* ╾  arms:--HL */

        0x257F => Entry::one(lines(Some(Weight::Heavy), Some(Weight::Light), None, None)), /* ╿  arms:HL-- */

        0x2580 => Entry::one(fill(0, 0, 48, 24)), /* ▀ */

        0x2581 => Entry::one(fill(0, 42, 48, 48)), /* ▁ */

        0x2582 => Entry::one(fill(0, 36, 48, 48)), /* ▂ */

        0x2583 => Entry::one(fill(0, 30, 48, 48)), /* ▃ */

        0x2584 => Entry::one(fill(0, 24, 48, 48)), /* ▄ */

        0x2585 => Entry::one(fill(0, 18, 48, 48)), /* ▅ */

        0x2586 => Entry::one(fill(0, 12, 48, 48)), /* ▆ */

        0x2587 => Entry::one(fill(0, 6, 48, 48)), /* ▇ */

        0x2588 => Entry::one(fill(0, 0, 48, 48)), /* █ */

        0x2589 => Entry::one(fill(0, 0, 42, 48)), /* ▉ */

        0x258A => Entry::one(fill(0, 0, 36, 48)), /* ▊ */

        0x258B => Entry::one(fill(0, 0, 30, 48)), /* ▋ */

        0x258C => Entry::one(fill(0, 0, 24, 48)), /* ▌ */

        0x258D => Entry::one(fill(0, 0, 18, 48)), /* ▍ */

        0x258E => Entry::one(fill(0, 0, 12, 48)), /* ▎ */

        0x258F => Entry::one(fill(0, 0, 6, 48)), /* ▏ */

        0x2590 => Entry::one(fill(24, 0, 48, 48)), /* ▐ */

        0x2591 => Entry::one(shade(0.25)), /* ░ */

        0x2592 => Entry::one(shade(0.5)), /* ▒ */

        0x2593 => Entry::one(shade(0.75)), /* ▓ */

        0x2594 => Entry::one(fill(0, 0, 48, 6)), /* ▔ */

        0x2595 => Entry::one(fill(42, 0, 48, 48)), /* ▕ */

        0x2596 => Entry::one(fill(0, 24, 24, 48)), /* ▖ */

        0x2597 => Entry::one(fill(24, 24, 48, 48)), /* ▗ */

        0x2598 => Entry::one(fill(0, 0, 24, 24)), /* ▘ */

        0x2599 => Entry::two(fill(0, 24, 48, 48), fill(0, 0, 24, 24)), /* ▙ */

        0x259A => Entry::two(fill(0, 0, 24, 24), fill(24, 24, 48, 48)), /* ▚ */

        0x259B => Entry::two(fill(0, 0, 48, 24), fill(0, 24, 24, 48)), /* ▛ */

        0x259C => Entry::two(fill(0, 0, 48, 24), fill(24, 24, 48, 48)), /* ▜ */

        0x259D => Entry::one(fill(24, 0, 48, 24)), /* ▝ */

        0x259E => Entry::two(fill(0, 24, 24, 48), fill(24, 0, 48, 24)), /* ▞ */

        0x259F => Entry::two(fill(0, 24, 48, 48), fill(24, 0, 48, 24)), /* ▟ */

        _ => Entry::NONE,
    }
}

/// The shapes that draw `c`, or `None` for everything else (including the rounded corners and
/// diagonals above, which keep going through the font).
///
/// `#[inline]` because the draw list asks for every cell in the grid, and almost all of them are
/// plain text: inlined, that question is a bounds check on a register.
#[inline]
pub fn shapes(c: char) -> Option<&'static [Shape]> {
    let cp = c as u32;
    if !(FIRST..=LAST).contains(&cp) {
        return None;
    }
    let e = &TABLE[(cp - FIRST) as usize];
    (e.len > 0).then(|| &e.shapes[..e.len as usize])
}

/// Which end of an axis a position sits on: the low end (left along x, top along y) or the high one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Low,
    High,
}

/// One axis of a joint between lines: the rails it paints, and the side its single arm points to when
/// it has one. Rails are ordered along the axis they are separated by — the left rail of a vertical
/// line is `lo`, its top rail is `lo` — and a line that is not double has one band, which `hi`
/// repeats so a caller can walk both cases the same way.
struct Axis {
    /// Whether anything runs along this axis at all.
    present: bool,
    /// Whether the line is double, and so has two rails with a channel between them.
    double: bool,
    /// Whether arms run past the joint on both sides, which is what lets a line cross a joint instead
    /// of stopping at it.
    through: bool,
    lo: (i64, i64),
    hi: (i64, i64),
    /// The side the arm is on when the axis has one arm and not two (`None` for a through line, or an
    /// empty axis). Expressed along the axis the line travels along — which is the axis the
    /// perpendicular line's rails are ordered along, so the two can be compared.
    arm: Option<Side>,
}

/// Cell coordinates in, physical-pixel-accurate quads out.
///
/// Built once per frame from the widget's bounds, cell size and display scale, and allocation-free
/// while drawing: [`Geometry::cover`] walks a shape's pieces and hands each finished quad back.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    /// The grid's top-left corner and one cell, in logical pixels — the units iced is given, and
    /// therefore the units its shader multiplies by the scale.
    bx: f32,
    by: f32,
    cw: f32,
    ch: f32,
    scale: f32,
    /// A [`Weight::Light`] line's thickness in physical pixels: `Heavy` is twice this, `Double` two
    /// lines of it with a gap of it between.
    light: i64,
}

impl Geometry {
    /// Geometry for one grid: `bounds` is where its top-left cell starts in logical pixels, `cell`
    /// is one cell, and `scale` the window's physical pixels per logical one. A scale that is not a
    /// positive finite number becomes 1.0, as in [`super::images::CellGeometry`].
    pub fn new(bounds: Rectangle, cell: Size, scale: f32) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        // The light thickness follows the rule the underline and strike lines use — `cell height /
        // 16`, rounded, at least one pixel — but applies it in physical pixels, where those lines
        // apply it in logical ones: the same formula, and at an integer scale the same number, while
        // at a fractional one they can land a physical pixel apart. Shapes are measured in physical
        // pixels because that is the unit in which two cells agree on an edge.
        Self {
            bx: bounds.x,
            by: bounds.y,
            cw: cell.width,
            ch: cell.height,
            scale,
            light: (f64::from(cell.height) * f64::from(scale) / 16.0)
                .round()
                .max(1.0) as i64,
        }
    }

    /// The first physical pixel of a logical position, exactly as iced's quad shader computes the near
    /// edge of a box: `round(pos × scale + 0.001)`, f32 throughout (`solid.wgsl`; its far edge is
    /// `round(pos + size + 0.001)`, which scales the size and adds it instead). The background quads
    /// `render` draws go through that arithmetic, so matching it is what makes a shape start where its
    /// cell's background starts: at scale 1.25 on an 8.4-wide cell, column 1 is logical `8.4`, whose
    /// f32 form sits just under the true width — multiplied in f64 that is 10.4999995 and lands on 10,
    /// while the shader's f32 product is exactly 10.5 and its slack carries it to 11.
    fn px(&self, pos: f32) -> i64 {
        (pos * self.scale + 0.001).round() as i64
    }

    /// Cell boundary `col`, in physical pixels: the pixel iced's shader gives the left edge of that
    /// column's background. Never a sum of rounded cell widths, so neighbours agree on the edge by
    /// construction; the far boundary is `edge_x(col + 1)`, where the next cell's background starts.
    fn edge_x(&self, col: i64) -> i64 {
        self.px(self.bx + col as f32 * self.cw)
    }

    fn edge_y(&self, row: i64) -> i64 {
        self.px(self.by + row as f32 * self.ch)
    }

    /// The boundary at `part` parts into cell `col`, rounded once from that cell's two edges.
    fn part_x(&self, col: i64, part: u32) -> i64 {
        let (a, b) = (self.edge_x(col), self.edge_x(col + 1));
        a + snap((b - a) as f64 * f64::from(part) / f64::from(PARTS))
    }

    fn part_y(&self, row: i64, part: u32) -> i64 {
        let (a, b) = (self.edge_y(row), self.edge_y(row + 1));
        a + snap((b - a) as f64 * f64::from(part) / f64::from(PARTS))
    }

    /// The cell's centre line along x, in physical pixels and still unrounded: strokes are centred on
    /// it and their widths come from whole pixels, so the rounding happens once, at the stack's near
    /// edge. It scales the same logical position [`Geometry::px`] snaps, so it stays anchored to the
    /// cell the background quads are drawn over.
    fn axis_x(&self, col: i64) -> f64 {
        ((self.bx + (col as f32 + 0.5) * self.cw) * self.scale) as f64
    }

    fn axis_y(&self, row: i64) -> f64 {
        ((self.by + (row as f32 + 0.5) * self.ch) * self.scale) as f64
    }

    /// Where a stack of `bands` light thicknesses centred on `axis` starts.
    fn stack(&self, axis: f64, bands: i64) -> i64 {
        snap(axis - bands as f64 * self.light as f64 * 0.5)
    }

    /// The inked spans of a stroke of weight `w` centred on `axis`, along that axis. An unused span
    /// is empty, which [`Geometry::cover`] skips.
    fn bands(&self, axis: f64, w: Weight) -> [(i64, i64); 2] {
        let start = self.stack(axis, w.bands());
        match w {
            Weight::Light => [(start, start + self.light), (0, 0)],
            Weight::Heavy => [(start, start + 2 * self.light), (0, 0)],
            Weight::Double => [
                (start, start + self.light),
                (start + 2 * self.light, start + 3 * self.light),
            ],
        }
    }

    /// The rails of the line running along one axis. `low_arm` and `high_arm` are its weights toward
    /// the low and the high end of the axis it travels along; `across` is the cell's centre line in
    /// the coordinate its rails are separated by. Both arms carry the same weight whenever both are
    /// present — no character in [`TABLE`] mixes a double line with another weight on the same axis —
    /// so one weight builds the stack.
    fn line_axis(&self, low_arm: Option<Weight>, high_arm: Option<Weight>, across: f64) -> Axis {
        let Some(w) = low_arm.or(high_arm) else {
            return Axis {
                present: false,
                double: false,
                through: false,
                lo: (0, 0),
                hi: (0, 0),
                arm: None,
            };
        };
        let [(l0, l1), (h0, h1)] = self.bands(across, w);
        let double = w == Weight::Double;
        Axis {
            present: true,
            double,
            through: low_arm.is_some() && high_arm.is_some(),
            lo: (l0, l1),
            hi: if double { (h0, h1) } else { (l0, l1) },
            arm: match (low_arm.is_some(), high_arm.is_some()) {
                (true, true) | (false, false) => None,
                (true, false) => Some(Side::Low),
                (false, true) => Some(Side::High),
            },
        }
    }

    /// The inked spans of a dash pattern across one cell of `span` physical pixels, as offsets from
    /// that cell's near edge. Whole periods are divided out of the cell and each dash gets its share
    /// rounded to a whole pixel with at least one pixel left for the gap — so dashes stay the same
    /// width everywhere and stay visible even in a narrow cell, which rounding cell fractions would
    /// not guarantee (a 3-pixel-wide dash and a half-pixel gap both land on 4 pixels of one cell).
    /// Spare pixels fall into the last gap.
    fn dash_spans(&self, span: i64, dash: Dash) -> [(i64, i64); 4] {
        let periods = (PARTS / dash.period()) as i64;
        let per = span / periods;
        let mut out = [(0, 0); 4];
        if per <= 0 {
            return out; // a cell narrower than the pattern's dashes cannot show them
        }
        // The inked part of one period, rounded from its fraction of it, keeping room for a gap.
        let on = if per == 1 {
            1 // no room to dash at this size: draw the run solid rather than nothing
        } else {
            ((per * i64::from(dash.on()) + i64::from(dash.period() / 2)) / i64::from(dash.period()))
                .clamp(1, per - 1)
        };
        for (k, slot) in out.iter_mut().enumerate().take(periods as usize) {
            let start = k as i64 * per;
            *slot = (start, start + on);
        }
        out
    }

    /// Calls `f` with every quad covering `shape` over the cells `(col, row)` to
    /// `(col + cells - 1, row)`, in logical pixels. A shape that spans its cell's full width is one
    /// quad across the whole run; anything else repeats per cell, where it must.
    pub fn cover(
        &self,
        shape: &Shape,
        col: u32,
        row: u32,
        cells: u32,
        mut f: impl FnMut(Rectangle),
    ) {
        let (col, row, cells) = (i64::from(col), i64::from(row), i64::from(cells));
        match *shape {
            Shape::Empty => {}
            Shape::Fill { x0, y0, x1, y1, .. } => {
                let (top, bottom) = (
                    self.part_y(row, u32::from(y0)),
                    self.part_y(row, u32::from(y1)),
                );
                if shape.full_width() {
                    self.emit(
                        &mut f,
                        self.edge_x(col),
                        top,
                        self.edge_x(col + cells),
                        bottom,
                    );
                } else {
                    for c in col..col + cells {
                        self.emit(
                            &mut f,
                            self.part_x(c, u32::from(x0)),
                            top,
                            self.part_x(c, u32::from(x1)),
                            bottom,
                        );
                    }
                }
            }
            Shape::Lines { .. } => self.cover_lines(&mut f, shape, col, row, cells),
        }
    }

    fn cover_lines(
        &self,
        f: &mut impl FnMut(Rectangle),
        shape: &Shape,
        col: i64,
        row: i64,
        cells: i64,
    ) {
        let Shape::Lines {
            up,
            down,
            left,
            right,
            dash,
        } = *shape
        else {
            return;
        };
        let bands_of = |w: Option<Weight>| w.map_or(0, |w| w.bands());
        // How far arms of one axis reach into the crossing region is set by the widest arm of the
        // other: that is where a tee or a cross joins solidly. With nothing crossing, they stop at
        // the centre line.
        let vb = bands_of(up).max(bands_of(down));
        let hb = bands_of(left).max(bands_of(right));

        if dash != Dash::Solid {
            // A dashed character has one axis across the cell and nothing crossing it, so its dashes
            // are laid out in whole pixels inside each cell and land in the same place in every one.
            let w = left.or(right).or(up).or(down).unwrap_or(Weight::Light);
            let across = left.is_some() || right.is_some();
            for c in col..col + cells {
                if across {
                    let (a, b) = (self.edge_x(c), self.edge_x(c + 1));
                    for (s, e) in self.dash_spans(b - a, dash) {
                        for (t0, t1) in self.bands(self.axis_y(row), w) {
                            self.emit(f, a + s, t0, a + e, t1);
                        }
                    }
                } else {
                    let (a, b) = (self.edge_y(row), self.edge_y(row + 1));
                    for (s, e) in self.dash_spans(b - a, dash) {
                        for (t0, t1) in self.bands(self.axis_x(c), w) {
                            self.emit(f, t0, a + s, t1, a + e);
                        }
                    }
                }
            }
            return;
        }

        // A double line is two rails with a channel between them, and that channel is what makes its
        // joints more than one stroke crossing another: see [`Geometry::cover_joint`].
        if [up, down, left, right]
            .iter()
            .flatten()
            .any(|w| *w == Weight::Double)
        {
            self.cover_joint(f, shape, col, row, cells);
            return;
        }

        // Equal arms on an axis are one stroke across the cell. Across x that is also what lets a run
        // of them stand as one quad for the whole merged span, edges included: it starts on the first
        // cell's left edge and ends on the last one's right edge, so it meets the cells either side.
        if matches!((left, right), (Some(w), Some(d)) if w == d) {
            let w = left.unwrap_or(Weight::Light);
            for (t0, t1) in self.bands(self.axis_y(row), w) {
                self.emit(f, self.edge_x(col), t0, self.edge_x(col + cells), t1);
            }
        }

        for c in col..col + cells {
            let (left_edge, right_edge) = (self.edge_x(c), self.edge_x(c + 1));
            let (top, bottom) = (self.edge_y(row), self.edge_y(row + 1));
            let (cross_x0, cross_x1) = if vb > 0 {
                let s = self.stack(self.axis_x(c), vb);
                (s, s + vb * self.light)
            } else {
                let m = snap(self.axis_x(c));
                (m, m)
            };
            let (cross_y0, cross_y1) = if hb > 0 {
                let s = self.stack(self.axis_y(row), hb);
                (s, s + hb * self.light)
            } else {
                let m = snap(self.axis_y(row));
                (m, m)
            };
            if matches!((up, down), (Some(w), Some(d)) if w == d) {
                let w = up.unwrap_or(Weight::Light);
                for (x0, x1) in self.bands(self.axis_x(c), w) {
                    self.emit(f, x0, top, x1, bottom);
                }
            } else {
                // Unequal or single arms: each runs from its cell edge into the crossing region, so
                // they meet inside it and a joint is solid at any rounding.
                for (w, y0, y1) in [(up, top, cross_y1), (down, cross_y0, bottom)] {
                    let Some(w) = w else { continue };
                    for (x0, x1) in self.bands(self.axis_x(c), w) {
                        self.emit(f, x0, y0, x1, y1);
                    }
                }
            }
            if !matches!((left, right), (Some(w), Some(d)) if w == d) {
                for (w, x0, x1) in [(left, left_edge, cross_x1), (right, cross_x0, right_edge)] {
                    let Some(w) = w else { continue };
                    for (t0, t1) in self.bands(self.axis_y(row), w) {
                        self.emit(f, x0, t0, x1, t1);
                    }
                }
            }
        }
    }

    /// The quads of a shape with a double line in it, which has to be laid out rail by rail and not as
    /// one stroke across the cell: the channel between a double line's two rails has to stay open
    /// through the joint, or `╔` comes out as lines crossing with a filled box at every corner. Each
    /// rule below was read off a real monospace's outlines of these characters.
    ///
    /// A rail stops at the near edge of the perpendicular rail it joins, so it never bridges a channel.
    /// Two things do cross: a line that runs on past the joint (a single line through a double one is
    /// unbroken), and the rail that goes around the outside of a bend — there the channel ends at the
    /// joint, because the perpendicular line has only one arm. That is what makes `╔`'s two outer rails
    /// one nested corner, and what breaks the crossbar's inner rail in `╦` instead.
    fn cover_joint(
        &self,
        f: &mut impl FnMut(Rectangle),
        shape: &Shape,
        col: i64,
        row: i64,
        cells: i64,
    ) {
        let Shape::Lines {
            up,
            down,
            left,
            right,
            ..
        } = *shape
        else {
            return;
        };
        // The horizontal line's rails are the same in every cell of a merged run; the vertical line's
        // are centred on their own cell.
        let h = self.line_axis(left, right, self.axis_y(row));
        if up.is_none() && down.is_none() && left == right {
            // Nothing crosses this line: it is one stroke across the whole run, edges included, which
            // is what lets a row of `═` stand as a single quad that meets the cells either side.
            for (y0, y1) in [h.lo, h.hi] {
                self.emit(f, self.edge_x(col), y0, self.edge_x(col + cells), y1);
            }
            return;
        }
        for c in col..col + cells {
            let (x0, x1) = (self.edge_x(c), self.edge_x(c + 1));
            let (y0, y1) = (self.edge_y(row), self.edge_y(row + 1));
            let v = self.line_axis(up, down, self.axis_x(c));
            // A line that runs through the cell and meets no other double line is one stroke across
            // it: only two doubles have rails that stop short of each other.
            if v.through && !(v.double && h.double) {
                for (_, rail) in rails(&v) {
                    self.emit(f, rail.0, y0, rail.1, y1);
                }
            } else {
                for (side, arm) in [(Side::Low, up), (Side::High, down)] {
                    if arm.is_none() {
                        continue;
                    }
                    for (rail_side, rail) in rails(&v) {
                        let stop = reach(&v, &h, side, rail_side, snap(self.axis_y(row)));
                        match side {
                            Side::Low => self.emit(f, rail.0, y0, rail.1, stop),
                            Side::High => self.emit(f, rail.0, stop, rail.1, y1),
                        }
                    }
                }
            }
            if h.through && !(v.double && h.double) {
                for (_, rail) in rails(&h) {
                    self.emit(f, x0, rail.0, x1, rail.1);
                }
            } else {
                for (side, arm) in [(Side::Low, left), (Side::High, right)] {
                    if arm.is_none() {
                        continue;
                    }
                    for (rail_side, rail) in rails(&h) {
                        let stop = reach(&h, &v, side, rail_side, snap(self.axis_x(c)));
                        match side {
                            Side::Low => self.emit(f, x0, rail.0, stop, rail.1),
                            Side::High => self.emit(f, stop, rail.0, x1, rail.1),
                        }
                    }
                }
            }
        }
    }

    /// A box in physical pixels as the logical quad iced draws: dividing by the scale leaves every
    /// corner on a whole physical pixel, so iced's snapping has nothing left to change.
    fn emit(&self, f: &mut impl FnMut(Rectangle), x0: i64, y0: i64, x1: i64, y1: i64) {
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let scale = f64::from(self.scale);
        f(Rectangle {
            x: (x0 as f64 / scale) as f32,
            y: (y0 as f64 / scale) as f32,
            width: ((x1 - x0) as f64 / scale) as f32,
            height: ((y1 - y0) as f64 / scale) as f32,
        });
    }
}

/// A line's rails to walk in order: both for a double line, and one band for anything else — the slot
/// left empty is skipped by [`Geometry::emit`], as everywhere else in this module.
fn rails(a: &Axis) -> [(Side, (i64, i64)); 2] {
    [
        (Side::Low, a.lo),
        (Side::High, if a.double { a.hi } else { (0, 0) }),
    ]
}

/// How far one rail of `a` reaches toward `side` before it stops, given the perpendicular axis `p`.
/// The coordinate is the one this line travels along — which is the one `p`'s rails are ordered along,
/// so its two rails and this line's arms sit on the same scale.
fn reach(a: &Axis, p: &Axis, side: Side, rail: Side, centre: i64) -> i64 {
    if !p.present {
        return centre; // nothing crosses here, so an arm stops at the cell's centre line
    }
    // The far edge of the whole perpendicular stack from this side, and the near edge of the rail of
    // it that is on this side.
    let (across, near) = match side {
        Side::Low => (p.hi.1, p.lo.1),
        Side::High => (p.lo.0, p.hi.0),
    };
    if !p.double {
        return across; // a single band is not split by anything: the line runs right across it
    }
    // A double line's outer rail turns around the bend with the perpendicular line's outer rail, so it
    // covers the whole stack — possible because the channel ends at the joint when the perpendicular
    // line has only one arm. Its inner rail joins the inner one and stops short of the channel. The
    // side the perpendicular arm goes to is the inside of that bend. A single line has no rails to keep
    // apart: it crosses the channel as well, unless it stops here while the channel runs on past.
    let crosses = if a.double {
        p.arm.is_some() && rail != p.arm.unwrap_or(Side::Low)
    } else {
        a.through || p.arm.is_some()
    };
    if crosses { across } else { near }
}

/// Rounds to the nearest physical pixel, half away from zero.
fn snap(v: f64) -> i64 {
    v.round() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cell sizes and display scales a window really reports: whole pixels, a retina scale on a
    /// fractional cell (what a scaled font gives), and a fractional scale between the two.
    const CASES: [(f32, f32); 3] = [(1.0, 8.0), (2.0, 8.43), (1.25, 9.6)];

    fn grid(cell: (f32, f32), scale: f32) -> Geometry {
        Geometry::new(
            Rectangle {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            Size::new(cell.0, cell.1),
            scale,
        )
    }

    fn grid_at(origin: (f32, f32), cell: (f32, f32), scale: f32) -> Geometry {
        Geometry::new(
            Rectangle {
                x: origin.0,
                y: origin.1,
                width: 0.0,
                height: 0.0,
            },
            Size::new(cell.0, cell.1),
            scale,
        )
    }

    fn drawn(c: char) -> &'static [Shape] {
        shapes(c).unwrap_or_else(|| panic!("{c:?} should be drawn as a shape"))
    }

    /// The quads `c` covers over `cells` cells from `(col, row)`, in logical pixels.
    fn quads(g: &Geometry, c: char, col: u32, row: u32, cells: u32) -> Vec<Rectangle> {
        let mut out = Vec::new();
        for shape in drawn(c) {
            g.cover(shape, col, row, cells, |r| out.push(r));
        }
        out
    }

    fn one(g: &Geometry, c: char, col: u32, row: u32) -> Rectangle {
        quads(g, c, col, row, 1)
            .pop()
            .expect("a shape covers something")
    }

    /// What iced's quad shader paints for a box: `solid.wgsl` scales the logical position and size by
    /// the window scale in f32, then snaps to `round(pos + 0.001)` and `round(pos + size + 0.001)`.
    /// That is the arithmetic every quad on screen goes through, including the background cells
    /// `render` draws, so it is the yardstick for [`Geometry::px`].
    fn shader_rect(pos: f32, size: f32, scale: f32) -> (i64, i64) {
        let (pos, size) = (pos * scale, size * scale);
        (
            (pos + 0.001).round() as i64,
            (pos + size + 0.001).round() as i64,
        )
    }

    /// The pixels the background of cell `col` gets: the rect `render`'s `rect()` builds for a
    /// one-cell span at that column, through [`shader_rect`].
    fn background_cell(bx: f32, cw: f32, col: u32, scale: f32) -> (i64, i64) {
        shader_rect(bx + col as f32 * cw, cw, scale)
    }

    /// A quad's box back in physical pixels the way the shader computes them: the unit this module
    /// works in, and the number that lands on screen.
    fn px(r: Rectangle, scale: f32) -> (i64, i64, i64, i64) {
        let (l, right) = shader_rect(r.x, r.width, scale);
        let (top, bottom) = shader_rect(r.y, r.height, scale);
        (l, top, right, bottom)
    }

    /// A quad's box in physical pixels, with the scale taken from the geometry that made it.
    fn box_of(g: &Geometry, r: Rectangle) -> (i64, i64, i64, i64) {
        px(r, g.scale)
    }

    /// Whether two quads share more than an edge: a joint is solid only if they do.
    fn overlaps(a: &(i64, i64, i64, i64), b: &(i64, i64, i64, i64)) -> bool {
        a.2.min(b.2) - a.0.max(b.0) > 0 && a.3.min(b.3) - a.1.max(b.1) > 0
    }

    /// A quad standing up rather than lying down, which decides which pairs must cross.
    fn stands_up(q: &(i64, i64, i64, i64)) -> bool {
        q.3 - q.1 > q.2 - q.0
    }

    #[test]
    fn cell_edges_are_shared_across_the_grid() {
        for &(scale, w) in &CASES {
            for origin in [(0.0, 0.0), (0.5, 1.25)] {
                let g = grid_at(origin, (w, w * 1.2), scale);
                let mut prev = None;
                let mut widths = Vec::new();
                for col in 0..200 {
                    // A full block is the cell itself: its edges are the boundaries in the raw form.
                    let (x0, _, x1, _) = box_of(&g, one(&g, '█', col, 0));
                    if let Some(prev) = prev {
                        assert_eq!(
                            prev, x0,
                            "cell {col} must start exactly where the cell before it ended \
                             (scale {scale}, width {w}, origin {origin:?})"
                        );
                    }
                    let (want0, want1) = background_cell(origin.0, w, col, scale);
                    assert_eq!(
                        (x0, x1),
                        (want0, want1),
                        "cell {col} must land where the shader puts that column's background \
                         (scale {scale}, width {w}, origin {origin:?})"
                    );
                    widths.push(x1 - x0);
                    prev = Some(x1);
                }
                // Rounding spreads the width by at most a pixel between cells, and the total over
                // 200 of them stays within a pixel of the true size.
                let (min, max) = (*widths.iter().min().unwrap(), *widths.iter().max().unwrap());
                assert!(
                    max - min <= 1,
                    "widths {min}..={max} spread too far ({scale}, {w})"
                );
                let total: i64 = widths.iter().sum();
                let exact = 200.0 * f64::from(w) * f64::from(scale);
                assert!(((total as f64) - exact).abs() < 1.0, "{total} vs {exact}");
            }
        }
    }

    /// A shape must start where its cell's background starts and end where the next one starts, or a
    /// block shows a hairline of the colour under it at an edge. [`Geometry`]'s boundaries are the
    /// shader's snap of each column's left position; this compares them against that column's own
    /// right edge as well, which the shader computes in a different f32 order and which agrees over
    /// the coordinates here (a simulation finds disagreement only past some 9400 physical pixels at
    /// scale 3).
    #[test]
    fn cell_edges_land_where_the_shader_puts_the_background() {
        let cases = [
            (1.0f32, 8.0f32),
            (2.0, 8.43),
            (1.25, 9.6),
            // The near-tie that used to disagree: `8.4` as f32 sits just under the true width, so the
            // old f64 product was 10.4999995 and rounded to 10, while the shader's f32 product is
            // exactly 10.5 and its slack carries it to 11.
            (1.25, 8.4),
            (1.5, 7.4666667),
        ];
        for &(scale, w) in &cases {
            for origin in [0.0f32, 0.5, -3.25, 7.3] {
                let g = grid_at((origin, origin), (w, w * 1.2), scale);
                for col in 0..240 {
                    let (left, right) = background_cell(origin, w, col, scale);
                    assert_eq!(
                        (g.edge_x(i64::from(col)), g.edge_x(i64::from(col) + 1)),
                        (left, right),
                        "column {col} must end where its background ends ({scale}, {w}, {origin})"
                    );
                }
                for row in 0..80 {
                    let (top, bottom) = background_cell(origin, w * 1.2, row, scale);
                    assert_eq!(
                        (g.edge_y(i64::from(row)), g.edge_y(i64::from(row) + 1)),
                        (top, bottom),
                        "row {row} must end where its background ends ({scale}, {w}, {origin})"
                    );
                }
            }
        }
    }

    #[test]
    fn a_full_block_is_exactly_one_cell() {
        for &(scale, w) in &CASES {
            let g = grid((w, w * 1.2), scale);
            let (x0, y0, x1, y1) = box_of(&g, one(&g, '█', 4, 3));
            assert_eq!((x0, y0), (g.edge_x(4), g.edge_y(3)));
            assert_eq!((x1, y1), (g.edge_x(5), g.edge_y(4)));
            // And a merged run of them is one quad over the same span.
            let run = quads(&g, '█', 4, 3, 6);
            assert_eq!(run.len(), 1, "a run of blocks is one quad");
            assert_eq!(box_of(&g, run[0]), (x0, y0, g.edge_x(10), g.edge_y(4)));
        }
    }

    #[test]
    fn halves_meet_at_the_edge_between_their_cells() {
        for &(scale, w) in &CASES {
            let g = grid((w, w * 1.2), scale);
            // Vertically: ▀ and ▄ are the two halves of one cell, split at its middle boundary.
            let up = box_of(&g, one(&g, '▀', 3, 4));
            let dn = box_of(&g, one(&g, '▄', 3, 4));
            assert_eq!((up.0, up.2), (g.edge_x(3), g.edge_x(4)), "▀ spans the cell");
            assert_eq!(up.1, g.edge_y(4), "▀ starts at the cell's top edge");
            assert_eq!(
                (up.3, dn.1),
                (g.part_y(4, PARTS / 2), g.part_y(4, PARTS / 2))
            );
            assert_eq!((dn.0, dn.2, dn.3), (g.edge_x(3), g.edge_x(4), g.edge_y(5)));
            // A block sitting on the row below starts where this row's bottom half ended: stacked
            // blocks have no seam between them, which is what the font's short glyphs gave.
            let below = box_of(&g, one(&g, '█', 3, 5));
            assert_eq!(dn.3, below.1, "no seam between rows");
            // Horizontally: ▌ and ▐ share the cell's centre boundary.
            let left = box_of(&g, one(&g, '▌', 7, 2));
            let right = box_of(&g, one(&g, '▐', 7, 2));
            assert_eq!(left.2, g.part_x(7, PARTS / 2));
            assert_eq!(right.0, left.2, "▌ and ▐ meet at the centre");
            assert_eq!((left.0, right.2), (g.edge_x(7), g.edge_x(8)));
        }
    }

    #[test]
    fn eighths_meet_where_the_names_say_they_should() {
        for &(scale, w) in &CASES {
            let g = grid((w, w * 1.2), scale);
            // ▔ is the top eighth and ▇ the bottom seven: together one cell, no overlap.
            let thin = box_of(&g, one(&g, '▔', 0, 1));
            let rest = box_of(&g, one(&g, '▇', 0, 1));
            assert_eq!(thin.1, g.edge_y(1));
            assert_eq!((thin.3, rest.1), (g.part_y(1, 6), g.part_y(1, 6)));
            assert_eq!(rest.3, g.edge_y(2));
            // ▏ is one eighth wide from the left edge, and it does not reach the centre.
            let sliver = box_of(&g, one(&g, '▏', 5, 5));
            assert_eq!(
                (sliver.0, sliver.1, sliver.3),
                (g.edge_x(5), g.edge_y(5), g.edge_y(6))
            );
            assert!(
                sliver.2 > sliver.0,
                "an eighth of a cell is at least a pixel"
            );
            assert!(
                sliver.2 <= g.part_x(5, PARTS / 2),
                "an eighth is less than a half"
            );
        }
    }

    #[test]
    fn a_row_of_horizontal_lines_is_one_unbroken_line() {
        for &(scale, w) in &CASES {
            let g = grid((w, w * 1.2), scale);
            // Cell by cell: one height for all of them, and each starting where the last ended.
            let parts: Vec<_> = (0..4).map(|c| box_of(&g, one(&g, '─', c, 1))).collect();
            for p in &parts[1..] {
                assert_eq!((p.1, p.3), (parts[0].1, parts[0].3), "one height");
            }
            for pair in parts.windows(2) {
                assert_eq!(pair[0].2, pair[1].0, "no gap and no overlap between cells");
            }
            // Merged, which is how a run of them is drawn: one quad over the same ground.
            let run = quads(&g, '─', 0, 1, 4);
            assert_eq!(run.len(), 1);
            assert_eq!(
                box_of(&g, run[0]),
                (parts[0].0, parts[0].1, parts[3].2, parts[0].3)
            );
        }
    }

    #[test]
    fn a_column_of_vertical_lines_is_one_unbroken_line() {
        for &(scale, w) in &CASES {
            let g = grid((w, w * 1.2), scale);
            let parts: Vec<_> = (0..4).map(|r| box_of(&g, one(&g, '│', 2, r))).collect();
            for p in &parts[1..] {
                assert_eq!((p.0, p.2), (parts[0].0, parts[0].2), "one width");
            }
            for pair in parts.windows(2) {
                assert_eq!(pair[0].3, pair[1].1, "rows meet at the boundary");
            }
        }
    }

    #[test]
    fn junctions_reach_every_edge_their_arms_are_named_for() {
        for &(scale, w) in &CASES {
            let g = grid((w, w * 1.2), scale);
            // Which edges each of these is supposed to touch, by its own name.
            let cases: [(char, &[&str]); 8] = [
                ('┌', &["bottom", "right"]),
                ('┐', &["bottom", "left"]),
                ('└', &["top", "right"]),
                ('┘', &["top", "left"]),
                ('├', &["top", "bottom", "right"]),
                ('┤', &["top", "bottom", "left"]),
                ('┬', &["left", "right", "bottom"]),
                ('┼', &["top", "bottom", "left", "right"]),
            ];
            for (c, want) in cases {
                let qs: Vec<_> = quads(&g, c, 6, 2, 1)
                    .iter()
                    .map(|r| box_of(&g, *r))
                    .collect();
                let (x_left, y_top) = (g.edge_x(6), g.edge_y(2));
                let (x_right, y_bottom) = (g.edge_x(7), g.edge_y(3));
                let touches = |edge: &str| {
                    qs.iter().any(|&(x0, y0, x1, y1)| match edge {
                        "left" => x0 == x_left,
                        "right" => x1 == x_right,
                        "top" => y0 == y_top,
                        "bottom" => y1 == y_bottom,
                        _ => unreachable!(),
                    })
                };
                for edge in want {
                    assert!(
                        touches(edge),
                        "{c:?} must reach the {edge} edge ({scale}, {w})"
                    );
                }
                assert!(
                    !qs.iter().any(|q| q.0 == x_left
                        && q.1 == y_top
                        && q.2 == x_right
                        && q.3 == y_bottom),
                    "{c:?} is not a filled cell"
                );
                // Arms on different axes overlap rather than merely touching, so a joint is solid
                // however each one rounds on its own.
                for a in &qs {
                    for b in &qs {
                        if stands_up(a) != stands_up(b) {
                            assert!(overlaps(a, b), "{c:?} has a hollow joint ({scale}, {w})");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn heavy_lines_are_exactly_twice_light_ones() {
        for &(scale, w) in &CASES {
            let g = grid((w, w * 1.2), scale);
            let light = g.light;
            let (thin, fat) = (
                box_of(&g, one(&g, '─', 0, 0)),
                box_of(&g, one(&g, '━', 0, 0)),
            );
            assert_eq!(
                thin.3 - thin.1,
                light,
                "a light line is the reference thickness"
            );
            assert_eq!(fat.3 - fat.1, 2 * light, "a heavy line is twice it");
            let (v_thin, v_fat) = (
                box_of(&g, one(&g, '│', 0, 0)),
                box_of(&g, one(&g, '┃', 0, 0)),
            );
            assert_eq!(v_thin.2 - v_thin.0, light);
            assert_eq!(v_fat.2 - v_fat.0, 2 * light);
            // Arms take the weight their name gives them: ┞ is heavy up, light down, light right.
            let arms: Vec<_> = quads(&g, '┞', 1, 1, 1)
                .iter()
                .map(|r| box_of(&g, *r))
                .collect();
            let up = arms.iter().find(|q| q.1 == g.edge_y(1)).expect("up arm");
            let down = arms.iter().find(|q| q.3 == g.edge_y(2)).expect("down arm");
            let right = arms.iter().find(|q| q.2 == g.edge_x(2)).expect("right arm");
            assert_eq!(up.2 - up.0, 2 * light, "the heavy arm is twice as wide");
            assert_eq!(down.2 - down.0, light, "the light arm is single");
            assert_eq!(right.3 - right.1, light);
        }
    }

    #[test]
    fn double_lines_are_two_light_lines_with_a_light_gap() {
        for &(scale, w) in &CASES {
            let g = grid((w, w * 1.2), scale);
            let light = g.light;
            let single = box_of(&g, one(&g, '─', 0, 0));
            let mut qs: Vec<_> = quads(&g, '═', 0, 0, 1)
                .iter()
                .map(|r| box_of(&g, *r))
                .collect();
            qs.sort();
            assert_eq!(qs.len(), 2, "a double line is two lines");
            for q in &qs {
                assert_eq!(q.3 - q.1, light, "each of them is one light thickness");
                assert_eq!((q.0, q.2), (single.0, single.2), "and they span the cell");
            }
            assert_eq!(
                qs[1].1 - qs[0].3,
                light,
                "the gap between them is one light too"
            );
            // The pair sits on the same centre line a single line uses.
            let double_centre = (qs[0].1 + qs[1].3) as f64 / 2.0;
            let single_centre = (single.1 + single.3) as f64 / 2.0;
            assert_eq!(double_centre, single_centre);
            // Vertically too, and ╬ is a double crossing both ways.
            let vqs: Vec<_> = quads(&g, '║', 0, 0, 1)
                .iter()
                .map(|r| box_of(&g, *r))
                .collect();
            assert_eq!(vqs.len(), 2);
            for (x0, _, x1, _) in vqs {
                assert_eq!(x1 - x0, light);
            }
            // A cross of two doubles is four hooks — each of the four arms is painted by its own two
            // rails, because neither line's channel may be bridged. See the joint tests for the shape.
            assert_eq!(quads(&g, '╬', 2, 2, 1).len(), 8, "two doubles crossing");
        }
    }

    #[test]
    fn dashes_repeat_in_step_across_cells() {
        for &(scale, w) in &CASES {
            let g = grid((w, w * 1.2), scale);
            // Each cell carries its own dashes, at the same offsets along its own axis, so a row or
            // column of them keeps step across the boundaries.
            for (c, expected, vertical) in [
                ('╌', 2usize, false),
                ('┄', 3, false),
                ('┈', 4, false),
                ('╎', 2, true),
                ('┆', 3, true),
                ('┊', 4, true),
            ] {
                // Cells are not all the same width (rounding spreads the remainder), so compare the
                // ones that are: those must draw the same dashes, or a row of them would drift.
                let wide = (0u32..8).filter(|&col| {
                    let col = i64::from(col);
                    let w = if vertical {
                        g.edge_y(1) - g.edge_y(0)
                    } else {
                        g.edge_x(col + 1) - g.edge_x(col)
                    };
                    // A cell wide enough for one dash and one gap per period.
                    w >= 2 * expected as i64
                });
                let mut patterns: Vec<Vec<(i64, i64)>> = Vec::new();
                for col in wide.take(4) {
                    let spans: Vec<_> = quads(&g, c, col, 0, 1)
                        .iter()
                        .map(|r| {
                            let q = box_of(&g, *r);
                            if vertical {
                                (q.1 - g.edge_y(0), q.3 - g.edge_y(0))
                            } else {
                                let base = g.edge_x(i64::from(col));
                                (q.0 - base, q.2 - base)
                            }
                        })
                        .collect();
                    assert_eq!(spans.len(), expected, "{c:?} dash count ({scale}, {w})");
                    let span = if vertical {
                        g.edge_y(1) - g.edge_y(0)
                    } else {
                        g.edge_x(i64::from(col) + 1) - g.edge_x(i64::from(col))
                    };
                    let inked: i64 = spans.iter().map(|&(a, b)| b - a).sum();
                    assert!(inked > 0 && inked < span, "{c:?} should dash, not fill");
                    patterns.push(spans);
                }
                assert!(!patterns.is_empty(), "no cell wide enough to test {c:?}");
                let first = patterns[0].clone();
                for p in &patterns[1..] {
                    assert_eq!(
                        &first, p,
                        "{c:?} lands in the same place in every like-sized cell"
                    );
                }
            }
        }
    }

    #[test]
    fn shades_fill_the_cell_at_a_fraction_of_the_foreground() {
        let g = grid((8.0, 9.6), 2.0);
        for (c, alpha) in [('░', 0.25f32), ('▒', 0.5), ('▓', 0.75)] {
            assert_eq!(drawn(c).len(), 1);
            assert_eq!(drawn(c)[0].alpha(), alpha, "{c:?} opacity");
            let q = box_of(&g, one(&g, c, 2, 2));
            assert_eq!(
                (q.0, q.1, q.2, q.3),
                (g.edge_x(2), g.edge_y(2), g.edge_x(3), g.edge_y(3))
            );
        }
    }

    #[test]
    fn only_the_named_characters_are_drawn_as_shapes() {
        // Blocks, shades and every line character in the block are covered...
        let drawn_count = (FIRST..=LAST)
            .filter(|cp| shapes(char::from_u32(*cp).unwrap()).is_some())
            .count();
        assert_eq!(drawn_count, COUNT - 7);
        // ...and the only things left out are the rounded corners and the diagonals.
        let left_out: Vec<u32> = (FIRST..=LAST)
            .filter(|cp| shapes(char::from_u32(*cp).unwrap()).is_none())
            .collect();
        assert_eq!(left_out, (0x256D..=0x2573).collect::<Vec<u32>>());
        // Everything else keeps going through the font.
        for c in ['a', ' ', '\u{24ff}', '\u{25a0}', '\u{2800}'] {
            assert_eq!(shapes(c), None, "{c:?} is not ours to draw");
        }
        assert!(shapes('\u{2500}').is_some(), "the first of them");
        assert!(shapes('\u{259f}').is_some(), "the last of them");
    }

    #[test]
    fn mergeable_shapes_are_the_ones_spanning_a_whole_cell() {
        for c in ['█', '▄', '▀', '─', '━', '═', '░', '▓'] {
            assert!(drawn(c)[0].full_width(), "{c:?} spans the cell width");
        }
        for c in ['│', '┌', '╌', '▏', '▐', '┼', '▚'] {
            let qs = drawn(c);
            assert!(!qs.iter().any(|s| s.full_width()), "{c:?} repeats per cell");
        }
    }

    #[test]
    fn a_bogus_scale_is_treated_as_one() {
        for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let g = grid((8.0, 9.6), scale);
            assert_eq!(g.scale, 1.0);
            assert_eq!(g.light, (9.6f64 / 16.0).round().max(1.0) as i64);
            assert!(!quads(&g, '█', 0, 0, 1).is_empty());
        }
    }

    /// One cell's worth of pixels, as the shader paints them: quads rasterized into a grid the size of
    /// the cell, so a test can say exactly where the ink is.
    struct Pixels {
        w: usize,
        h: usize,
        ink: Vec<bool>,
    }

    impl Pixels {
        fn new(w: i64, h: i64) -> Self {
            Self {
                w: w as usize,
                h: h as usize,
                ink: vec![false; (w * h) as usize],
            }
        }

        /// The expected picture: the union of these boxes.
        fn boxes(w: i64, h: i64, rects: &[(i64, i64, i64, i64)]) -> Self {
            let mut out = Self::new(w, h);
            for &(x0, y0, x1, y1) in rects {
                out.fill((x0, y0, x1, y1));
            }
            out
        }

        fn fill(&mut self, (x0, y0, x1, y1): (i64, i64, i64, i64)) {
            for y in y0.max(0)..y1.min(self.h as i64) {
                for x in x0.max(0)..x1.min(self.w as i64) {
                    self.ink[(y * self.w as i64 + x) as usize] = true;
                }
            }
        }

        /// Whether every pixel of the box is inked.
        fn covers(&self, (x0, y0, x1, y1): (i64, i64, i64, i64)) -> bool {
            (y0..y1).all(|y| (x0..x1).all(|x| self.ink[(y * self.w as i64 + x) as usize]))
        }

        /// Whether the box is free of ink.
        fn clear(&self, (x0, y0, x1, y1): (i64, i64, i64, i64)) -> bool {
            (y0..y1).all(|y| (x0..x1).all(|x| !self.ink[(y * self.w as i64 + x) as usize]))
        }
    }

    /// The joint tests all use one geometry: scale 2 on a 10×16 cell, where a light line is two pixels,
    /// a double line's rails are 7..9 and 11..13 across the cell and 13..15 and 17..19 down it, and the
    /// channel between each pair of rails is two pixels wide. Small enough to write the pixels out.
    fn joint_grid() -> Geometry {
        grid((10.0, 16.0), 2.0)
    }

    /// A character's cell as [`Pixels`].
    fn paint(g: &Geometry, c: char) -> Pixels {
        let (ox, oy) = (g.edge_x(0), g.edge_y(0));
        let mut out = Pixels::new(g.edge_x(1) - ox, g.edge_y(1) - oy);
        for r in quads(g, c, 0, 0, 1) {
            let q = box_of(g, r);
            out.fill((q.0 - ox, q.1 - oy, q.2 - ox, q.3 - oy));
        }
        out
    }

    /// A double line's rails and the channel between them along one axis, as (near rail, gap, far rail).
    fn stack_of(g: &Geometry, axis: f64) -> [(i64, i64); 3] {
        let [(a0, a1), (b0, b1)] = g.bands(axis, Weight::Double);
        [(a0, a1), (a1, b0), (b0, b1)]
    }

    /// The rails, channels and cell edges of [`joint_grid`], as pixel ranges.
    struct Joints {
        g: Geometry,
        l: i64,
        r: i64,
        t: i64,
        b: i64,
        /// A vertical double line's rails across x, near and far from the cell's left edge.
        v_lo: (i64, i64),
        v_hi: (i64, i64),
        /// A horizontal double line's rails down y, near and far from its top edge.
        h_top: (i64, i64),
        h_bot: (i64, i64),
    }

    impl Joints {
        fn new() -> Self {
            let g = joint_grid();
            let v = stack_of(&g, g.axis_x(0));
            let h = stack_of(&g, g.axis_y(0));
            Self {
                l: g.edge_x(0),
                r: g.edge_x(1),
                t: g.edge_y(0),
                b: g.edge_y(1),
                v_lo: v[0],
                v_hi: v[2],
                h_top: h[0],
                h_bot: h[2],
                g,
            }
        }

        /// Where a single (light) line on this axis would sit.
        fn light_v(&self) -> (i64, i64) {
            self.g.bands(self.g.axis_x(0), Weight::Light)[0]
        }

        fn light_h(&self) -> (i64, i64) {
            self.g.bands(self.g.axis_y(0), Weight::Light)[0]
        }
    }

    /// The exact pixels of the double-line joints, read off a real monospace's outlines: `╔` is two
    /// nested corners, `╦` breaks the crossbar's inner rail instead of bridging the arm's channel, `╬`
    /// is four hooks with nothing crossing the middle, a light arm stops at the near band (`╟`, `╤`), a
    /// double arm lands on a single line with both its bands (`╒`, `╓`), and `╪`'s single line runs
    /// through both bands and across the gap. None of this was true before, when every band was drawn
    /// from the far side of the crossing region and nothing trimmed the inner one.
    #[test]
    fn double_joints_are_two_rails_and_not_two_crossing_lines() {
        let j = Joints::new();
        let (xl, xr, yt, yb) = (j.l, j.r, j.t, j.b);
        let (v0a, v0b) = j.v_lo;
        let (v1a, v1b) = j.v_hi;
        let (h0a, h0b) = j.h_top;
        let (h1a, h1b) = j.h_bot;
        let lv = j.light_v();
        let lh = j.light_h();
        type Box = (i64, i64, i64, i64);
        let want: [(char, Vec<Box>); 10] = [
            // Corners: the outer rail of each arm goes around the outside of the bend, the inner one
            // meets the other line's inner rail, and both channels stay open.
            (
                '╔',
                vec![
                    (v0a, h0a, xr, h0b),
                    (v1a, h1a, xr, h1b),
                    (v0a, h0a, v0b, yb),
                    (v1a, h1a, v1b, yb),
                ],
            ),
            (
                '╗',
                vec![
                    (xl, h0a, v1b, h0b),
                    (xl, h1a, v0b, h1b),
                    (v1a, h0a, v1b, yb),
                    (v0a, h1a, v0b, yb),
                ],
            ),
            // A tee: the through line's outer rail is unbroken, its inner one stops where the arm's
            // channel passes, and each rail of the arm joins that inner rail.
            (
                '╠',
                vec![
                    (v0a, yt, v0b, yb),
                    (v1a, yt, v1b, h0b),
                    (v1a, h1a, v1b, yb),
                    (v1a, h0a, xr, h0b),
                    (v1a, h1a, xr, h1b),
                ],
            ),
            (
                '╦',
                vec![
                    (xl, h0a, xr, h0b),
                    (xl, h1a, v0b, h1b),
                    (v1a, h1a, xr, h1b),
                    (v0a, h1a, v0b, yb),
                    (v1a, h1a, v1b, yb),
                ],
            ),
            // A cross of two doubles: four hooks, each rail stopping at the near rail of the other.
            (
                '╬',
                vec![
                    (xl, h0a, v0b, h0b),
                    (v1a, h0a, xr, h0b),
                    (xl, h1a, v0b, h1b),
                    (v1a, h1a, xr, h1b),
                    (v0a, yt, v0b, h0b),
                    (v1a, yt, v1b, h0b),
                    (v0a, h1a, v0b, yb),
                    (v1a, h1a, v1b, yb),
                ],
            ),
            // A light arm against a double line stops at the near band instead of bridging the gap; the
            // through line's two rails run the whole way.
            (
                '╟',
                vec![
                    (v0a, yt, v0b, yb),
                    (v1a, yt, v1b, yb),
                    (v1a, lh.0, xr, lh.1),
                ],
            ),
            (
                '╤',
                vec![
                    (xl, h0a, xr, h0b),
                    (xl, h1a, xr, h1b),
                    (lv.0, h1a, lv.1, yb),
                ],
            ),
            // A double arm against a single line joins it with both bands, and a light line crossing a
            // double stack bridges it: the channel ends at that joint.
            (
                '╒',
                vec![
                    (lv.0, h0a, xr, h0b),
                    (lv.0, h1a, xr, h1b),
                    (lv.0, h0a, lv.1, yb),
                ],
            ),
            (
                '╓',
                vec![
                    (v0a, lh.0, xr, lh.1),
                    (v0a, lh.0, v0b, yb),
                    (v1a, lh.0, v1b, yb),
                ],
            ),
            // The crosses mix one of each: the single line runs through both bands and across their
            // gap, and the double's bands run through as well.
            (
                '╪',
                vec![(xl, h0a, xr, h0b), (xl, h1a, xr, h1b), (lv.0, yt, lv.1, yb)],
            ),
        ];
        for (c, rects) in want {
            let got = paint(&j.g, c);
            let want = Pixels::boxes(got.w as i64, got.h as i64, &rects);
            assert_eq!(
                got.ink, want.ink,
                "{c:?} does not draw the joints the font draws"
            );
        }
    }

    /// The same joints by the properties that matter rather than an exact picture: the channel between
    /// a double line's rails is empty inside the joint, every band reaches the cell edge its name
    /// mentions, and where two bands meet they overlap instead of merely touching.
    #[test]
    fn double_joints_keep_their_channels_open_and_reach_their_edges() {
        let j = Joints::new();
        let (xl, xr, yt, yb) = (j.l, j.r, j.t, j.b);
        let (v0a, v0b) = j.v_lo;
        let (v1a, v1b) = j.v_hi;
        let (h0a, h0b) = j.h_top;
        let (h1a, h1b) = j.h_bot;
        let lv = j.light_v();
        let lh = j.light_h();
        type Box = (i64, i64, i64, i64);

        // Where a channel has to stay empty. Which ones those are comes from the same outlines as the
        // exact pictures above: a corner's channels run out through the joint (`╔` keeps its own open
        // below the miter and `╗` above it, and neither lets the other line's channel be bridged), a
        // tee's arm channel runs out into the through line (`╠`, `╦`), `╬` has both channels running
        // the whole way across the cell, and where a single line crosses a double stack only that line
        // may fill it (`╒`, `╓`, `╪`).
        let open: [(char, Vec<Box>); 10] = [
            ('╔', vec![(v0b, h0b, v1a, yb), (v0b, h0b, xr, h1a)]),
            (
                '╗',
                vec![
                    (v0b, h0b, v1a, yb),
                    (xl, h0b, v1a, h1a),
                    (v1b, h0b, xr, h1a),
                ],
            ),
            (
                '╠',
                vec![
                    (v0b, yt, v1a, h0a),
                    (v0b, h1b, v1a, yb),
                    (v0b, h0b, xr, h1a),
                ],
            ),
            ('╦', vec![(v0b, h1b, v1a, yb), (xl, h0b, xr, h1a)]),
            ('╬', vec![(v0b, yt, v1a, yb), (xl, h0b, xr, h1a)]),
            ('╟', vec![(v0b, yt, v1a, yb)]),
            ('╤', vec![(xl, h0b, xr, h1a)]),
            ('╒', vec![(xl, h0b, lv.0, h1a), (lv.1, h0b, xr, h1a)]),
            ('╓', vec![(v0b, yt, v1a, lh.0), (v0b, lh.1, v1a, yb)]),
            ('╪', vec![(xl, h0b, lv.0, h1a), (lv.1, h0b, xr, h1a)]),
        ];
        for (c, boxes) in open {
            let p = paint(&j.g, c);
            for b in boxes {
                assert!(p.clear(b), "{c:?} fills the channel at {b:?}");
            }
        }
        // Where a single line does cross a double stack it closes the channel there, and must: that is
        // what makes `╪`'s light line run through unbroken rather than split in two.
        let bridges = [
            ('╒', (lv.0, h0b, lv.1, h1a)),
            ('╓', (v0b, lh.0, v1a, lh.1)),
            ('╪', (lv.0, h0b, lv.1, h1a)),
        ];
        for (c, b) in bridges {
            assert!(
                paint(&j.g, c).covers(b),
                "{c:?} should bridge the gap where it crosses"
            );
        }

        // Every band reaches the cell edge its arms are named for, and both rails of a double line
        // reach it, not just one: `╔`'s two horizontal bands both run to the right edge and its two
        // vertical rails both run to the bottom, `╗`'s run to the left edge and the bottom (its arms
        // are down and left, so nothing of it touches the top), and a tee's through line reaches both
        // of its ends.
        let edges: [(char, Vec<Box>); 10] = [
            (
                '╔',
                vec![
                    (xr - 2, h0a, xr, h0b),
                    (xr - 2, h1a, xr, h1b),
                    (v0a, yb - 2, v0b, yb),
                    (v1a, yb - 2, v1b, yb),
                ],
            ),
            (
                '╗',
                vec![
                    (xl, h0a, xl + 2, h0b),
                    (xl, h1a, xl + 2, h1b),
                    (v0a, yb - 2, v0b, yb),
                    (v1a, yb - 2, v1b, yb),
                ],
            ),
            (
                '╠',
                vec![
                    (xr - 2, h0a, xr, h0b),
                    (xr - 2, h1a, xr, h1b),
                    (v0a, yt, v0b, yt + 2),
                    (v0a, yb - 2, v0b, yb),
                    (v1a, yt, v1b, yt + 2),
                    (v1a, yb - 2, v1b, yb),
                ],
            ),
            (
                '╦',
                vec![
                    (xl, h0a, xl + 2, h0b),
                    (xr - 2, h0a, xr, h0b),
                    (xl, h1a, xl + 2, h1b),
                    (xr - 2, h1a, xr, h1b),
                    (v0a, yb - 2, v0b, yb),
                    (v1a, yb - 2, v1b, yb),
                ],
            ),
            (
                '╬',
                vec![
                    (xl, h0a, xl + 2, h0b),
                    (xr - 2, h0a, xr, h0b),
                    (xl, h1a, xl + 2, h1b),
                    (xr - 2, h1a, xr, h1b),
                    (v0a, yt, v0b, yt + 2),
                    (v1a, yt, v1b, yt + 2),
                    (v0a, yb - 2, v0b, yb),
                    (v1a, yb - 2, v1b, yb),
                ],
            ),
            (
                '╟',
                vec![
                    (xr - 2, lh.0, xr, lh.1),
                    (v0a, yt, v0b, yt + 2),
                    (v0a, yb - 2, v0b, yb),
                    (v1a, yt, v1b, yt + 2),
                    (v1a, yb - 2, v1b, yb),
                ],
            ),
            (
                '╤',
                vec![
                    (xl, h0a, xl + 2, h0b),
                    (xr - 2, h0a, xr, h0b),
                    (xl, h1a, xl + 2, h1b),
                    (xr - 2, h1a, xr, h1b),
                    (lv.0, yb - 2, lv.1, yb),
                ],
            ),
            (
                '╒',
                vec![
                    (xr - 2, h0a, xr, h0b),
                    (xr - 2, h1a, xr, h1b),
                    (lv.0, yb - 2, lv.1, yb),
                ],
            ),
            (
                '╓',
                vec![
                    (xr - 2, lh.0, xr, lh.1),
                    (v0a, yb - 2, v0b, yb),
                    (v1a, yb - 2, v1b, yb),
                ],
            ),
            (
                '╪',
                vec![
                    (xl, h0a, xl + 2, h0b),
                    (xr - 2, h0a, xr, h0b),
                    (xl, h1a, xl + 2, h1b),
                    (xr - 2, h1a, xr, h1b),
                    (lv.0, yt, lv.1, yt + 2),
                    (lv.0, yb - 2, lv.1, yb),
                ],
            ),
        ];
        for (c, boxes) in edges {
            let p = paint(&j.g, c);
            for b in boxes {
                assert!(p.covers(b), "{c:?} does not reach the edge at {b:?}");
            }
        }

        // Where two bands join they share pixels rather than merely touching: these squares are those
        // joins — the miters of a corner, and the place each band of an arm meets the line it joins —
        // and every one of them has to be solid ink.
        let joins: [(char, Vec<Box>); 10] = [
            ('╔', vec![(v0a, h0a, v0b, h0b), (v1a, h1a, v1b, h1b)]),
            ('╗', vec![(v0a, h1a, v0b, h1b), (v1a, h0a, v1b, h0b)]),
            ('╠', vec![(v1a, h0a, v1b, h0b), (v1a, h1a, v1b, h1b)]),
            ('╦', vec![(v0a, h1a, v0b, h1b), (v1a, h1a, v1b, h1b)]),
            (
                '╬',
                vec![
                    (v0a, h0a, v0b, h0b),
                    (v1a, h0a, v1b, h0b),
                    (v0a, h1a, v0b, h1b),
                    (v1a, h1a, v1b, h1b),
                ],
            ),
            ('╟', vec![(v1a, lh.0, v1b, lh.1)]),
            ('╤', vec![(lv.0, h1a, lv.1, h1b)]),
            ('╒', vec![(lv.0, h0a, lv.1, h0b), (lv.0, h1a, lv.1, h1b)]),
            ('╓', vec![(v0a, lh.0, v0b, lh.1), (v1a, lh.0, v1b, lh.1)]),
            ('╪', vec![(lv.0, h0a, lv.1, h0b), (lv.0, h1a, lv.1, h1b)]),
        ];
        for (c, boxes) in joins {
            let p = paint(&j.g, c);
            for b in boxes {
                assert!(p.covers(b), "{c:?} has a hollow joint at {b:?}");
            }
        }
    }
}
