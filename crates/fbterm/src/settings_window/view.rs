//! The Settings window: Appearance, Terminal, Tunnels, Saved secrets, Updates and Keyboard
//! cards of labelled controls, and a footer with the settings file actions.

use fbterm_config::keys::{Action, ActionGroup, Bindings, FixedShortcut, fixed_shortcuts, risk};
use fbterm_config::settings::{FONT_SIZE_RANGE, OPACITY_RANGE};
use fbterm_config::{SecretStorage, ThemeMode, WindowDecorations};
use iced::widget::{
    Space, button, column, container, pick_list, row, scrollable, slider, text, text_input, toggler,
};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding};

use super::{RecordProblem, Recording, SettingsMsg, SettingsState};
use crate::app::Message;
use crate::chrome::{self, Platform};
use crate::saved_secrets::{SectionInfo, choice_label, storage_note};
use crate::schemes::SchemeChoice;
use crate::theme::{Tokens, UI_FONT};
use crate::ui::{self, MONO};

const CONTENT_WIDTH: f32 = 560.0;
/// Diameter of the dot beside a newer version (the one on the settings button's size).
const UPDATE_DOT: f32 = 6.0;

/// The `Saved secrets` section's line while secrets move.
const MOVING: &str = "Moving saved secrets\u{2026}";
/// The `Saved secrets` section's line while the profile editor writes saved secrets.
pub(crate) const EDITOR_SAVING: &str = "Saving the profile editor's changes\u{2026}";

fn msg(m: SettingsMsg) -> Message {
    Message::Settings(m)
}

