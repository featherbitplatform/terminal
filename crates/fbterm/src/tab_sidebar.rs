//! Tabs listed down the left side of the window (`tab_bar = "left"`): the sidebar beside
//! the content, its width per window (dragged at its rule, see `sidebar_resize.rs`), the
//! tooltip a row shows when its title is cut off, the group headers the rows fold under
//! (`tab_groups.rs`), and switching sides.

use fbterm_config::keys::{Action, Bindings};
use fbterm_config::{SettingChange, TabBar};
use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{
    Alignment, Background, Border, Color, Element, Event, Length, Padding, Rectangle, Size, Task,
    Vector, mouse, window,
};

use crate::app::{App, Message, TabBarMsg};
use crate::tab_drag::{Axis, DragState, Strip, drag_event};
use crate::tab_groups::{Group, Section};
use crate::tab_look::{Surroundings, look};
use crate::tabs::{Tab, TabId};
use crate::theme::{Tokens, UI_FONT};
use crate::tooltip::{tip, tooltip_text};
use crate::ui::{ICON_DOWN, ICON_RIGHT, ICON_X, icon};
use crate::window::{MIN_TERMINAL_WIDTH, dot, page_icon, snap_rect, state_color};
use crate::windows::Carry;

/// The sidebar's width when the user never changed it, in logical pixels.
pub(crate) const SIDEBAR_WIDTH: f32 = 220.0;
/// The narrowest and widest the user can make the sidebar.
pub(crate) const MIN_SIDEBAR_WIDTH: f32 = 140.0;
pub(crate) const MAX_SIDEBAR_WIDTH: f32 = 480.0;

/// `width` within [`MIN_SIDEBAR_WIDTH`]..=[`MAX_SIDEBAR_WIDTH`]; [`SIDEBAR_WIDTH`] when it is
/// not a number.
pub(crate) fn clamp_sidebar_width(width: f32) -> f32 {
    if width.is_finite() {
        width.clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH)
    } else {
        SIDEBAR_WIDTH
    }
}
/// A row's height.
pub(crate) const ROW_HEIGHT: f32 = 32.0;
/// Space between rows.
pub(crate) const ROW_SPACING: f32 = 2.0;
/// Space around the list.
pub(crate) const LIST_PADDING: f32 = 6.0;

/// How wide the sidebar is drawn in a window `total` wide whose rule is `hairline` wide, when
/// the user chose `chosen`: that width while the terminal keeps [`MIN_TERMINAL_WIDTH`], less
/// in a narrower window, down to nothing.
pub(crate) fn sidebar_width(chosen: f32, total: f32, hairline: f32) -> f32 {
    chosen.min(total - MIN_TERMINAL_WIDTH - hairline).max(0.0)
}

/// A group header's height, and the space above one (except the first item).
pub(crate) const HEADER_HEIGHT: f32 = 26.0;
pub(crate) const HEADER_GAP: f32 = 8.0;

/// One element of the list: a group header, or a tab's row (by window index).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Item {
    Header {
        group: Group,
        /// How many rows the header hides: 0 while its group is unfolded.
        hidden: usize,
        /// The active tab is among the hidden rows.
        holds_active: bool,
    },
    Row(usize),
}

impl Item {
    fn height(&self) -> f32 {
        match self {
            Self::Header { .. } => HEADER_HEIGHT,
            Self::Row(_) => ROW_HEIGHT,
        }
    }
}

/// The list's items for `sections` (the active tab at window index `active`): with
/// `folding` off one row per tab and no headers; with it on, each section's header followed
/// by its rows unless it is collapsed.
pub(crate) fn items(sections: &[Section], active: usize, folding: bool) -> Vec<Item> {
    if !folding {
        return sections
            .iter()
            .flat_map(|s| s.rows.iter().map(|&r| Item::Row(r)))
            .collect();
    }
    let mut out = Vec::new();
    for s in sections {
        out.push(Item::Header {
            group: s.group.clone(),
            hidden: if s.collapsed { s.rows.len() } else { 0 },
            holds_active: s.collapsed && s.rows.contains(&active),
        });
        if !s.collapsed {
            out.extend(s.rows.iter().map(|&r| Item::Row(r)));
        }
    }
    out
}

/// Where each item starts, from the top of the list's content: [`LIST_PADDING`] first,
/// items [`ROW_SPACING`] apart, [`HEADER_GAP`] more above a header that is not the first item.
pub(crate) fn item_tops(items: &[Item]) -> Vec<f32> {
    let mut y = LIST_PADDING;
    let mut tops = Vec::with_capacity(items.len());
    for (n, item) in items.iter().enumerate() {
        if n > 0 && matches!(item, Item::Header { .. }) {
            y += HEADER_GAP;
        }
        tops.push(y);
        y += item.height() + ROW_SPACING;
    }
    tops
}

/// The scroll offset that shows the span at `top`, `height` long, when it is not fully in
/// view (`offset` and `viewport` are the list's or strip's current offset and visible
/// length): the span with `pad` beside it on the side it was hidden, never before the start;
/// a span longer than the view shows its start. `None` when it shows. The sidebar's rows
/// (`LIST_PADDING`) and the tab strip's tabs (no padding) both reveal through this.
pub(crate) fn reveal(top: f32, height: f32, offset: f32, viewport: f32, pad: f32) -> Option<f32> {
    let bottom = top + height;
    if top < offset || height > viewport {
        Some((top - pad).max(0.0))
    } else if bottom > offset + viewport {
        Some((bottom + pad - viewport).max(0.0))
    } else {
        None
    }
}

/// The size a row's label is drawn at.
const LABEL_SIZE: f32 = 12.5;
/// A row's side padding.
const ROW_PADDING: f32 = 8.0;
/// Space between a row's parts.
const PART_SPACING: f32 = 8.0;

