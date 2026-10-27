//! The panel's view: header, path bar, filter, `Show hidden files`, sortable column headers,
//! and a list that builds only the rows on screen, so a folder of 100000 items costs no more
//! per frame than one of 30.

use std::ops::Range;
use std::time::SystemTime;

use chrono::Datelike;
use fbterm_session::ssh::sftp::{Entry, EntryKind, SortKey};
use iced::advanced::widget::{Id, Operation, operation::Focusable};
use iced::widget::{
    Space, button, column, container, mouse_area, responsive, row, scrollable, text, text_input,
    toggler,
};
use iced::{Alignment, Border, Element, Length, Padding, Rectangle, Size, Task, window};

use super::state::{self, PARENT, WindowPanel};
use super::{Body, FilePanelMsg, ProfilePanel, plain_name};
use crate::app::{App, Message};
use crate::tabs::WindowState;
use crate::theme::{Tokens, UI_FONT};
use crate::ui::{
    ICON_DOWN, ICON_FILE, ICON_FILE_LINK, ICON_FILE_OTHER, ICON_FOLDER, ICON_FOLDER_LINK,
    ICON_PARENT, ICON_SEARCH, ICON_UP, ICON_WARNING, ICON_X, MONO, icon,
};

/// The fewest characters of a link's target worth showing (a shorter target shows whole).
const MIN_TARGET: usize = 8;
/// The height of one list row, in logical pixels.
pub(crate) const ROW_HEIGHT: f32 = 24.0;
/// The list's height assumed before it has reported one.
const ASSUMED_HEIGHT: f32 = 400.0;
/// Rows built while the list's height is not known yet.
const UNKNOWN_HEIGHT_ROWS: usize = 100;
/// The panel's side padding; wider than the edge's grab band, so no control sits in it.
const PAD_X: f32 = 12.0;
/// Name, size and modified text sizes; Geist Mono's advance is 0.6 em.
const NAME_SIZE: f32 = 12.0;
const VALUE_SIZE: f32 = 11.0;
const MONO_ADVANCE: f32 = 0.6;
const ICON_SIZE: f32 = 14.0;
const GAP: f32 = 8.0;
/// The size and modified columns.
const SIZE_WIDTH: f32 = 52.0;
const MODIFIED_WIDTH: f32 = 82.0;
/// The modified column shows only when the list is at least this wide.
const MODIFIED_FROM: f32 = 250.0;
/// Where a row's content starts and ends inside the list (the selection is inset by half).
const ROW_PAD: f32 = 6.0;

/// One row's text, as shown: names plain, sizes and times short.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RowText {
    pub name: String,
    /// A link's target as stored.
    pub target: Option<String>,
    pub size: String,
    pub modified: String,
}

/// The text of `entry`'s row; times in `this_year` leave the year out.
pub(crate) fn row_text(entry: &Entry, this_year: i32) -> RowText {
    let target = match &entry.kind {
        EntryKind::Link { target, .. } if !target.is_empty() => Some(plain_name(target)),
        _ => None,
    };
    let size = match &entry.kind {
        EntryKind::File => human_size(entry.size),
        EntryKind::Link { to: Some(to), .. } if **to == EntryKind::File => human_size(entry.size),
        _ => String::new(),
    };
    RowText {
        name: plain_name(&entry.name),
        target,
        size,
        modified: entry
            .modified
            .map(|m| short_time(m, this_year))
            .unwrap_or_default(),
    }
}

/// `bytes` in a few characters: `512 B`, `1.2 KB`, `34 MB`.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// `Oct 27 09:15` this year, `Oct 27  2025` before (both twelve characters), in local time.
fn short_time(time: SystemTime, this_year: i32) -> String {
    let local = chrono::DateTime::<chrono::Local>::from(time);
    if local.year() == this_year {
        local.format("%b %d %H:%M").to_string()
    } else {
        local.format("%b %d  %Y").to_string()
    }
}