/// `notice`: the banner line about `settings.toml` (a load error or clamped values), if any.
/// `secrets`: what the `Saved secrets` section shows. `chrome_fell_back`: the desktop refused
/// drawn chrome this session (the `Title bar` row then shows `System`). `opaque_windows`: an
/// open terminal window was created opaque (see [`restart_note`]).
pub fn view<'a>(
    t: &'static Tokens,
    state: &'a SettingsState,
    schemes: Vec<SchemeChoice>,
    notice: Option<&'a str>,
    secrets: SectionInfo,
    chrome_fell_back: bool,
    opaque_windows: bool,
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
        .push(section(
            t,
            "Appearance",
            appearance(
                t,
                state,
                schemes,
                editable,
                chrome_fell_back,
                opaque_windows,
            ),
        ))
        .push(section(t, "Terminal", terminal(t, state, editable)))
        .push(section(t, "Tunnels", tunnels(t, state, editable)))
        .push(section(
            t,
            "Saved secrets",
            saved_secrets(t, secrets, editable),
        ))
        .push(section(t, "Updates", updates(t, state, editable)))
        .push(keyboard(t, state, editable));
    let page = scrollable(
        container(container(body).max_width(CONTENT_WIDTH))
            .padding(Padding::from([24.0, 32.0]))
            .center_x(Length::Fill),
    )
    .id(state.page.clone())
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
        t.text_muted
    } else {
        t.text
    })]
    .spacing(3)
    .width(Length::Fill);
    if let Some(h) = hint {
        let hint_color = if dimmed {
            Color {
                a: 0.5,
                ..t.text_muted
            }
        } else {
            t.text_muted
        };
        words = words.push(text(h).font(UI_FONT).size(12).color(hint_color));
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
    schemes: Vec<SchemeChoice>,
    editable: bool,
    chrome_fell_back: bool,
    opaque_windows: bool,
) -> Vec<Element<'a, Message>> {
    let v = &state.values;
    let theme = row![
        segment(
            t,
            "Dark",
            v.theme == ThemeMode::Dark,
            editable.then(|| msg(SettingsMsg::Theme(ThemeMode::Dark))),
        ),
        segment(
            t,
            "Light",
            v.theme == ThemeMode::Light,
            editable.then(|| msg(SettingsMsg::Theme(ThemeMode::Light))),
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
    let current = Some(v.color_scheme.as_str());
    let picker: Element<'a, Message> = if editable {
        let selected = crate::schemes::chosen(&schemes, current);
        pick_list(schemes, selected, |c: SchemeChoice| {
            msg(SettingsMsg::ColorScheme(c.id.unwrap_or_default()))
        })
        .placeholder(crate::schemes::NOT_FOUND)
        .font(UI_FONT)
        .text_size(12.5)
        .padding(Padding::from([6.0, 10.0]))
        .width(200)
        .style(move |_, status| pick_style(t, status))
        .menu_style(move |_| menu_style(t))
        .into()
    } else {
        fixed_choice(t, crate::schemes::shown(&schemes, current), 200.0)
    };
    let scheme = row![
        picker,
        small_button(
            t,
            "Edit schemes\u{2026}",
            msg(SettingsMsg::EditSchemes),
            ui::secondary
        ),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
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
        unit(t, "pt", UNIT_WIDTH),
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
        unit(t, "%", UNIT_WIDTH),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    let mut opacity_row = column![setting(
        t,
        "Background opacity",
        Some("Text stays opaque"),
        false,
        opacity,
    )];
    if let Some(note) = restart_note(v.background_opacity, opaque_windows) {
        opacity_row = opacity_row.push(
            container(
                row![
                    ui::icon(ui::ICON_WARNING, 13.0, t.warning),
                    text(note).font(UI_FONT).size(12).color(t.text_secondary),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .padding(Padding {
                top: 0.0,
                right: 16.0,
                bottom: 12.0,
                left: 16.0,
            }),
        );
    }
    let mut rows = vec![
        setting(t, "Theme", None, false, theme),
        setting(t, "Font family", None, false, fonts),
        setting(t, "Color scheme", None, false, scheme),
        setting(t, "Font size", None, false, size),
        opacity_row.into(),
        blur_setting(
            t,
            v.background_blur,
            editable,
            chrome::blur_here(),
            v.background_opacity,
        ),
    ];
    if shows_title_bar_row(chrome::setup().look) {
        let shown = title_bar_shown(v.window_decorations, chrome_fell_back);
        let title_bar = row![
            segment(
                t,
                "fbterm",
                shown == WindowDecorations::Fbterm,
                editable.then(|| msg(SettingsMsg::TitleBar(WindowDecorations::Fbterm))),
            ),
            segment(
                t,
                "System",
                shown == WindowDecorations::System,
                editable.then(|| msg(SettingsMsg::TitleBar(WindowDecorations::System))),
            ),
        ]
        .spacing(2);
        let title_bar = container(title_bar).padding(2).style(move |_| well(t));
        rows.push(setting(t, "Title bar", None, false, title_bar));
    }
    rows
}

/// The line under `Background opacity` at `opacity` when `opaque_windows` (an open terminal
/// window was created opaque, which never happens on macOS): such a window shows a lower
/// opacity only once fbterm restarts; windows opened from now on show it at once.
pub fn restart_note(opacity: f32, opaque_windows: bool) -> Option<&'static str> {
    (opacity < 1.0 && opaque_windows).then_some("Restart fbterm to apply this to open windows.")
}

/// The `Background blur` row: the switch for `on`, usable only when `editable` and the system
/// can blur (`supported`), with the hint for `opacity` (see [`blur_row`]).
pub(super) fn blur_setting<'a>(
    t: &'static Tokens,
    on: bool,
    editable: bool,
    supported: bool,
    opacity: f32,
) -> Element<'a, Message> {
    let blur = blur_row(supported, opacity);
    setting(
        t,
        "Background blur",
        Some(blur.hint),
        blur.inert,
        switch(
            t,
            on,
            editable && blur.enabled,
            blur.inert,
            SettingsMsg::Blur,
        ),
    )
}

/// What the `Title bar` row shows as chosen: what is on screen. After the desktop refused
/// drawn chrome that is `System` even though the setting stays `"fbterm"`, so one click on
/// `fbterm` tries drawn chrome again.
pub fn title_bar_shown(setting: WindowDecorations, fell_back: bool) -> WindowDecorations {
    if fell_back {
        WindowDecorations::System
    } else {
        setting
    }
}

/// Whether the Settings window offers the `Title bar` choice: only where windows follow
/// Linux's conventions (the `window_decorations` setting is ignored elsewhere).
pub fn shows_title_bar_row(look: Platform) -> bool {
    look == Platform::Linux
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
        unit(t, "lines", Length::Shrink),
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

fn tunnels<'a>(
    t: &'static Tokens,
    state: &'a SettingsState,
    editable: bool,
) -> Vec<Element<'a, Message>> {
    let retries = row![
        text_input("10", &state.retries_text)
            .on_input_maybe(editable.then_some(|s| msg(SettingsMsg::RetriesText(s))))
            .on_submit_maybe(editable.then(|| msg(SettingsMsg::RetriesSubmit)))
            .font(MONO)
            .size(12.5)
            .padding(Padding::from([6.0, 10.0]))
            .width(110)
            .align_x(Alignment::End)
            .style(move |_, s| ui::input_style(t, s)),
        unit(t, "attempts", Length::Shrink),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    vec![setting(
        t,
        "Reconnect attempts",
        Some("0 turns automatic reconnecting off"),
        false,
        retries,
    )]
}

