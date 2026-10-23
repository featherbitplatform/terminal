//! The SSH parts of the editor form: authentication and the jump chain.

use std::fmt;

use fbterm_config::ProfileId;
use iced::widget::{button, checkbox, column, container, pick_list, row, text};
use iced::{Alignment, Border, Color, Element, Length, Padding};

use super::view::{detail, input, msg};
use super::{AuthChoice, EditorMsg, EditorState, SshDraft};
use crate::app::Message;
use crate::redact::Redacted;
use crate::theme::{Tokens, UI_FONT};
use crate::ui::{self, MONO};

pub(super) fn auth<'a>(
    t: &'static Tokens,
    s: &'a SshDraft,
    editable: bool,
    degraded: bool,
    rsa_key: bool,
) -> Element<'a, Message> {
    let segment = |caption: &'a str, choice: AuthChoice| {
        let selected = s.auth == choice;
        button(text(caption).font(UI_FONT).size(12.5))
            .on_press_maybe(editable.then(|| msg(EditorMsg::Auth(choice))))
            .padding(Padding::from([6.0, 14.0]))
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
    };
    let segments = container(
        row![
            segment("Password", AuthChoice::Password),
            segment("Key file", AuthChoice::Key),
            segment("Agent", AuthChoice::Agent),
        ]
        .spacing(2),
    )
    .padding(2)
    .style(move |_| container::Style {
        background: Some(t.surface.into()),
        border: Border {
            color: t.border,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..container::Style::default()
    });
    let secret = |caption: &'a str, placeholder: &'a str| -> Element<'a, Message> {
        column![
            ui::label(t, caption),
            input(
                t,
                placeholder,
                &s.secret,
                false,
                editable.then_some(secret_msg as fn(String) -> EditorMsg)
            )
            .secure(true),
        ]
        .spacing(6)
        .into()
    };
    let mut col = column![segments].spacing(12);
    match s.auth {
        AuthChoice::Password => {
            col = col.push(secret(
                "Password",
                "Leave empty to keep the saved one or be asked when connecting",
            ));
        }
        AuthChoice::Key => {
            let browse = ui::text_button(
                t,
                "Browse",
                editable.then(|| msg(EditorMsg::PickKey)),
                ui::secondary,
            );
            col = col
                .push(
                    column![
                        ui::label(t, "Private key"),
                        row![
                            input(
                                t,
                                "~/.ssh/id_ed25519",
                                &s.key_path,
                                true,
                                editable.then_some(EditorMsg::KeyPath as fn(String) -> EditorMsg)
                            )
                            .width(Length::Fill),
                            browse
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                    ]
                    .spacing(6),
                )
                .push(
                    checkbox(s.key_passphrase)
                        .label("The key has a passphrase")
                        .on_toggle_maybe(editable.then_some(|b| msg(EditorMsg::KeyPassphrase(b))))
                        .font(UI_FONT)
                        .text_size(13)
                        .size(15),
                );
            if rsa_key {
                col = col.push(
                    text(super::RSA_ADVICE)
                        .font(UI_FONT)
                        .size(12.5)
                        .color(t.warning),
                );
            }
            if s.key_passphrase {
                col = col.push(secret(
                    "Passphrase",
                    "Leave empty to keep the saved one or be asked when connecting",
                ));
            }
        }
        AuthChoice::Agent => {
            col = col.push(
                text("Uses the keys of the running SSH agent.")
                    .font(UI_FONT)
                    .size(12.5)
                    .color(t.text_muted),
            );
        }
    }
    let stores_secret =
        matches!(s.auth, AuthChoice::Password) || (s.auth == AuthChoice::Key && s.key_passphrase);
    if stores_secret {
        let note = if degraded {
            "Keychain unavailable: passwords are kept for this session only"
        } else {
            "Saved in the system keychain, never in profiles.toml."
        };
        col = col.push(text(note).font(UI_FONT).size(12).color(if degraded {
            t.warning
        } else {
            t.text_muted
        }));
    }
    col.into()
}

/// A jump host choice in the "add" list.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hop {
    id: ProfileId,
    name: String,
}

impl fmt::Display for Hop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

pub(super) fn jumps<'a>(
    t: &'static Tokens,
    state: &'a EditorState,
    id: ProfileId,
    s: &'a SshDraft,
    editable: bool,
) -> Element<'a, Message> {
    let mut col = column![].spacing(6);
    let n = s.jump.len();
    if n == 0 {
        col = col.push(
            text("Connects directly.")
                .font(UI_FONT)
                .size(12.5)
                .color(t.text_muted),
        );
    } else {
        col = col.push(
            text("Connects through these hosts, in order.")
                .font(UI_FONT)
                .size(12.5)
                .color(t.text_muted),
        );
    }
    let tool = |glyph: &'static [u8], m: Option<EditorMsg>| {
        let m = m.filter(|_| editable);
        let tint = if m.is_some() {
            t.text_secondary
        } else {
            t.border
        };
        button(ui::icon(glyph, 13.0, tint))
            .on_press_maybe(m.map(msg))
            .padding(5)
            .style(move |_, st| ui::ghost(t, st))
    };
    let mut list = column![].spacing(0);
    for (i, hop) in s.jump.iter().enumerate() {
        let (name, at) = match state.draft(*hop) {
            Some(h) => (h.name.clone(), detail(h)),
            None => ("missing profile".to_owned(), hop.to_string()),
        };
        list = list.push(
            container(
                row![
                    text(format!("{}", i + 1))
                        .font(MONO)
                        .size(12)
                        .color(t.text_muted)
                        .width(18),
                    text(name).font(UI_FONT).size(13).color(t.text),
                    text(at)
                        .font(MONO)
                        .size(12)
                        .color(t.text_muted)
                        .width(Length::Fill),
                    tool(ui::ICON_UP, (i > 0).then_some(EditorMsg::JumpUp(i))),
                    tool(ui::ICON_DOWN, (i + 1 < n).then_some(EditorMsg::JumpDown(i))),
                    tool(ui::ICON_X, Some(EditorMsg::JumpRemove(i))),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([4.0, 10.0]))
            .style(move |_| container::Style {
                border: Border {
                    color: t.border,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                ..container::Style::default()
            }),
        );
    }
    if n > 0 {
        col = col.push(
            container(list)
                .padding(Padding::from([4.0, 0.0]))
                .width(Length::Fill)
                .style(move |_| ui::card(t)),
        );
    }
    let options: Vec<Hop> = state
        .jump_options(id)
        .into_iter()
        .map(|(id, name)| Hop { id, name })
        .collect();
    if editable && !options.is_empty() {
        col = col.push(
            pick_list(options, None::<Hop>, |h: Hop| msg(EditorMsg::JumpAdd(h.id)))
                .placeholder("Add a jump host")
                .font(UI_FONT)
                .text_size(13)
                .padding(Padding::from([7.0, 10.0]))
                .width(240),
        );
    }
    col.into()
}

fn secret_msg(v: String) -> EditorMsg {
    EditorMsg::Secret(Redacted(v))
}