/// The room a row's label gets in a sidebar `sidebar_width` wide, from the parts `row_view`
/// lays out beside it: the list's and the row's paddings, the stripe (3), the state dot (7) or
/// a page tab's icon (14), the trouble dot (7) when `trouble`, the close button (12 plus 3 of
/// padding each side) and the spacing between them. Never below 0.
pub(crate) fn label_width(sidebar_width: f32, page: bool, trouble: bool) -> f32 {
    let lead = if page { 14.0 } else { 7.0 };
    let trouble = if trouble { 7.0 + PART_SPACING } else { 0.0 };
    let close = 12.0 + 2.0 * 3.0;
    (sidebar_width
        - 2.0 * LIST_PADDING
        - 2.0 * ROW_PADDING
        - 3.0
        - PART_SPACING
        - lead
        - PART_SPACING
        - trouble
        - close
        - PART_SPACING)
        .max(0.0)
}

/// How many measured titles [`text_width`] keeps; the cache starts over when full.
pub(crate) const WIDTH_CACHE_CAP: usize = 512;

thread_local! {
    /// Measured title widths: every redraw measures every row, and titles rarely change.
    static WIDTHS: std::cell::RefCell<std::collections::HashMap<String, f32>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// How many widths the cache holds (for tests).
#[cfg(test)]
pub(crate) fn width_cache_len() -> usize {
    WIDTHS.with(|w| w.borrow().len())
}

/// How wide `s` is laid out as a row draws it: [`measure`], remembered per title.
pub(crate) fn text_width(s: &str) -> f32 {
    if let Some(w) = WIDTHS.with(|w| w.borrow().get(s).copied()) {
        return w;
    }
    let width = measure(s);
    WIDTHS.with(|w| {
        let mut w = w.borrow_mut();
        if w.len() >= WIDTH_CACHE_CAP {
            w.clear();
        }
        w.insert(s.to_owned(), width);
    });
    width
}

/// How wide `s` is laid out as a row draws it (the UI font at [`LABEL_SIZE`], one line),
/// measured through the renderer's font system, in logical pixels.
pub(crate) fn measure(s: &str) -> f32 {
    use iced::advanced::text::Paragraph as _;
    let paragraph = iced::advanced::graphics::text::Paragraph::with_text(iced::advanced::Text {
        content: s,
        bounds: Size::new(f32::INFINITY, f32::INFINITY),
        size: iced::Pixels(LABEL_SIZE),
        line_height: iced::advanced::text::LineHeight::default(),
        font: UI_FONT,
        align_x: iced::advanced::text::Alignment::Default,
        align_y: iced::alignment::Vertical::Top,
        // The row's `text()` leaves shaping at its default: measure the same way.
        shaping: iced::advanced::text::Shaping::default(),
        wrapping: iced::advanced::text::Wrapping::None,
    });
    paragraph.min_width()
}

/// Whether a label `text_width` wide is cut off in `label_width` of room.
pub(crate) fn clipped(text_width: f32, label_width: f32) -> bool {
    text_width > label_width
}

/// The tooltip a row shows: its whole `title`, only when the title is cut off in a sidebar
/// `sidebar_width` wide and tooltips are on (`tips`: nothing is carried, no resize is on and
/// the window is not modal).
pub(crate) fn row_tip(
    tips: bool,
    sidebar_width: f32,
    title: &str,
    page: bool,
    trouble: bool,
) -> Option<String> {
    (tips && clipped(text_width(title), label_width(sidebar_width, page, trouble)))
        .then(|| title.to_owned())
}

/// The list's scrollable id in window `window`.
pub(crate) fn scroll_id(window: window::Id) -> iced::widget::Id {
    iced::widget::Id::from(format!("tabs-{window:?}"))
}

/// How a window's sidebar is drawn.
#[derive(Clone, Copy)]
pub(crate) struct Sidebar<'b> {
    pub window: window::Id,
    pub tokens: &'static Tokens,
    /// What the rows are painted against (the active row takes the terminal's color).
    pub around: Surroundings,
    /// The sidebar's background: the bar's.
    pub bar_bg: Color,
    pub bindings: &'b Bindings,
    /// A tab or tile being dragged in this window.
    pub carry: Option<Carry>,
    /// The palette or a dialog covers the window: no drags, no menu.
    pub modal: bool,
    /// Physical pixels per logical pixel.
    pub scale: f32,
    /// The sidebar's width (see [`sidebar_width`]).
    pub width: f32,
    /// Rows may show their tooltip: nothing is carried, no resize is on, not modal.
    pub tips: bool,
    /// What the list shows, in order (see [`items`]).
    pub items: &'b [Item],
    /// The group of the tile being carried, if one is: where it may become a tab.
    pub tile_group: Option<&'b Group>,
    /// The rename under way in this window, if any: its row shows the field.
    pub renaming: Option<&'b crate::tab_rename::Renaming>,
}

