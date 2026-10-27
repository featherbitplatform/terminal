//! The archive window's look: a title with the Export / Import switch, the mode's form in a
//! scrolling page, and a footer with the outcome and the buttons.

use fbterm_config::ProfileStore;
use fbterm_secrets::archive::ArchiveKind;
use iced::widget::{Space, button, checkbox, column, container, row, scrollable, text, text_input};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding};

use super::{
    ArchiveMode, ArchiveMsg, ArchiveState, Busy, INCLUDE_KEYS, INCLUDED_FOR_JUMPS, KIND_FULL,
    KIND_SHARED, NEW, OPENING, PreviewRow, READ_ONLY, SEALING, archive_summary, default_key_dir,
    export_groups, preview_rows, scheme_lines, shown_dir,
};
use crate::app::Message;
use crate::redact::Redacted;
use crate::theme::{Tokens, UI_FONT};
use crate::ui::{self, MONO};

const CONTENT_WIDTH: f32 = 640.0;

fn msg(m: ArchiveMsg) -> Message {
    Message::Archive(m)
}

/// `read_only`: `profiles.toml` failed to load (nothing can be imported).
pub fn view<'a>(
    t: &'static Tokens,
    state: &'a ArchiveState,
    profiles: &'a ProfileStore,
    read_only: bool,
) -> Element<'a, Message> {
    let (title, hint) = match state.mode {
        ArchiveMode::Export => (
            "Export profiles",
            "Save profiles to an encrypted .fbprofiles file.",
        ),
        ArchiveMode::Import => (
            "Import profiles",
            "Add profiles from an encrypted .fbprofiles file.",
        ),
    };
    let head = row![
        column![
            text(title).font(UI_FONT).size(18).color(t.text),
            text(hint).font(UI_FONT).size(12.5).color(t.text_muted),
        ]
        .spacing(4)
        .width(Length::Fill),
        switch(t, state.mode),
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    let form = match state.mode {
        ArchiveMode::Export => export_form(t, state, profiles),
        ArchiveMode::Import => import_form(t, state, profiles, read_only),
    };
    let body = column![head, form].spacing(18).width(Length::Fill);
    let page = scrollable(
        container(container(body).max_width(CONTENT_WIDTH))
            .padding(Padding::from([24.0, 32.0]))
            .center_x(Length::Fill),
    )
    .direction(scrollable::Direction::Vertical(
        scrollable::Scrollbar::new()
            .width(6)
            .scroller_width(6)
            .margin(3),
    ))
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
    container(column![page, footer(t, state, read_only)])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(t.app.into()),
            text_color: Some(t.text),
            ..container::Style::default()
        })
        .into()
}

/// One button of a segmented control.
fn segment<'a>(
    t: &'static Tokens,
    caption: &'a str,
    selected: bool,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    button(text(caption).font(UI_FONT).size(12.5))
        .on_press_maybe(on_press)
        .padding(Padding::from([4.0, 12.0]))
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: if selected {
                    Some(t.raised.into())
                } else {
                    hovered.then(|| t.hover.into())
                },
                text_color: if selected { t.text } else { t.text_secondary },
                border: Border {
                    color: if selected {
                        t.border
                    } else {
                        Color::TRANSPARENT
                    },
                    width: 1.0,
                    radius: 5.0.into(),
                },
                ..button::Style::default()
            }
        })
        .into()
}

/// A segmented control's inset well.
fn segmented<'a>(t: &'static Tokens, buttons: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    container(row(buttons).spacing(2))
        .padding(2)
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

fn switch<'a>(t: &'static Tokens, mode: ArchiveMode) -> Element<'a, Message> {
    segmented(
        t,
        vec![
            segment(
                t,
                "Export",
                mode == ArchiveMode::Export,
                Some(msg(ArchiveMsg::Mode(ArchiveMode::Export))),
            ),
            segment(
                t,
                "Import",
                mode == ArchiveMode::Import,
                Some(msg(ArchiveMsg::Mode(ArchiveMode::Import))),
            ),
        ],
    )
}

/// A titled card of rows separated by hairlines.
fn section<'a>(
    t: &'static Tokens,
    title: &'a str,
    rows: Vec<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut list = column![];
    let count = rows.len();
    for (i, r) in rows.into_iter().enumerate() {
        list = list.push(r);
        if i + 1 < count {
            list = list.push(hairline(t));
        }
    }
    column![
        text(title).font(UI_FONT).size(13.5).color(t.text),
        container(list)
            .width(Length::Fill)
            .style(move |_| ui::card(t)),
    ]
    .spacing(8)
    .into()
}

