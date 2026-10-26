//! The find-in-all-tabs box: a centered card like the palette, results grouped by tab.

use iced::widget::{
    button, column, container, mouse_area, opaque, rich_text, row, span, text, text_input,
};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Shadow, Vector};

use super::all::{FindAll, FindAllMsg, Group, TOTAL};
use crate::app::Message;
use crate::theme::{Tokens, UI_FONT};
use crate::ui::{MONO, to_color};

const CARD_WIDTH: f32 = 680.0;
const HEADER_HEIGHT: f32 = 30.0;
const ROW_HEIGHT: f32 = 28.0;
const NOTE_HEIGHT: f32 = 24.0;
/// Entries (headers, matches, notes) shown at once.
const VISIBLE: usize = 14;

/// One line of the results list.
enum Entry {
    Header(usize),
    /// Group, match in the group, position among the listed matches.
    Hit(usize, usize, usize),
    More,
}

fn entries(state: &FindAll) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut last = None;
    for (position, (g, h)) in state.listed().into_iter().enumerate() {
        if last != Some(g) {
            if let Some(prev) = last
                && state.groups()[prev].more
            {
                out.push(Entry::More);
            }
            out.push(Entry::Header(g));
            last = Some(g);
        }
        out.push(Entry::Hit(g, h, position));
    }
    if let Some(prev) = last
        && state.groups()[prev].more
    {
        out.push(Entry::More);
    }
    out
}

fn note<'a>(t: &'static Tokens, s: String) -> Element<'a, Message> {
    container(text(s).font(UI_FONT).size(12).color(t.text_muted))
        .padding(Padding::from([0.0, 16.0]))
        .height(NOTE_HEIGHT)
        .align_y(Alignment::Center)
        .into()
}

fn header<'a>(t: &'static Tokens, group: &'a Group) -> Element<'a, Message> {
    let accent = group.accent.map_or(t.border, to_color);
    let stripe = container(text(""))
        .width(3)
        .height(14)
        .style(move |_| container::Style {
            background: Some(accent.into()),
            border: Border {
                radius: 1.5.into(),
                ..Border::default()
            },
            ..container::Style::default()
        });
    let mut line = row![
        stripe,
        text(group.name.as_str())
            .font(UI_FONT)
            .size(12.5)
            .color(t.text)
            .wrapping(text::Wrapping::None)
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if let Some(w) = &group.window_label {
        line = line.push(
            text(w.as_str())
                .font(UI_FONT)
                .size(11.5)
                .color(t.text_muted),
        );
    }
    let count = format!("{}{}", group.hits.len(), if group.more { "+" } else { "" });
    line = line.push(
        container(text(count).font(MONO).size(11.5).color(t.text_muted))
            .width(Length::Fill)
            .align_x(Alignment::End),
    );
    container(line)
        .padding(Padding {
            top: 6.0,
            right: 16.0,
            bottom: 0.0,
            left: 14.0,
        })
        .height(HEADER_HEIGHT)
        .align_y(Alignment::Center)
        .into()
}

fn hit_row<'a>(
    t: &'static Tokens,
    window: iced::window::Id,
    group: &'a Group,
    h: usize,
    position: usize,
    selected: bool,
) -> Element<'a, Message> {
    let edge = container(text(""))
        .width(2)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: selected.then(|| t.accent_edge.into()),
            ..container::Style::default()
        });
    let line: Element<'a, Message> = match &group.hits[h].snippet {
        Some(s) => {
            let (a, b) = (s.highlight.start, s.highlight.end);
            rich_text([
                span(&s.text[..a]).color(t.text_secondary),
                span(&s.text[a..b])
                    .color(Color::WHITE)
                    .background(t.accent)
                    .border(Border {
                        radius: 3.0.into(),
                        ..Border::default()
                    }),
                span(&s.text[b..]).color(t.text_secondary),
            ])
            .on_link_click(iced::never)
            .font(MONO)
            .size(12.5)
            .wrapping(text::Wrapping::None)
            .into()
        }
        None => text("").into(),
    };
    let inner = row![
        edge,
        container(line)
            .padding(Padding::from([0.0, 14.0]))
            .width(Length::Fill)
            .clip(true)
    ]
    .align_y(Alignment::Center)
    .height(Length::Fill);
    button(inner)
        .on_press(Message::FindAll(window, FindAllMsg::Pick(position)))
        .width(Length::Fill)
        .height(ROW_HEIGHT)
        .padding(0)
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: if selected {
                    Some(t.accent_soft.into())
                } else {
                    hovered.then(|| t.hover.into())
                },
                text_color: t.text,
                border: Border::default(),
                ..button::Style::default()
            }
        })
        .into()
}

