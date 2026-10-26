//! The Settings window: Appearance, Terminal and Keyboard cards of labelled controls, and a
//! footer with the settings file actions.

use fbterm_config::ThemeMode;
use fbterm_config::keys::{Action, ActionGroup, Bindings, risk};
use fbterm_config::settings::{FONT_SIZE_RANGE, OPACITY_RANGE};
use iced::widget::{
    Space, button, column, container, pick_list, row, scrollable, slider, text, text_input, toggler,
};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding};

use super::{RecordProblem, Recording, SettingsMsg, SettingsState};
use crate::app::Message;
use crate::theme::{Tokens, UI_FONT};
use crate::ui::{self, MONO};

const CONTENT_WIDTH: f32 = 560.0;

fn msg(m: SettingsMsg) -> Message {
    Message::Settings(m)
}

/// `notice`: the banner line about `settings.toml` (a load error or clamped values), if any.
pub fn view<'a>(
    t: &'static Tokens,
    state: &'a SettingsState,
    notice: Option<&'a str>,
) -> Element<'a, Message> {
    let editable = state.read_only.is_none();
    let mut body = column![
        column![
            text("Settings").font(UI_FONT).size(18).color(t.text),
            text("Changes apply right away and are saved to settings.toml.")
                .font(UI_FONT)
                .size(12.5)
                .color(t.text_muted),
        ]
        .spacing(4)
    ]
    .spacing(18)
    .width(Length::Fill);
    if let Some(err) = &state.read_only {
        body = body.push(read_only_banner(t, err));
    }
    body = body
        .push(section(t, "Appearance", appearance(t, state, editable)))
        .push(section(t, "Terminal", terminal(t, state, editable)))
        .push(keyboard(t, state, editable));
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
    container(column![page, footer(t, state, notice)])
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
                text("settings.toml could not be read. Settings are read-only until it is fixed.")
                    .font(UI_FONT)
                    .size(13)
                    .color(t.text),
                text(location).font(MONO).size(12).color(t.text_secondary),
                text(err.message.as_str())
                    .font(MONO)
                    .size(12)
                    .color(t.danger),
            ]
            .spacing(3)
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
    })
    .into()
}

/// A titled card of setting rows, separated by hairlines.
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

/// One setting: its name and a one-line hint on the left, the control on the right.
/// `dimmed`: the setting has no effect right now.
fn setting<'a>(
    t: &'static Tokens,
    label: &'a str,
    hint: Option<&'a str>,
    dimmed: bool,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut words = column![text(label).font(UI_FONT).size(13).color(if dimmed {
        t.text_secondary
    } else {
        t.text
    })]
    .spacing(3)
    .width(Length::Fill);
    if let Some(h) = hint {
        words = words.push(text(h).font(UI_FONT).size(12).color(t.text_muted));
    }
    row![words, control.into()]
        .spacing(16)
        .align_y(Alignment::Center)
        .padding(Padding::from([12.0, 16.0]))
        .into()
}