/// The sidebar: the rows in a scrollable list, with a rule on the right.
pub(crate) fn view<'a>(s: Sidebar<'_>, tabs: &'a [Tab], active: usize) -> Element<'a, Message> {
    let t = s.tokens;
    let window = s.window;
    // The column's children: headers (with a spacer above all but the first item) and rows.
    // `is_row` tells the drag which children are rows; `ids` lists the rows' tabs in order.
    let mut children: Vec<Element<'a, Message>> = Vec::new();
    let mut is_row = Vec::new();
    let mut ids = Vec::new();
    let mut indices = Vec::new();
    let mut hole = None;
    let mut section = 0;
    // Each header's group with the strip indices its rows span (`lo..hi`; empty when
    // folded), for the drag's range.
    let mut spans: Vec<(&Group, usize, usize)> = Vec::new();
    for (n, item) in s.items.iter().enumerate() {
        match item {
            Item::Header {
                group,
                hidden,
                holds_active,
            } => {
                if n > 0 {
                    // The column's spacing lies on both sides of the spacer.
                    children.push(Space::new().height(HEADER_GAP - ROW_SPACING).into());
                    is_row.push(false);
                }
                children.push(header_view(&s, section, group, *hidden, *holds_active));
                is_row.push(false);
                section += 1;
                spans.push((group, ids.len(), ids.len()));
            }
            Item::Row(i) => {
                let Some(tab) = tabs.get(*i) else {
                    continue;
                };
                if *i == active {
                    hole = Some(ids.len());
                }
                ids.push(tab.id);
                indices.push(*i);
                if let Some(last) = spans.last_mut() {
                    last.2 = ids.len();
                }
                children.push(row_view(&s, tab, *i, *i == active));
                is_row.push(true);
            }
        }
    }
    // Where the carry may land: a carried tab among its own group's rows (refused elsewhere),
    // a carried tile in its group's span, or at the gap where its group would sort (moved
    // there from elsewhere). Without headers (folding off) anything goes.
    let (range, clamp) = match s.carry {
        Some(Carry::Tab(id)) => {
            let at = ids.iter().position(|i| *i == id);
            let span = at.and_then(|at| {
                spans
                    .iter()
                    .find(|(_, lo, hi)| (*lo..*hi).contains(&at))
                    .map(|(_, lo, hi)| (*lo, *hi))
            });
            (span, false)
        }
        Some(Carry::Tile(_)) => match s.tile_group {
            Some(g) if !spans.is_empty() => {
                let span = spans
                    .iter()
                    .find(|(sg, _, _)| sg.same(g))
                    .map(|(_, lo, hi)| (*lo, *hi))
                    .unwrap_or_else(|| {
                        let lo = spans
                            .iter()
                            .find(|(sg, _, _)| *sg > g)
                            .map_or(ids.len(), |(_, lo, _)| *lo);
                        (lo, lo)
                    });
                (Some(span), true)
            }
            _ => (None, false),
        },
        None => (None, false),
    };
    let list = column(children)
        .spacing(ROW_SPACING)
        .padding(LIST_PADDING)
        .width(Length::Fill);
    let list = Element::new(Rows {
        window,
        ids,
        indices,
        range,
        clamp,
        is_row,
        carry: s.carry,
        modal: s.modal,
        renaming: s.renaming.map(|r| r.tab),
        hole,
        tokens: t,
        scale: s.scale,
        content: list.into(),
    });
    let list = scrollable(list)
        .id(scroll_id(window))
        .on_scroll(move |v: scrollable::Viewport| {
            Message::TabBar(
                window,
                TabBarMsg::Scrolled {
                    offset: v.absolute_offset().y,
                    height: v.bounds().height,
                },
            )
        })
        .direction(scrollable::Direction::Vertical(crate::ui::edge_scrollbar(
            false,
        )))
        .style(move |theme, status| crate::ui::scroll_style(t, theme, status))
        .width(Length::Fill)
        .height(Length::Fill);
    let bar_bg = s.bar_bg;
    let list = container(list)
        .width(s.width)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(bar_bg.into()),
            ..container::Style::default()
        });
    row![list, crate::ui::vertical_rule(t, s.scale)]
        .height(Length::Fill)
        .into()
}

/// One row: the accent stripe, the state dot, the trouble dot, the title and the close
/// button, as a tab on the bar shows them.
fn row_view<'a>(s: &Sidebar<'_>, tab: &'a Tab, index: usize, active: bool) -> Element<'a, Message> {
    let t = s.tokens;
    let window = s.window;
    let (accent, accent_style) = (tab.accent(), tab.accent_style());
    let idle = look(t, accent, accent_style, active, false, s.around);
    let hover = look(t, accent, accent_style, active, true, s.around);
    let stripe = container(text(""))
        .width(3)
        .height(14)
        .style(move |_| container::Style {
            background: idle.stripe.map(Background::Color),
            border: Border {
                radius: 1.5.into(),
                ..Border::default()
            },
            ..container::Style::default()
        });
    let renaming = s.renaming.filter(|r| r.tab == tab.id);
    let close = button(icon(ICON_X, 12.0, idle.icon))
        .on_press(Message::TabBar(window, TabBarMsg::Close(index)))
        .padding(3)
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: hovered.then_some(Background::Color(t.hover)),
                text_color: t.text_secondary,
                border: Border {
                    radius: 5.0.into(),
                    ..Border::default()
                },
                ..button::Style::default()
            }
        });
    let close = tip(
        t,
        close,
        if active && tab.panes.len() == 1 {
            tooltip_text("Close tab", Action::CloseTab, s.bindings)
        } else {
            "Close tab".to_owned()
        },
        false,
    );
    // A title cut off at the sidebar's edge shows whole when the pointer rests on it: the tip
    // sits on the label cell alone, so the close button keeps its own single bubble.
    let label_cell: Element<'a, Message> = match renaming {
        Some(r) => crate::tab_rename::field(t, r, LABEL_SIZE),
        None => {
            let label = text(tab.shown_title())
                .font(UI_FONT)
                .size(LABEL_SIZE)
                .wrapping(text::Wrapping::None)
                .color(idle.label);
            let label_cell = container(label).width(Length::Fill).clip(true);
            match row_tip(
                s.tips,
                s.width,
                tab.shown_title(),
                tab.page.is_some(),
                tab.trouble(),
            ) {
                Some(title) => tip(t, label_cell, title, false),
                None => label_cell.into(),
            }
        }
    };
    let mut inner = row![stripe];
    // A page tab has no session to show: it carries its page's icon, as on the bar.
    inner = match tab.page {
        None => inner.push(dot(state_color(t, tab.state()))),
        Some(page) => inner.push(icon(page_icon(page), 14.0, idle.icon)),
    };
    if tab.trouble() {
        inner = inner.push(dot(t.warning));
    }
    let inner = inner
        .push(label_cell)
        .push(close)
        .spacing(PART_SPACING)
        .height(Length::Fill)
        .align_y(Alignment::Center);
    let row = button(inner)
        .on_press(Message::TabBar(window, TabBarMsg::Select(index)))
        .width(Length::Fill)
        .height(ROW_HEIGHT)
        .padding(Padding::from([0.0, ROW_PADDING]))
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let background = if active {
                idle.background
            } else if hovered {
                hover.background.or(Some(Background::Color(t.hover)))
            } else {
                idle.background
            };
            button::Style {
                background,
                text_color: idle.label,
                border: Border {
                    radius: 5.0.into(),
                    ..Border::default()
                },
                snap: true,
                ..button::Style::default()
            }
        });
    row.into()
}

