//! Design system v2 "Graphite" as an iced theme, plus the tokens the chrome draws with.

use fbterm_config::ThemeMode;
use iced::theme::{Palette, palette};
use iced::{Color, Font, Theme, color};

/// UI text face (bundled).
pub const UI_FONT: Font = Font::with_name("Geist");

/// Values the chrome uses directly, beyond what an iced palette carries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tokens {
    pub app: Color,
    pub surface: Color,
    pub raised: Color,
    pub border: Color,
    pub hover: Color,
    pub text: Color,
    pub text_secondary: Color,
    pub text_muted: Color,
    pub accent: Color,
    pub accent_soft: Color,
    /// Edge of the selected row (a lighter violet on dark).
    pub accent_edge: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub danger_soft: Color,
    /// The terminal's default background (the Graphite color scheme's, in both modes).
    pub terminal: Color,
}

pub const DARK: Tokens = Tokens {
    app: color!(0x101013),
    surface: color!(0x151518),
    raised: color!(0x1f1f22),
    border: color!(0x313135),
    hover: Color::from_rgba(1.0, 1.0, 1.0, 0.045),
    text: color!(0xf5f5f7),
    text_secondary: color!(0xb6b6bf),
    text_muted: color!(0x919199),
    accent: color!(0x7c49e3),
    accent_soft: Color::from_rgba(165.0 / 255.0, 132.0 / 255.0, 1.0, 0.14),
    accent_edge: color!(0xa584ff),
    success: color!(0x2bbb71),
    warning: color!(0xf2a635),
    danger: color!(0xed4a49),
    danger_soft: Color::from_rgba(237.0 / 255.0, 74.0 / 255.0, 73.0 / 255.0, 0.14),
    terminal: color!(0x0b0b0e),
};

pub const LIGHT: Tokens = Tokens {
    app: color!(0xfafafb),
    surface: color!(0xfdfdfe),
    raised: color!(0xffffff),
    border: color!(0xdadadf),
    hover: Color::from_rgba(28.0 / 255.0, 28.0 / 255.0, 34.0 / 255.0, 0.04),
    text: color!(0x1c1c22),
    text_secondary: color!(0x4f4f56),
    text_muted: color!(0x686870),
    accent: color!(0x7c49e3),
    accent_soft: Color::from_rgba(124.0 / 255.0, 73.0 / 255.0, 227.0 / 255.0, 0.09),
    accent_edge: color!(0x7c49e3),
    success: color!(0x00884b),
    warning: color!(0xbe7100),
    danger: color!(0xc52b30),
    danger_soft: Color::from_rgba(197.0 / 255.0, 43.0 / 255.0, 48.0 / 255.0, 0.08),
    terminal: color!(0x0b0b0e),
};

pub fn tokens(mode: ThemeMode) -> &'static Tokens {
    match mode {
        ThemeMode::Dark => &DARK,
        ThemeMode::Light => &LIGHT,
    }
}

/// The app theme: Graphite neutrals, violet as the single accent.
pub fn graphite_theme(mode: ThemeMode) -> Theme {
    let t = *tokens(mode);
    let name = match mode {
        ThemeMode::Dark => "Graphite",
        ThemeMode::Light => "Graphite Light",
    };
    let base = Palette {
        background: t.app,
        text: t.text,
        primary: t.accent,
        success: t.success,
        warning: t.warning,
        danger: t.danger,
    };
    Theme::custom_with_fn(name, base, move |p| {
        let mut ext = palette::Extended::generate(p);
        ext.background.weak = palette::Pair::new(t.surface, t.text);
        ext.background.neutral = palette::Pair::new(t.raised, t.text);
        ext.background.strong = palette::Pair::new(t.border, t.text);
        ext.primary.base = palette::Pair::new(t.accent, Color::WHITE);
        ext
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_palette_uses_graphite_values() {
        let p = graphite_theme(ThemeMode::Dark).palette();
        assert_eq!(p.background, color!(0x101013));
        assert_eq!(p.text, color!(0xf5f5f7));
        assert_eq!(p.primary, color!(0x7c49e3));
        assert_eq!(p.success, color!(0x2bbb71));
        assert_eq!(p.danger, color!(0xed4a49));
    }

    #[test]
    fn terminal_token_is_the_terminal_background() {
        let bg = fbterm_term::ColorScheme::graphite().background;
        for t in [&DARK, &LIGHT] {
            assert_eq!(t.terminal, Color::from_rgb8(bg.r, bg.g, bg.b));
        }
    }

    #[test]
    fn light_palette_comes_from_the_light_tokens() {
        let p = graphite_theme(ThemeMode::Light).palette();
        assert_eq!(p.background, color!(0xfafafb));
        assert_eq!(p.text, color!(0x1c1c22));
        assert_eq!(p.primary, color!(0x7c49e3));
    }
}
