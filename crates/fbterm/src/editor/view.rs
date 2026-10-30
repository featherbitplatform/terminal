//! The profile editor window: profiles by group on the left, the selected one's form on the right.

use fbterm_config::{AccentStyle, Rgb};
use iced::widget::{
    Space, button, column, container, pick_list, row, scrollable, text, text_input,
};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding};

use super::ssh_view::{auth, jumps, tunnels};
use super::{ACCENTS, Draft, DraftKind, EditorMsg, EditorState};
use crate::app::Message;
use crate::schemes::SchemeChoice;
use crate::settings_window::view::{fixed_choice, menu_style, pick_style};
use crate::theme::{Tokens, UI_FONT};
use crate::ui::{self, MONO};

const SIDEBAR_WIDTH: f32 = 260.0;
const FORM_WIDTH: f32 = 560.0;

pub(super) fn msg(m: EditorMsg) -> Message {
    Message::Editor(m)
}

pub fn view<'a>(
    t: &'static Tokens,
    state: &'a EditorState,
    schemes: Vec<SchemeChoice>,
    storage: fbterm_secrets::Storage,
    busy: bool,
    scale: f32,
) -> Element<'a, Message> {
    let editable = state.read_only.is_none();
    let mut main = column![];
    if let Some(err) = &state.read_only {
        main = main.push(read_only_banner(t, err));
    }
    let form: Element<'a, Message> = match state.selected_draft() {
        Some(d) => scrollable(
            container(form(t, state, d, schemes, editable, storage))
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
        None => container(
            text("No profiles yet. Add one on the left.")
                .font(UI_FONT)
                .size(13)
                .color(t.text_muted),
        )
        .center(Length::Fill)
        .into(),
    };
    main = main.push(
        row![
            sidebar(t, state, editable, scale),
            if state.selected.is_some() {
                column![form, footer(t, state, editable, busy, scale)]
            } else {
                column![form]
            }
        ]
        .height(Length::Fill),
    );
    container(main)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(t.app.into()),
            text_color: Some(t.text),
            ..container::Style::default()
        })
        .into()
}

fn read_only_banner<'a>(
    t: &'static Tokens,
    err: &'a fbterm_config::ConfigError,
) -> Element<'a, Message> {
    let location = match err.line {
        Some(line) => format!("{}:{line}", err.path.display()),
        None => err.path.display().to_string(),
    };
    container(
        row![
            ui::icon(ui::ICON_SHIELD_ALERT, 16.0, t.danger),
            column![
                text("profiles.toml could not be read. Profiles are read-only until it is fixed and reloaded.")
                    .font(UI_FONT)
                    .size(13)
                    .color(t.text),
                text(location).font(MONO).size(12).color(t.text_secondary),
                text(err.message.as_str()).font(MONO).size(12).color(t.danger),
            ]
            .spacing(3)
            .width(Length::Fill),
            ui::text_button(t, "Reload", Some(msg(EditorMsg::Reload)), ui::secondary),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([10.0, 16.0]))
    .width(Length::Fill)
    .style(move |_| container::Style {
        background: Some(t.danger_soft.into()),
        ..container::Style::default()
    })
    .into()
}

pub(super) fn detail(d: &Draft) -> String {
    match &d.kind {
        DraftKind::Local(l) => l.program.clone(),
        DraftKind::Ssh(s) if s.host.is_empty() => "no host".into(),
        DraftKind::Ssh(s) => format!("{}@{}", s.user, s.host),
    }
}

fn swatch<'a>(color: Option<Color>, size: f32) -> container::Container<'a, Message> {
    container(text(""))
        .width(size)
        .height(size)
        .style(move |_| container::Style {
            background: color.map(Background::Color),
            border: Border {
                radius: (size / 2.0).into(),
                ..Border::default()
            },
            ..container::Style::default()
        })
}