/// A group's header: the chevron, the name and, while folded, how many rows it hides. A click
/// anywhere on it folds or unfolds the group (the sidebar's section `section`).
fn header_view<'a>(
    s: &Sidebar<'_>,
    section: usize,
    group: &Group,
    hidden: usize,
    holds_active: bool,
) -> Element<'a, Message> {
    let t = s.tokens;
    let chevron = icon(
        if hidden > 0 { ICON_RIGHT } else { ICON_DOWN },
        12.0,
        t.text_muted,
    );
    let name_color = if matches!(group, Group::Featherbit) {
        t.accent_edge
    } else {
        t.text_muted
    };
    let name = text(group.title().to_owned())
        .font(UI_FONT)
        .size(11)
        .wrapping(text::Wrapping::None)
        .color(name_color);
    let mut inner = row![chevron, container(name).width(Length::Fill).clip(true)]
        .spacing(6)
        .height(Length::Fill)
        .align_y(Alignment::Center);
    if hidden > 0 {
        // The count of hidden rows, in the accent when the active tab is among them.
        let tone = if holds_active {
            t.accent_edge
        } else {
            t.text_secondary
        };
        let badge = container(
            text(hidden.to_string())
                .font(UI_FONT)
                .size(11)
                .line_height(text::LineHeight::Absolute(16.0.into()))
                .color(tone),
        )
        .padding(Padding::from([0.0, 6.0]))
        .style(move |_| container::Style {
            background: Some(t.raised.into()),
            border: Border {
                color: if holds_active {
                    t.accent_edge
                } else {
                    t.border
                },
                width: 1.0,
                radius: 8.0.into(),
            },
            ..container::Style::default()
        });
        inner = inner.push(badge);
    }
    button(inner)
        .on_press(Message::TabBar(s.window, TabBarMsg::ToggleGroup(section)))
        .width(Length::Fill)
        .height(HEADER_HEIGHT)
        .padding(Padding::from([0.0, 6.0]))
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: hovered.then_some(Background::Color(t.hover)),
                text_color: t.text_muted,
                border: Border {
                    radius: 5.0.into(),
                    ..Border::default()
                },
                snap: true,
                ..button::Style::default()
            }
        })
        .into()
}

/// The list of rows inside the scrollable: it handles the drag protocol and the right-click
/// over the rows (the column's children that are rows), and draws the gap where a carried
/// tile would become a tab or a carried tab would move to.
struct Rows<'a> {
    window: window::Id,
    ids: Vec<TabId>,
    /// Which of the column's children are rows (the others are headers and spacers).
    is_row: Vec<bool>,
    /// Each row's window index, and where the carry may land (see `view`).
    indices: Vec<usize>,
    range: Option<(usize, usize)>,
    clamp: bool,
    carry: Option<Carry>,
    modal: bool,
    /// The tab showing the rename field (see `tab_drag::Strip::renaming`).
    renaming: Option<TabId>,
    hole: Option<usize>,
    tokens: &'static Tokens,
    scale: f32,
    content: Element<'a, Message>,
}

impl Rows<'_> {
    /// The strip as of one event. The scrollable hands its content a viewport shifted by
    /// the scroll translation and a layout that is not: their difference is the offset the
    /// rows and the pointer are measured at, read here rather than kept (iced reports no
    /// scroll when the list comes to fit, and a rebuilt list starts at 0).
    fn strip(&self, layout: Layout<'_>, viewport: &Rectangle) -> Strip<'_> {
        Strip {
            window: self.window,
            ids: &self.ids,
            indices: &self.indices,
            range: self.range,
            clamp: self.clamp,
            carry: self.carry,
            modal: self.modal,
            hole: self.hole,
            axis: Axis::Y,
            scroll: viewport.y - layout.bounds().y,
            renaming: self.renaming,
            first: 0,
        }
    }
}

/// Where the drop marker for strip gap `gap` is drawn among `rows` (the list starts at
/// `top`): 2 px above the row the gap precedes; under the previous row when more than the
/// list's spacing lies between them (a group header), so a drop at a group's end shows inside
/// that group; under the last row after the end.
pub(crate) fn gap_marker_y(rows: &[Rectangle], gap: usize, top: f32) -> f32 {
    let below = |r: &Rectangle| r.y + r.height;
    match (gap.checked_sub(1).and_then(|p| rows.get(p)), rows.get(gap)) {
        (Some(prev), Some(next)) if next.y - below(prev) > ROW_SPACING + 0.5 => below(prev),
        (_, Some(next)) => next.y - 2.0,
        (Some(prev), None) => below(prev),
        (None, None) => top,
    }
}

/// The rows' bounds: the column's children that are rows, in order.
fn row_rects(layout: Layout<'_>, is_row: &[bool]) -> Vec<Rectangle> {
    layout
        .children()
        .zip(is_row)
        .filter(|(_, row)| **row)
        .map(|(c, _)| c.bounds())
        .collect()
}

