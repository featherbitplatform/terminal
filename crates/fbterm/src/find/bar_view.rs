//! The find bar: a slim floating bar at the top right of the tab's terminal.

use fbterm_term::{Cover, GridSize};
use iced::widget::{button, container, opaque, row, text, text_input};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Shadow, Vector};

use super::{FindBar, FindMsg};
use crate::app::Message;
use crate::keys::{Action, Bindings};
use crate::theme::Tokens;
use crate::tooltip::{tip, tooltip_text};
use crate::ui::{ICON_DOWN, ICON_UP, ICON_X, MONO, icon};

const BAR_WIDTH: f32 = 460.0;
/// Space between the bar and the terminal's top and right edges.
const TOP: f32 = 6.0;
const RIGHT: f32 = 10.0;
/// Height of the bar (its field plus padding and border).
const BAR_HEIGHT: f32 = 36.0;

/// The bar's width over a terminal `width` wide: narrower on narrow windows.
fn bar_width(width: f32) -> f32 {
    BAR_WIDTH.min(width - 2.0 * RIGHT).max(0.0)
}

/// The cells of a terminal of `size` the bar is drawn over.
pub fn cover(size: GridSize) -> Cover {
    let (cw, ch) = (size.cell_width.max(1.0), size.cell_height.max(1.0));
    let width = f32::from(size.cols) * cw;
    let left = width - RIGHT - bar_width(width);
    Cover {
        rows: ((TOP + BAR_HEIGHT) / ch).ceil() as usize,
        from_col: (left / cw).floor().max(0.0) as usize,
    }
}

fn ghost(t: &Tokens, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: hovered.then_some(Background::Color(t.hover)),
        text_color: t.text_secondary,
        border: Border {
            radius: 5.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

/// The ".*" toggle: accent-tinted while regular expressions are on.
fn toggle(t: &Tokens, on: bool, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: if on {
            Some(Background::Color(t.accent_soft))
        } else {
            hovered.then_some(Background::Color(t.hover))
        },
        text_color: if on { t.accent_edge } else { t.text_muted },
        border: Border {
            color: if on {
                t.accent_edge
            } else {
                Color::TRANSPARENT
            },
            width: 1.0,
            radius: 5.0.into(),
        },
        ..button::Style::default()
    }
}

/// The bar, placed over the top right of `window`'s terminal area.
pub fn view<'a>(
    t: &'static Tokens,
    window: iced::window::Id,
    bar: &'a FindBar,
    bindings: &Bindings,
) -> Element<'a, Message> {
    let msg = move |m| Message::Find(window, m);
    let input = text_input("Find", &bar.query)
        .id(bar.input.clone())
        .on_input(move |q| msg(FindMsg::Query(q)))
        .font(MONO)
        .size(13)
        .padding(Padding::from([5.0, 8.0]))
        .width(Length::Fill)
        .style(move |_, _| text_input::Style {
            background: Background::Color(Color::TRANSPARENT),
            border: Border::default(),
            icon: t.text_muted,
            placeholder: t.text_muted,
            value: t.text,
            selection: t.accent_soft,
        });
    let counter = bar.counter();
    let counter_color = if bar.invalid.is_some() {
        t.danger
    } else {
        t.text_muted
    };
    let counter = container(
        text(counter)
            .font(MONO)
            .size(12)
            .color(counter_color)
            .wrapping(text::Wrapping::None),
    )
    .align_x(Alignment::End);
    let regex = button(text(".*").font(MONO).size(12.5))
        .on_press(msg(FindMsg::ToggleRegex))
        .padding(Padding::from([2.0, 6.0]))
        .style(move |_, status| toggle(t, bar.regex, status));
    let regex = tip(t, regex, "Regular expression", false);
    let nav = |glyph, m, label, action| {
        let b = button(icon(glyph, 14.0, t.text_secondary))
            .on_press(msg(m))
            .padding(4)
            .style(move |_, status| ghost(t, status));
        tip(t, b, tooltip_text(label, action, bindings), false)
    };
    let older = nav(ICON_UP, FindMsg::Next, "Older match", Action::FindNext);
    let newer = nav(
        ICON_DOWN,
        FindMsg::Previous,
        "Newer match",
        Action::FindPrevious,
    );
    let close = button(icon(ICON_X, 13.0, t.text_muted))
        .on_press(msg(FindMsg::Close))
        .padding(4)
        .style(move |_, status| ghost(t, status));
    let close = tip(t, close, "Close (Esc)", false);
    let content = row![input, counter, regex, older, newer, close]
        .spacing(4)
        .align_y(Alignment::Center);
    let card = container(content)
        .width(Length::Fill)
        .max_width(BAR_WIDTH)
        .padding(Padding {
            top: 3.0,
            right: 4.0,
            bottom: 3.0,
            left: 4.0,
        })
        .style(move |_| container::Style {
            background: Some(t.raised.into()),
            border: Border {
                color: t.border,
                width: 1.0,
                radius: 8.0.into(),
            },
            shadow: Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
                offset: Vector::new(0.0, 6.0),
                blur_radius: 18.0,
            },
            ..container::Style::default()
        });
    container(opaque(card))
        .width(Length::Fill)
        .align_x(Alignment::End)
        .padding(Padding {
            top: TOP,
            right: RIGHT,
            bottom: 0.0,
            left: RIGHT,
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_covers_the_top_right_cells_and_narrows_on_small_windows() {
        let size = GridSize {
            cols: 100,
            rows: 40,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let c = cover(size);
        assert_eq!(c.rows, 3);
        // 800 px wide: the bar spans 330..790.
        assert_eq!(c.from_col, 41);
        let narrow = GridSize { cols: 30, ..size };
        assert_eq!(bar_width(240.0), 220.0);
        assert_eq!(cover(narrow).from_col, 1);
    }
}