/// The update check's switch and, when a newer version is known, a line with its download
/// link under it.
fn updates<'a>(
    t: &'static Tokens,
    state: &'a SettingsState,
    editable: bool,
) -> Vec<Element<'a, Message>> {
    let on = state.values.checks_updates();
    let mut rows = vec![setting(
        t,
        "Check for updates automatically",
        Some("Asks GitHub once a day for a newer version; nothing is downloaded"),
        false,
        switch(t, on, editable, false, SettingsMsg::CheckUpdates),
    )];
    if let Some(found) = state.update.as_ref().filter(|_| on) {
        rows.push(
            row![
                ui::dot(t.danger, UPDATE_DOT),
                text(found.message())
                    .font(UI_FONT)
                    .size(12.5)
                    .color(t.text)
                    .width(Length::Fill),
                small_button(t, "Download", msg(SettingsMsg::DownloadUpdate), ui::primary),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .padding(Padding::from([10.0, 16.0]))
            .into(),
        );
    }
    rows
}

/// Where saved passwords and passphrases live, with a line for a move under way or its
/// outcome, and `Reset vault` while a vault exists.
fn saved_secrets<'a>(
    t: &'static Tokens,
    info: SectionInfo,
    editable: bool,
) -> Vec<Element<'a, Message>> {
    let free = editable && !info.busy;
    let mut choices = row![].spacing(2);
    for choice in [
        SecretStorage::Auto,
        SecretStorage::Keychain,
        SecretStorage::Vault,
    ] {
        choices = choices.push(segment(
            t,
            choice_label(choice),
            info.choice == choice,
            free.then(|| msg(SettingsMsg::SecretStorage(choice))),
        ));
    }
    let choices = container(choices).padding(2).style(move |_| well(t));
    let hint = format!("{}.", storage_note(info.active));
    let mut storage = column![
        row![
            column![
                text("Storage").font(UI_FONT).size(13).color(t.text),
                text(hint).font(UI_FONT).size(12).color(t.text_muted),
            ]
            .spacing(3)
            .width(Length::Fill),
            choices,
        ]
        .spacing(16)
        .align_y(Alignment::Center),
    ]
    .spacing(8)
    .padding(Padding::from([12.0, 16.0]));
    let status = if info.busy {
        let line = if info.editor_saving {
            EDITOR_SAVING
        } else {
            MOVING
        };
        Some((line.to_owned(), t.text_secondary))
    } else {
        info.notice
            .map(|n| (n.text, if n.error { t.danger } else { t.text_secondary }))
    };
    if let Some((line, color)) = status {
        storage = storage.push(
            text(line)
                .font(UI_FONT)
                .size(12)
                .color(color)
                .wrapping(text::Wrapping::WordOrGlyph),
        );
    }
    let mut rows = vec![storage.into()];
    if info.vault_exists {
        let reset = button(text("Reset vault").font(UI_FONT).size(12))
            .on_press_maybe(free.then(|| msg(SettingsMsg::ResetVault)))
            .padding(Padding::from([3.0, 10.0]))
            .style(move |_, status| ui::danger_ghost(t, status));
        rows.push(setting(
            t,
            "Encrypted vault",
            Some("Forgot the main password? Reset deletes the saved secrets; profiles are kept."),
            false,
            reset,
        ));
    }
    rows
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
        list = list.push(group_title(t, group.title()));
        for action in Action::ALL.into_iter().filter(|a| a.group() == group) {
            let recording = state.recording.as_ref().filter(|r| r.action == action);
            list = list.push(key_row(t, &bindings, action, recording, editable));
        }
    }
    // The shortcuts outside the table: shown so they can be found, never recorded.
    let mac = bindings.mac();
    list = list.push(hairline(t)).push(group_title(t, "Fixed"));
    for shortcut in fixed_shortcuts(mac) {
        list = list.push(fixed_key_row(t, shortcut, mac));
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

/// The name of a group of shortcuts in the Keyboard section.
fn group_title<'a>(t: &'static Tokens, title: &'a str) -> Element<'a, Message> {
    container(text(title).font(UI_FONT).size(11.5).color(t.text_muted))
        .padding(Padding {
            top: 12.0,
            right: 16.0,
            bottom: 2.0,
            left: 16.0,
        })
        .into()
}

/// A shortcut that cannot be changed: its name, a `Fixed` tag and its chord, which is not a
/// button; below, its other chord and what to know about it.
pub(super) fn fixed_key_row<'a>(
    t: &'static Tokens,
    shortcut: FixedShortcut,
    mac: bool,
) -> Element<'a, Message> {
    let pill = container(
        text(shortcut.chord.label(mac))
            .font(MONO)
            .size(12)
            .color(t.text_secondary),
    )
    .center_x(PILL_WIDTH)
    .padding(Padding::from([4.0, 10.0]))
    .style(move |_| container::Style {
        border: Border {
            color: t.border,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..container::Style::default()
    });
    let line = row![
        text(shortcut.title)
            .font(UI_FONT)
            .size(13)
            .color(t.text)
            .width(Length::Fill),
        text("Fixed").font(UI_FONT).size(11.5).color(t.text_muted),
        Space::new().width(4),
        pill,
    ]
    .spacing(4)
    .align_y(Alignment::Center);
    let note = match (shortcut.also, shortcut.note) {
        (Some(also), Some(note)) => Some(format!("Also {}. {note}", also.label(mac))),
        (Some(also), None) => Some(format!("Also {}.", also.label(mac))),
        (None, Some(note)) => Some(note.to_owned()),
        (None, None) => None,
    };
    let mut body = column![line].spacing(6);
    if let Some(n) = note {
        body = body.push(text(n).font(UI_FONT).size(12).color(t.text_muted));
    }
    container(body)
        .padding(Padding::from([5.0, 16.0]))
        .width(Length::Fill)
        .into()
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
            text("Fixed on macOS. While transfers run it asks first.")
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

/// A list drawn as its chosen value only, for a read-only window.
pub(crate) fn fixed_choice<'a>(
    t: &'static Tokens,
    value: String,
    width: f32,
) -> Element<'a, Message> {
    container(
        text(value)
            .font(UI_FONT)
            .size(12.5)
            .color(t.text_secondary)
            .wrapping(text::Wrapping::None),
    )
    .padding(Padding::from([6.0, 10.0]))
    .width(width)
    .clip(true)
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

