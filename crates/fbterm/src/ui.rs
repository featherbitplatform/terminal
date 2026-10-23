//! Shared chrome pieces for the editor and the dialogs: Graphite controls (radius 6), cards
//! (radius 8), overlays (radius 12), Lucide icons.

use iced::widget::{button, container, svg, text, text_input};
use iced::{Background, Border, Color, Element, Font, Padding, Shadow, Vector};

use crate::app::Message;
use crate::theme::{Tokens, UI_FONT};

/// Real values (hosts, ports, paths, fingerprints) are set in Geist Mono.
pub const MONO: Font = Font::with_name("Geist Mono");

// Lucide icons (ISC License, Copyright (c) Lucide Contributors).
pub const ICON_X: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></svg>"#;
pub const ICON_PLUS: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12h14"/><path d="M12 5v14"/></svg>"#;
pub const ICON_UP: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m18 15-6-6-6 6"/></svg>"#;
pub const ICON_DOWN: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></svg>"#;
pub const ICON_COPY: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>"#;
pub const ICON_SHIELD_ALERT: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z"/><path d="M12 8v4"/><path d="M12 16h.01"/></svg>"#;
pub const ICON_LOCK: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="18" height="11" x="3" y="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>"#;

pub fn icon<'a>(bytes: &'static [u8], size: f32, color: Color) -> Element<'a, Message> {
    svg(svg::Handle::from_memory(bytes))
        .width(size)
        .height(size)
        .style(move |_, _| svg::Style { color: Some(color) })
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