impl Widget<Message, iced::Theme, iced::Renderer> for Rows<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<DragState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(DragState::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<DragState>();
        let rects = row_rects(layout, &self.is_row);
        let strip = self.strip(layout, viewport);
        // The row pressed at the start of a drag is let go without a click: it sees the
        // release with no pointer over it.
        let cursor = if drag_event(&strip, state, event, &rects, layout.bounds(), cursor, shell) {
            mouse::Cursor::Unavailable
        } else {
            cursor
        };
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        // A carried tile or tab: a violet line in the gap where it would land.
        let state = tree.state.downcast_ref::<DragState>();
        if let (Some(Carry::Tile(_) | Carry::Tab(_)), Some(gap)) = (self.carry, state.gap) {
            let bounds = layout.bounds();
            let rows = row_rects(layout, &self.is_row);
            let y = gap_marker_y(&rows, gap, bounds.y);
            let marker = snap_rect(
                Rectangle {
                    x: bounds.x + LIST_PADDING,
                    y,
                    width: bounds.width - 2.0 * LIST_PADDING,
                    height: 2.0,
                },
                self.scale,
            );
            renderer.with_layer(bounds, |renderer| {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: marker,
                        border: Border::default(),
                        shadow: Default::default(),
                        snap: true,
                    },
                    self.tokens.accent,
                );
            });
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, iced::Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl App {
    /// The sidebar width the user chose for window `id`: [`SIDEBAR_WIDTH`] until changed.
    pub(crate) fn sidebar_width_of(&self, id: window::Id) -> f32 {
        self.sidebar_widths
            .get(&id)
            .copied()
            .unwrap_or(SIDEBAR_WIDTH)
    }

    /// The sidebar's rule is being dragged in window `wid`: the chosen width becomes `width`,
    /// clamped, and the window's rows show no tooltips until the drag ends.
    pub(crate) fn sidebar_resize(&mut self, wid: window::Id, width: f32) {
        self.sidebar_widths.insert(wid, clamp_sidebar_width(width));
        self.sidebar_resizing.insert(wid);
    }

    /// The drag ended: remember the width.
    pub(crate) fn sidebar_resize_end(&mut self, wid: window::Id) -> Task<Message> {
        self.sidebar_resizing.remove(&wid);
        self.save_state();
        Task::none()
    }

    /// The list scrolled (or was laid out): remember its offset and visible height, for the
    /// drag's coordinates and for scrolling a selected tab into view.
    pub(crate) fn sidebar_scrolled(&mut self, wid: window::Id, offset: f32, height: f32) {
        self.sidebar_scroll.insert(wid, (offset, height));
    }

    /// After every update: a window whose active tab changed scrolls its sidebar just enough
    /// to show the new active row, when it is not fully in view; `None` when nothing is to be
    /// scrolled (the common case, so the update's task is passed through untouched). Nothing
    /// while tabs are on top, where the lists do not exist: what was known about them is
    /// forgotten, so a list built afresh is read afresh.
    pub(crate) fn reveal_active_tabs(&mut self) -> Option<Task<Message>> {
        if !self.settings.tabs_left() {
            // No column: no drag on its edge can end, and the windows that come back to the
            // left are revealed afresh without unfolding what was folded.
            self.sidebar_resizing.clear();
            self.sidebar_quiet.extend(self.windows.keys().copied());
            self.sidebar_shown.clear();
            self.sidebar_scroll.clear();
            self.reveal_strips();
            return None;
        }
        // Tabs on the left: no strip to scroll, and what was known about it is forgotten.
        self.strip_offset.clear();
        self.strip_seen.clear();
        let bar = crate::chrome::Chrome::current().bar_height();
        let mut tasks = Vec::new();
        // The windows whose active tab changed since the sidebar last showed it; none while a
        // carry is on there (see `reveal_strips`): it is revealed once the carry ends.
        let changed: Vec<(window::Id, TabId, usize)> = self
            .windows
            .iter()
            .filter(|(wid, _)| self.carry_in(**wid).is_none())
            .filter_map(|(wid, w)| {
                let tab = w.active_tab()?;
                (self.sidebar_shown.get(wid) != Some(&tab.id)).then_some((*wid, tab.id, w.active))
            })
            .collect();
        for (wid, id, active) in changed {
            self.sidebar_shown.insert(wid, id);
            // Selected by other means than keyboard travel (which never lands in a folded
            // group): its group unfolds; not when the column only came back from the top.
            if !self.sidebar_quiet.remove(&wid) {
                self.expand_active(wid);
            }
            let (offset, height) = match self.sidebar_scroll.get(&wid) {
                Some(&v) => v,
                None => (0.0, self.sizes.get(&wid).map_or(f32::MAX, |(_, h)| h - bar)),
            };
            // The row's place among headers and the other rows, after any unfolding above.
            let items = items(&self.sections_of(wid), active, self.groups_on());
            let Some(n) = items
                .iter()
                .position(|i| matches!(i, Item::Row(r) if *r == active))
            else {
                continue;
            };
            let top = item_tops(&items)[n];
            if let Some(y) = reveal(top, ROW_HEIGHT, offset, height, LIST_PADDING) {
                self.sidebar_scroll.insert(wid, (y, height));
                tasks.push(iced::widget::operation::scroll_to(
                    scroll_id(wid),
                    iced::widget::operation::AbsoluteOffset { x: 0.0, y },
                ));
            }
        }
        (!tasks.is_empty()).then(|| Task::batch(tasks))
    }

    /// The `Vertical tabs` action: tabs to the left, or back along the top. An unanswered
    /// setting counts as top, so the first press moves them left. Written and applied like a
    /// Settings tab change.
    pub(crate) fn toggle_vertical_tabs(&mut self) -> Task<Message> {
        let next = if self.settings.tabs_left() {
            TabBar::Top
        } else {
            TabBar::Left
        };
        self.set_setting(SettingChange::TabBar(next))
    }
}

#[cfg(test)]
mod tests {
    use fbterm_config::keys::Action;
    use iced::advanced::layout::Limits;
    use iced::advanced::widget::Tree;
    use iced::{Pixels, Size, window};

    use super::{
        Item, LIST_PADDING, MAX_SIDEBAR_WIDTH, MIN_SIDEBAR_WIDTH, ROW_HEIGHT, SIDEBAR_WIDTH,
        clamp_sidebar_width, clipped, gap_marker_y, item_tops, items, label_width, measure, reveal,
        row_tip, sidebar_width, text_width,
    };
    use crate::app::{App, Message, TabBarMsg};
    use crate::restore_tests::{paths_with, profile};
    use crate::tab_groups::tests::{app_with_groups, fold};
    use crate::tab_groups::{Group, Section};
    use crate::tabs::{Pane, Tab, WindowState};

