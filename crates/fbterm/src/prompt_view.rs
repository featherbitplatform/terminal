//! The connection dialog drawn over a terminal window: trust a host key, a changed host key,
//! or a password / key passphrase.

use fbterm_secrets::SecretKind;
use iced::widget::{Space, button, checkbox, column, container, opaque, row, text, text_input};
use iced::window;
use iced::{Alignment, Color, Element, Length, Padding};

use crate::app::Message;
use crate::prompts::{Dialog, PromptMsg, host_port, keygen_command};
use crate::redact::Redacted;
use crate::theme::{Tokens, UI_FONT};
use crate::ui::{self, MONO};

const CARD_WIDTH: f32 = 460.0;

pub fn view<'a>(
    t: &'static Tokens,
    window: window::Id,
    dialog: &'a Dialog,
) -> Element<'a, Message> {
    let msg = move |m| Message::Prompt(m);
    let body: Element<'a, Message> = match dialog {
        Dialog::HostKey {
            host,
            port,
            fingerprint,
            ..
        } => column![
            header(t, ui::ICON_SHIELD_ALERT, t.warning, "Unknown host"),
            text("This host has not been seen before. Check that the fingerprint matches the server's before trusting it; it is then added to known_hosts.")
                .font(UI_FONT)
                .size(13)
                .color(t.text_secondary),
            column![
                ui::label(t, "Host"),
                ui::value_well(t, host_port(host, *port)).width(Length::Fill),
            ]
            .spacing(6),
            column![
                ui::label(t, "Fingerprint (SHA-256)"),
                ui::value_well(t, fingerprint).width(Length::Fill),
            ]
            .spacing(6),
            buttons(vec![
                ui::text_button(t, "Cancel", Some(msg(PromptMsg::Cancel(window))), ui::secondary)
                    .into(),
                ui::text_button(
                    t,
                    "Trust and connect",
                    Some(msg(PromptMsg::Trust(window))),
                    ui::primary,
                )
                .into(),
            ]),
        ]
        .spacing(14)
        .into(),
        Dialog::Changed { host, port } => {
            let command = keygen_command(host, *port);
            let copy = button(
                row![
                    ui::icon(ui::ICON_COPY, 13.0, t.text_secondary),
                    text("Copy").font(UI_FONT).size(12.5)
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .on_press(msg(PromptMsg::Copy(command.clone())))
            .padding(Padding::from([6.0, 10.0]))
            .style(move |_, s| ui::secondary(t, s));
            column![
                header(t, ui::ICON_SHIELD_ALERT, t.danger, "Host key changed"),
                text(format!(
                    "The host key presented by {} does not match the one in known_hosts, so the connection was refused. Someone may be intercepting it.",
                    host_port(host, *port)
                ))
                .font(UI_FONT)
                .size(13)
                .color(t.text_secondary),
                text("If the server was reinstalled, remove its old entry and connect again:")
                    .font(UI_FONT)
                    .size(13)
                    .color(t.text_secondary),
                ui::well(
                    t,
                    row![
                        container(
                            text(command)
                                .font(MONO)
                                .size(12.5)
                                .color(t.text)
                                .wrapping(text::Wrapping::None)
                        )
                        .width(Length::Fill)
                        .clip(true),
                        copy
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center)
                )
                .padding(Padding {
                    top: 4.0,
                    right: 4.0,
                    bottom: 4.0,
                    left: 10.0
                })
                .width(Length::Fill),
                buttons(vec![
                    ui::text_button(t, "Close", Some(msg(PromptMsg::Cancel(window))), ui::secondary)
                        .into(),
                ]),
            ]
            .spacing(14)
            .into()
        }
        Dialog::Secret {
            profile,
            kind,
            retry,
            value,
            remember,
            input,
            ..
        } => {
            let (title, what) = match kind {
                SecretKind::Password => ("Password", "password"),
                SecretKind::KeyPassphrase => ("Key passphrase", "key passphrase"),
            };
            let field = text_input(&format!("Enter the {what}"), value)
                .id(input.clone())
                .secure(true)
                .on_input(move |v| msg(PromptMsg::Input(window, Redacted(v))))
                .on_submit_maybe(dialog.can_submit().then(|| msg(PromptMsg::Submit(window))))
                .font(UI_FONT)
                .size(13.5)
                .padding(Padding::from([8.0, 10.0]))
                .style(move |_, s| ui::input_style(t, s));
            let mut field_col = column![field].spacing(6);
            if *retry {
                field_col = field_col.push(
                    text("That didn't work, try again.")
                        .font(UI_FONT)
                        .size(12)
                        .color(t.danger),
                );
            }
            column![
                header(t, ui::ICON_LOCK, t.accent_edge, title),
                row![
                    text(format!("The {what} for")).font(UI_FONT).size(13).color(t.text_secondary),
                    text(profile.as_str()).font(MONO).size(13).color(t.text),
                ]
                .spacing(5)
                .align_y(Alignment::Center),
                field_col,
                checkbox(*remember)
                    .label("Remember in keychain")
                    .on_toggle(move |b| msg(PromptMsg::Remember(window, b)))
                    .font(UI_FONT)
                    .text_size(13)
                    .size(15),
                buttons(vec![
                    ui::text_button(t, "Cancel", Some(msg(PromptMsg::Cancel(window))), ui::secondary)
                        .into(),
                    ui::text_button(
                        t,
                        "Connect",
                        dialog.can_submit().then(|| msg(PromptMsg::Submit(window))),
                        ui::primary,
                    )
                        .into(),
                ]),
            ]
            .spacing(14)
            .into()
        }
    };
    let card = container(body)
        .width(CARD_WIDTH)
        .padding(20)
        .style(move |_| ui::overlay(t));
    let backdrop = container(opaque(card))
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .padding(Padding {
            top: 96.0,
            ..Padding::ZERO
        })
        .style(|_| container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.4).into()),
            ..container::Style::default()
        });
    // Clicks outside the card do nothing (the question needs an answer) and never reach the
    // terminal beneath.
    opaque(backdrop)
}

fn header<'a>(
    t: &'static Tokens,
    glyph: &'static [u8],
    tint: Color,
    title: &'a str,
) -> Element<'a, Message> {
    row![
        ui::icon(glyph, 18.0, tint),
        text(title).font(UI_FONT).size(16).color(t.text)
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

fn buttons(items: Vec<Element<'_, Message>>) -> Element<'_, Message> {
    let mut r = row![Space::new().width(Length::Fill)]
        .spacing(8)
        .align_y(Alignment::Center);
    for b in items {
        r = r.push(b);
    }
    r.into()
}
