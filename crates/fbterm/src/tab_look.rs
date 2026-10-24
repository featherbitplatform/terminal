//! How a tab is painted: its background, the colors of its label and icons, and the profile
//! accent (a stripe at the left, or the whole tab tinted).

use fbterm_config::{AccentStyle, Rgb};
use iced::gradient::Linear;
use iced::{Background, Color, Radians};

use crate::theme::{self, Tokens};

/// Accent share of an active tinted tab's color (over the terminal's background).
pub const ACTIVE_TINT: f32 = 0.34;
/// Accent share of an inactive tinted tab (over the bar).
pub const INACTIVE_TINT: f32 = 0.13;
/// Accent share of a hovered inactive tinted tab.
pub const HOVER_TINT: f32 = 0.20;
/// Where an active tinted tab's tint starts fading into the terminal's background (0 = top).
const FADE_FROM: f32 = 0.3;
/// Stops of that fade (at most 7: a gradient holds 8 stops).
const FADE_STEPS: u8 = 5;

/// What the bar needs to paint one tab.
#[derive(Debug, Clone, Copy)]
pub struct Surroundings {
    /// The terminal's background, opaque.
    pub terminal: Color,
    /// The bar's background, opaque.
    pub bar: Color,
    /// Background opacity (`background_opacity`).
    pub opacity: f32,
}

/// One tab's colors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabLook {
    pub background: Option<Background>,
    pub label: Color,
    /// The close button's icon.
    pub icon: Color,
    /// The accent stripe at the left, if shown.
    pub stripe: Option<Color>,
}

/// `k` of `a` over `b`, opaque.
pub fn mix(a: Color, b: Color, k: f32) -> Color {
    let l = |x: f32, y: f32| x * k + y * (1.0 - k);
    Color::from_rgb(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b))
}

/// WCAG relative luminance of an opaque color.
pub fn luminance(c: Color) -> f32 {
    let lin = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// WCAG contrast ratio of two opaque colors (1 to 21).
pub fn contrast(a: Color, b: Color) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The text colors that read best on `bg`: the dark theme's (light text) or the light
/// theme's (dark text).
pub fn ink(bg: Color) -> &'static Tokens {
    if contrast(theme::DARK.text, bg) >= contrast(theme::LIGHT.text, bg) {
        &theme::DARK
    } else {
        &theme::LIGHT
    }
}

fn to_color(c: Rgb) -> Color {
    Color::from_rgb8(c.r, c.g, c.b)
}