fn appearance<'a>(
    t: &'static Tokens,
    state: &'a SettingsState,
    editable: bool,
) -> Vec<Element<'a, Message>> {
    let v = &state.values;
    let theme = row![
        segment(
            t,
            "Dark",
            v.theme == ThemeMode::Dark,
            editable,
            ThemeMode::Dark
        ),
        segment(
            t,
            "Light",
            v.theme == ThemeMode::Light,
            editable,
            ThemeMode::Light
        ),
    ]
    .spacing(2);
    let theme = container(theme).padding(2).style(move |_| well(t));
    let fonts = pick_list(state.fonts.as_slice(), state.selected_font(), |f| {
        msg(SettingsMsg::Font(f))
    })
    .font(MONO)
    .text_size(12.5)
    .padding(Padding::from([6.0, 10.0]))
    .width(240)
    .style(move |_, status| pick_style(t, status))
    .menu_style(move |_| menu_style(t));
    let size = row![
        slider(FONT_SIZE_RANGE.0..=FONT_SIZE_RANGE.1, v.font_size, |x| msg(
            SettingsMsg::FontSize(x)
        ))
        .step(0.5_f32)
        .on_release(msg(SettingsMsg::Release))
        .width(160)
        .style(move |_, status| slider_style(t, status, editable)),
        number(
            t,
            &state.font_size_text,
            editable,
            SettingsMsg::FontSizeText,
            SettingsMsg::FontSizeSubmit
        ),
        unit(t, "pt"),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    let opacity = row![
        slider(
            OPACITY_RANGE.0..=OPACITY_RANGE.1,
            v.background_opacity,
            |x| msg(SettingsMsg::Opacity(x))
        )
        .step(0.01_f32)
        .on_release(msg(SettingsMsg::Release))
        .width(160)
        .style(move |_, status| slider_style(t, status, editable)),
        number(
            t,
            &state.opacity_text,
            editable,
            SettingsMsg::OpacityText,
            SettingsMsg::OpacitySubmit
        ),
        unit(t, "%"),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    let (blur_hint, blur_inert) = blur_hint(cfg!(target_os = "macos"), v.background_opacity);
    vec![
        setting(t, "Theme", None, false, theme),
        setting(t, "Font family", None, false, fonts),
        setting(t, "Font size", None, false, size),
        setting(
            t,
            "Background opacity",
            Some("Text stays opaque"),
            false,
            opacity,
        ),
        setting(
            t,
            "Background blur",
            Some(blur_hint),
            blur_inert,
            switch(
                t,
                v.background_blur,
                editable,
                blur_inert,
                SettingsMsg::Blur,
            ),
        ),
    ]
}

fn terminal<'a>(
    t: &'static Tokens,
    state: &'a SettingsState,
    editable: bool,
) -> Vec<Element<'a, Message>> {
    let scrollback = row![
        text_input("10000", &state.scrollback_text)
            .on_input_maybe(editable.then_some(|s| msg(SettingsMsg::ScrollbackText(s))))
            .on_submit_maybe(editable.then(|| msg(SettingsMsg::ScrollbackSubmit)))
            .font(MONO)
            .size(12.5)
            .padding(Padding::from([6.0, 10.0]))
            .width(110)
            .align_x(Alignment::End)
            .style(move |_, s| ui::input_style(t, s)),
        unit(t, "lines"),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    let rate = pick_list(state.rate_choices(), state.selected_rate(), |c| {
        msg(SettingsMsg::MaxFps(c.value))
    })
    .font(MONO)
    .text_size(12.5)
    .padding(Padding::from([6.0, 10.0]))
    .width(200)
    .style(move |_, status| pick_style(t, status))
    .menu_style(move |_| menu_style(t));
    vec![
        scrollback_row(t, state, scrollback),
        setting(
            t,
            "Kitty keyboard protocol",
            Some("Lets programs ask for a richer key encoding (modifiers, key releases)"),
            false,
            switch(
                t,
                state.values.kitty_keyboard,
                editable,
                false,
                SettingsMsg::Kitty,
            ),
        ),
        setting(t, "Redraw rate", Some(state.rate_hint()), false, rate),
    ]
}

/// The Scrollback row. While a lowered value waits, a warning with Apply and Cancel sits under
/// it; above the threshold a quiet note does.
fn scrollback_row<'a>(
    t: &'static Tokens,
    state: &'a SettingsState,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    let row_ = setting(
        t,
        "Scrollback",
        Some("Lines of history kept per tab, open ones included"),
        false,
        control,
    );
    let extra: Option<Element<'a, Message>> = if let Some(why) = state.scrollback_warning() {
        Some(
            container(
                row![
                    ui::icon(ui::ICON_WARNING, 13.0, t.warning),
                    text(why)
                        .font(UI_FONT)
                        .size(12)
                        .color(t.text)
                        .width(Length::Fill),
                    small_button(t, "Apply", msg(SettingsMsg::ScrollbackApply), ui::primary),
                    small_button(
                        t,
                        "Cancel",
                        msg(SettingsMsg::ScrollbackCancel),
                        ui::secondary
                    ),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([8.0, 12.0]))
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(t.raised.into()),
                border: Border {
                    color: t.warning,
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..container::Style::default()
            })
            .into(),
        )
    } else {
        state.scrollback_note().map(|n| {
            text(n)
                .font(UI_FONT)
                .size(12)
                .color(t.text_muted)
                .width(Length::Fill)
                .into()
        })
    };
    match extra {
        Some(e) => column![
            row_,
            container(e).padding(Padding::from([0.0, 16.0]).bottom(12.0))
        ]
        .into(),
        None => row_,
    }
}