/// `name` in at most `room` characters: cut in the middle with an ellipsis, keeping the end
/// (where extensions and dates are).
fn fit(name: &str, room: usize) -> String {
    let count = name.chars().count();
    if count <= room {
        return name.to_owned();
    }
    if room <= 1 {
        return "\u{2026}".to_owned();
    }
    let tail = (room - 1) / 3;
    let head = room - 1 - tail;
    let mut out: String = name.chars().take(head).collect();
    out.push('\u{2026}');
    out.extend(name.chars().skip(count - tail));
    out
}

/// The path bar's labels for `folder`, control characters shown as `?`.
pub(crate) fn path_labels(folder: &str) -> Vec<String> {
    state::path_segments(folder)
        .into_iter()
        .map(|(label, _)| plain_name(&label))
        .collect()
}

/// The rows to build for a list of `rows` rows scrolled to `offset` with `height` visible
/// (`height` 0 while unknown).
pub(crate) fn visible_rows(rows: usize, offset: f32, height: f32) -> Range<usize> {
    let first = ((offset.max(0.0) / ROW_HEIGHT) as usize).min(rows);
    let count = if height > 0.0 {
        (height / ROW_HEIGHT).ceil() as usize + 1
    } else {
        UNKNOWN_HEIGHT_ROWS
    };
    first..rows.min(first + count)
}

/// The list's visible height, or a guess while it is unknown.
pub(crate) fn visible_height(height: f32) -> f32 {
    if height > 0.0 { height } else { ASSUMED_HEIGHT }
}

/// The scroll offset that brings row `index` into view, if it is not.
pub(crate) fn scroll_into_view(index: usize, offset: f32, height: f32) -> Option<f32> {
    let top = index as f32 * ROW_HEIGHT;
    let height = visible_height(height);
    if top < offset {
        Some(top)
    } else if top + ROW_HEIGHT > offset + height {
        Some(top + ROW_HEIGHT - height)
    } else {
        None
    }
}

/// Lets go of the keyboard in `ui`'s fields only (the find bar and other windows keep theirs).
pub(crate) fn unfocus_fields(ui: &WindowPanel) -> Task<Message> {
    struct Unfocus(Vec<Id>);
    impl<T> Operation<T> for Unfocus {
        fn focusable(&mut self, id: Option<&Id>, _bounds: Rectangle, state: &mut dyn Focusable) {
            if id.is_some_and(|id| self.0.contains(id)) {
                state.unfocus();
            }
        }

        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
            operate(self);
        }
    }
    iced::advanced::widget::operate(Unfocus(vec![ui.filter_id.clone(), ui.path_id.clone()]))
}

/// `wid`'s panel and its width, when it is open.
pub(crate) fn panel(app: &App, wid: window::Id) -> Option<(Element<'_, Message>, f32)> {
    let body = app.file_panel_body(wid)?;
    let ui = app.file_panels.ui.get(&wid);
    let t = app.tokens;
    let title = app
        .windows
        .get(&wid)
        .and_then(WindowState::active_tab)
        .map(|tab| tab.focused_pane().name.clone())
        .unwrap_or_default();
    let close = button(icon(ICON_X, 14.0, t.text_muted))
        .on_press(Message::FilePanel(FilePanelMsg::Close(wid)))
        .padding(4)
        .style(move |_, status| crate::ui::ghost(t, status));
    let header = row![
        text(title)
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
    let mut content = column![header].width(Length::Fill).height(Length::Fill);
    if let (Some(folder), Some(ui)) = (app.file_panel_folder(wid), ui) {
        content = content.push(path_bar(t, wid, folder, ui));
    }
    let remote = !matches!(body, Body::Note(_));
    if let (true, Some(ui)) = (remote, ui) {
        let filter = match &body {
            Body::Entries(panel) => panel.filter.as_str(),
            _ => "",
        };
        content = content.push(filter_bar(t, wid, filter, ui, app.file_panels.show_hidden));
    }
    content = content.push(match body {
        Body::Note(line) => note(t, line),
        Body::Loading => note(t, "Loading\u{2026}".into()),
        Body::Error(line) => error(t, wid, line),
        Body::Entries(panel) => match ui {
            Some(ui) => listing(t, wid, panel, ui),
            None => note(t, String::new()),
        },
    });
    let rule = container(Space::new())
        .width(1)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(t.border.into()),
            ..container::Style::default()
        });
    let opacity = app.settings.background_opacity;
    let surface = t.surface.scale_alpha(opacity);
    let panel = container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(surface.into()),
            ..container::Style::default()
        });
    Some((row![rule, panel].into(), app.file_panels.width))
}