/// `scale`: the window's physical scale, for the hairline rule.
fn sidebar<'a>(
    t: &'static Tokens,
    state: &'a EditorState,
    editable: bool,
    scale: f32,
) -> Element<'a, Message> {
    let add = |caption: &'a str, m: EditorMsg| {
        button(
            row![
                ui::icon(ui::ICON_PLUS, 13.0, t.text_secondary),
                text(caption).font(UI_FONT).size(12.5)
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .on_press_maybe(editable.then(|| msg(m)))
        .padding(Padding::from([5.0, 9.0]))
        .style(move |_, s| ui::secondary(t, s))
    };
    let transfer = |caption: &'a str, icon: &'static str, m: EditorMsg| {
        button(
            row![
                ui::icon(icon, 13.0, t.text_secondary),
                text(caption).font(UI_FONT).size(12.5)
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .on_press(msg(m))
        .padding(Padding::from([5.0, 9.0]))
        .style(move |_, s| ui::ghost(t, s))
    };
    let mut list = column![].spacing(2);
    for (group, drafts) in state.groups() {
        let title = if group.is_empty() {
            "Ungrouped".to_owned()
        } else {
            group
        };
        list = list.push(
            container(text(title).font(UI_FONT).size(11.5).color(t.text_muted)).padding(Padding {
                top: 12.0,
                right: 10.0,
                bottom: 4.0,
                left: 10.0,
            }),
        );
        for d in drafts {
            list = list.push(profile_row(t, d, state.selected == Some(d.id)));
        }
    }
    // Only a rule along the right edge sets the sidebar apart: the window header's rule
    // runs above it and the footer's beside it, and a border all around would double both.
    container(row![
        column![
            row![
                text("Profiles").font(UI_FONT).size(14).color(t.text),
                Space::new().width(Length::Fill),
                add("SSH", EditorMsg::NewSsh),
                add("Local", EditorMsg::NewLocal),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .padding(Padding::from([12.0, 12.0])),
            scrollable(container(list).padding(Padding::from([0.0, 8.0]))).height(Length::Fill),
            row![
                transfer("Export", ui::ICON_UPLOAD, EditorMsg::Export),
                transfer("Import", ui::ICON_DOWNLOAD, EditorMsg::Import),
            ]
            .spacing(6)
            .padding(Padding::from([12.0, 12.0])),
        ]
        .width(Length::Fill)
        .height(Length::Fill),
        ui::vertical_rule(t, scale),
    ])
    .width(SIDEBAR_WIDTH)
    .height(Length::Fill)
    .style(move |_| container::Style {
        background: Some(t.surface.into()),
        ..container::Style::default()
    })
    .into()
}

fn profile_row<'a>(t: &'static Tokens, d: &'a Draft, selected: bool) -> Element<'a, Message> {
    let dot = swatch(d.accent.map(ui::to_color).or(Some(t.border)), 8.0);
    let name = if d.name.trim().is_empty() {
        "Untitled".to_owned()
    } else {
        d.name.clone()
    };
    let content = row![
        dot,
        column![
            text(name)
                .font(UI_FONT)
                .size(13)
                .color(t.text)
                .wrapping(text::Wrapping::None),
            text(detail(d))
                .font(MONO)
                .size(11.5)
                .color(t.text_muted)
                .wrapping(text::Wrapping::None),
        ]
        .spacing(1),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    button(container(content).clip(true))
        .on_press(msg(EditorMsg::Select(d.id)))
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

/// A labelled text field; `mono` for real values. Disabled when `on_input` is `None`.
fn field<'a>(
    t: &'static Tokens,
    caption: &'a str,
    placeholder: &'a str,
    value: &'a str,
    mono: bool,
    on_input: Option<fn(String) -> EditorMsg>,
) -> Element<'a, Message> {
    column![
        ui::label(t, caption),
        input(t, placeholder, value, mono, on_input)
    ]
    .spacing(6)
    .into()
}

/// The profile's scheme list (`Use global scheme` or one scheme; a still value when read-only)
/// and the way to the Color schemes window.
fn scheme_pick<'a>(
    t: &'static Tokens,
    d: &'a Draft,
    schemes: Vec<SchemeChoice>,
    editable: bool,
) -> Element<'a, Message> {
    let current = d.color_scheme.as_deref();
    let picker: Element<'a, Message> = if editable {
        let selected = crate::schemes::chosen(&schemes, current);
        pick_list(schemes, selected, |c: SchemeChoice| {
            msg(EditorMsg::ColorScheme(c.id))
        })
        .placeholder(crate::schemes::NOT_FOUND)
        .font(UI_FONT)
        .text_size(13)
        .padding(Padding::from([7.0, 10.0]))
        .width(240)
        .style(move |_, status| pick_style(t, status))
        .menu_style(move |_| menu_style(t))
        .into()
    } else {
        fixed_choice(t, crate::schemes::shown(&schemes, current), 240.0)
    };
    let edit = button(text("Edit schemes\u{2026}").font(UI_FONT).size(12.5))
        .on_press(msg(EditorMsg::EditSchemes))
        .padding(Padding::from([5.0, 8.0]))
        .style(move |_, s| ui::ghost(t, s));
    row![picker, edit]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

pub(super) fn input<'a>(
    t: &'static Tokens,
    placeholder: &'a str,
    value: &'a str,
    mono: bool,
    on_input: Option<fn(String) -> EditorMsg>,
) -> text_input::TextInput<'a, Message> {
    text_input(placeholder, value)
        .on_input_maybe(on_input.map(|f| move |v| msg(f(v))))
        .font(if mono { MONO } else { UI_FONT })
        .size(13)
        .padding(Padding::from([7.0, 10.0]))
        .style(move |_, s| ui::input_style(t, s))
}