/// The Keyboard card: every action with its chord, by group. A click on a chord records a new
/// one; changed rows can go back to their default, any row can be left without a chord.
fn keyboard<'a>(
    t: &'static Tokens,
    state: &'a SettingsState,
    editable: bool,
) -> Element<'a, Message> {
    let bindings = state.bindings();
    let changed = !state.values.keys.map.is_empty();
    let header = row![
        column![
            text("Keyboard").font(UI_FONT).size(13.5).color(t.text),
            text("Click a shortcut to record a new one.")
                .font(UI_FONT)
                .size(12)
                .color(t.text_muted),
        ]
        .spacing(3)
        .width(Length::Fill),
        ui::text_button(
            t,
            "Restore defaults",
            (editable && changed).then(|| msg(SettingsMsg::ResetKeys)),
            ui::secondary
        ),
    ]
    .spacing(12)
    .align_y(Alignment::End);
    let mut list = column![];
    for (g, group) in ActionGroup::ALL.into_iter().enumerate() {
        if g > 0 {
            list = list.push(hairline(t));
        }
        list = list.push(
            container(
                text(group.title())
                    .font(UI_FONT)
                    .size(11.5)
                    .color(t.text_muted),
            )
            .padding(Padding {
                top: 12.0,
                right: 16.0,
                bottom: 2.0,
                left: 16.0,
            }),
        );
        for action in Action::ALL.into_iter().filter(|a| a.group() == group) {
            let recording = state.recording.as_ref().filter(|r| r.action == action);
            list = list.push(key_row(t, &bindings, action, recording, editable));
        }
    }
    let list = container(list.padding(Padding {
        top: 0.0,
        right: 0.0,
        bottom: 8.0,
        left: 0.0,
    }))
    .width(Length::Fill)
    .style(move |_| ui::card(t));
    column![header, list].spacing(8).into()
}

/// Width of a chord in the Keyboard section.
const PILL_WIDTH: f32 = 150.0;
/// Size of the small icon buttons of a row (a 13 px icon with 5 px padding).
const CLEAR_SIZE: f32 = 23.0;

/// One action: its name, then reset and clear buttons and its chord; below, what recording
/// says or a warning when the chord takes a key from programs in the terminal.
fn key_row<'a>(
    t: &'static Tokens,
    bindings: &Bindings,
    action: Action,
    recording: Option<&'a Recording>,
    editable: bool,
) -> Element<'a, Message> {
    let mac = bindings.mac();
    let chord = bindings.chord(action);
    let small = |glyph, tip_text: &'static str, m: SettingsMsg| -> Element<'a, Message> {
        let b = button(ui::icon(glyph, 13.0, t.text_muted))
            .on_press_maybe(editable.then(|| msg(m)))
            .padding(5)
            .style(move |_, status| ui::ghost(t, status));
        crate::tooltip::tip(t, b, tip_text, false)
    };
    let fixed = action.is_fixed(mac);
    let editable = editable && !fixed;
    let title = || {
        text(action.title())
            .font(UI_FONT)
            .size(13)
            .color(t.text)
            .width(Length::Fill)
    };
    let mut line = row![title()].spacing(4).align_y(Alignment::Center);
    if !bindings.is_default(action) {
        line = line.push(small(
            ui::ICON_RESET,
            "Reset to default",
            SettingsMsg::ResetKey(action),
        ));
    }
    // The clear button shows only while the pointer is on the row (see `hover` below); this
    // keeps its place.
    line = line.push(Space::new().width(CLEAR_SIZE).height(CLEAR_SIZE));
    let (caption, color) = match (recording, chord) {
        (Some(_), _) => ("Press a shortcut".to_owned(), t.accent),
        (None, Some(c)) => (c.label(mac), t.text),
        (None, None) => ("Not set".to_owned(), t.text_muted),
    };
    let active = recording.is_some();
    let pill =
        button(container(text(caption).font(MONO).size(12).color(color)).center_x(Length::Fill))
            .on_press_maybe(editable.then(|| {
                msg(if active {
                    SettingsMsg::CancelRecording
                } else {
                    SettingsMsg::Record(action)
                })
            }))
            .width(PILL_WIDTH)
            .padding(Padding::from([4.0, 10.0]))
            .style(move |_, status| {
                let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
                button::Style {
                    background: Some(if hovered && !active { t.hover } else { t.app }.into()),
                    text_color: t.text,
                    border: Border {
                        color: if active { t.accent_edge } else { t.border },
                        width: 1.0,
                        radius: 6.0.into(),
                    },
                    ..button::Style::default()
                }
            });
    let line = line.push(Space::new().width(4)).push(pill);
    let line: Element<'a, Message> = if chord.is_some() && editable && recording.is_none() {
        let clear = row![
            Space::new().width(Length::Fill),
            small(ui::ICON_X, "Remove shortcut", SettingsMsg::ClearKey(action)),
            Space::new().width(4),
            Space::new().width(PILL_WIDTH),
        ]
        .spacing(4)
        .align_y(Alignment::Center);
        iced::widget::hover(line, clear)
    } else {
        line.into()
    };
    let note: Option<Element<'a, Message>> = match recording {
        Some(Recording { problem: None, .. }) => Some(
            text("Press the new shortcut. Esc cancels.")
                .font(UI_FONT)
                .size(12)
                .color(t.text_muted)
                .into(),
        ),
        Some(Recording {
            problem: Some(RecordProblem::NotAllowed(why)),
            ..
        }) => Some(
            text(format!("{why}. Press another shortcut, or Esc to cancel."))
                .font(UI_FONT)
                .size(12)
                .color(t.danger)
                .into(),
        ),
        Some(Recording {
            problem: Some(RecordProblem::Taken { chord, other }),
            ..
        }) => Some(
            row![
                text(format!(
                    "{} is used by {}. Take it over? {} will have no shortcut.",
                    chord.label(mac),
                    other.title(),
                    other.title()
                ))
                .font(UI_FONT)
                .size(12)
                .color(t.text_secondary)
                .width(Length::Fill),
                small_button(t, "Take over", msg(SettingsMsg::TakeOver), ui::primary),
                small_button(
                    t,
                    "Cancel",
                    msg(SettingsMsg::CancelRecording),
                    ui::secondary
                ),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into(),
        ),
        None if fixed => Some(
            text("Fixed on macOS: the app menu owns it.")
                .font(UI_FONT)
                .size(12)
                .color(t.text_muted)
                .into(),
        ),
        None => chord
            .filter(|_| !action.is_find_navigation())
            .and_then(|c| risk(mac, c))
            .map(|why| {
                row![
                    ui::icon(ui::ICON_WARNING, 13.0, t.warning),
                    text(why)
                        .font(UI_FONT)
                        .size(12)
                        .color(t.text_secondary)
                        .width(Length::Fill),
                ]
                .spacing(6)
                .align_y(Alignment::Center)
                .into()
            }),
    };
    let mut body = column![line].spacing(6);
    if let Some(n) = note {
        body = body.push(n);
    }
    container(body)
        .padding(Padding::from([5.0, 16.0]))
        .width(Length::Fill)
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

