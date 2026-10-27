//! The Color schemes window: the schemes on the left, the selected one's preview, colors and
//! font on the right, and a footer with Import, Export and Save.

use fbterm_config::Rgb;
use fbterm_config::schemes::Scheme;
use fbterm_term::{TerminalStyle, highlighted_terminal_view};
use iced::widget::{
    Space, button, column, container, pick_list, row, scrollable, slider, stack, text, text_input,
};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding};

use super::{DeleteAsk, Entry, ImportAsk, ImportChoice, SchemesMsg, SchemesState};
use crate::app::Message;
use crate::settings_window::view::{menu_style, pick_style, slider_style};
use crate::theme::{Tokens, UI_FONT};
use crate::ui::{self, MONO};

const SIDEBAR_WIDTH: f32 = 250.0;
const FORM_WIDTH: f32 = 640.0;
/// Height of the preview terminal: the sample's seven lines at the usual sizes.
const PREVIEW_HEIGHT: f32 = 116.0;

fn msg(m: SchemesMsg) -> Message {
    Message::Schemes(m)
}

/// `base`: the global terminal look, for what the scheme does not set.
pub fn view<'a>(
    t: &'static Tokens,
    state: &'a SchemesState,
    base: &TerminalStyle,
) -> Element<'a, Message> {
    let form: Element<'a, Message> = match state.selected_draft() {
        Some(d) => scrollable(
            container(form(t, state, d, base))
                .padding(Padding::from([20.0, 28.0]))
                .max_width(FORM_WIDTH + 56.0),
        )
        // The default scrollbar, kept clear of the window's resize edge.
        .direction(scrollable::Direction::Vertical(ui::edge_scrollbar(
            crate::chrome::Chrome::current().draws_controls(),
            10.0,
            10.0,
            0.0,
        )))
        .height(Length::Fill)
        .into(),
        None => container(text("")).height(Length::Fill).into(),
    };
    let page =
        container(row![sidebar(t, state), column![form, footer(t, state)]].height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| container::Style {
                background: Some(t.app.into()),
                text_color: Some(t.text),
                ..container::Style::default()
            });
    match (&state.confirm, &state.import) {
        (Some(ask), _) => stack![page, confirm(t, ask)].into(),
        (None, Some(ask)) => stack![page, import_question(t, ask)].into(),
        (None, None) => page.into(),
    }
}

/// A small square of `color`.
fn swatch<'a>(color: Rgb, size: f32, ring: Option<Color>) -> container::Container<'a, Message> {
    container(text(""))
        .width(size)
        .height(size)
        .style(move |_| container::Style {
            background: Some(Background::Color(ui::to_color(color))),
            border: Border {
                color: ring.unwrap_or(Color::from_rgba(1.0, 1.0, 1.0, 0.12)),
                width: if ring.is_some() { 2.0 } else { 1.0 },
                radius: 4.0.into(),
            },
            ..container::Style::default()
        })
}

/// A scheme in miniature: "Aa" in its foreground on its background, with its red, green and
/// blue.
fn chip<'a>(s: &Scheme) -> Element<'a, Message> {
    let c = s.colors;
    let dot = |rgb: Rgb| {
        container(text(""))
            .width(5)
            .height(5)
            .style(move |_| container::Style {
                background: Some(Background::Color(ui::to_color(rgb))),
                border: Border {
                    radius: 2.5.into(),
                    ..Border::default()
                },
                ..container::Style::default()
            })
    };
    container(
        row![
            text("Aa")
                .font(MONO)
                .size(11)
                .color(ui::to_color(c.foreground)),
            column![dot(c.ansi[1]), dot(c.ansi[2]), dot(c.ansi[4])].spacing(1),
        ]
        .spacing(4)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([3.0, 6.0]))
    .style(move |_| container::Style {
        background: Some(Background::Color(ui::to_color(c.background))),
        border: Border {
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.12),
            width: 1.0,
            radius: 5.0.into(),
        },
        ..container::Style::default()
    })
    .into()
}

