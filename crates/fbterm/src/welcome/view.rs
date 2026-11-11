//! The Welcome page: the Settings page's shape (title, a line, sections, a footer) with
//! the two questions, the tab layout one as two cards with drawn miniatures of a window.

use fbterm_config::{Settings, TabBar};
use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding};

use super::{INTRO, TABS_QUESTION, TITLE, UPDATES_BODY, WelcomeMsg};
use crate::app::Message;
use crate::settings_window::view::{section, segment, well};
use crate::theme::{Tokens, UI_FONT};
use crate::ui;

const CONTENT_WIDTH: f32 = 560.0;

fn msg(m: WelcomeMsg) -> Message {
    Message::Welcome(m)
}

pub(crate) fn view<'a>(t: &'static Tokens, settings: &Settings) -> Element<'a, Message> {
    let header = column![
        text(TITLE).font(UI_FONT).size(18).color(t.text),
        text(INTRO).font(UI_FONT).size(12.5).color(t.text_muted),
    ]
    .spacing(4);
    let tabs = column![
        text(TABS_QUESTION).font(UI_FONT).size(13).color(t.text),
        row![
            card(t, "On top", TabBar::Top, !settings.tabs_left()),
            card(t, "On the left", TabBar::Left, settings.tabs_left()),
        ]
        .spacing(12),
    ]
    .spacing(12);
    let answered = settings.check_updates;
    let updates = column![
        text(UPDATES_BODY).font(UI_FONT).size(13).color(t.text),
        container(
            row![
                segment(
                    t,
                    "Not now",
                    answered == Some(false),
                    Some(msg(WelcomeMsg::ChooseUpdates(false))),
                ),
                segment(
                    t,
                    "Check automatically",
                    answered == Some(true),
                    Some(msg(WelcomeMsg::ChooseUpdates(true))),
                ),
            ]
            .spacing(2)
        )
        .padding(2)
        .style(move |_| well(t)),
    ]
    .spacing(12)
    .align_x(Alignment::Start);
    // The Settings page's rows bring their own padding; these blocks bring theirs.
    let padded = |block: Element<'a, Message>| -> Element<'a, Message> {
        container(block)
            .padding(Padding::from([14.0, 16.0]))
            .width(Length::Fill)
            .into()
    };
    // `Done` follows the last question inside the content column, so it stays next to the
    // choices however wide the window is.
    let done = row![
        Space::new().width(Length::Fill),
        ui::text_button(t, "Done", Some(msg(WelcomeMsg::Close)), ui::primary),
    ]
    .align_y(Alignment::Center);
    let body = column![
        header,
        section(t, "Tabs", vec![padded(tabs.into())]),
        section(t, "Updates", vec![padded(updates.into())]),
        done,
    ]
    .spacing(18)
    .width(Length::Fill);
    let drawn = crate::chrome::Chrome::current().draws_controls();
    let page = scrollable(
        container(container(body).max_width(CONTENT_WIDTH))
            .padding(Padding::from([24.0, 32.0]))
            .center_x(Length::Fill),
    )
    .direction(scrollable::Direction::Vertical(ui::edge_scrollbar(drawn)))
    .style(move |theme, status| ui::scroll_style(t, theme, status))
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

/// One layout to choose: its miniature over its caption, outlined with the accent when it is
/// the current choice.
fn card<'a>(
    t: &'static Tokens,
    caption: &'static str,
    side: TabBar,
    selected: bool,
) -> Element<'a, Message> {
    let body = column![
        preview(t, side),
        text(caption).font(UI_FONT).size(12.5).color(t.text),
    ]
    .spacing(8)
    .align_x(Alignment::Center);
    button(body)
        .on_press(msg(WelcomeMsg::ChooseTabs(side)))
        .padding(12)
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: Some(if hovered { t.hover } else { t.raised }.into()),
                text_color: t.text,
                border: Border {
                    color: if selected { t.accent } else { t.border },
                    width: if selected { 2.0 } else { 1.0 },
                    radius: 8.0.into(),
                },
                ..button::Style::default()
            }
        })
        .into()
}

/// A small window: the bar with three tab shapes along it, or an empty bar and three row
/// shapes down a left column.
fn preview<'a>(t: &'static Tokens, side: TabBar) -> Element<'a, Message> {
    let shape = move |w: f32, h: f32, color: Color| {
        container(Space::new())
            .width(w)
            .height(h)
            .style(move |_| container::Style {
                background: Some(Background::Color(color)),
                border: Border {
                    radius: 2.0.into(),
                    ..Border::default()
                },
                ..container::Style::default()
            })
    };
    let surface = move |_: &iced::Theme| container::Style {
        background: Some(t.surface.into()),
        ..container::Style::default()
    };
    let bar: Element<'a, Message> = match side {
        TabBar::Top => row![
            shape(26.0, 8.0, t.accent),
            shape(26.0, 8.0, t.border),
            shape(26.0, 8.0, t.border),
        ]
        .spacing(4)
        .padding(Padding::from([3.0, 6.0]))
        .into(),
        TabBar::Left => Space::new().into(),
    };
    let bar = container(bar).width(Length::Fill).height(14).style(surface);
    let body: Element<'a, Message> = match side {
        TabBar::Top => Space::new().width(Length::Fill).height(Length::Fill).into(),
        TabBar::Left => row![
            container(
                column![
                    shape(30.0, 6.0, t.accent),
                    shape(30.0, 6.0, t.border),
                    shape(30.0, 6.0, t.border),
                ]
                .spacing(5)
                .padding(5)
            )
            .width(40)
            .height(Length::Fill)
            .style(surface),
            Space::new().width(Length::Fill),
        ]
        .into(),
    };
    container(column![bar, body])
        .width(132)
        .height(84)
        .clip(true)
        .style(move |_| container::Style {
            background: Some(t.app.into()),
            border: Border {
                color: t.border,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}
