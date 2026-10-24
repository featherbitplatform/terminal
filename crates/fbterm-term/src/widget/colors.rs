//! Cell colors: palette lookup with OSC overrides, then inverse, dim, hidden and selection.

use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb as VteRgb};
use fbterm_config::Rgb;

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

pub(super) fn resolve(color: Color, colors: &Colors, scheme: &ColorScheme) -> Rgb {
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