fn msg(m: FilePanelMsg) -> Message {
    Message::FilePanel(m)
}

/// The folder as clickable segments, or a text field while a path is typed. A click on the
/// empty end turns it into the field.
fn path_bar<'a>(
    t: &'static Tokens,
    wid: window::Id,
    folder: &'a str,
    ui: &'a WindowPanel,
) -> Element<'a, Message> {
    let bar: Element<'a, Message> = match &ui.editing {
        Some(typed) => text_input("", typed)
            .id(ui.path_id.clone())
            .on_input(move |s| msg(FilePanelMsg::PathInput(wid, s)))
            .on_submit(msg(FilePanelMsg::PathSubmit(wid)))
            .font(MONO)
            .size(NAME_SIZE)
            .padding(Padding::from([5.0, 8.0]))
            .style(move |_, status| crate::ui::input_style(t, status))
            .into(),
        None => {
            let mut segments = row![].align_y(Alignment::Center);
            let all = state::path_segments(folder);
            let last = all.len() - 1;
            let labels = path_labels(folder);
            for (i, ((_, path), label)) in all.into_iter().zip(labels).enumerate() {
                if i > 1 {
                    segments = segments.push(
                        text("/")
                            .font(MONO)
                            .size(NAME_SIZE)
                            .color(t.text_muted)
                            .wrapping(text::Wrapping::None),
                    );
                }
                let current = i == last;
                segments = segments.push(
                    button(
                        text(label)
                            .font(MONO)
                            .size(NAME_SIZE)
                            .wrapping(text::Wrapping::None),
                    )
                    .on_press(msg(FilePanelMsg::Navigate(wid, path)))
                    .padding(Padding::from([2.0, 3.0]))
                    .style(move |_, status| {
                        let mut style = crate::ui::ghost(t, status);
                        style.text_color = if current { t.text } else { t.text_secondary };
                        style
                    }),
                );
            }
            let segments = scrollable(segments)
                .direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::hidden(),
                ))
                .anchor_right()
                .width(Length::Shrink);
            let end = mouse_area(
                Space::new()
                    .width(Length::Fill)
                    .height(Length::Fixed(NAME_SIZE * 1.3 + 4.0)),
            )
            .on_press(msg(FilePanelMsg::EditPath(wid)))
            .interaction(iced::mouse::Interaction::Text);
            container(
                row![segments, end]
                    .align_y(Alignment::Center)
                    .width(Length::Fill),
            )
            .padding(Padding::from([3.0, 5.0]))
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
    };
    container(bar)
        .padding(Padding::from([4.0, PAD_X]))
        .width(Length::Fill)
        .into()
}

