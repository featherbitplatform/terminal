//! Shared chrome pieces for the editor and the dialogs: Graphite controls (radius 6), cards
//! (radius 8), overlays (radius 12), Lucide icons.

use iced::widget::{button, column, container, text, text_input};
use iced::{Background, Border, Color, Element, Font, Length, Padding, Shadow, Vector, window};

use crate::app::Message;
use crate::theme::{Tokens, UI_FONT};

/// Real values (hosts, ports, paths, fingerprints) are set in Geist Mono.
pub const MONO: Font = Font::with_name("Geist Mono");

/// The bundled Lucide icon font (ISC License, Copyright (c) Lucide Contributors), subset to
/// the icons below by `dev/subset-icons.sh`. Icons are glyphs, so they come from the glyph
/// atlas that text already uses, not from an image atlas.
pub const ICONS: Font = Font::with_name("lucide");

// Glyphs of the icon font, by Lucide icon name.
pub const ICON_X: &str = "\u{e1b2}";
pub const ICON_PLUS: &str = "\u{e13d}";
pub const ICON_USERS: &str = "\u{e1a4}";
pub const ICON_UP: &str = "\u{e070}"; // chevron-up
pub const ICON_DOWN: &str = "\u{e06d}"; // chevron-down
pub const ICON_COPY: &str = "\u{e09e}";
pub const ICON_SHIELD_ALERT: &str = "\u{e1fe}";
pub const ICON_LOCK: &str = "\u{e10b}";
pub const ICON_SETTINGS: &str = "\u{e154}";
pub const ICON_RESET: &str = "\u{e148}"; // rotate-ccw
pub const ICON_WARNING: &str = "\u{e193}"; // triangle-alert
pub const ICON_GRIP: &str = "\u{e0ea}"; // grip-horizontal
pub const ICON_TUNNELS: &str = "\u{e4e3}"; // cable
pub const ICON_SEARCH: &str = "\u{e151}";

/// An icon `size` pixels square. The font draws each icon on the full em square (the 24-unit
/// SVG grid scaled to the em, baseline at the bottom), so a one-em line holds it exactly.
pub fn icon<'a>(glyph: &'static str, size: f32, color: Color) -> Element<'a, Message> {
    text(glyph)
        .font(ICONS)
        .size(size)
        .line_height(text::LineHeight::Absolute(size.into()))
        .shaping(text::Shaping::Basic)
        .wrapping(text::Wrapping::None)
        .color(color)
        .width(size)
        .height(size)
        .into()
}

pub fn label<'a>(t: &Tokens, s: impl text::IntoFragment<'a>) -> text::Text<'a> {
    text(s).font(UI_FONT).size(12).color(t.text_secondary)
}

pub fn input_style(t: &Tokens, status: text_input::Status) -> text_input::Style {
    let (border, background) = match status {
        text_input::Status::Focused { .. } => (t.accent_edge, t.app),
        text_input::Status::Hovered => (t.text_muted, t.app),
        text_input::Status::Active => (t.border, t.app),
        text_input::Status::Disabled => (t.border, t.surface),
    };
    text_input::Style {
        background: Background::Color(background),
        border: Border {
            color: border,
            width: 1.0,
            radius: 6.0.into(),
        },
        icon: t.text_muted,
        placeholder: t.text_muted,
        value: if matches!(status, text_input::Status::Disabled) {
            t.text_secondary
        } else {
            t.text
        },
        selection: t.accent_soft,
    }
}

fn hovered(status: button::Status) -> bool {
    matches!(status, button::Status::Hovered | button::Status::Pressed)
}

pub fn primary(t: &Tokens, status: button::Status) -> button::Style {
    let disabled = matches!(status, button::Status::Disabled);
    button::Style {
        background: Some(Background::Color(if disabled {
            t.raised
        } else if hovered(status) {
            t.accent_edge
        } else {
            t.accent
        })),
        text_color: if disabled { t.text_muted } else { Color::WHITE },
        border: Border {
            color: t.border,
            width: if disabled { 1.0 } else { 0.0 },
            radius: 6.0.into(),
        },
        ..button::Style::default()
    }
}