fn section<'a>(t: &'static Tokens, title: &'a str) -> Element<'a, Message> {
    container(text(title).font(UI_FONT).size(13.5).color(t.text))
        .padding(Padding {
            top: 10.0,
            ..Padding::ZERO
        })
        .into()
}

fn form<'a>(
    t: &'static Tokens,
    state: &'a EditorState,
    d: &'a Draft,
    schemes: Vec<SchemeChoice>,
    editable: bool,
    storage: fbterm_secrets::Storage,
) -> Element<'a, Message> {
    let on = |f: fn(String) -> EditorMsg| editable.then_some(f);
    let mut col = column![
        section(t, "General"),
        row![
            container(
                column![
                    ui::label(t, "Name"),
                    input(t, "Profile name", &d.name, false, on(EditorMsg::Name))
                        .id(state.name_input.clone())
                ]
                .spacing(6)
            )
            .width(Length::FillPortion(3)),
            container(field(
                t,
                "Group",
                "None",
                &d.group,
                false,
                on(EditorMsg::Group)
            ))
            .width(Length::FillPortion(2)),
        ]
        .spacing(12),
        row![
            column![ui::label(t, "Accent"), accents(t, d.accent, editable)].spacing(6),
            column![
                ui::label(t, "Color as"),
                color_as(t, d.accent_style, editable && d.accent.is_some())
            ]
            .spacing(6),
        ]
        .spacing(24),
        column![
            ui::label(t, "Color scheme"),
            scheme_pick(t, d, schemes, editable)
        ]
        .spacing(6),
    ]
    .spacing(14)
    .width(Length::Fill);
    match &d.kind {
        DraftKind::Local(l) => {
            col = col
                .push(section(t, "Shell"))
                .push(field(
                    t,
                    "Program",
                    "/bin/zsh",
                    &l.program,
                    true,
                    on(EditorMsg::Program),
                ))
                .push(field(
                    t,
                    "Arguments",
                    "-l",
                    &l.args_text,
                    true,
                    on(EditorMsg::Args),
                ))
                .push(field(
                    t,
                    "Working directory",
                    "Home directory",
                    &l.cwd,
                    true,
                    on(EditorMsg::Cwd),
                ));
        }
        DraftKind::Ssh(s) => {
            col = col
                .push(section(t, "Connection"))
                .push(
                    row![
                        container(field(
                            t,
                            "Host",
                            "example.com",
                            &s.host,
                            true,
                            on(EditorMsg::Host)
                        ))
                        .width(Length::Fill),
                        container(field(t, "Port", "22", &s.port, true, on(EditorMsg::Port)))
                            .width(90),
                    ]
                    .spacing(12),
                )
                .push(field(t, "User", "user", &s.user, true, on(EditorMsg::User)))
                .push(section(t, "Authentication"))
                .push(auth(t, s, editable, storage, state.selected_key_is_rsa()))
                .push(section(t, "Jump hosts"))
                .push(jumps(t, state, d.id, s, editable))
                .push(section(t, "Tunnels"))
                .push(tunnels(t, s, editable));
        }
    }
    col.into()
}