fn sidebar<'a>(t: &'static Tokens, state: &'a SchemesState) -> Element<'a, Message> {
    let mut list = column![].spacing(2);
    for s in state.drafts() {
        list = list.push(scheme_row(
            t,
            s,
            state.selected == s.id,
            state.is_dirty(&s.id),
        ));
    }
    container(
        column![
            container(text("Color schemes").font(UI_FONT).size(14).color(t.text))
                .padding(Padding::from([14.0, 16.0])),
            scrollable(container(list).padding(Padding::from([0.0, 8.0]))).height(Length::Fill),
        ]
        .height(Length::Fill),
    )
    .width(SIDEBAR_WIDTH)
    .height(Length::Fill)
    .style(move |_| container::Style {
        background: Some(t.surface.into()),
        border: Border {
            color: t.border,
            width: 1.0,
            radius: 0.0.into(),
        },
        ..container::Style::default()
    })
    .into()
}

fn scheme_row<'a>(
    t: &'static Tokens,
    s: &'a Scheme,
    selected: bool,
    dirty: bool,
) -> Element<'a, Message> {
    let detail = if s.builtin {
        "Built-in"
    } else if dirty {
        "Unsaved changes"
    } else {
        "Custom"
    };
    let name = if s.name.trim().is_empty() {
        "Untitled"
    } else {
        s.name.as_str()
    };
    let content = row![
        chip(s),
        column![
            text(name)
                .font(UI_FONT)
                .size(13)
                .color(t.text)
                .wrapping(text::Wrapping::None),
            text(detail)
                .font(UI_FONT)
                .size(11.5)
                .color(t.text_muted)
                .wrapping(text::Wrapping::None),
        ]
        .spacing(1),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    button(container(content).clip(true))
        .on_press(msg(SchemesMsg::Select(s.id.clone())))
        .width(Length::Fill)
        .padding(Padding::from([6.0, 10.0]))
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: if selected {
                    Some(t.accent_soft.into())
                } else {
                    hovered.then(|| t.hover.into())
                },
                text_color: t.text,
                border: Border {
                    color: if selected {
                        t.accent_edge
                    } else {
                        Color::TRANSPARENT
                    },
                    width: if selected { 1.0 } else { 0.0 },
                    radius: 6.0.into(),
                },
                ..button::Style::default()
            }
        })
        .into()
}

fn small<'a>(
    t: &'static Tokens,
    caption: &'a str,
    on_press: Option<SchemesMsg>,
    style: fn(&Tokens, button::Status) -> button::Style,
) -> Element<'a, Message> {
    button(text(caption).font(UI_FONT).size(12.5))
        .on_press_maybe(on_press.map(msg))
        .padding(Padding::from([5.0, 12.0]))
        .style(move |_, status| style(t, status))
        .into()
}

fn section<'a>(t: &'static Tokens, title: &'a str) -> Element<'a, Message> {
    container(text(title).font(UI_FONT).size(13.5).color(t.text))
        .padding(Padding {
            top: 8.0,
            ..Padding::ZERO
        })
        .into()
}