pub fn secondary(t: &Tokens, status: button::Status) -> button::Style {
    let disabled = matches!(status, button::Status::Disabled);
    button::Style {
        background: Some(Background::Color(if hovered(status) {
            t.border
        } else {
            t.raised
        })),
        text_color: if disabled { t.text_muted } else { t.text },
        border: Border {
            color: t.border,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..button::Style::default()
    }
}

pub fn ghost(t: &Tokens, status: button::Status) -> button::Style {
    button::Style {
        background: hovered(status).then_some(Background::Color(t.hover)),
        text_color: if matches!(status, button::Status::Disabled) {
            t.text_muted
        } else {
            t.text_secondary
        },
        border: Border {
            radius: 6.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

pub fn danger_ghost(t: &Tokens, status: button::Status) -> button::Style {
    button::Style {
        background: hovered(status).then_some(Background::Color(t.danger_soft)),
        text_color: if matches!(status, button::Status::Disabled) {
            t.text_muted
        } else {
            t.danger
        },
        border: Border {
            radius: 6.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

/// A text button in one of the styles above.
pub fn text_button<'a>(
    t: &'static Tokens,
    caption: &'a str,
    on_press: Option<Message>,
    style: fn(&Tokens, button::Status) -> button::Style,
) -> button::Button<'a, Message> {
    button(text(caption).font(UI_FONT).size(13))
        .on_press_maybe(on_press)
        .padding(Padding::from([6.0, 14.0]))
        .style(move |_, status| style(t, status))
}

/// A card surface (radius 8).
pub fn card(t: &Tokens) -> container::Style {
    container::Style {
        background: Some(t.surface.into()),
        border: Border {
            color: t.border,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..container::Style::default()
    }
}

/// A floating overlay card (radius 12).
pub fn overlay(t: &Tokens) -> container::Style {
    container::Style {
        background: Some(t.raised.into()),
        border: Border {
            color: t.border,
            width: 1.0,
            radius: 12.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.45),
            offset: Vector::new(0.0, 12.0),
            blur_radius: 32.0,
        },
        ..container::Style::default()
    }
}

/// A mono value on an inset well, for fingerprints and commands.
pub fn value_well<'a>(
    t: &'static Tokens,
    value: impl text::IntoFragment<'a>,
) -> container::Container<'a, Message> {
    well(t, text(value).font(MONO).size(12.5).color(t.text))
}

/// An inset well (radius 6) around `content`.
pub fn well<'a>(
    t: &'static Tokens,
    content: impl Into<Element<'a, Message>>,
) -> container::Container<'a, Message> {
    container(content)
        .padding(Padding::from([8.0, 10.0]))
        .style(move |_| container::Style {
            background: Some(t.app.into()),
            border: Border {
                color: t.border,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..container::Style::default()
        })
}

pub fn to_color(c: fbterm_config::Rgb) -> Color {
    Color::from_rgb8(c.r, c.g, c.b)
}

/// The bar of the editor and Settings windows where the OS title bar is gone: room for the
/// window buttons and to move the window. It stays blank; the window's content titles it.
pub fn window_header<'a>(t: &'static Tokens, window: window::Id) -> Element<'a, Message> {
    let header = container(text(""))
        .width(Length::Fill)
        .height(crate::chrome::Chrome::current().bar_height())
        .style(move |_| container::Style {
            background: Some(t.surface.into()),
            ..container::Style::default()
        });
    let rule = container(text(""))
        .width(Length::Fill)
        .height(1)
        .style(move |_| container::Style {
            background: Some(t.border.into()),
            ..container::Style::default()
        });
    crate::chrome::title_bar(column![header, rule], false, move |a| {
        Message::TitleBar(window, a)
    })
}

#[cfg(test)]
mod icon_font_tests {
    #[test]
    fn bundled_icon_font_has_every_icon_codepoint() {
        use read_fonts::{FontRef, TableProvider};
        let font = FontRef::new(include_bytes!("../assets/fonts/lucide-icons.ttf"))
            .expect("icon font parses");
        let cmap = font.cmap().expect("icon font has a cmap");
        let src = include_str!("ui.rs");
        let mut seen = 0;
        for line in src.lines() {
            let Some(rest) = line.strip_prefix("pub const ICON_") else {
                continue;
            };
            let Some(hex) = rest.split("\\u{").nth(1).and_then(|r| r.split('}').next()) else {
                continue;
            };
            let ch = char::from_u32(u32::from_str_radix(hex, 16).unwrap()).unwrap();
            assert!(
                cmap.map_codepoint(ch).is_some(),
                "missing U+{hex} in {rest}"
            );
            seen += 1;
        }
        assert!(seen >= 8, "found only {seen} ICON_ constants");
    }
}