fn hairline<'a>(t: &'static Tokens) -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(1)
        .style(move |_| container::Style {
            background: Some(t.border.into()),
            ..container::Style::default()
        })
        .into()
}

fn padded<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(Padding::from([10.0, 16.0]))
        .width(Length::Fill)
        .into()
}

/// Warnings as lines with a warning icon.
fn warnings<'a>(t: &'static Tokens, lines: Vec<String>) -> Option<Element<'a, Message>> {
    if lines.is_empty() {
        return None;
    }
    let mut list = column![].spacing(6);
    for line in lines {
        list = list.push(
            row![
                ui::icon(ui::ICON_WARNING, 13.0, t.warning),
                text(line)
                    .font(UI_FONT)
                    .size(12)
                    .color(t.text_secondary)
                    .wrapping(text::Wrapping::WordOrGlyph)
                    .width(Length::Fill),
            ]
            .spacing(8),
        );
    }
    Some(list.into())
}

/// A muted tag next to a row.
fn tag<'a>(t: &'static Tokens, caption: String, color: Color) -> Element<'a, Message> {
    container(text(caption).font(UI_FONT).size(11.5).color(color))
        .padding(Padding::from([2.0, 8.0]))
        .style(move |_| container::Style {
            background: Some(t.raised.into()),
            border: Border {
                color: t.border,
                width: 1.0,
                radius: 5.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

fn password<'a>(
    t: &'static Tokens,
    placeholder: &'a str,
    value: &'a str,
    id: Option<&iced::widget::Id>,
    on_input: fn(Redacted) -> ArchiveMsg,
    on_submit: Option<ArchiveMsg>,
    enabled: bool,
) -> Element<'a, Message> {
    let mut input = text_input(placeholder, value)
        .secure(true)
        .on_input_maybe(enabled.then_some(move |v| msg(on_input(Redacted(v)))))
        .on_submit_maybe(on_submit.filter(|_| enabled).map(msg))
        .font(UI_FONT)
        .size(13)
        .padding(Padding::from([7.0, 10.0]))
        .style(move |_, s| ui::input_style(t, s));
    if let Some(id) = id {
        input = input.id(id.clone());
    }
    input.into()
}

fn export_form<'a>(
    t: &'static Tokens,
    state: &'a ArchiveState,
    profiles: &'a ProfileStore,
) -> Element<'a, Message> {
    let form = &state.export;
    let idle = state.busy.is_none();

    let mut list = column![];
    for (group, rows) in export_groups(profiles, &form.unchecked) {
        let title = if group.is_empty() {
            "Ungrouped".to_owned()
        } else {
            group
        };
        list = list.push(
            container(text(title).font(UI_FONT).size(11.5).color(t.text_muted)).padding(Padding {
                top: 10.0,
                right: 16.0,
                bottom: 2.0,
                left: 16.0,
            }),
        );
        for r in rows {
            let id = r.id;
            let mut line = row![
                checkbox(r.checked)
                    .on_toggle_maybe(idle.then_some(move |b| msg(ArchiveMsg::Toggle(id, b))))
                    .size(15),
                column![
                    text(r.name).font(UI_FONT).size(13).color(t.text),
                    text(r.detail).font(MONO).size(12).color(t.text_secondary),
                ]
                .spacing(2)
                .width(Length::Fill),
            ]
            .spacing(10)
            .align_y(Alignment::Center);
            if r.via_jump {
                line = line.push(tag(t, INCLUDED_FOR_JUMPS.into(), t.text_secondary));
            }
            list = list.push(container(line).padding(Padding::from([6.0, 16.0])));
        }
    }
    let profiles_card = column![
        text("Profiles").font(UI_FONT).size(13.5).color(t.text),
        container(list.padding(Padding {
            bottom: 6.0,
            ..Padding::ZERO
        }))
        .width(Length::Fill)
        .style(move |_| ui::card(t)),
    ]
    .spacing(8);

    let kind = |k: ArchiveKind, caption: &'a str| {
        segment(
            t,
            caption,
            form.kind == k,
            idle.then(|| msg(ArchiveMsg::Kind(k))),
        )
    };
    let mut kind_rows = vec![padded(segmented(
        t,
        vec![
            kind(ArchiveKind::Full, KIND_FULL),
            kind(ArchiveKind::Shared, KIND_SHARED),
        ],
    ))];
    if form.kind == ArchiveKind::Full {
        kind_rows.push(padded(
            column![
                checkbox(form.include_keys)
                    .label(INCLUDE_KEYS)
                    .on_toggle_maybe(idle.then_some(|b| msg(ArchiveMsg::IncludeKeys(b))))
                    .font(UI_FONT)
                    .text_size(13)
                    .size(15),
                text("Saved passwords and passphrases always travel in a full archive.")
                    .font(UI_FONT)
                    .size(12)
                    .color(t.text_muted),
            ]
            .spacing(6),
        ));
    } else {
        kind_rows.push(padded(
            text("SSH profiles only, without saved secrets or key file paths.")
                .font(UI_FONT)
                .size(12)
                .color(t.text_muted),
        ));
    }

    let password_rows = vec![padded(
        column![
            password(
                t,
                "Archive password",
                &form.password,
                Some(&state.password_input),
                ArchiveMsg::Password,
                None,
                idle,
            ),
            password(
                t,
                "Type the password again",
                &form.confirm,
                None,
                ArchiveMsg::Confirm,
                Some(ArchiveMsg::Export),
                idle,
            ),
            text("The archive cannot be opened without it.")
                .font(UI_FONT)
                .size(12)
                .color(t.text_muted),
        ]
        .spacing(8),
    )];

    let mut out = column![profiles_card, section(t, "Kind", kind_rows)].spacing(18);
    match &form.preview {
        Ok(lines) => {
            if let Some(w) = warnings(t, lines.clone()) {
                out = out.push(w);
            }
        }
        Err(e) => {
            out = out.push(text(e.as_str()).font(UI_FONT).size(12).color(t.danger));
        }
    }
    out.push(section(t, "Password", password_rows)).into()
}