/// The box over `window`: a dimmed backdrop (click to close) with the card near the top.
pub fn view<'a>(
    t: &'static Tokens,
    window: iced::window::Id,
    state: &'a FindAll,
) -> Element<'a, Message> {
    let msg = move |m| Message::FindAll(window, m);
    let input = text_input("Find in all tabs", &state.query)
        .id(state.input.clone())
        .on_input(move |q| msg(FindAllMsg::Query(q)))
        .font(MONO)
        .size(15)
        .padding(Padding::from([14.0, 16.0]))
        .width(Length::Fill)
        .style(move |_, _| text_input::Style {
            background: Background::Color(Color::TRANSPARENT),
            border: Border::default(),
            icon: t.text_muted,
            placeholder: t.text_muted,
            value: t.text,
            selection: t.accent_soft,
        });
    let on = state.regex;
    let regex = button(text(".*").font(MONO).size(13))
        .on_press(msg(FindAllMsg::ToggleRegex))
        .padding(Padding::from([3.0, 8.0]))
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: if on {
                    Some(Background::Color(t.accent_soft))
                } else {
                    hovered.then_some(Background::Color(t.hover))
                },
                text_color: if on { t.accent_edge } else { t.text_muted },
                border: Border {
                    color: if on { t.accent_edge } else { t.border },
                    width: 1.0,
                    radius: 5.0.into(),
                },
                ..button::Style::default()
            }
        });
    let regex = crate::tooltip::tip(t, regex, "Regular expression", false);
    let top = row![input, container(regex).padding(Padding::from([0.0, 12.0]))]
        .align_y(Alignment::Center);

    let all = entries(state);
    let selected = state.selected_position();
    let focus = all
        .iter()
        .position(|e| matches!(e, Entry::Hit(_, _, p) if Some(*p) == selected))
        .unwrap_or(0);
    let start = (focus + 2).saturating_sub(VISIBLE);
    let mut list = column![];
    for e in all.iter().skip(start).take(VISIBLE) {
        list = list.push(match e {
            Entry::Header(g) => header(t, &state.groups()[*g]),
            Entry::Hit(g, h, p) => {
                hit_row(t, window, &state.groups()[*g], *h, *p, Some(*p) == selected)
            }
            Entry::More => note(t, "More matches in this tab".into()),
        });
    }
    let mut body = column![list];
    if let Some(status) = state.status() {
        let color = if state.invalid.is_some() {
            t.danger
        } else {
            t.text_muted
        };
        body = body.push(
            container(text(status).font(UI_FONT).size(13).color(color))
                .padding(Padding::from([14.0, 16.0])),
        );
    } else if state.total() > TOTAL {
        body = body.push(note(t, format!("Showing the first {TOTAL} matches")));
    } else if state.searching() {
        body = body.push(note(t, "Searching more tabs".into()));
    }
    let divider = container(text(""))
        .width(Length::Fill)
        .height(1)
        .style(move |_| container::Style {
            background: Some(t.border.into()),
            ..container::Style::default()
        });
    let card = container(column![
        top,
        divider,
        container(body).padding(Padding {
            top: 2.0,
            right: 0.0,
            bottom: 6.0,
            left: 0.0,
        })
    ])
    .width(CARD_WIDTH)
    .clip(true)
    .style(move |_| container::Style {
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
    });
    let backdrop = container(opaque(card))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .padding(Padding {
            top: 72.0,
            ..Padding::ZERO
        })
        .style(|_| container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.32).into()),
            ..container::Style::default()
        });
    mouse_area(backdrop).on_press(msg(FindAllMsg::Close)).into()
}