fn form<'a>(
    t: &'static Tokens,
    state: &'a SchemesState,
    d: &'a Scheme,
    base: &TerminalStyle,
) -> Element<'a, Message> {
    let editable = state.editable();
    let title: Element<'a, Message> = if state.renaming && editable {
        text_input("Scheme name", &d.name)
            .on_input(|s| msg(SchemesMsg::Rename(s)))
            .on_submit(msg(SchemesMsg::RenameEnd))
            .font(UI_FONT)
            .size(15)
            .padding(Padding::from([6.0, 10.0]))
            .width(280)
            .style(move |_, s| ui::input_style(t, s))
            .into()
    } else {
        text(if d.name.trim().is_empty() {
            "Untitled"
        } else {
            d.name.as_str()
        })
        .font(UI_FONT)
        .size(18)
        .color(t.text)
        .into()
    };
    let rename = if state.renaming {
        SchemesMsg::RenameEnd
    } else {
        SchemesMsg::RenameStart
    };
    let header = row![
        title,
        Space::new().width(Length::Fill),
        small(t, "Duplicate", Some(SchemesMsg::Duplicate), ui::secondary),
        small(t, "Rename", editable.then_some(rename), ui::secondary),
        small(
            t,
            "Delete",
            editable.then_some(SchemesMsg::Delete),
            ui::danger_ghost
        ),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let mut col = column![header].spacing(14).width(Length::Fill);
    if !editable {
        col = col.push(
            row![
                ui::icon(ui::ICON_LOCK, 13.0, t.text_muted),
                text("Built-in schemes are read-only. Duplicate one to make it your own.")
                    .font(UI_FONT)
                    .size(12.5)
                    .color(t.text_secondary),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    col = col.push(preview(t, state, d, base));
    if let Some(hint) = state.contrast_hint() {
        col = col.push(
            row![
                ui::icon(ui::ICON_WARNING, 13.0, t.warning),
                text(hint).font(UI_FONT).size(12.5).color(t.text_secondary),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    col = col.push(section(t, "Colors"));
    if editable {
        col = col.push(mixer(t, state, d));
    }
    let entries = |range: std::ops::Range<u8>| {
        let mut c = column![].spacing(6).width(Length::Fill);
        for n in range {
            c = c.push(entry_row(t, state, d, Entry::Ansi(n), editable));
        }
        c
    };
    col = col
        .push(
            row![
                column![
                    entry_row(t, state, d, Entry::Foreground, editable),
                    entry_row(t, state, d, Entry::Background, editable),
                ]
                .spacing(6)
                .width(Length::Fill),
                column![
                    entry_row(t, state, d, Entry::Cursor, editable),
                    entry_row(t, state, d, Entry::Selection, editable),
                ]
                .spacing(6)
                .width(Length::Fill),
            ]
            .spacing(24),
        )
        .push(
            row![
                column![ui::label(t, "Normal"), entries(0..8)]
                    .spacing(8)
                    .width(Length::Fill),
                column![ui::label(t, "Bright"), entries(8..16)]
                    .spacing(8)
                    .width(Length::Fill),
            ]
            .spacing(24),
        )
        .push(section(t, "Font"))
        .push(font(t, state, editable));
    col.into()
}

/// The preview terminal, drawn with the draft's colors and font, on its own background.
fn preview<'a>(
    t: &'static Tokens,
    state: &'a SchemesState,
    d: &'a Scheme,
    base: &TerminalStyle,
) -> Element<'a, Message> {
    let bg = ui::to_color(d.colors.background);
    container(
        container(highlighted_terminal_view(
            &state.preview,
            state.preview_style(base),
            None,
            |out| msg(SchemesMsg::Preview(out)),
        ))
        .height(PREVIEW_HEIGHT)
        .width(Length::Fill),
    )
    .padding(Padding::from([10.0, 12.0]))
    .width(Length::Fill)
    .style(move |_| container::Style {
        background: Some(Background::Color(bg)),
        border: Border {
            color: t.border,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..container::Style::default()
    })
    .into()
}

/// The entry being edited: a large swatch and a slider per channel.
fn mixer<'a>(t: &'static Tokens, state: &'a SchemesState, d: &'a Scheme) -> Element<'a, Message> {
    let entry = state.editing;
    let rgb = entry.get(&d.colors);
    let channel = |name: &'a str, value: u8, set: fn(Rgb, u8) -> Rgb| {
        row![
            text(name).font(MONO).size(12).color(t.text_muted).width(14),
            slider(0..=255u8, value, move |v| msg(SchemesMsg::Color(
                entry,
                set(rgb, v)
            )))
            .width(Length::Fill)
            .style(move |_, status| slider_style(t, status, true)),
            text(value.to_string())
                .font(MONO)
                .size(12)
                .color(t.text_secondary)
                .width(30)
                .align_x(Alignment::End),
        ]
        .spacing(10)
        .align_y(Alignment::Center)
    };
    container(
        row![
            column![
                swatch(rgb, 44.0, None),
                text(entry.label()).font(UI_FONT).size(12).color(t.text),
                text(rgb.to_string())
                    .font(MONO)
                    .size(11.5)
                    .color(t.text_secondary),
            ]
            .spacing(4)
            .width(110),
            column![
                channel("R", rgb.r, |c, v| Rgb { r: v, ..c }),
                channel("G", rgb.g, |c, v| Rgb { g: v, ..c }),
                channel("B", rgb.b, |c, v| Rgb { b: v, ..c }),
            ]
            .spacing(6)
            .width(Length::Fill),
        ]
        .spacing(16)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([12.0, 14.0]))
    .width(Length::Fill)
    .style(move |_| ui::card(t))
    .into()
}

/// One color: its swatch (a click shows its sliders), its name and its hex value.
fn entry_row<'a>(
    t: &'static Tokens,
    state: &'a SchemesState,
    d: &'a Scheme,
    entry: Entry,
    editable: bool,
) -> Element<'a, Message> {
    let editing = editable && state.editing == entry;
    let valid = state.hex_valid(entry);
    let sw = button(swatch(
        entry.get(&d.colors),
        18.0,
        editing.then_some(t.accent),
    ))
    .on_press_maybe(editable.then_some(msg(SchemesMsg::Edit(entry))))
    .padding(0)
    .style(|_, _| button::Style::default());
    let hex = text_input("#rrggbb", state.hex_text(entry))
        .on_input_maybe(editable.then_some(move |s| msg(SchemesMsg::Hex(entry, s))))
        .font(MONO)
        .size(12)
        .padding(Padding::from([4.0, 8.0]))
        .width(92)
        .style(move |_, s| {
            let mut style = ui::input_style(t, s);
            if !valid {
                style.border.color = t.danger;
            }
            style
        });
    row![
        sw,
        text(entry.label())
            .font(UI_FONT)
            .size(12.5)
            .color(if editing { t.text } else { t.text_secondary })
            .width(Length::Fill),
        hex,
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

/// A read-only value where a list would be.
fn fixed<'a>(t: &'static Tokens, value: String) -> Element<'a, Message> {
    container(text(value).font(UI_FONT).size(12.5).color(t.text_secondary))
        .padding(Padding::from([6.0, 10.0]))
        .width(Length::Fill)
        .style(move |_| container::Style {
            background: Some(t.surface.into()),
            border: Border {
                color: t.border,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

fn font<'a>(t: &'static Tokens, state: &'a SchemesState, editable: bool) -> Element<'a, Message> {
    let (family, size): (Element<'a, Message>, Element<'a, Message>) = if editable {
        (
            pick_list(state.font_options(), Some(state.selected_font()), |p| {
                msg(SchemesMsg::FontFamily(p.0))
            })
            .font(UI_FONT)
            .text_size(12.5)
            .padding(Padding::from([6.0, 10.0]))
            .width(Length::Fill)
            .style(move |_, status| pick_style(t, status))
            .menu_style(move |_| menu_style(t))
            .into(),
            pick_list(state.size_options(), Some(state.selected_size()), |p| {
                msg(SchemesMsg::FontSize(p.0))
            })
            .font(UI_FONT)
            .text_size(12.5)
            .padding(Padding::from([6.0, 10.0]))
            .width(Length::Fill)
            .style(move |_, status| pick_style(t, status))
            .menu_style(move |_| menu_style(t))
            .into(),
        )
    } else {
        (
            fixed(t, state.selected_font().to_string()),
            fixed(t, state.selected_size().to_string()),
        )
    };
    row![
        column![ui::label(t, "Family"), family]
            .spacing(6)
            .width(Length::FillPortion(3)),
        column![ui::label(t, "Size"), size]
            .spacing(6)
            .width(Length::FillPortion(2)),
    ]
    .spacing(16)
    .into()
}

fn footer<'a>(t: &'static Tokens, state: &'a SchemesState) -> Element<'a, Message> {
    let editable = state.editable();
    let dirty = state.is_dirty(&state.selected);
    let mut messages = column![].spacing(3).width(Length::Fill);
    if let Some(e) = &state.error {
        messages = messages.push(
            text(e.as_str())
                .font(UI_FONT)
                .size(12)
                .color(t.danger)
                .wrapping(text::Wrapping::WordOrGlyph),
        );
    } else if let Some(n) = &state.notice {
        messages = messages.push(
            text(n.as_str())
                .font(UI_FONT)
                .size(12)
                .color(t.text_secondary),
        );
    } else if editable && dirty {
        messages = messages.push(
            text("Unsaved changes")
                .font(UI_FONT)
                .size(12)
                .color(t.text_muted),
        );
    }
    container(
        row![
            ui::text_button(
                t,
                "Import\u{2026}",
                Some(msg(SchemesMsg::Import)),
                ui::secondary
            ),
            ui::text_button(
                t,
                "Export\u{2026}",
                Some(msg(SchemesMsg::Export)),
                ui::secondary
            ),
            messages,
            ui::text_button(
                t,
                "Save",
                (editable && dirty).then_some(msg(SchemesMsg::Save)),
                ui::primary
            ),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([12.0, 20.0]))
    .width(Length::Fill)
    .style(move |_| container::Style {
        background: Some(t.surface.into()),
        border: Border {
            color: t.border,
            width: 1.0,
            radius: 0.0.into(),
        },
        ..container::Style::default()
    })
    .into()
}

/// The delete question, over the window like the connection dialogs.
fn confirm<'a>(t: &'static Tokens, ask: &'a DeleteAsk) -> Element<'a, Message> {
    let mut body = column![crate::prompt_view::header(
        t,
        ui::ICON_WARNING,
        t.warning,
        ask.title()
    )]
    .spacing(14);
    if let Some(b) = ask.body() {
        body = body.push(text(b).font(UI_FONT).size(13).color(t.text_secondary));
    }
    body = body.push(crate::prompt_view::buttons(vec![
        ui::text_button(
            t,
            "Cancel",
            Some(msg(SchemesMsg::CancelDelete)),
            ui::secondary,
        )
        .into(),
        ui::text_button(
            t,
            "Delete",
            Some(msg(SchemesMsg::ConfirmDelete)),
            ui::primary,
        )
        .into(),
    ]));
    crate::prompt_view::modal(t, body.into())
}

/// The question for an import whose id is taken: Replace, Keep both (the default, which loses
/// nothing) or Cancel.
fn import_question<'a>(t: &'static Tokens, ask: &'a ImportAsk) -> Element<'a, Message> {
    let body = column![
        crate::prompt_view::header(t, ui::ICON_WARNING, t.warning, ask.title()),
        text(ask.body())
            .font(UI_FONT)
            .size(13)
            .color(t.text_secondary),
        crate::prompt_view::buttons(vec![
            ui::text_button(
                t,
                "Cancel",
                Some(msg(SchemesMsg::ImportChoice(ImportChoice::Cancel))),
                ui::secondary,
            )
            .into(),
            ui::text_button(
                t,
                "Replace",
                Some(msg(SchemesMsg::ImportChoice(ImportChoice::Replace))),
                ui::secondary,
            )
            .into(),
            ui::text_button(
                t,
                "Keep both",
                Some(msg(SchemesMsg::ImportChoice(ImportChoice::KeepBoth))),
                ui::primary,
            )
            .into(),
        ]),
    ]
    .spacing(14);
    crate::prompt_view::modal(t, body.into())
}