/// The unit after a typed number. `width`: [`UNIT_WIDTH`] in a slider row, so the sliders of
/// neighbouring rows line up whatever the unit; `Length::Shrink` in a row with the input alone,
/// so a long unit never runs past the card.
fn unit<'a>(t: &'static Tokens, s: &'a str, width: impl Into<Length>) -> Element<'a, Message> {
    container(text(s).font(UI_FONT).size(12).color(t.text_muted))
        .width(width)
        .into()
}

/// Width of a unit next to a slider: fits the short units those rows use.
const UNIT_WIDTH: f32 = 32.0;

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

/// How the blur row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlurRow {
    pub hint: &'static str,
    /// Drawn muted: blur has no effect now.
    pub inert: bool,
    /// The switch can be used.
    pub enabled: bool,
}

/// The blur row where the system can blur (`supported`, see [`chrome::blur_supported`]) at
/// `opacity`: blur only shows below full opacity; where it is not supported the switch cannot
/// be used at all.
pub fn blur_row(supported: bool, opacity: f32) -> BlurRow {
    let (hint, inert, enabled) = if !supported {
        ("Not supported on this system", true, false)
    } else if opacity >= 1.0 {
        ("Needs opacity below 100%", true, false)
    } else {
        ("Applies to new windows", false, true)
    };
    BlurRow {
        hint,
        inert,
        enabled,
    }
}

/// A toggle; `inert`: drawn muted because it has no effect right now.
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

/// One button of a segmented control; `on_press`: `None` while it cannot change.
fn segment<'a>(
    t: &'static Tokens,
    caption: &'a str,
    selected: bool,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    button(text(caption).font(UI_FONT).size(12.5))
        .on_press_maybe(on_press)
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
pub(crate) fn slider_style(t: &Tokens, status: slider::Status, editable: bool) -> slider::Style {
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

pub(crate) fn toggler_style(t: &Tokens, status: toggler::Status, inert: bool) -> toggler::Style {
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

pub(crate) fn pick_style(t: &Tokens, status: pick_list::Status) -> pick_list::Style {
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

pub(crate) fn menu_style(t: &Tokens) -> iced::overlay::menu::Style {
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
