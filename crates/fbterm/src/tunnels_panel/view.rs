//! The tunnels list's look: a title and search bar, then a card per profile with a row per
//! tunnel (state dot, name, addresses in Geist Mono, state, Retry or Restart to apply, switch).
//! Still laid out as the former Tunnels window's page; the panel view builds on it.
// Nothing draws the panel yet; the panel view task wires this into the dock and drops the allow.
#![allow(dead_code)]

use iced::widget::{Space, button, column, container, row, scrollable, text, text_input, toggler};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, window};

use super::{EMPTY, NO_MATCHES, RESTART_TO_APPLY, SEARCH_PLACEHOLDER, TunnelsMsg};
use crate::app::Message;
use crate::theme::{Tokens, UI_FONT};
use crate::tunnels_panel::{Group, Row, Tone};
use crate::ui::{self, MONO};

const CONTENT_WIDTH: f32 = 600.0;

fn msg(m: TunnelsMsg) -> Message {
    Message::Tunnels(m)
}

/// `groups`: the list for the current query; `any`: whether any profile has a tunnel at all.
pub fn view<'a>(
    t: &'static Tokens,
    wid: window::Id,
    query: &'a str,
    input: &iced::widget::Id,
    groups: Vec<Group>,
    any: bool,
) -> Element<'a, Message> {
    let search = text_input(SEARCH_PLACEHOLDER, query)
        .id(input.clone())
        .on_input(move |q| msg(TunnelsMsg::Query(wid, q)))
        .font(UI_FONT)
        .size(13.5)
        .padding(Padding {
            top: 8.0,
            right: 12.0,
            bottom: 8.0,
            left: 34.0,
        })
        .style(move |_, status| ui::input_style(t, status));
    let search = iced::widget::stack![
        search,
        container(ui::icon(ui::ICON_SEARCH, 14.0, t.text_muted))
            .padding(Padding {
                left: 12.0,
                ..Padding::ZERO
            })
            .center_y(Length::Fill),
    ];
    let mut body = column![text("Tunnels").font(UI_FONT).size(18).color(t.text), search,]
        .spacing(14)
        .width(Length::Fill);
    if groups.is_empty() {
        let note = if any { NO_MATCHES } else { EMPTY };
        body = body.push(
            container(text(note).font(UI_FONT).size(13).color(t.text_muted))
                .padding(Padding::from([18.0, 2.0])),
        );
    }
    for g in groups {
        body = body.push(group(t, wid, g));
    }
    let page = scrollable(
        container(container(body).max_width(CONTENT_WIDTH))
            .padding(Padding::from([24.0, 32.0]))
            .center_x(Length::Fill),
    )
    .direction(scrollable::Direction::Vertical(crate::ui::edge_scrollbar(
        crate::chrome::Chrome::current().draws_controls(),
        6.0,
        6.0,
        3.0,
    )))
    .style(move |theme, status| {
        let mut s = scrollable::default(theme, status);
        s.vertical_rail.background = None;
        s.vertical_rail.border = Border::default();
        s.vertical_rail.scroller.background = Background::Color(t.border);
        s.vertical_rail.scroller.border = Border {
            radius: 3.0.into(),
            ..Border::default()
        };
        s
    })
    .height(Length::Fill);
    container(page)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(t.app.into()),
            text_color: Some(t.text),
            ..container::Style::default()
        })
        .into()
}

/// A profile's tunnels: its name over a card of rows separated by hairlines.
fn group<'a>(t: &'static Tokens, wid: window::Id, g: Group) -> Element<'a, Message> {
    let mut list = column![];
    let count = g.rows.len();
    for (i, r) in g.rows.into_iter().enumerate() {
        list = list.push(tunnel_row(t, wid, r));
        if i + 1 < count {
            list = list.push(container(Space::new()).width(Length::Fill).height(1).style(
                move |_| container::Style {
                    background: Some(t.border.into()),
                    ..container::Style::default()
                },
            ));
        }
    }
    column![
        text(g.profile).font(UI_FONT).size(13.5).color(t.text),
        container(list)
            .width(Length::Fill)
            .style(move |_| ui::card(t)),
    ]
    .spacing(8)
    .into()
}

fn tone_color(t: &Tokens, tone: Tone) -> Color {
    match tone {
        Tone::Up => t.success,
        Tone::Busy => t.warning,
        Tone::Failed => t.danger,
        Tone::Off => t.text_muted,
    }
}

fn dot<'a>(color: Color) -> Element<'a, Message> {
    container(Space::new())
        .width(8)
        .height(8)
        .style(move |_| container::Style {
            background: Some(color.into()),
            border: Border {
                radius: 4.0.into(),
                ..Border::default()
            },
            ..container::Style::default()
        })
        .into()
}

fn small_button<'a>(
    t: &'static Tokens,
    caption: &'a str,
    on_press: Message,
    style: fn(&Tokens, button::Status) -> button::Style,
) -> Element<'a, Message> {
    button(text(caption).font(UI_FONT).size(12))
        .on_press(on_press)
        .padding(Padding::from([3.0, 10.0]))
        .style(move |_, status| style(t, status))
        .into()
}

fn tunnel_row<'a>(t: &'static Tokens, wid: window::Id, r: Row) -> Element<'a, Message> {
    let color = tone_color(t, r.tone);
    let state_color = if r.tone == Tone::Failed {
        t.danger
    } else {
        t.text_secondary
    };
    let words = column![
        text(r.name).font(UI_FONT).size(13.5).color(t.text),
        text(r.address)
            .font(MONO)
            .size(12)
            .color(t.text_secondary)
            .wrapping(text::Wrapping::WordOrGlyph),
        text(r.state)
            .font(UI_FONT)
            .size(12)
            .color(state_color)
            .wrapping(text::Wrapping::WordOrGlyph),
    ]
    .spacing(3)
    .width(Length::Fill);
    let mut actions = row![].spacing(10).align_y(Alignment::Center);
    if r.restart_to_apply {
        actions = actions.push(small_button(
            t,
            RESTART_TO_APPLY,
            msg(TunnelsMsg::Retry(wid, r.key.clone())),
            restart_style,
        ));
    }
    if r.retry {
        actions = actions.push(small_button(
            t,
            "Retry",
            msg(TunnelsMsg::Retry(wid, r.key.clone())),
            ui::secondary,
        ));
    }
    let key = r.key;
    actions = actions.push(
        toggler(r.on)
            .on_toggle(move |_| msg(TunnelsMsg::Toggle(wid, key.clone())))
            .size(18)
            .style(move |_, status| crate::settings_window::view::toggler_style(t, status, false)),
    );
    row![
        container(dot(color)).padding(Padding {
            top: 5.0,
            ..Padding::ZERO
        }),
        words,
        actions,
    ]
    .spacing(12)
    .align_y(Alignment::Start)
    .padding(Padding::from([12.0, 16.0]))
    .into()
}

/// The marker on a changed running tunnel: a warning-colored chip that restarts it.
fn restart_style(t: &Tokens, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(Background::Color(t.warning.scale_alpha(if hovered {
            0.24
        } else {
            0.14
        }))),
        text_color: t.warning,
        border: Border {
            radius: 6.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}
