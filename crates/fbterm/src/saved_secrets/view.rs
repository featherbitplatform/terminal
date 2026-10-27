//! The main password dialogs: `Unlock saved secrets`, `Set a main password` (typed twice)
//! and the `Reset vault` confirmation, on a card over the window that asked.

use iced::widget::{column, text, text_input};
use iced::{Element, Padding};

use super::{
    Kind, Purpose, RESET_QUESTION, RESET_TITLE, SET_TITLE, UNLOCK_TITLE, VaultDialog, VaultMsg,
};
use crate::app::Message;
use crate::prompt_view::{buttons, header, modal};
use crate::redact::Redacted;
use crate::theme::{Tokens, UI_FONT};
use crate::ui;

fn msg(m: VaultMsg) -> Message {
    Message::Vault(m)
}

pub fn view<'a>(t: &'static Tokens, d: &'a VaultDialog) -> Element<'a, Message> {
    let idle = d.busy.is_none();
    let line = |s: &'a str| text(s).font(UI_FONT).size(13).color(t.text_secondary);
    let field = |placeholder: &'a str,
                 value: &'a str,
                 id: &iced::widget::Id,
                 on: fn(Redacted) -> VaultMsg| {
        text_input(placeholder, value)
            .id(id.clone())
            .secure(true)
            .on_input_maybe(idle.then_some(move |v| msg(on(Redacted(v)))))
            .on_submit_maybe(idle.then(|| msg(VaultMsg::Submit)))
            .font(UI_FONT)
            .size(13.5)
            .padding(Padding::from([8.0, 10.0]))
            .style(move |_, s| ui::input_style(t, s))
    };
    let (title, about, action, danger) = match d.kind {
        Kind::Unlock => (
            UNLOCK_TITLE,
            "Saved passwords and passphrases are in the encrypted vault. Enter its main password to use them.",
            "Unlock",
            false,
        ),
        Kind::Set => (
            SET_TITLE,
            "Saved passwords and passphrases will be kept in an encrypted vault opened with this password. It cannot be recovered if forgotten.",
            if matches!(d.purpose, Purpose::Switch(_)) {
                "Set and move"
            } else {
                "Set password"
            },
            false,
        ),
        Kind::Reset => (RESET_TITLE, RESET_QUESTION, "Reset vault", true),
    };
    let mut body = column![
        header(
            t,
            if danger {
                ui::ICON_WARNING
            } else {
                ui::ICON_LOCK
            },
            if danger { t.danger } else { t.accent_edge },
            title
        ),
        line(about),
    ]
    .spacing(14);
    match d.kind {
        Kind::Unlock => {
            body = body.push(field(
                "Main password",
                &d.password,
                &d.input,
                VaultMsg::Password,
            ));
        }
        Kind::Set => {
            body = body.push(
                column![
                    field("Main password", &d.password, &d.input, VaultMsg::Password),
                    field(
                        "Type it again",
                        &d.confirm,
                        &d.confirm_input,
                        VaultMsg::Confirm
                    ),
                ]
                .spacing(8),
            );
        }
        Kind::Reset => {}
    }
    if let Some(busy) = d.busy.map(|_| d.busy_text()) {
        body = body.push(text(busy).font(UI_FONT).size(12).color(t.text_secondary));
    } else if let Some(p) = &d.problem {
        body = body.push(text(p.as_str()).font(UI_FONT).size(12).color(t.danger));
    }
    body = body.push(buttons(vec![
        ui::text_button(
            t,
            "Cancel",
            idle.then(|| msg(VaultMsg::Cancel)),
            ui::secondary,
        )
        .into(),
        ui::text_button(
            t,
            action,
            d.can_submit().then(|| msg(VaultMsg::Submit)),
            ui::primary,
        )
        .into(),
    ]));
    modal(t, body.into())
}
