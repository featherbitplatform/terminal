//! Color schemes for the terminal widget.

use fbterm_config::Rgb;

use crate::Terminal;

/// The colors a terminal is drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorScheme {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// Selection background, already flattened over `background`.
    pub selection: Rgb,
    pub ansi: [Rgb; 16],
}

const fn hex(v: u32) -> Rgb {
    Rgb {
        r: (v >> 16) as u8,
        g: (v >> 8) as u8,
        b: v as u8,
    }
}

impl ColorScheme {
    /// The dark "Graphite" scheme of the design system.
    pub fn graphite() -> Self {
        let background = hex(0x0b0b0e);
        Self {
            foreground: hex(0xf5f5f7),
            background,
            cursor: hex(0xa584ff),
            selection: blend(hex(0xa584ff), background, 0.30),
            ansi: [
                hex(0x262629), // black (ink-700)
                hex(0xed4a49), // red
                hex(0x2bbb71), // green
                hex(0xf2a635), // yellow
                hex(0x70b3f7), // blue
                hex(0xa584ff), // magenta (violet-400)
                hex(0x59c5f5), // cyan (cyan-400)
                hex(0xd0d0d9), // white (ink-200)
                hex(0x919199), // bright black (ink-400)
                hex(0xfd736d), // bright red
                hex(0x52cd86), // bright green
                hex(0xfac053), // bright yellow
                hex(0x9ccbfa), // bright blue
                hex(0xbea8ff), // bright magenta (violet-300)
                hex(0x8fd6f7), // bright cyan (cyan-300)
                hex(0xf5f5f7), // bright white (ink-50)
            ],
        }
    }

    /// Make the terminal answer color queries (OSC 10/11/4) with this scheme.
    pub fn apply_to(&self, term: &mut Terminal) {
        term.set_colors(self.foreground, self.background, self.cursor, self.ansi);
    }

    /// Color of a 256-color palette index under this scheme.
    pub fn indexed(&self, index: usize) -> Rgb {
        match index {
            0..=15 => self.ansi[index],
            16..=231 => {
                let i = (index - 16) as u8;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Rgb {
                    r: level(i / 36),
                    g: level(i / 6 % 6),
                    b: level(i % 6),
                }
            }
            232..=255 => {
                let v = 8 + (index - 232) as u8 * 10;
                Rgb { r: v, g: v, b: v }
            }
            _ => self.foreground,
        }
    }
}

/// `over * alpha + under * (1 - alpha)`, per channel.
pub fn blend(over: Rgb, under: Rgb, alpha: f32) -> Rgb {
    let mix = |o: u8, u: u8| (f32::from(o) * alpha + f32::from(u) * (1.0 - alpha)).round() as u8;
    Rgb {
        r: mix(over.r, under.r),
        g: mix(over.g, under.g),
        b: mix(over.b, under.b),
    }
}

/// WCAG 2 contrast ratio between two colors (1.0 to 21.0).
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f32 {
    fn luminance(c: Rgb) -> f32 {
        let lin = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
    }
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GridSize, TermEvent};

    #[test]
    fn graphite_uses_design_tokens() {
        let s = ColorScheme::graphite();
        assert_eq!(s.background, hex(0x0b0b0e));
        assert_eq!(s.foreground, hex(0xf5f5f7));
        assert_eq!(s.cursor, hex(0xa584ff));
        assert_eq!(s.ansi[1], hex(0xed4a49));
        assert_eq!(s.ansi[2], hex(0x2bbb71));
        assert_eq!(s.ansi[3], hex(0xf2a635));
        assert_eq!(s.ansi[4], hex(0x70b3f7));
        assert_eq!(s.ansi[5], hex(0xa584ff));
        assert_eq!(s.ansi[9], hex(0xfd736d));
        assert_eq!(s.ansi[10], hex(0x52cd86));
        assert_eq!(s.ansi[11], hex(0xfac053));
        // rgba(165,132,255,0.30) over #0b0b0e
        assert_eq!(s.selection, hex(0x392f56));
    }

    #[test]
    fn graphite_scheme_contrast_at_least_4_5() {
        let s = ColorScheme::graphite();
        for (i, c) in s.ansi.iter().enumerate().skip(1) {
            let ratio = contrast_ratio(*c, s.background);
            assert!(ratio >= 4.5, "ansi {i} {c} has contrast {ratio:.2}");
        }
    }

    #[test]
    fn contrast_ratio_extremes() {
        assert!((contrast_ratio(hex(0xffffff), hex(0x000000)) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(hex(0x777777), hex(0x777777)) - 1.0).abs() < 0.001);
    }

    #[test]
    fn indexed_covers_cube_and_greys() {
        let s = ColorScheme::graphite();
        assert_eq!(s.indexed(1), s.ansi[1]);
        assert_eq!(s.indexed(16), hex(0x000000));
        assert_eq!(s.indexed(231), hex(0xffffff));
        assert_eq!(s.indexed(232), hex(0x080808));
    }

    #[test]
    fn apply_to_answers_color_queries_with_scheme() {
        let mut t = Terminal::new(
            GridSize {
                cols: 80,
                rows: 24,
                cell_width: 8.0,
                cell_height: 16.0,
            },
            100,
        );
        ColorScheme::graphite().apply_to(&mut t);
        t.feed(b"\x1b]11;?\x07\x1b]4;1;?\x07");
        let replies: String = t
            .take_events()
            .into_iter()
            .filter_map(|e| match e {
                TermEvent::PtyWrite(b) => Some(String::from_utf8(b).unwrap()),
                _ => None,
            })
            .collect();
        assert!(replies.contains("rgb:0b0b/0b0b/0e0e"), "{replies:?}");
        assert!(replies.contains("rgb:eded/4a4a/4949"), "{replies:?}");
    }
}