    fn app_with_tabs(names: &[&str]) -> (App, window::Id) {
        let profiles: Vec<_> = names.iter().map(|n| profile(n)).collect();
        let (mut app, _) = App::build(paths_with(&profiles), true);
        app.state_saving_enabled = false;
        let wid = window::Id::unique();
        let mut w = WindowState::new();
        for p in &profiles {
            w.push(Tab::single(Pane::for_profile(p)));
        }
        w.active = 0;
        app.windows.clear();
        app.windows.insert(wid, w);
        app.sizes.insert(wid, (900.0, 600.0));
        (app, wid)
    }

    /// Build and lay the window's view out, as a frame would: a widget that cannot lay out
    /// panics here.
    fn lay_out(app: &App, wid: window::Id) {
        let renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::DEFAULT,
            Pixels(16.0),
        ));
        let mut element = crate::window::view(app, wid);
        let mut tree = Tree::new(&element);
        let _ = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, Size::new(900.0, 600.0)),
        );
    }

    #[test]
    fn the_sidebar_yields_in_a_narrow_window() {
        assert_eq!(sidebar_width(220.0, 1000.0, 1.0), 220.0);
        assert_eq!(sidebar_width(220.0, 360.0, 1.0), 159.0);
        assert_eq!(sidebar_width(220.0, 150.0, 1.0), 0.0);
        // A chosen width larger than the room: the yield applies to the chosen width.
        assert_eq!(sidebar_width(480.0, 1000.0, 1.0), 480.0);
        assert_eq!(sidebar_width(480.0, 500.0, 1.0), 299.0);
    }

    #[test]
    fn resizing_sets_the_chosen_width_clamped_per_window() {
        let (mut app, wid) = app_with_tabs(&["a"]);
        app.settings.tab_bar = Some(fbterm_config::TabBar::Left);
        let other = window::Id::unique();
        let _ = app.update(Message::TabBar(wid, TabBarMsg::SidebarResize(300.0)));
        assert_eq!(app.sidebar_width_of(wid), 300.0);
        assert_eq!(
            app.sidebar_width_of(other),
            SIDEBAR_WIDTH,
            "another window keeps its own"
        );
        assert!(app.sidebar_resizing.contains(&wid));
        let _ = app.update(Message::TabBar(wid, TabBarMsg::SidebarResize(5000.0)));
        assert_eq!(app.sidebar_width_of(wid), MAX_SIDEBAR_WIDTH);
        let _ = app.update(Message::TabBar(wid, TabBarMsg::SidebarResize(1.0)));
        assert_eq!(app.sidebar_width_of(wid), MIN_SIDEBAR_WIDTH);
    }

    #[test]
    fn resize_end_saves() {
        let (mut app, wid) = app_with_tabs(&["a"]);
        app.state_saving_enabled = true;
        let _ = app.update(Message::TabBar(wid, TabBarMsg::SidebarResize(300.0)));
        let _ = app.update(Message::TabBar(wid, TabBarMsg::SidebarResizeEnd));
        assert!(!app.sidebar_resizing.contains(&wid));
        let saved = fbterm_config::SavedState::load_or_default(&app.paths.state());
        assert_eq!(saved.windows.len(), 1);
        assert_eq!(saved.windows[0].sidebar_width, Some(300.0));
    }

    #[test]
    fn the_label_gets_what_the_rows_parts_leave() {
        let plain = 220.0 - 12.0 - 16.0 - 3.0 - 8.0 - 7.0 - 8.0 - 18.0 - 8.0;
        assert_eq!(label_width(220.0, false, false), plain);
        assert_eq!(
            label_width(220.0, true, false),
            plain - 7.0,
            "a page icon is wider"
        );
        assert_eq!(
            label_width(220.0, false, true),
            plain - 15.0,
            "a trouble dot and its gap"
        );
        assert_eq!(label_width(10.0, false, false), 0.0);
    }

    #[test]
    fn a_row_is_clipped_when_its_text_is_wider_than_its_label() {
        assert!(clipped(150.0, 140.0));
        assert!(!clipped(140.0, 140.0));
        assert!(text_width("prod-eu-west-1-bastion-with-a-long-name") > text_width("zsh"));
        assert_eq!(text_width(""), 0.0);
    }

    /// The measurement shapes as the row draws (`text()` leaves shaping at its default): a
    /// title in a joining script lays out to the same width both ways.
    #[test]
    fn the_measurement_shapes_as_the_row_draws() {
        use iced::advanced::text::Paragraph as _;
        let title = "\u{633}\u{644}\u{627}\u{645} prod";
        let drawn = iced::advanced::graphics::text::Paragraph::with_text(iced::advanced::Text {
            content: title,
            bounds: Size::new(f32::INFINITY, f32::INFINITY),
            size: iced::Pixels(super::LABEL_SIZE),
            line_height: iced::advanced::text::LineHeight::default(),
            font: crate::theme::UI_FONT,
            align_x: iced::advanced::text::Alignment::Default,
            align_y: iced::alignment::Vertical::Top,
            shaping: iced::advanced::text::Shaping::default(),
            wrapping: iced::advanced::text::Wrapping::None,
        })
        .min_width();
        let measured = text_width(title);
        assert!(
            (drawn - measured).abs() < 0.5,
            "drawn {drawn} vs measured {measured}"
        );
    }

    #[test]
    fn only_a_clipped_row_gets_a_tooltip_and_none_while_tips_are_off() {
        let long = "prod-eu-west-1-bastion-with-a-very-long-name";
        assert_eq!(
            row_tip(true, 140.0, long, false, false),
            Some(long.to_owned())
        );
        assert_eq!(row_tip(true, 220.0, "zsh", false, false), None);
        assert_eq!(row_tip(false, 140.0, long, false, false), None);
    }

    #[test]
    fn the_chosen_width_is_clamped_and_defaults() {
        assert_eq!(clamp_sidebar_width(10.0), MIN_SIDEBAR_WIDTH);
        assert_eq!(clamp_sidebar_width(5000.0), MAX_SIDEBAR_WIDTH);
        assert_eq!(clamp_sidebar_width(300.0), 300.0);
        assert_eq!(clamp_sidebar_width(f32::NAN), SIDEBAR_WIDTH);
        assert_eq!(clamp_sidebar_width(f32::INFINITY), SIDEBAR_WIDTH);
    }

    #[test]
    fn rows_are_revealed_only_when_hidden() {
        let flat = vec![Section {
            group: Group::Other,
            rows: (0..12).collect(),
            collapsed: false,
        }];
        let tops = item_tops(&items(&flat, 0, false));
        assert_eq!(tops[0], LIST_PADDING);
        assert_eq!(tops[3], LIST_PADDING + 3.0 * (ROW_HEIGHT + 2.0));
        assert_eq!(reveal(tops[0], ROW_HEIGHT, 0.0, 300.0, LIST_PADDING), None);
        assert_eq!(reveal(tops[2], ROW_HEIGHT, 0.0, 300.0, LIST_PADDING), None);
        assert_eq!(
            reveal(tops[10], ROW_HEIGHT, 0.0, 300.0, LIST_PADDING),
            Some(tops[10] + ROW_HEIGHT + LIST_PADDING - 300.0)
        );
        assert_eq!(
            reveal(tops[0], ROW_HEIGHT, 100.0, 300.0, LIST_PADDING),
            Some(0.0)
        );
        assert_eq!(
            reveal(tops[5], ROW_HEIGHT, 400.0, 300.0, LIST_PADDING),
            Some(tops[5] - LIST_PADDING)
        );
    }

    #[test]
    fn items_without_folding_are_rows_only_and_tops_match_the_flat_list() {
        let sections = vec![Section {
            group: Group::Other,
            rows: vec![0, 1, 2],
            collapsed: false,
        }];
        let items = items(&sections, 1, false);
        assert!(items.iter().all(|i| matches!(i, Item::Row(_))));
        assert_eq!(item_tops(&items), vec![6.0, 6.0 + 34.0, 6.0 + 68.0]);
    }

    #[test]
    fn items_with_folding_carry_headers_counts_and_the_active_marker() {
        let sections = vec![
            Section {
                group: Group::Featherbit,
                rows: vec![0],
                collapsed: false,
            },
            Section {
                group: Group::Named("prod".into()),
                rows: vec![1, 2],
                collapsed: true,
            },
            Section {
                group: Group::Other,
                rows: vec![3],
                collapsed: false,
            },
        ];
        let items = items(&sections, 2, true);
        // Header(featherbit, 0, false), Row(0), Header(prod, 2, true), Header(other, 0, false), Row(3)
        assert_eq!(items.len(), 5);
        assert!(matches!(
            &items[0],
            Item::Header {
                hidden: 0,
                holds_active: false,
                ..
            }
        ));
        assert!(matches!(&items[1], Item::Row(0)));
        assert!(matches!(
            &items[2],
            Item::Header {
                hidden: 2,
                holds_active: true,
                ..
            }
        ));
        assert!(matches!(&items[4], Item::Row(3)));
        let tops = item_tops(&items);
        assert_eq!(tops[0], 6.0);
        assert_eq!(tops[1], 6.0 + 26.0 + 2.0);
        assert_eq!(
            tops[2],
            tops[1] + 32.0 + 2.0 + 8.0,
            "a gap above a later header"
        );
        assert_eq!(tops[3], tops[2] + 26.0 + 2.0 + 8.0);
    }

    /// The drop marker sits 2 px above the row a gap precedes, unless a header lies between
    /// that row and the one before: then it sits under the previous row, inside its group.
    #[test]
    fn the_gap_marker_stays_in_the_group_at_its_end() {
        let row = |y: f32| iced::Rectangle::new(iced::Point::new(0.0, y), Size::new(100.0, 32.0));
        // Two rows of one group, a header (26 + gaps) and a row of the next group.
        let rows = [
            row(6.0),
            row(40.0),
            row(40.0 + 32.0 + 2.0 + 8.0 + 26.0 + 2.0),
        ];
        assert_eq!(gap_marker_y(&rows, 0, 0.0), 4.0, "before the first row");
        assert_eq!(
            gap_marker_y(&rows, 1, 0.0),
            38.0,
            "between two rows of a group"
        );
        assert_eq!(
            gap_marker_y(&rows, 2, 0.0),
            72.0,
            "at the group's end: under the last row, not under the next header"
        );
        assert_eq!(
            gap_marker_y(&rows, 3, 0.0),
            rows[2].y + 32.0,
            "after the last row"
        );
        assert_eq!(gap_marker_y(&[], 0, 50.0), 50.0, "no rows: the list's top");
    }

    #[test]
    fn switching_the_tabs_to_the_top_mid_drag_turns_tooltips_back_on() {
        let (mut app, wid) = app_with_tabs(&["a", "b"]);
        let _ = app.update(Message::Shortcut(wid, Action::VerticalTabs));
        let _ = app.update(Message::TabBar(wid, TabBarMsg::SidebarResize(300.0)));
        assert!(app.sidebar_resizing.contains(&wid));
        // The column goes away while the button is held: its drag never ends.
        let _ = app.update(Message::Shortcut(wid, Action::VerticalTabs));
        assert!(app.sidebar_resizing.is_empty());
    }

    #[test]
    fn title_widths_are_cached_and_the_cache_stays_bounded() {
        assert_eq!(text_width("prod-eu-west-1"), measure("prod-eu-west-1"));
        for i in 0..(super::WIDTH_CACHE_CAP + 50) {
            let _ = text_width(&format!("tab-{i}"));
        }
        assert!(super::width_cache_len() <= super::WIDTH_CACHE_CAP);
        assert_eq!(text_width("prod-eu-west-1"), measure("prod-eu-west-1"));
    }

    #[test]
    fn reveal_uses_the_items_top() {
        assert_eq!(
            reveal(100.0, 32.0, 0.0, 50.0, LIST_PADDING),
            Some(100.0 + 32.0 + 6.0 - 50.0)
        );
        assert_eq!(reveal(100.0, 32.0, 90.0, 300.0, LIST_PADDING), None);
    }

    #[test]
    fn a_folded_sidebar_lays_out() {
        let (mut app, wid) = app_with_groups(&[
            ("a", Some("prod")),
            ("b", Some("staging")),
            ("c", None),
            ("d", Some("prod")),
        ]);
        fold(&mut app);
        let _ = app.update(Message::TabBar(wid, TabBarMsg::Select(0)));
        let _ = app.update(Message::TabBar(wid, TabBarMsg::ToggleGroup(1)));
        assert!(app.collapsed[&wid].contains("staging"));
        lay_out(&app, wid);
    }

    #[test]
    fn the_active_row_is_revealed_under_headers() {
        let specs: Vec<(String, Option<&str>)> = (0..18)
            .map(|i| (format!("t{i}"), Some(["prod", "staging", "qa"][i % 3])))
            .collect();
        let refs: Vec<(&str, Option<&str>)> = specs.iter().map(|(n, g)| (n.as_str(), *g)).collect();
        let (mut app, wid) = app_with_groups(&refs);
        fold(&mut app);
        let _ = app.update(Message::TabBar(wid, TabBarMsg::Select(0)));
        let scrolled = TabBarMsg::Scrolled {
            offset: 0.0,
            height: 200.0,
        };
        let _ = app.update(Message::TabBar(wid, scrolled));
        let _ = app.update(Message::SelectTab(17));
        let active = app.windows[&wid].active;
        let items = items(&app.sections_of(wid), active, true);
        let top = item_tops(&items)[items
            .iter()
            .position(|i| matches!(i, Item::Row(r) if *r == active))
            .unwrap()];
        assert_eq!(
            app.sidebar_scroll.get(&wid),
            Some(&(top + ROW_HEIGHT + LIST_PADDING - 200.0, 200.0))
        );
    }

    #[test]
    fn switching_sides_keeps_tabs_order_and_the_active_tab_and_lays_out() {
        let (mut app, wid) = app_with_tabs(&["a", "b", "c"]);
        app.windows.get_mut(&wid).unwrap().active = 1;
        let titles = |app: &App| -> Vec<String> {
            app.windows[&wid]
                .tabs
                .iter()
                .map(|t| t.title().to_owned())
                .collect()
        };
        let _ = app.action(None, Action::VerticalTabs);
        assert!(app.settings.tabs_left());
        assert_eq!(app.windows[&wid].active, 1);
        assert_eq!(titles(&app), ["a", "b", "c"]);
        lay_out(&app, wid);
        let _ = app.action(None, Action::VerticalTabs);
        assert!(!app.settings.tabs_left());
        assert_eq!(titles(&app), ["a", "b", "c"]);
        lay_out(&app, wid);
    }

    #[test]
    fn a_narrow_window_with_a_panel_still_lays_out() {
        let (mut app, wid) = app_with_tabs(&["a"]);
        app.sizes.insert(wid, (360.0, 300.0));
        let _ = app.action(None, Action::VerticalTabs);
        let _ = app.open_tunnels_panel(wid);
        lay_out(&app, wid);
    }

    #[test]
    fn nothing_to_scroll_means_no_task_and_tabs_on_top_forget_the_scroll() {
        let (mut app, wid) = app_with_tabs(&["a", "b", "c"]);
        assert!(app.reveal_active_tabs().is_none(), "tabs on top");
        let _ = app.update(Message::Shortcut(wid, Action::VerticalTabs));
        assert!(app.reveal_active_tabs().is_none(), "already shown");
        let scrolled = TabBarMsg::Scrolled {
            offset: 400.0,
            height: 200.0,
        };
        let _ = app.update(Message::TabBar(wid, scrolled));
        assert!(app.sidebar_scroll.contains_key(&wid));
        // Back on top: the scrollable is gone, and so is what we knew about it.
        let _ = app.update(Message::Shortcut(wid, Action::VerticalTabs));
        assert!(!app.sidebar_scroll.contains_key(&wid));
        assert!(!app.sidebar_shown.contains_key(&wid));
    }

    #[test]
    fn switching_back_to_the_left_reveals_the_active_tab_afresh() {
        let names: Vec<String> = (0..20).map(|i| format!("t{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let (mut app, wid) = app_with_tabs(&refs);
        app.windows.get_mut(&wid).unwrap().active = 19;
        let _ = app.update(Message::Shortcut(wid, Action::VerticalTabs));
        // Revealed against the window's height: the list scrolled down to row 19.
        let (offset, _) = app.sidebar_scroll[&wid];
        assert!(offset > 0.0, "{offset}");
        // Scrolled back up by hand, then top and left again: revealed again, from 0.
        let _ = app.update(Message::TabBar(
            wid,
            TabBarMsg::Scrolled {
                offset: 0.0,
                height: 200.0,
            },
        ));
        let _ = app.update(Message::Shortcut(wid, Action::VerticalTabs));
        let _ = app.update(Message::Shortcut(wid, Action::VerticalTabs));
        let (offset, _) = app.sidebar_scroll[&wid];
        assert!(offset > 0.0, "{offset}");
    }

    #[test]
    fn selecting_a_hidden_row_scrolls_the_sidebar_and_a_visible_one_does_not() {
        let names: Vec<String> = (0..20).map(|i| format!("t{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let (mut app, wid) = app_with_tabs(&refs);
        let _ = app.update(Message::Shortcut(wid, Action::VerticalTabs));
        let first = app.windows[&wid].tabs[0].id;
        assert_eq!(app.sidebar_shown.get(&wid), Some(&first));
        let scrolled = TabBarMsg::Scrolled {
            offset: 0.0,
            height: 200.0,
        };
        let _ = app.update(Message::TabBar(wid, scrolled));
        assert_eq!(app.sidebar_scroll.get(&wid), Some(&(0.0, 200.0)));
        let _ = app.update(Message::SelectTab(19));
        let last = app.windows[&wid].tabs[19].id;
        assert_eq!(app.sidebar_shown.get(&wid), Some(&last));
        let _ = app.update(Message::SelectTab(18));
        assert_eq!(
            app.sidebar_shown.get(&wid),
            Some(&app.windows[&wid].tabs[18].id)
        );
    }
}
