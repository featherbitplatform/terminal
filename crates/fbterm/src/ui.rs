//! Shared chrome pieces for the editor and the dialogs: Graphite controls (radius 6), cards
//! (radius 8), overlays (radius 12), Lucide icons.

use iced::advanced::widget::{Id, Operation, operation::Focusable};
use iced::widget::{Space, button, column, container, row, scrollable, text, text_input};
use iced::{
    Alignment, Background, Border, Color, Element, Font, Length, Padding, Rectangle, Shadow, Task,
    Vector, window,
};

use crate::app::Message;
use crate::chrome::{
    self, BarAction, Chrome, Control, LINUX_CONTROL_GAP, LINUX_CONTROL_SIZE, Platform, Side,
    WINDOWS_CONTROL_WIDTH,
};
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
pub const ICON_FOLDER: &str = "\u{e0d7}";
pub const ICON_FILE: &str = "\u{e0c0}";
pub const ICON_FOLDER_LINK: &str = "\u{e33b}"; // folder-symlink
pub const ICON_FILE_LINK: &str = "\u{e327}"; // file-symlink
pub const ICON_PARENT: &str = "\u{e0a4}"; // corner-left-up
pub const ICON_FILE_OTHER: &str = "\u{e322}"; // file-question-mark
pub const ICON_MINUS: &str = "\u{e11c}"; // minimize
pub const ICON_SQUARE: &str = "\u{e167}"; // maximize (`copy` restores)

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

/// A filled circle `size` across: the update dot on the settings button and in Settings.
pub fn dot<'a>(color: Color, size: f32) -> Element<'a, Message> {
    container(Space::new())
        .width(size)
        .height(size)
        .style(move |_| container::Style {
            background: Some(color.into()),
            border: Border {
                radius: (size / 2.0).into(),
                ..Border::default()
            },
            ..container::Style::default()
        })
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

/// The least margin of a scrollbar along a window's right edge where the chrome is drawn: the
/// bar then lies clear of the resize band, so a press on it scrolls instead of resizing.
pub const EDGE_SCROLLBAR_MARGIN: f32 = chrome::EDGE_BAND + 1.0;

/// The margin a scrollbar along a window's right edge gets: `margin`, or at least
/// [`EDGE_SCROLLBAR_MARGIN`] where the chrome is `drawn`.
pub fn edge_scrollbar_margin(drawn: bool, margin: f32) -> f32 {
    if drawn {
        margin.max(EDGE_SCROLLBAR_MARGIN)
    } else {
        margin
    }
}

/// A vertical scrollbar `width` wide (its scroller `scroller_width`) with `margin` around it,
/// for a scrollable along a window's right edge: clear of the resize band where the chrome is
/// `drawn` (see [`edge_scrollbar_margin`]).
pub fn edge_scrollbar(
    drawn: bool,
    width: f32,
    scroller_width: f32,
    margin: f32,
) -> scrollable::Scrollbar {
    scrollable::Scrollbar::new()
        .width(width)
        .scroller_width(scroller_width)
        .margin(edge_scrollbar_margin(drawn, margin))
}

/// The bar of the editor and Settings windows where the OS title bar is gone: room for the
/// window buttons (or the drawn controls) and to move the window. It stays blank otherwise;
/// the window's content titles it.
pub fn window_header<'a>(
    t: &'static Tokens,
    window: window::Id,
    maximized: bool,
) -> Element<'a, Message> {
    let chrome = Chrome::current();
    let height = chrome.bar_height();
    let inside: Element<'a, Message> = if chrome.draws_controls() {
        let controls = window_controls(t, window, maximized, height);
        match chrome::setup().controls.side {
            Side::Left => row![controls, Space::new().width(Length::Fill)].into(),
            Side::Right => row![Space::new().width(Length::Fill), controls].into(),
        }
    } else {
        text("").into()
    };
    let header = container(inside)
        .width(Length::Fill)
        .height(height)
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
    chrome::title_bar(chrome, column![header, rule], false, move |a| {
        Message::TitleBar(window, a)
    })
}

/// The Windows close button's hover color, white glyph on it.
const WINDOWS_CLOSE_RED: Color = Color::from_rgb(
    0xc4 as f32 / 255.0,
    0x2b as f32 / 255.0,
    0x1c as f32 / 255.0,
);