fn import_form<'a>(
    t: &'static Tokens,
    state: &'a ArchiveState,
    profiles: &'a ProfileStore,
    read_only: bool,
) -> Element<'a, Message> {
    let form = &state.import;
    let idle = state.busy.is_none();
    let mut out = column![].spacing(18);
    if read_only {
        out = out.push(
            container(
                row![
                    ui::icon(ui::ICON_SHIELD_ALERT, 16.0, t.danger),
                    text(READ_ONLY)
                        .font(UI_FONT)
                        .size(13)
                        .color(t.text)
                        .width(Length::Fill),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([10.0, 14.0]))
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(t.danger_soft.into()),
                border: Border {
                    radius: 8.0.into(),
                    ..Border::default()
                },
                ..container::Style::default()
            }),
        );
    }

    let file: Element<'a, Message> = match &form.file {
        Some(path) => text(path.display().to_string())
            .font(MONO)
            .size(12.5)
            .color(t.text)
            .wrapping(text::Wrapping::WordOrGlyph)
            .width(Length::Fill)
            .into(),
        None => text("No file chosen")
            .font(UI_FONT)
            .size(13)
            .color(t.text_muted)
            .width(Length::Fill)
            .into(),
    };
    let archive_rows = vec![
        padded(
            row![
                file,
                ui::text_button(
                    t,
                    "Choose file\u{2026}",
                    idle.then(|| msg(ArchiveMsg::ChooseFile)),
                    ui::secondary
                ),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        ),
        padded(
            row![
                container(password(
                    t,
                    "Archive password",
                    &form.password,
                    Some(&state.import_password_input),
                    ArchiveMsg::ImportPassword,
                    Some(ArchiveMsg::Open),
                    idle,
                ))
                .width(Length::Fill),
                ui::text_button(
                    t,
                    "Open",
                    (idle && form.file.is_some()).then(|| msg(ArchiveMsg::Open)),
                    ui::secondary
                ),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        ),
    ];
    out = out.push(section(t, "Archive", archive_rows));

    let Some(opened) = &form.opened else {
        return out.into();
    };
    let plan = &opened.plan;
    out = out.push(
        text(archive_summary(plan, opened.archive.kind))
            .font(UI_FONT)
            .size(12.5)
            .color(t.text_secondary),
    );
    if let Some(w) = warnings(t, plan.warnings.clone()) {
        out = out.push(w);
    }
    for line in scheme_lines(plan) {
        out = out.push(
            text(line)
                .font(UI_FONT)
                .size(12)
                .color(t.text_secondary)
                .wrapping(text::Wrapping::WordOrGlyph),
        );
    }
    let rows: Vec<Element<'a, Message>> = preview_rows(plan, profiles)
        .into_iter()
        .enumerate()
        .map(|(i, r)| preview_row(t, i, r, idle))
        .collect();
    out = out.push(section(t, "Profiles", rows));

    if plan.has_keys {
        let dir = form.key_dir.clone().or_else(default_key_dir);
        let shown = dir.as_deref().map_or_else(|| "None".into(), shown_dir);
        out = out.push(section(
            t,
            "Key files",
            vec![padded(
                row![
                    column![
                        text(shown).font(MONO).size(12.5).color(t.text),
                        text("Written readable by you only; an existing file is never replaced.")
                            .font(UI_FONT)
                            .size(12)
                            .color(t.text_muted),
                    ]
                    .spacing(3)
                    .width(Length::Fill),
                    ui::text_button(
                        t,
                        "Choose folder\u{2026}",
                        idle.then(|| msg(ArchiveMsg::ChooseKeyFolder)),
                        ui::secondary
                    ),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
            )],
        ));
    }
    out.into()
}

fn preview_row<'a>(
    t: &'static Tokens,
    i: usize,
    r: PreviewRow,
    idle: bool,
) -> Element<'a, Message> {
    let mut meta = Vec::new();
    if !r.jumps.is_empty() {
        meta.push(format!("via {}", r.jumps));
    }
    match r.tunnels {
        0 => {}
        1 => meta.push("1 tunnel".into()),
        n => meta.push(format!("{n} tunnels")),
    }
    let is_new = r.matched == NEW;
    let choices: Vec<Element<'a, Message>> = r
        .choices
        .iter()
        .map(|&c| {
            segment(
                t,
                c.label(),
                r.choice == c,
                idle.then(|| msg(ArchiveMsg::Choice(i, c))),
            )
        })
        .collect();
    let mut words = column![
        row![
            text(r.name).font(UI_FONT).size(13.5).color(t.text),
            tag(t, r.matched, if is_new { t.success } else { t.warning }),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        text(r.host).font(MONO).size(12).color(t.text_secondary),
    ]
    .spacing(3)
    .width(Length::Fill);
    if !meta.is_empty() {
        words = words.push(
            text(meta.join(" \u{b7} "))
                .font(UI_FONT)
                .size(12)
                .color(t.text_muted),
        );
    }
    if let Some(e) = r.error {
        words = words.push(
            text(e)
                .font(UI_FONT)
                .size(12)
                .color(t.danger)
                .wrapping(text::Wrapping::WordOrGlyph),
        );
    }
    padded(
        row![words, segmented(t, choices)]
            .spacing(12)
            .align_y(Alignment::Center),
    )
}

fn footer<'a>(
    t: &'static Tokens,
    state: &'a ArchiveState,
    read_only: bool,
) -> Element<'a, Message> {
    let idle = state.busy.is_none() && state.dialog.is_none();
    let (problem, notice) = match state.mode {
        ArchiveMode::Export => (&state.export.problem, &state.export.notice),
        ArchiveMode::Import => (&state.import.problem, &state.import.notice),
    };
    let mut note = column![].spacing(3).width(Length::Fill);
    let line = |s: String, color: Color| {
        text(s)
            .font(UI_FONT)
            .size(12)
            .color(color)
            .wrapping(text::Wrapping::WordOrGlyph)
    };
    if let Some((_, busy)) = state.busy {
        let s = match busy {
            Busy::Sealing => SEALING,
            Busy::Opening => OPENING,
        };
        note = note.push(line(s.into(), t.text_secondary));
    } else if let Some(p) = problem {
        note = note.push(line(p.clone(), t.danger));
    } else if let Some(n) = notice {
        note = note.push(line(
            n.text.clone(),
            if n.error { t.danger } else { t.text_secondary },
        ));
    }
    let primary = match state.mode {
        ArchiveMode::Export => ui::text_button(
            t,
            "Export\u{2026}",
            idle.then(|| msg(ArchiveMsg::Export)),
            ui::primary,
        ),
        ArchiveMode::Import => ui::text_button(
            t,
            "Import",
            (idle && !read_only && state.import.opened.is_some()).then(|| msg(ArchiveMsg::Import)),
            ui::primary,
        ),
    };
    container(
        row![
            note,
            ui::text_button(t, "Close", Some(msg(ArchiveMsg::Close)), ui::secondary),
            primary,
        ]
        .spacing(10)
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