/// The filter box and the `Show hidden files` toggle.
fn filter_bar<'a>(
    t: &'static Tokens,
    wid: window::Id,
    filter: &'a str,
    ui: &'a WindowPanel,
    show_hidden: bool,
) -> Element<'a, Message> {
    let search = ICON_SEARCH.chars().next().unwrap_or(' ');
    let input = text_input("Filter", filter)
        .id(ui.filter_id.clone())
        .on_input(move |s| msg(FilePanelMsg::Filter(wid, s)))
        .icon(text_input::Icon {
            font: crate::ui::ICONS,
            code_point: search,
            size: Some(13.0.into()),
            spacing: 6.0,
            side: text_input::Side::Left,
        })
        .font(UI_FONT)
        .size(12.5)
        .padding(Padding::from([5.0, 8.0]))
        .style(move |_, status| crate::ui::input_style(t, status));
    let hidden = row![
        toggler(show_hidden)
            .on_toggle(|on| msg(FilePanelMsg::ShowHidden(on)))
            .size(16)
            .style(move |_, status| {
                crate::settings_window::view::toggler_style(t, status, false)
            }),
        text("Show hidden files")
            .font(UI_FONT)
            .size(12)
            .color(t.text_secondary)
            .wrapping(text::Wrapping::None),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    column![input, hidden]
        .spacing(8)
        .padding(Padding {
            top: 4.0,
            right: PAD_X,
            bottom: 6.0,
            left: PAD_X,
        })
        .into()
}

fn note<'a>(t: &Tokens, line: String) -> Element<'a, Message> {
    container(
        text(line)
            .font(UI_FONT)
            .size(12.5)
            .color(t.text_muted)
            .wrapping(text::Wrapping::WordOrGlyph),
    )
    .padding(Padding::from([8.0, PAD_X]))
    .width(Length::Fill)
    .into()
}

/// An error inline: the server's message (or why SFTP is not available) and `Retry`.
fn error<'a>(t: &'static Tokens, wid: window::Id, line: String) -> Element<'a, Message> {
    let message = row![
        icon(ICON_WARNING, 14.0, t.danger),
        text(line)
            .font(UI_FONT)
            .size(12.5)
            .color(t.text_secondary)
            .wrapping(text::Wrapping::WordOrGlyph)
            .width(Length::Fill),
    ]
    .spacing(8);
    column![
        message,
        crate::ui::text_button(
            t,
            "Retry",
            Some(msg(FilePanelMsg::Retry(wid))),
            crate::ui::secondary
        )
    ]
    .spacing(10)
    .padding(Padding::from([8.0, PAD_X]))
    .width(Length::Fill)
    .into()
}

/// The column headers and the rows on screen.
fn listing<'a>(
    t: &'static Tokens,
    wid: window::Id,
    panel: &'a ProfilePanel,
    ui: &'a WindowPanel,
) -> Element<'a, Message> {
    let entries: &'a [Entry] = match &panel.listing {
        state::Listing::Loaded(entries) => entries,
        _ => &[],
    };
    let focused = ui.focused;
    let offset = ui.offset;
    let list_id = ui.list_id.clone();
    let this_year = chrono::Local::now().year();
    responsive(move |size: Size| {
        let wide = size.width >= MODIFIED_FROM;
        let headers = headers(t, wid, panel, wide);
        let parent = state::has_parent(&panel.folder);
        let rows = panel.shown.len() + usize::from(parent);
        // The name gets what the other columns leave.
        let name_width = size.width
            - 2.0 * (PAD_X - ROW_PAD)
            - 2.0 * ROW_PAD
            - ICON_SIZE
            - GAP
            - GAP
            - SIZE_WIDTH
            - if wide { GAP + MODIFIED_WIDTH } else { 0.0 };
        let room = (name_width / (NAME_SIZE * MONO_ADVANCE)).floor().max(1.0) as usize;
        let list_height = size.height - HEADER_HEIGHT;
        // The offset may be older than the rows (they shrank, and a list that fits reports no
        // scroll): never past the end, or no row would be built.
        let end = (rows as f32 * ROW_HEIGHT - list_height).max(0.0);
        let range = visible_rows(rows, offset.clamp(0.0, end), list_height);
        let mut list = column![Space::new().height(range.start as f32 * ROW_HEIGHT)];
        for index in range.clone() {
            let item = if parent && index == 0 {
                None
            } else {
                Some(&entries[panel.shown[index - usize::from(parent)]])
            };
            let name = item.map_or(PARENT, |e| e.name.as_str());
            let selected = panel.selected.as_deref() == Some(name);
            list = list.push(list_row(
                t, wid, item, selected, focused, wide, room, this_year,
            ));
        }
        list = list.push(Space::new().height((rows - range.end) as f32 * ROW_HEIGHT));
        let list = scrollable(
            container(list.width(Length::Fill)).padding(Padding::from([0.0, PAD_X - ROW_PAD])),
        )
        .id(list_id.clone())
        .on_scroll(move |v| {
            msg(FilePanelMsg::Scrolled(
                wid,
                v.absolute_offset().y,
                v.bounds().height,
            ))
        })
        .direction(scrollable::Direction::Vertical(
            scrollable::Scrollbar::new()
                .width(6)
                .scroller_width(4)
                .margin(1),
        ))
        .style(move |theme, status| scroll_style(t, theme, status))
        .height(Length::Fill);
        column![headers, list].into()
    })
    .into()
}