/// The drawn window controls (minimize, maximize or restore, close) of `window`, in this
/// run's order and its system's style, for a bar `height` tall. Each publishes its
/// [`BarAction`] as [`Message::TitleBar`].
pub fn window_controls<'a>(
    t: &'static Tokens,
    window: window::Id,
    maximized: bool,
    height: f32,
) -> Element<'a, Message> {
    let setup = chrome::setup();
    let windows = setup.look == Platform::Windows;
    let mut controls = row![].align_y(Alignment::Center).height(height);
    if !windows {
        controls = controls
            .spacing(LINUX_CONTROL_GAP)
            .padding(Padding::from([0.0, LINUX_CONTROL_GAP]));
    }
    for &control in &setup.controls.order {
        let (glyph, action) = match control {
            Control::Minimize => (ICON_MINUS, BarAction::Minimize),
            Control::Maximize if maximized => (ICON_COPY, BarAction::ToggleMaximize),
            Control::Maximize => (ICON_SQUARE, BarAction::ToggleMaximize),
            Control::Close => (ICON_X, BarAction::Close),
        };
        let close = control == Control::Close;
        // No color of its own: the glyph takes the button style's text color.
        let glyph = text(glyph)
            .font(ICONS)
            .size(if windows { 14.0 } else { 13.0 })
            .line_height(text::LineHeight::Relative(1.0))
            .shaping(text::Shaping::Basic)
            .wrapping(text::Wrapping::None);
        let (width, height) = if windows {
            (WINDOWS_CONTROL_WIDTH, height)
        } else {
            (LINUX_CONTROL_SIZE, LINUX_CONTROL_SIZE)
        };
        let b = button(container(glyph).center(Length::Fill))
            .on_press(Message::TitleBar(window, action))
            .padding(0)
            .width(width)
            .height(height)
            .style(move |_, status| {
                if windows {
                    windows_control(t, status, close)
                } else {
                    linux_control(t, status)
                }
            });
        controls = controls.push(b);
    }
    controls.into()
}

/// A Windows caption button: flat, a light wash on hover (lighter while pressed), the close
/// button red with a white glyph.
fn windows_control(t: &Tokens, status: button::Status, close: bool) -> button::Style {
    let background = match (status, close) {
        (button::Status::Hovered, true) => Some(WINDOWS_CLOSE_RED),
        (button::Status::Pressed, true) => Some(WINDOWS_CLOSE_RED.scale_alpha(0.8)),
        (button::Status::Hovered, false) => Some(t.text.scale_alpha(0.08)),
        (button::Status::Pressed, false) => Some(t.text.scale_alpha(0.05)),
        _ => None,
    };
    let text_color = if close && hovered(status) {
        Color::WHITE
    } else {
        t.text_secondary
    };
    button::Style {
        background: background.map(Background::Color),
        text_color,
        border: Border::default(),
        ..button::Style::default()
    }
}

/// A Linux title-bar button: a round, faint plate that deepens on hover and more while
/// pressed.
fn linux_control(t: &Tokens, status: button::Status) -> button::Style {
    let alpha = match status {
        button::Status::Hovered => 0.14,
        button::Status::Pressed => 0.22,
        _ => 0.08,
    };
    button::Style {
        background: Some(Background::Color(t.text.scale_alpha(alpha))),
        text_color: if hovered(status) {
            t.text
        } else {
            t.text_secondary
        },
        border: Border {
            radius: (LINUX_CONTROL_SIZE / 2.0).into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
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

#[cfg(test)]
mod tests {
    use super::edge_scrollbar_margin;
    use crate::chrome::EDGE_BAND;

    #[test]
    fn edge_scrollbars_clear_the_resize_band_only_where_drawn() {
        // Drawn chrome: the bar (between its margins) starts past the band.
        for margin in [0.0, 1.0, 3.0] {
            assert!(edge_scrollbar_margin(true, margin) > EDGE_BAND);
        }
        assert_eq!(edge_scrollbar_margin(true, 8.0), 8.0);
        // Elsewhere (macOS, the system title bar) nothing changes.
        for margin in [0.0, 1.0, 3.0] {
            assert_eq!(edge_scrollbar_margin(false, margin), margin);
        }
    }
}

/// Lets go of the keyboard in the fields `ids` only (other fields and windows keep theirs).
pub(crate) fn unfocus(ids: Vec<Id>) -> Task<Message> {
    struct Unfocus(Vec<Id>);
    impl<T> Operation<T> for Unfocus {
        fn focusable(&mut self, id: Option<&Id>, _bounds: Rectangle, state: &mut dyn Focusable) {
            if id.is_some_and(|id| self.0.contains(id)) {
                state.unfocus();
            }
        }

        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
            operate(self);
        }
    }
    iced::advanced::widget::operate(Unfocus(ids))
}