/// How the accent shows on tabs: a stripe or the whole tab (only with an accent set).
fn color_as<'a>(t: &'static Tokens, current: AccentStyle, editable: bool) -> Element<'a, Message> {
    let choice = |caption: &'a str, value: AccentStyle| {
        super::ssh_view::segment(
            t,
            caption,
            current == value,
            editable.then(|| msg(EditorMsg::AccentStyle(value))),
        )
    };
    super::ssh_view::segmented(
        t,
        row![
            choice("Stripe", AccentStyle::Stripe),
            choice("Whole tab", AccentStyle::Tab),
        ],
    )
    .into()
}

fn accents<'a>(t: &'static Tokens, current: Option<Rgb>, editable: bool) -> Element<'a, Message> {
    let choice = |value: Option<Rgb>| {
        let selected = current == value;
        let inner: Element<'a, Message> = match value {
            Some(c) => swatch(Some(ui::to_color(c)), 16.0).into(),
            // "None": an empty ring.
            None => container(text(""))
                .width(16)
                .height(16)
                .style(move |_| container::Style {
                    border: Border {
                        color: t.text_muted,
                        width: 1.0,
                        radius: 8.0.into(),
                    },
                    ..container::Style::default()
                })
                .into(),
        };
        button(inner)
            .on_press_maybe(editable.then(|| msg(EditorMsg::Accent(value))))
            .padding(4)
            .style(move |_, _| button::Style {
                background: None,
                border: Border {
                    color: if selected { t.text } else { Color::TRANSPARENT },
                    width: 1.5,
                    radius: 12.0.into(),
                },
                ..button::Style::default()
            })
    };
    let mut r = row![choice(None)].spacing(4).align_y(Alignment::Center);
    for c in ACCENTS {
        r = r.push(choice(Some(c)));
    }
    r.into()
}

/// The buttons and messages under the form. `busy`: a saved-secret write of an editor save
/// or duplicate runs, so Save and Duplicate wait.
fn footer<'a>(
    t: &'static Tokens,
    state: &'a EditorState,
    editable: bool,
    busy: bool,
    scale: f32,
) -> Element<'a, Message> {
    let mut messages = column![].spacing(3).width(Length::Fill);
    for e in &state.errors {
        messages = messages.push(text(e.to_string()).font(UI_FONT).size(12).color(t.danger));
    }
    if let Some(n) = &state.notice {
        messages = messages.push(
            text(n.as_str())
                .font(UI_FONT)
                .size(12)
                .color(t.text_secondary),
        );
    } else if state.dirty {
        messages = messages.push(
            text("Unsaved changes")
                .font(UI_FONT)
                .size(12)
                .color(t.text_muted),
        );
    }
    let has_selection = state.selected.is_some();
    // A rule along the top only: the sidebar's rule runs down its left side.
    let buttons = container(
        row![
            ui::text_button(
                t,
                "Delete",
                (editable && has_selection).then(|| msg(EditorMsg::Delete)),
                ui::danger_ghost
            ),
            ui::text_button(
                t,
                "Duplicate",
                (editable && has_selection && !busy).then(|| msg(EditorMsg::Duplicate)),
                ui::ghost
            ),
            messages,
            ui::text_button(t, "Close", Some(msg(EditorMsg::Close)), ui::secondary),
            ui::text_button(
                t,
                "Save",
                (editable && state.dirty && !busy).then(|| msg(EditorMsg::Save)),
                ui::primary
            ),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([12.0, 20.0]))
    .width(Length::Fill);
    container(column![ui::horizontal_rule(t, scale), buttons])
        .width(Length::Fill)
        .style(move |_| container::Style {
            background: Some(t.surface.into()),
            ..container::Style::default()
        })
        .into()
}