const HEADER_HEIGHT: f32 = 24.0;

/// `Name`, `Size` and `Modified`: a click sorts by it, again reverses.
fn headers<'a>(
    t: &'static Tokens,
    wid: window::Id,
    panel: &ProfilePanel,
    wide: bool,
) -> Element<'a, Message> {
    let header = |caption: &'static str, key: SortKey, right: bool| -> Element<'a, Message> {
        let active = panel.sort == key;
        let mut label = row![].spacing(2).align_y(Alignment::Center);
        let caption = text(caption)
            .font(UI_FONT)
            .size(11.5)
            .color(if active {
                t.text_secondary
            } else {
                t.text_muted
            })
            .wrapping(text::Wrapping::None);
        let arrow = active.then(|| {
            icon(
                if panel.descending { ICON_DOWN } else { ICON_UP },
                11.0,
                t.text_secondary,
            )
        });
        if right {
            label = label.extend(arrow);
            label = label.push(caption);
        } else {
            label = label.push(caption);
            label = label.extend(arrow);
        }
        button(container(label).width(Length::Fill).align_x(if right {
            Alignment::End
        } else {
            Alignment::Start
        }))
        .on_press(msg(FilePanelMsg::Sort(wid, key)))
        .padding(Padding::from([4.0, 0.0]))
        .style(move |_, _| button::Style::default())
        .into()
    };
    let mut cells = row![
        container(header("Name", SortKey::Name, false))
            .width(Length::Fill)
            .padding(Padding {
                left: ICON_SIZE + GAP,
                ..Padding::ZERO
            }),
        container(header("Size", SortKey::Size, true)).width(SIZE_WIDTH),
    ]
    .spacing(GAP)
    .align_y(Alignment::Center);
    if wide {
        cells = cells
            .push(container(header("Modified", SortKey::Modified, true)).width(MODIFIED_WIDTH));
    }
    container(cells)
        .height(HEADER_HEIGHT)
        .padding(Padding::from([0.0, PAD_X]))
        .align_y(Alignment::Center)
        .style(move |_| container::Style {
            border: Border {
                color: t.border,
                width: 0.0,
                radius: 0.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

/// One row: `None` is the `..` row.
#[allow(
    clippy::too_many_arguments,
    reason = "one row's look, all plain values"
)]
fn list_row<'a>(
    t: &'static Tokens,
    wid: window::Id,
    entry: Option<&'a Entry>,
    selected: bool,
    focused: bool,
    wide: bool,
    room: usize,
    this_year: i32,
) -> Element<'a, Message> {
    let (glyph, glyph_color, name_color) = match entry.map(|e| &e.kind) {
        None => (ICON_PARENT, t.text_muted, t.text_secondary),
        Some(EntryKind::Dir) => (ICON_FOLDER, t.text_secondary, t.text),
        Some(EntryKind::File) => (ICON_FILE, t.text_muted, t.text),
        Some(EntryKind::Link { to: Some(to), .. }) if to.is_folder() => {
            (ICON_FOLDER_LINK, t.text_secondary, t.text)
        }
        Some(EntryKind::Link { to: Some(_), .. }) => (ICON_FILE_LINK, t.text_muted, t.text),
        // Broken links and special files.
        Some(_) => (ICON_FILE_OTHER, t.text_muted, t.text_muted),
    };
    let text_of = entry.map(|e| row_text(e, this_year));
    let name = text_of.as_ref().map_or(PARENT, |r| r.name.as_str());
    let mut label = row![].align_y(Alignment::Center);
    match text_of.as_ref().and_then(|r| r.target.as_deref()) {
        Some(target) => {
            // The name first; the target gets what is left, when enough of it fits to read.
            let shown = fit(name, room);
            let left = room.saturating_sub(shown.chars().count() + 3);
            let fits = left >= target.chars().count().min(MIN_TARGET);
            label = label.push(mono(shown, NAME_SIZE, name_color));
            if fits {
                label = label.push(mono(
                    format!(" \u{2192} {}", fit(target, left)),
                    NAME_SIZE,
                    t.text_muted,
                ));
            }
        }
        None => label = label.push(mono(fit(name, room), NAME_SIZE, name_color)),
    }
    let mut cells = row![
        icon(glyph, ICON_SIZE, glyph_color),
        container(label).width(Length::Fill).clip(true),
        container(mono(
            text_of.as_ref().map(|r| r.size.clone()).unwrap_or_default(),
            VALUE_SIZE,
            t.text_muted,
        ))
        .width(SIZE_WIDTH)
        .align_x(Alignment::End),
    ]
    .spacing(GAP)
    .align_y(Alignment::Center);
    if wide {
        cells = cells.push(
            container(mono(
                text_of.map(|r| r.modified).unwrap_or_default(),
                VALUE_SIZE,
                t.text_muted,
            ))
            .width(MODIFIED_WIDTH)
            .align_x(Alignment::End),
        );
    }
    let key = name_key(entry);
    let body = container(cells)
        .height(ROW_HEIGHT)
        .width(Length::Fill)
        .padding(Padding::from([0.0, ROW_PAD]))
        .align_y(Alignment::Center)
        .style(move |_| container::Style {
            background: selected.then(|| if focused { t.accent_soft } else { t.hover }.into()),
            border: Border {
                radius: 4.0.into(),
                ..Border::default()
            },
            ..container::Style::default()
        });
    mouse_area(body)
        .on_press(msg(FilePanelMsg::Select(wid, key.clone())))
        .on_double_click(msg(FilePanelMsg::Activate(wid, key)))
        .into()
}