fn footer<'a>(
    t: &'static Tokens,
    state: &'a SettingsState,
    notice: Option<&'a str>,
) -> Element<'a, Message> {
    let mut note = column![].spacing(3).width(Length::Fill);
    if let Some(e) = &state.write_error {
        note = note.push(text(e.as_str()).font(UI_FONT).size(12).color(t.danger));
    } else if state.read_only.is_some() {
        note = note.push(
            text("Read-only until settings.toml is fixed")
                .font(UI_FONT)
                .size(12)
                .color(t.danger),
        );
    } else if let Some(n) = notice {
        note = note.push(
            row![
                ui::icon(ui::ICON_SHIELD_ALERT, 13.0, t.warning),
                text(n)
                    .font(MONO)
                    .size(11.5)
                    .color(t.text_secondary)
                    .wrapping(text::Wrapping::WordOrGlyph),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        );
    }
    container(
        row![
            note,
            ui::text_button(
                t,
                "Reveal config folder",
                Some(msg(SettingsMsg::RevealFolder)),
                ui::secondary
            ),
            ui::text_button(
                t,
                "Open settings file",
                Some(msg(SettingsMsg::OpenFile)),
                ui::secondary
            ),
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

fn unit<'a>(t: &'static Tokens, s: &'a str) -> Element<'a, Message> {
    container(text(s).font(UI_FONT).size(12).color(t.text_muted))
        .width(32)
        .into()
}

/// A short typed number next to a slider.
fn number<'a>(
    t: &'static Tokens,
    value: &'a str,
    editable: bool,
    on_input: fn(String) -> SettingsMsg,
    on_submit: SettingsMsg,
) -> Element<'a, Message> {
    text_input("", value)
        .on_input_maybe(editable.then_some(move |s| msg(on_input(s))))
        .on_submit_maybe(editable.then(|| msg(on_submit)))
        .font(MONO)
        .size(12.5)
        .padding(Padding::from([6.0, 8.0]))
        .width(56)
        .align_x(Alignment::End)
        .style(move |_, s| ui::input_style(t, s))
        .into()
}

/// The blur row's hint, and whether blur has no effect now (it is still saved): only macOS
/// blurs for now, and only below full opacity.
pub fn blur_hint(mac: bool, opacity: f32) -> (&'static str, bool) {
    if !mac {
        ("macOS only for now", true)
    } else if opacity >= 1.0 {
        ("Needs opacity below 100%", true)
    } else {
        ("Applies to new windows", false)
    }
}

/// A toggle; `inert`: it still works, but is drawn muted because it has no effect right now.
fn switch<'a>(
    t: &'static Tokens,
    on: bool,
    editable: bool,
    inert: bool,
    f: fn(bool) -> SettingsMsg,
) -> Element<'a, Message> {
    toggler(on)
        .on_toggle_maybe(editable.then_some(move |b| msg(f(b))))
        .size(18)
        .style(move |_, status| toggler_style(t, status, inert))
        .into()
}

