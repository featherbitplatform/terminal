//! The tunnels panel's look, docked at the right edge of a terminal window like the file
//! panel (same rule along its left edge, same surface): a header with the title and a close
//! button, the search field, then a card per profile with a row per tunnel (state dot, name,
//! addresses in Geist Mono, state, Retry or Restart to apply, switch). The highlighted
//! profile's group comes first, its name in the accent and its card edged in it.

use iced::widget::{Space, button, column, container, row, scrollable, text, text_input, toggler};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, window};

use super::{EMPTY, NO_MATCHES, RESTART_TO_APPLY, SEARCH_PLACEHOLDER, TunnelsMsg};
use crate::app::{App, Message};
use crate::file_panel::view::RULE_WIDTH;
use crate::theme::{Tokens, UI_FONT};
use crate::tunnels_panel::{Group, Row, Tone};
use crate::ui::{self, ICON_SEARCH, ICON_X, MONO, icon};

/// The panel's side padding, as the file panel's.
const PAD_X: f32 = 12.0;

fn msg(m: TunnelsMsg) -> Message {
    Message::Tunnels(m)
}

/// `wid`'s panel and its width, when it is open.
pub(crate) fn panel(app: &App, wid: window::Id) -> Option<(Element<'_, Message>, f32)> {
    let panels = &app.tunnels_panels;
    if !panels.is_open(wid) {
        return None;
    }
    let t = app.tokens;
    let query = panels.query(wid);
    let groups = super::groups(
        &app.profiles,
        &app.tunnels,
        query,
        app.tunnels_highlight(wid),
    );
    let any = super::any_defined(&app.profiles);
    let close = button(icon(ICON_X, 14.0, t.text_muted))
        .on_press(msg(TunnelsMsg::Close(wid)))
        .padding(4)
        .style(move |_, status| ui::ghost(t, status));
    let header = row![
        text("Tunnels")
            .font(UI_FONT)
            .size(12.5)
            .color(t.text_secondary)
            .wrapping(text::Wrapping::None)
            .width(Length::Fill),
        close
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .padding(Padding {
        top: 8.0,
        right: PAD_X - 4.0,
        bottom: 4.0,
        left: PAD_X,
    });
    let mut search = text_input(SEARCH_PLACEHOLDER, query)
        .on_input(move |q| msg(TunnelsMsg::Query(wid, q)))
        .icon(text_input::Icon {
            font: ui::ICONS,
            code_point: ICON_SEARCH.chars().next().unwrap_or(' '),
            size: Some(13.0.into()),
            spacing: 6.0,
            side: text_input::Side::Left,
        })
        .font(UI_FONT)
        .size(12.5)
        .padding(Padding::from([5.0, 8.0]))
        .style(move |_, status| ui::input_style(t, status));
    if let Some(input) = panels.input.get(&wid) {
        search = search.id(input.clone());
    }
    let search = container(search).padding(Padding {
        top: 4.0,
        right: PAD_X,
        bottom: 8.0,
        left: PAD_X,
    });
    let mut list = column![].spacing(14).width(Length::Fill);
    if groups.is_empty() {
        let note = if any { NO_MATCHES } else { EMPTY };
        list = list.push(
            text(note)
                .font(UI_FONT)
                .size(12.5)
                .color(t.text_muted)
                .wrapping(text::Wrapping::WordOrGlyph),
        );
    }
    for g in groups {
        list = list.push(group(t, wid, g));
    }
    // The panel docks at the window's right edge: its scrollbar keeps clear of the resize band.
    let drawn = app.chrome.get().draws_controls();
    let list = scrollable(container(list).padding(Padding {
        top: 2.0,
        right: PAD_X,
        bottom: 12.0,
        left: PAD_X,
    }))
    .direction(scrollable::Direction::Vertical(ui::edge_scrollbar(
        drawn, 6.0, 6.0, 3.0,
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
    let content = column![header, search, list]
        .width(Length::Fill)
        .height(Length::Fill);
    let rule = container(Space::new())
        .width(RULE_WIDTH)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(t.border.into()),
            ..container::Style::default()
        });
    let surface = t.surface.scale_alpha(app.settings.background_opacity);
    let panel = container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(surface.into()),
            text_color: Some(t.text),
            ..container::Style::default()
        });
    Some((row![rule, panel].into(), panels.width))
}

/// A profile's tunnels: its name over a card of rows separated by hairlines. The highlighted
/// profile's name is in the accent and its card edged in it.
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
    let (name_color, edge) = if g.highlighted {
        (t.accent, t.accent)
    } else {
        (t.text_secondary, t.border)
    };
    column![
        text(g.profile)
            .font(UI_FONT)
            .size(12.5)
            .color(name_color)
            .wrapping(text::Wrapping::WordOrGlyph),
        container(list)
            .width(Length::Fill)
            .style(move |_| container::Style {
                border: Border {
                    color: edge,
                    ..ui::card(t).border
                },
                ..ui::card(t)
            }),
    ]
    .spacing(6)
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

/// One tunnel: the dot, its words (with Restart to apply or Retry under them, so a narrow
/// panel keeps room for the addresses) and the switch.
fn tunnel_row<'a>(t: &'static Tokens, wid: window::Id, r: Row) -> Element<'a, Message> {
    let color = tone_color(t, r.tone);
    let state_color = if r.tone == Tone::Failed {
        t.danger
    } else {
        t.text_secondary
    };
    let mut words = column![
        text(r.name)
            .font(UI_FONT)
            .size(13)
            .color(t.text)
            .wrapping(text::Wrapping::WordOrGlyph),
        text(r.address)
            .font(MONO)
            .size(11.5)
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
    let mut chips = row![].spacing(8);
    if r.restart_to_apply {
        chips = chips.push(small_button(
            t,
            RESTART_TO_APPLY,
            msg(TunnelsMsg::Retry(wid, r.key.clone())),
            restart_style,
        ));
    }
    if r.retry {
        chips = chips.push(small_button(
            t,
            "Retry",
            msg(TunnelsMsg::Retry(wid, r.key.clone())),
            ui::secondary,
        ));
    }
    if r.restart_to_apply || r.retry {
        words = words.push(container(chips).padding(Padding {
            top: 4.0,
            ..Padding::ZERO
        }));
    }
    let key = r.key;
    let switch = toggler(r.on)
        .on_toggle(move |_| msg(TunnelsMsg::Toggle(wid, key.clone())))
        .size(16)
        .style(move |_, status| crate::settings_window::view::toggler_style(t, status, false));
    row![
        container(ui::dot(color, 8.0)).padding(Padding {
            top: 5.0,
            ..Padding::ZERO
        }),
        words,
        switch,
    ]
    .spacing(10)
    .align_y(Alignment::Start)
    .padding(Padding::from([10.0, 12.0]))
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