/// How a selection names `entry`'s row.
fn name_key(entry: Option<&Entry>) -> String {
    entry.map_or_else(|| PARENT.to_owned(), |e| e.name.clone())
}

fn mono<'a>(s: impl text::IntoFragment<'a>, size: f32, color: iced::Color) -> Element<'a, Message> {
    text(s)
        .font(MONO)
        .size(size)
        .color(color)
        .wrapping(text::Wrapping::None)
        .into()
}

/// A thin scroller that shows only on hover, over the panel's surface.
fn scroll_style(t: &Tokens, theme: &iced::Theme, status: scrollable::Status) -> scrollable::Style {
    let mut style = scrollable::default(theme, status);
    let hovered = !matches!(status, scrollable::Status::Active { .. });
    let rail = || scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: if hovered { t.border } else { t.raised }.into(),
            border: Border {
                radius: 2.0.into(),
                ..Border::default()
            },
        },
    };
    style.vertical_rail = rail();
    style.horizontal_rail = rail();
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_names_fit() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(35 * 1024 * 1024), "35 MB");
        assert_eq!(fit("short", 10), "short");
        let cut = fit("app-export-2026-10-27.tar.gz", 12);
        assert_eq!(cut.chars().count(), 12);
        assert!(cut.contains('\u{2026}') && cut.ends_with(".gz"), "{cut}");
        assert_eq!(fit("abc", 1), "\u{2026}");
    }
}