/// The colors of a tab with `accent` shown as `style`; `active` tabs take the terminal's
/// background so the two read as one surface.
pub fn look(
    t: &Tokens,
    accent: Option<Rgb>,
    style: AccentStyle,
    active: bool,
    hovered: bool,
    around: Surroundings,
) -> TabLook {
    let Surroundings {
        terminal,
        bar,
        opacity,
    } = around;
    match (accent.map(to_color), style) {
        (Some(accent), AccentStyle::Tab) => {
            if active {
                // Tinted at the top, fading into the terminal's background at the bottom.
                let tint = mix(accent, terminal, ACTIVE_TINT);
                let ink = ink(tint);
                // The renderer blends stops in linear light, which keeps the tint until
                // late; stops mixed here (in sRGB) make the fade look even.
                let mut gradient = Linear::new(Radians(std::f32::consts::PI))
                    .add_stop(0.0, tint.scale_alpha(opacity));
                for i in 0..=FADE_STEPS {
                    let f = i as f32 / FADE_STEPS as f32;
                    let color = mix(terminal, tint, f).scale_alpha(opacity);
                    gradient = gradient.add_stop(FADE_FROM + (1.0 - FADE_FROM) * f, color);
                }
                TabLook {
                    background: Some(Background::Gradient(gradient.into())),
                    label: ink.text,
                    icon: ink.text_secondary,
                    stripe: None,
                }
            } else {
                // Drawn over the bar, so a see-through tint works at any opacity.
                let k = if hovered { HOVER_TINT } else { INACTIVE_TINT };
                let ink = ink(mix(accent, bar, k));
                TabLook {
                    background: Some(Background::Color(Color { a: k, ..accent })),
                    label: ink.text_secondary,
                    icon: ink.text_muted,
                    stripe: None,
                }
            }
        }
        // The active tab is the terminal's surface, which need not follow the app theme
        // (a dark scheme under the light theme): its text follows its own contrast.
        (accent, _) if active => TabLook {
            background: Some(Background::Color(terminal.scale_alpha(opacity))),
            label: ink(terminal).text,
            icon: ink(terminal).text_muted,
            stripe: accent,
        },
        (accent, _) => TabLook {
            background: hovered.then_some(Background::Color(t.hover)),
            label: t.text_secondary,
            icon: t.text_muted,
            stripe: accent,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{DARK, LIGHT};

    const RED: Rgb = Rgb {
        r: 0xed,
        g: 0x4a,
        b: 0x49,
    };
    const YELLOW: Rgb = Rgb {
        r: 0xff,
        g: 0xe0,
        b: 0x40,
    };

    fn dark() -> Surroundings {
        Surroundings {
            terminal: DARK.app,
            bar: DARK.surface,
            opacity: 1.0,
        }
    }

    fn light() -> Surroundings {
        Surroundings {
            terminal: LIGHT.raised,
            bar: LIGHT.surface,
            opacity: 1.0,
        }
    }

    #[test]
    fn contrast_follows_wcag() {
        assert!((contrast(Color::BLACK, Color::WHITE) - 21.0).abs() < 0.01);
        assert!((contrast(Color::WHITE, Color::WHITE) - 1.0).abs() < 0.001);
        assert!(luminance(Color::WHITE) > luminance(DARK.text_muted));
        assert_eq!(
            mix(Color::WHITE, Color::BLACK, 0.25),
            Color::from_rgb(0.25, 0.25, 0.25)
        );
    }

    #[test]
    fn ink_picks_the_readable_text() {
        assert_eq!(ink(DARK.app).text, DARK.text);
        assert_eq!(ink(LIGHT.raised).text, LIGHT.text);
        assert_eq!(ink(to_color(YELLOW)).text, LIGHT.text);
        assert_eq!(ink(Color::from_rgb8(0x20, 0x10, 0x60)).text, DARK.text);
    }

    #[test]
    fn without_an_accent_or_as_a_stripe_a_tab_looks_as_before() {
        for (accent, style) in [
            (None, AccentStyle::Stripe),
            (None, AccentStyle::Tab),
            (Some(RED), AccentStyle::Stripe),
        ] {
            let active = look(&DARK, accent, style, true, false, dark());
            assert_eq!(active.background, Some(Background::Color(DARK.app)));
            assert_eq!(active.label, DARK.text);
            assert_eq!(active.stripe, accent.map(to_color));
            let idle = look(&DARK, accent, style, false, false, dark());
            assert_eq!(idle.background, None);
            assert_eq!(idle.label, DARK.text_secondary);
            let hover = look(&DARK, accent, style, false, true, dark());
            assert_eq!(hover.background, Some(Background::Color(DARK.hover)));
        }
    }

    #[test]
    fn a_plain_active_tab_reads_on_the_terminal_not_the_theme() {
        // Light app theme over a dark terminal scheme.
        let around = Surroundings {
            terminal: DARK.app,
            bar: LIGHT.surface,
            opacity: 1.0,
        };
        let active = look(&LIGHT, None, AccentStyle::Stripe, true, false, around);
        assert_eq!(active.label, DARK.text);
        let idle = look(&LIGHT, None, AccentStyle::Stripe, false, false, around);
        assert_eq!(idle.label, LIGHT.text_secondary);
    }

    #[test]
    fn a_whole_tab_accent_tints_active_tabs_more_and_keeps_text_readable() {
        for (t, around) in [(&DARK, dark()), (&LIGHT, light())] {
            for accent in [RED, YELLOW] {
                let active = look(t, Some(accent), AccentStyle::Tab, true, false, around);
                let idle = look(t, Some(accent), AccentStyle::Tab, false, false, around);
                let hover = look(t, Some(accent), AccentStyle::Tab, false, true, around);
                assert_eq!(active.stripe, None);
                let Some(Background::Color(i)) = idle.background else {
                    panic!("inactive tint is a color")
                };
                let Some(Background::Color(h)) = hover.background else {
                    panic!("hover tint is a color")
                };
                assert!(ACTIVE_TINT > HOVER_TINT && h.a > i.a, "hover shows");
                assert!(matches!(active.background, Some(Background::Gradient(_))));
                let tint = mix(to_color(accent), around.terminal, ACTIVE_TINT);
                assert!(contrast(active.label, tint) >= 4.5, "{accent} active");
                let idle_bg = mix(to_color(accent), around.bar, INACTIVE_TINT);
                assert!(contrast(idle.label, idle_bg) >= 4.5, "{accent} inactive");
            }
        }
    }

    #[test]
    fn an_active_tint_fades_into_the_terminal_at_the_bars_opacity() {
        let around = Surroundings {
            opacity: 0.6,
            ..dark()
        };
        let active = look(&DARK, Some(RED), AccentStyle::Tab, true, false, around);
        let Some(Background::Gradient(iced::Gradient::Linear(g))) = active.background else {
            panic!("gradient")
        };
        let stops: Vec<_> = g.stops.iter().flatten().collect();
        assert_eq!(stops.last().unwrap().color, DARK.app.scale_alpha(0.6));
        assert!(stops.iter().all(|s| (s.color.a - 0.6).abs() < 1e-6));
    }
}