fn segment<'a>(
    t: &'static Tokens,
    caption: &'a str,
    selected: bool,
    editable: bool,
    mode: ThemeMode,
) -> Element<'a, Message> {
    button(text(caption).font(UI_FONT).size(12.5))
        .on_press_maybe(editable.then(|| msg(SettingsMsg::Theme(mode))))
        .padding(Padding::from([4.0, 14.0]))
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

fn well(t: &Tokens) -> container::Style {
    container::Style {
        background: Some(t.app.into()),
        border: Border {
            color: t.border,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..container::Style::default()
    }
}

/// `editable` false: drawn muted (a slider has no disabled state of its own).
fn slider_style(t: &Tokens, status: slider::Status, editable: bool) -> slider::Style {
    let active = !matches!(status, slider::Status::Active);
    if !editable {
        return slider::Style {
            rail: slider::Rail {
                backgrounds: (Background::Color(t.text_muted), Background::Color(t.border)),
                width: 4.0,
                border: Border {
                    radius: 2.0.into(),
                    ..Border::default()
                },
            },
            handle: slider::Handle {
                shape: slider::HandleShape::Circle { radius: 7.0 },
                background: Background::Color(t.raised),
                border_width: 2.0,
                border_color: t.text_muted,
            },
        };
    }
    slider::Style {
        rail: slider::Rail {
            backgrounds: (Background::Color(t.accent), Background::Color(t.border)),
            width: 4.0,
            border: Border {
                radius: 2.0.into(),
                ..Border::default()
            },
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Circle { radius: 7.0 },
            background: Background::Color(if active { t.accent_edge } else { t.text }),
            border_width: 2.0,
            border_color: t.accent,
        },
    }
}

fn toggler_style(t: &Tokens, status: toggler::Status, inert: bool) -> toggler::Style {
    let (on, hovered, disabled) = match status {
        toggler::Status::Active { is_toggled } => (is_toggled, false, false),
        toggler::Status::Hovered { is_toggled } => (is_toggled, true, false),
        toggler::Status::Disabled { is_toggled } => (is_toggled, false, true),
    };
    let track = match (on, disabled) {
        (_, true) => t.raised,
        // Muted like a read-only slider: on reads as the muted text color, off as the border.
        (true, false) if inert => t.text_muted,
        (false, false) if inert => t.border,
        (true, false) if hovered => t.accent_edge,
        (true, false) => t.accent,
        (false, false) if hovered => t.text_muted,
        (false, false) => t.border,
    };
    toggler::Style {
        background: Background::Color(track),
        background_border_width: if disabled { 1.0 } else { 0.0 },
        background_border_color: t.border,
        foreground: Background::Color(if disabled || inert {
            t.text_muted
        } else {
            Color::WHITE
        }),
        foreground_border_width: 0.0,
        foreground_border_color: Color::TRANSPARENT,
        text_color: None,
        border_radius: None,
        padding_ratio: 0.15,
    }
}

fn pick_style(t: &Tokens, status: pick_list::Status) -> pick_list::Style {
    let border = match status {
        pick_list::Status::Opened { .. } => t.accent_edge,
        pick_list::Status::Hovered => t.text_muted,
        pick_list::Status::Active => t.border,
    };
    pick_list::Style {
        text_color: t.text,
        placeholder_color: t.text_muted,
        handle_color: t.text_secondary,
        background: Background::Color(t.app),
        border: Border {
            color: border,
            width: 1.0,
            radius: 6.0.into(),
        },
    }
}

fn menu_style(t: &Tokens) -> iced::overlay::menu::Style {
    iced::overlay::menu::Style {
        background: Background::Color(t.raised),
        border: Border {
            color: t.border,
            width: 1.0,
            radius: 6.0.into(),
        },
        text_color: t.text,
        selected_text_color: t.text,
        selected_background: Background::Color(t.accent_soft),
        shadow: iced::Shadow::default(),
    }
}
