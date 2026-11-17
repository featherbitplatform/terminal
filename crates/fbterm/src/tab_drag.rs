//! Pressing, dragging and dropping on a strip of tabs, shared by the tab bar (tabs along the
//! top) and the sidebar (tabs down the left): the same drag protocol ([`DragMsg`]) along
//! either axis, and the right-click that asks for a tab's menu.

use iced::advanced::Shell;
use iced::advanced::mouse::click;
use iced::{Event, Point, Rectangle, mouse, window};

use crate::app::{Message, TabBarMsg};
use crate::tabs::TabId;
use crate::windows::{Carry, DragMsg};

/// How far a press travels before it carries the tab instead of clicking it.
pub(crate) const DRAG_THRESHOLD: f32 = 6.0;

/// Along which axis the strip lays its tabs out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Axis {
    /// Side by side (the tab bar).
    X,
    /// One under the other (the sidebar).
    Y,
}

impl Axis {
    fn along(self, p: Point) -> f32 {
        match self {
            Axis::X => p.x,
            Axis::Y => p.y,
        }
    }

    fn center(self, r: &Rectangle) -> f32 {
        match self {
            Axis::X => r.center_x(),
            Axis::Y => r.center_y(),
        }
    }
}

/// A press on a tab, until its release. The tab is remembered by id, not by index: tabs can
/// come and go during a drag, and a release must act on the tab that was pressed, or on none.
pub(crate) struct Press {
    pub id: TabId,
    pub origin: Point,
    /// Travelled past the threshold: the tab is carried, the release is no click.
    pub dragging: bool,
}

#[derive(Default)]
pub(crate) struct DragState {
    pub press: Option<Press>,
    /// The last left press on a tab, and which tab: a second one on the same tab soon after
    /// is a double-click, which renames it.
    pub last_click: Option<(click::Click, TabId)>,
    /// The tab a carried tab hovers, as last reported.
    pub over: Option<usize>,
    /// Where a carried tile would become a tab, or a carried tab of this strip would move
    /// to, as last drawn.
    pub gap: Option<usize>,
    /// The pointer at the last move the bar saw: its edge-scroll goes by the distance moved.
    pub last_pointer: Option<Point>,
}

/// What a strip of tabs is while an event is handled.
pub(crate) struct Strip<'a> {
    pub window: window::Id,
    /// Each tab's id, in strip order.
    pub ids: &'a [TabId],
    /// Each row's index in its window's tab order (parallel to `ids`): the sidebar lists
    /// only the rows that can be seen, so the two differ while a group is folded.
    pub indices: &'a [usize],
    /// The gaps (strip indices, inclusive) the current carry may land in; `None`: any.
    pub range: Option<(usize, usize)>,
    /// A gap outside `range` moves to its nearer end (a carried tile lands in its group)
    /// instead of being refused (a carried tab stays in its group: it snaps back).
    pub clamp: bool,
    pub carry: Option<Carry>,
    /// The palette or a dialog covers the window: no tab drags, no menu.
    pub modal: bool,
    /// The active tab's index: a tile dropped on it goes nowhere.
    pub hole: Option<usize>,
    pub axis: Axis,
    /// How far the strip's coordinates are shifted from the window's: the sidebar's scroll
    /// offset (its rows are measured inside the scrollable), 0 for the bar.
    pub scroll: f32,
    /// The tab showing the rename field: a press on it is the field's (caret, selection),
    /// never a drag or a double-click.
    pub renaming: Option<TabId>,
    /// The strip index of the first rectangle passed with an event: the bar passes only the
    /// tabs it shows (scrolled out ones have no place on screen), the sidebar all its rows.
    pub first: usize,
}

/// Where something dropped at `p` lands between the tabs: before the tab whose first half
/// (along the axis) is under `p`, after the one whose second half is, else the nearest gap
/// between them (past either end: beside the tab at that end). Over its own tab (`own`):
/// nowhere.
pub(crate) fn gap_at(
    rects: &[Rectangle],
    own: Option<usize>,
    p: Point,
    axis: Axis,
) -> Option<usize> {
    // A strip listing no tabs (the bar while tabs are on the left) takes no drop.
    if rects.is_empty() {
        return None;
    }
    if let Some(i) = rects.iter().position(|r| r.contains(p)) {
        if Some(i) == own {
            return None;
        }
        return Some(if axis.along(p) < axis.center(&rects[i]) {
            i
        } else {
            i + 1
        });
    }
    Some(
        rects
            .iter()
            .position(|r| axis.along(p) < axis.center(r))
            .unwrap_or(rects.len()),
    )
}

/// The window index a drop in strip gap `gap` lands at: before the row there, or one past
/// the last row's window index after the end (0 for an empty strip).
fn window_gap(indices: &[usize], gap: usize) -> usize {
    match indices.get(gap) {
        Some(&i) => i,
        None => indices.last().map_or(0, |&last| last + 1),
    }
}

/// The tab-drag and right-click side of `event` over a strip whose tabs from
/// `strip.first` on sit at `rects` inside `bounds`: a press that travels [`DRAG_THRESHOLD`] carries its tab, a carried tab
/// of this strip dropped in a gap asks to move there ([`DragMsg::ReorderTab`]), a carried
/// tile dropped in a gap becomes a tab there. Returns `true` when the content beneath must
/// not see where the pointer is (the release that ends a tab drag is no click on a button; a
/// right-click is the menu's, not the button's).
pub(crate) fn drag_event(
    strip: &Strip<'_>,
    state: &mut DragState,
    event: &Event,
    rects: &[Rectangle],
    bounds: Rectangle,
    cursor: mouse::Cursor,
    shell: &mut Shell<'_, Message>,
) -> bool {
    let publish = |shell: &mut Shell<'_, Message>, msg: DragMsg| {
        shell.publish(Message::Drag(strip.window, msg));
    };
    let under = |p: Point| {
        rects
            .iter()
            .position(|r| r.contains(p))
            .map(|i| i + strip.first)
    };
    // A row's index in its window's tab order: what the app is told.
    let at_window = |row: usize| strip.indices.get(row).copied().unwrap_or(row);
    let index_of = |id: TabId| strip.ids.iter().position(|i| *i == id);
    let tile = matches!(strip.carry, Some(Carry::Tile(_)));
    // The index of the tab this strip is carrying, if any: it can be dropped back on the
    // strip, in another gap.
    let own = state
        .press
        .as_ref()
        .filter(|p| p.dragging && matches!(strip.carry, Some(Carry::Tab(id)) if id == p.id))
        .and_then(|p| index_of(p.id));
    // A drag that ended elsewhere (Escape) leaves no gap behind.
    if !tile && own.is_none() && state.gap.take().is_some() {
        shell.request_redraw();
    }
    let gap = |p: Option<Point>| {
        let p = p.filter(|_| tile || own.is_some())?;
        let own_rect = if tile { strip.hole } else { own }.and_then(|i| i.checked_sub(strip.first));
        let gap = gap_at(rects, own_rect, p, strip.axis)? + strip.first;
        // Outside the carry's group: a tile goes to the group's nearer end, a tab nowhere.
        let gap = match strip.range {
            Some((lo, hi)) if gap < lo || gap > hi => {
                if strip.clamp {
                    gap.clamp(lo, hi)
                } else {
                    return None;
                }
            }
            _ => gap,
        };
        // Right before or after the carried tab: no move.
        (own != Some(gap) && own.map(|o| o + 1) != Some(gap)).then_some(gap)
    };
    // The tab a carried tab of this strip hovers, told to the app when it changes.
    let track_over = |state: &mut DragState, shell: &mut Shell<'_, Message>| {
        let Some(mine) = state
            .press
            .as_ref()
            .filter(|p| p.dragging && matches!(strip.carry, Some(Carry::Tab(_))))
            .map(|p| index_of(p.id))
        else {
            return;
        };
        let over = cursor
            .position_over(bounds)
            .and_then(under)
            .filter(|i| Some(*i) != mine);
        if over != state.over {
            state.over = over;
            publish(shell, DragMsg::OverTab(over.map(at_window)));
        }
    };
    match event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
            let pressed = cursor
                .position_over(bounds)
                .filter(|_| !strip.modal)
                .and_then(|p| {
                    let row = under(p)?;
                    let id = *strip.ids.get(row)?;
                    (strip.renaming != Some(id)).then_some((row, id, p))
                });
            state.press = pressed.map(|(_, id, p)| Press {
                id,
                origin: p,
                dragging: false,
            });
            match pressed {
                Some((row, id, p)) => {
                    let previous = state.last_click.map(|(c, _)| c);
                    let click = click::Click::new(p, mouse::Button::Left, previous);
                    let again = state.last_click.is_some_and(|(_, last)| last == id);
                    // A third click is no new rename: it counts as a single again.
                    if again && click.kind() == click::Kind::Double {
                        state.last_click = None;
                        shell.publish(Message::TabBar(
                            strip.window,
                            TabBarMsg::Rename(at_window(row)),
                        ));
                    } else {
                        state.last_click = Some((click, id));
                    }
                }
                None => state.last_click = None,
            }
        }
        // A wheel click on a tab closes it, as in a browser.
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Middle)) if !strip.modal => {
            if let Some(index) = cursor.position_over(bounds).and_then(under) {
                shell.publish(Message::TabBar(
                    strip.window,
                    TabBarMsg::Close(at_window(index)),
                ));
            }
        }
        // A right-click on a tab asks for its menu, at the pointer in window coordinates.
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) if !strip.modal => {
            if let Some(p) = cursor.position_over(bounds)
                && let Some(index) = under(p)
            {
                let at = Point::new(p.x, p.y - strip.scroll);
                shell.publish(Message::TabBar(
                    strip.window,
                    TabBarMsg::Menu {
                        index: at_window(index),
                        at,
                    },
                ));
                return true;
            }
        }
        Event::Mouse(mouse::Event::CursorMoved { .. }) => {
            if let (Some(press), Some(p)) = (&mut state.press, cursor.position())
                && !press.dragging
                && p.distance(press.origin) >= DRAG_THRESHOLD
            {
                press.dragging = true;
                if strip.ids.contains(&press.id) {
                    shell.publish(Message::Drag(strip.window, DragMsg::StartTab(press.id)));
                }
            }
            track_over(state, shell);
            let at = gap(cursor.position_over(bounds));
            if at != state.gap {
                state.gap = at;
                shell.request_redraw();
            }
        }
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
            let at = gap(cursor.position_over(bounds));
            if state.gap.take().is_some() {
                shell.request_redraw();
            }
            if let Some(Press { dragging: true, .. }) = state.press.take() {
                state.over = None;
                // This strip's own tab let go in a gap: it moves there, then the drag ends.
                if let (Some(index), Some(tab)) =
                    (at.filter(|_| !tile), own.and_then(|o| strip.ids.get(o)))
                {
                    publish(
                        shell,
                        DragMsg::ReorderTab {
                            tab: *tab,
                            index: window_gap(strip.indices, index),
                        },
                    );
                }
                publish(shell, DragMsg::End);
                return true;
            }
            if let (Some(Carry::Tile(pane)), Some(index)) =
                (strip.carry, gap(cursor.position_over(bounds)))
            {
                publish(
                    shell,
                    DragMsg::TileToTab {
                        pane,
                        index: window_gap(strip.indices, index),
                    },
                );
            }
        }
        // Every frame is laid out afresh before it is drawn: a strip that scrolled under a
        // still pointer (edge-scroll, wheel, reveal) moves its tabs, so the gap is measured
        // again from these rects, as the release will, and the marker shows where the drop
        // lands. The frame is being drawn already: no redraw to ask for. The hovered tab is
        // measured again too (a message only when it changed).
        Event::Window(window::Event::RedrawRequested(_)) => {
            state.gap = gap(cursor.position_over(bounds));
            track_over(state, shell);
        }
        Event::Mouse(mouse::Event::CursorLeft) | Event::Window(window::Event::Unfocused) => {
            if let Some(Press { dragging: true, .. }) = state.press.take() {
                publish(shell, DragMsg::End);
            }
            state.over = None;
            if state.gap.take().is_some() {
                shell.request_redraw();
            }
        }
        _ => {}
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::{Point, Rectangle, Size};

    #[test]
    fn a_strip_passing_only_its_shown_tabs_reports_strip_indices() {
        use iced::advanced::Shell;
        // Tabs 0 and 3 are scrolled out: only tabs 1 and 2 are passed, at x 100 and 200.
        let ids = [TabId::next(), TabId::next(), TabId::next(), TabId::next()];
        let strip = Strip {
            window: window::Id::unique(),
            ids: &ids,
            indices: &[0, 1, 2, 3],
            range: None,
            clamp: false,
            carry: Some(Carry::Tile(crate::layout::PaneId::next())),
            modal: false,
            hole: Some(1),
            axis: Axis::X,
            scroll: 0.0,
            renaming: None,
            first: 1,
        };
        let rects =
            [100.0, 200.0].map(|x| Rectangle::new(Point::new(x, 0.0), Size::new(98.0, 30.0)));
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(400.0, 30.0));
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        let mut gap = |x: f32| {
            let mut state = DragState::default();
            let p = Point::new(x, 10.0);
            let event = Event::Mouse(mouse::Event::CursorMoved { position: p });
            let cursor = mouse::Cursor::Available(p);
            drag_event(
                &strip, &mut state, &event, &rects, bounds, cursor, &mut shell,
            );
            state.gap
        };
        assert_eq!(
            gap(50.0),
            Some(1),
            "left of the shown tabs: before the first"
        );
        assert_eq!(gap(350.0), Some(3), "right of them: after the last");
        assert_eq!(gap(260.0), Some(3), "second half of the last shown");
        assert_eq!(
            gap(120.0),
            None,
            "over the active tab (1): the tile goes nowhere"
        );
        assert_eq!(gap(220.0), Some(2), "first half of tab 2");
        // A middle click on the second rectangle closes tab 2.
        let p = Point::new(250.0, 10.0);
        let middle = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Middle));
        let mut state = DragState::default();
        let cursor = mouse::Cursor::Available(p);
        drag_event(
            &strip, &mut state, &middle, &rects, bounds, cursor, &mut shell,
        );
        assert!(
            matches!(
                messages.last(),
                Some(Message::TabBar(_, TabBarMsg::Close(2)))
            ),
            "{messages:?}"
        );
    }

    #[test]
    fn gap_at_works_along_either_axis() {
        let rows = [
            Rectangle::new(Point::new(0.0, 0.0), Size::new(100.0, 30.0)),
            Rectangle::new(Point::new(0.0, 34.0), Size::new(100.0, 30.0)),
        ];
        assert_eq!(gap_at(&rows, None, Point::new(10.0, 5.0), Axis::Y), Some(0));
        assert_eq!(
            gap_at(&rows, None, Point::new(10.0, 25.0), Axis::Y),
            Some(1)
        );
        assert_eq!(
            gap_at(&rows, None, Point::new(10.0, 60.0), Axis::Y),
            Some(2)
        );
        assert_eq!(
            gap_at(&rows, None, Point::new(10.0, 200.0), Axis::Y),
            Some(2)
        );
        assert_eq!(
            gap_at(&rows, None, Point::new(10.0, 32.0), Axis::Y),
            Some(1)
        );
        assert_eq!(
            gap_at(&rows, Some(1), Point::new(10.0, 40.0), Axis::Y),
            None
        );
        let tabs = [
            Rectangle::new(Point::new(0.0, 0.0), Size::new(100.0, 30.0)),
            Rectangle::new(Point::new(102.0, 0.0), Size::new(100.0, 30.0)),
        ];
        assert_eq!(
            gap_at(&tabs, None, Point::new(160.0, 5.0), Axis::X),
            Some(2)
        );
        assert_eq!(
            gap_at(&tabs, None, Point::new(120.0, 5.0), Axis::X),
            Some(1)
        );
        assert_eq!(gap_at(&tabs, None, Point::new(20.0, 5.0), Axis::X), Some(0));
        assert_eq!(gap_at(&tabs, Some(0), Point::new(20.0, 5.0), Axis::X), None);
        // A strip with no tabs (the bar while tabs are on the left) takes no drop.
        assert_eq!(gap_at(&[], None, Point::new(20.0, 5.0), Axis::X), None);
    }

    /// Runs `events` (each with the pointer position, in strip coordinates) through a strip
    /// of `rows` down the left, scrolled by `scroll`, while `carry` is under way. Returns the
    /// drag messages published.
    fn drive_rows(
        rows: &[Rectangle],
        ids: &[TabId],
        carry: Option<Carry>,
        scroll: f32,
        events: &[(Event, Point)],
    ) -> Vec<DragMsg> {
        use iced::advanced::Shell;
        let strip = Strip {
            window: window::Id::unique(),
            ids,
            indices: &(0..ids.len()).collect::<Vec<_>>(),
            range: None,
            clamp: false,
            carry,
            modal: false,
            hole: Some(0),
            axis: Axis::Y,
            scroll,
            renaming: None,
            first: 0,
        };
        let bounds = Rectangle::new(
            Point::new(0.0, rows[0].y),
            Size::new(100.0, rows.len() as f32 * 34.0),
        );
        let mut state = DragState::default();
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        for (event, at) in events {
            drag_event(
                &strip,
                &mut state,
                event,
                rows,
                bounds,
                mouse::Cursor::Available(*at),
                &mut shell,
            );
        }
        messages
            .into_iter()
            .filter_map(|m| match m {
                Message::Drag(_, d) => Some(d),
                _ => None,
            })
            .collect()
    }

    /// A strip of four rows (30 high, 34 apart from y 0) whose window indices are
    /// `[0, 1, 3, 4]` (window index 2 is hidden in a folded group). Runs `events` with
    /// `carry`, `range` and `clamp`, and returns every message published.
    fn drive_mapped(
        carry: Option<Carry>,
        range: Option<(usize, usize)>,
        clamp: bool,
        events: &[(Event, Point)],
    ) -> (Vec<TabId>, Vec<Message>) {
        drive_mapped_with(|_| carry, range, clamp, events)
    }

    fn drags(messages: &[Message]) -> Vec<DragMsg> {
        messages
            .iter()
            .filter_map(|m| match m {
                Message::Drag(
                    _,
                    d @ (DragMsg::ReorderTab { .. } | DragMsg::TileToTab { .. } | DragMsg::End),
                ) => Some(d.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn published_indices_are_window_indices() {
        let right = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right));
        let (_, messages) = drive_mapped(None, None, false, &[(right, Point::new(10.0, 70.0))]);
        assert!(
            matches!(
                messages.as_slice(),
                [Message::TabBar(_, TabBarMsg::Menu { index: 3, .. })]
            ),
            "{messages:?}"
        );
        let middle = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Middle));
        let (_, messages) = drive_mapped(None, None, false, &[(middle, Point::new(10.0, 110.0))]);
        assert!(
            matches!(
                messages.as_slice(),
                [Message::TabBar(_, TabBarMsg::Close(4))]
            ),
            "{messages:?}"
        );
    }

    #[test]
    fn a_tab_dropped_outside_its_range_snaps_back() {
        // Row 1's tab is carried; its group spans gaps 0..=2.
        let at = Point::new(10.0, 34.0 + 15.0);
        let away = Point::new(10.0, at.y + 10.0);
        let below_last = Point::new(10.0, 102.0 + 25.0); // row 3's lower half: gap 4
        let carried = |ids: &[TabId]| Some(Carry::Tab(ids[1]));
        let (_, messages) = drive_mapped_with(
            carried,
            Some((0, 2)),
            false,
            &[
                (press(), at),
                (moved(away), away),
                (moved(below_last), below_last),
                (release(), below_last),
            ],
        );
        assert_eq!(
            drags(&messages),
            [DragMsg::End],
            "outside the range: only the end"
        );
        let top = Point::new(10.0, 5.0); // row 0's upper half: gap 0
        let (ids, messages) = drive_mapped_with(
            carried,
            Some((0, 2)),
            false,
            &[
                (press(), at),
                (moved(away), away),
                (moved(top), top),
                (release(), top),
            ],
        );
        assert_eq!(
            drags(&messages),
            [
                DragMsg::ReorderTab {
                    tab: ids[1],
                    index: 0
                },
                DragMsg::End
            ]
        );
    }

    #[test]
    fn a_tile_dropped_outside_its_range_lands_at_its_groups_end() {
        let pane = crate::layout::PaneId::next();
        let between = Point::new(10.0, 32.0); // between rows 0 and 1: gap 1, clamped to 2
        let (_, messages) = drive_mapped(
            Some(Carry::Tile(pane)),
            Some((2, 4)),
            true,
            &[(moved(between), between), (release(), between)],
        );
        assert_eq!(drags(&messages), [DragMsg::TileToTab { pane, index: 3 }]);
    }

    #[test]
    fn a_gap_past_the_last_row_maps_to_one_past_the_last_window_index() {
        let at = Point::new(10.0, 15.0);
        let away = Point::new(10.0, 25.0);
        let below = Point::new(10.0, 130.0); // row 3's lower half: gap 4
        let (ids, messages) = drive_mapped_with(
            |ids| Some(Carry::Tab(ids[0])),
            None,
            false,
            &[
                (press(), at),
                (moved(away), away),
                (moved(below), below),
                (release(), below),
            ],
        );
        assert_eq!(
            drags(&messages),
            [
                DragMsg::ReorderTab {
                    tab: ids[0],
                    index: 5
                },
                DragMsg::End
            ]
        );
    }

    /// `drive_mapped` with the carry chosen from the strip's ids.
    fn drive_mapped_with(
        carry: impl Fn(&[TabId]) -> Option<Carry>,
        range: Option<(usize, usize)>,
        clamp: bool,
        events: &[(Event, Point)],
    ) -> (Vec<TabId>, Vec<Message>) {
        use iced::advanced::Shell;
        let rows: Vec<Rectangle> = (0..4)
            .map(|i| Rectangle::new(Point::new(0.0, i as f32 * 34.0), Size::new(100.0, 30.0)))
            .collect();
        let ids = vec![TabId::next(), TabId::next(), TabId::next(), TabId::next()];
        let indices = [0, 1, 3, 4];
        let strip = Strip {
            window: window::Id::unique(),
            ids: &ids,
            indices: &indices,
            range,
            clamp,
            carry: carry(&ids),
            modal: false,
            hole: Some(0),
            axis: Axis::Y,
            scroll: 0.0,
            renaming: None,
            first: 0,
        };
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(100.0, 136.0));
        let mut state = DragState::default();
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        for (event, at) in events {
            drag_event(
                &strip,
                &mut state,
                event,
                &rows,
                bounds,
                mouse::Cursor::Available(*at),
                &mut shell,
            );
        }
        (ids, messages)
    }

    fn press() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
    }

    fn release() -> Event {
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
    }

    fn moved(p: Point) -> Event {
        Event::Mouse(mouse::Event::CursorMoved { position: p })
    }

    #[test]
    fn a_tab_carried_down_the_sidebar_and_dropped_in_a_gap_asks_to_move_there() {
        // Three rows inside a list scrolled by 400 (content coordinates).
        let rows = [
            Rectangle::new(Point::new(0.0, 400.0), Size::new(100.0, 30.0)),
            Rectangle::new(Point::new(0.0, 434.0), Size::new(100.0, 30.0)),
            Rectangle::new(Point::new(0.0, 468.0), Size::new(100.0, 30.0)),
        ];
        let ids = [TabId::next(), TabId::next(), TabId::next()];
        let at = rows[0].center();
        let away = Point::new(at.x, at.y + 10.0);
        // The lower half of the second row: the gap after it, index 2.
        let over = Point::new(at.x, rows[1].center_y() + 10.0);
        let drags = drive_rows(
            &rows,
            &ids,
            Some(Carry::Tab(ids[0])),
            400.0,
            &[
                (press(), at),
                (moved(away), away),
                (moved(over), over),
                (release(), over),
            ],
        );
        assert_eq!(
            drags
                .iter()
                .filter(|d| matches!(d, DragMsg::ReorderTab { .. }))
                .collect::<Vec<_>>(),
            [&DragMsg::ReorderTab {
                tab: ids[0],
                index: 2
            }],
            "{drags:?}"
        );
        assert_eq!(drags.last(), Some(&DragMsg::End));

        // The upper half of the second row is the gap right after itself: no move.
        let over = Point::new(at.x, rows[1].center_y() - 10.0);
        let drags = drive_rows(
            &rows,
            &ids,
            Some(Carry::Tab(ids[0])),
            400.0,
            &[
                (press(), at),
                (moved(away), away),
                (moved(over), over),
                (release(), over),
            ],
        );
        assert!(
            !drags
                .iter()
                .any(|d| matches!(d, DragMsg::ReorderTab { .. })),
            "{drags:?}"
        );
        assert_eq!(drags.last(), Some(&DragMsg::End));
    }

    #[test]
    fn a_strip_scrolled_under_a_still_pointer_moves_the_gap_on_the_next_frame() {
        use iced::advanced::Shell;
        let ids = [TabId::next(), TabId::next(), TabId::next()];
        let strip = Strip {
            window: window::Id::unique(),
            ids: &ids,
            indices: &[0, 1, 2],
            range: None,
            clamp: false,
            carry: Some(Carry::Tile(crate::layout::PaneId::next())),
            modal: false,
            hole: None,
            axis: Axis::X,
            scroll: 0.0,
            renaming: None,
            first: 0,
        };
        let tabs = |shift: f32| {
            [0.0, 100.0, 200.0]
                .map(|x| Rectangle::new(Point::new(x - shift, 0.0), Size::new(96.0, 30.0)))
        };
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(300.0, 30.0));
        let at = mouse::Cursor::Available(Point::new(120.0, 10.0));
        let mut state = DragState::default();
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        let before = tabs(0.0);
        let event = Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(120.0, 10.0),
        });
        drag_event(&strip, &mut state, &event, &before, bounds, at, &mut shell);
        assert_eq!(state.gap, Some(1), "the first half of the second tab");
        // The strip scrolls 60 px: the pointer is now over the second half of that tab.
        let after = tabs(60.0);
        let frame = Event::Window(window::Event::RedrawRequested(std::time::Instant::now()));
        drag_event(&strip, &mut state, &frame, &after, bounds, at, &mut shell);
        assert_eq!(state.gap, Some(2));
    }

    #[test]
    fn a_strip_scrolled_under_a_carried_tab_reports_the_tab_now_under_it() {
        use iced::advanced::Shell;
        let ids = [TabId::next(), TabId::next(), TabId::next()];
        let strip = Strip {
            window: window::Id::unique(),
            ids: &ids,
            indices: &[0, 1, 2],
            range: None,
            clamp: false,
            carry: Some(Carry::Tab(ids[0])),
            modal: false,
            hole: None,
            axis: Axis::X,
            scroll: 0.0,
            renaming: None,
            first: 0,
        };
        let tabs = |shift: f32| {
            [0.0, 100.0, 200.0]
                .map(|x| Rectangle::new(Point::new(x - shift, 0.0), Size::new(96.0, 30.0)))
        };
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(300.0, 30.0));
        let mut state = DragState::default();
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        let before = tabs(0.0);
        let mut step = |state: &mut DragState, event: Event, rects: &[Rectangle], p: Point| {
            let cursor = mouse::Cursor::Available(p);
            drag_event(&strip, state, &event, rects, bounds, cursor, &mut shell);
        };
        let pointer = Point::new(150.0, 10.0);
        step(&mut state, press(), &before, before[0].center());
        step(&mut state, moved(pointer), &before, pointer);
        assert_eq!(state.over, Some(1));
        // The strip scrolls 100 px: the third tab is under the still pointer.
        let frame = Event::Window(window::Event::RedrawRequested(std::time::Instant::now()));
        step(&mut state, frame, &tabs(100.0), pointer);
        assert_eq!(state.over, Some(2));
        let overs: Vec<_> = messages
            .into_iter()
            .filter_map(|m| match m {
                Message::Drag(_, DragMsg::OverTab(o)) => Some(o),
                _ => None,
            })
            .collect();
        assert_eq!(overs, [Some(1), Some(2)]);
    }

    #[test]
    fn a_tab_that_left_the_strip_during_its_drag_is_not_reordered() {
        let rows = [
            Rectangle::new(Point::new(0.0, 0.0), Size::new(100.0, 30.0)),
            Rectangle::new(Point::new(0.0, 34.0), Size::new(100.0, 30.0)),
        ];
        let ids = [TabId::next(), TabId::next()];
        // The carried tab is not one of this strip's rows any more (it moved to another
        // window): a release over a gap here asks for no move.
        let gone = TabId::next();
        let at = rows[0].center();
        let over = Point::new(at.x, rows[1].center_y() + 10.0);
        let drags = drive_rows(
            &rows,
            &ids,
            Some(Carry::Tab(gone)),
            0.0,
            &[(press(), at), (moved(over), over), (release(), over)],
        );
        assert!(
            !drags
                .iter()
                .any(|d| matches!(d, DragMsg::ReorderTab { .. })),
            "{drags:?}"
        );
    }

    /// Drives two presses and releases at `at` over a strip of three rows (window indices
    /// 0, 2, 3), the row `renaming` showing the rename field. Returns the renames asked for.
    fn double_click(at: Point, renaming: Option<usize>) -> Vec<usize> {
        use iced::advanced::Shell;
        let rows: Vec<Rectangle> = (0..3)
            .map(|i| Rectangle::new(Point::new(0.0, i as f32 * 34.0), Size::new(100.0, 30.0)))
            .collect();
        let ids = vec![TabId::next(), TabId::next(), TabId::next()];
        let indices = [0, 2, 3];
        let strip = Strip {
            window: window::Id::unique(),
            ids: &ids,
            indices: &indices,
            range: None,
            clamp: false,
            carry: None,
            modal: false,
            hole: Some(0),
            axis: Axis::Y,
            scroll: 0.0,
            renaming: renaming.map(|r| ids[r]),
            first: 0,
        };
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(100.0, 102.0));
        let mut state = DragState::default();
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        for event in [press(), release(), press(), release()] {
            drag_event(
                &strip,
                &mut state,
                &event,
                &rows,
                bounds,
                mouse::Cursor::Available(at),
                &mut shell,
            );
        }
        messages
            .iter()
            .filter_map(|m| match m {
                Message::TabBar(_, TabBarMsg::Rename(i)) => Some(*i),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_double_click_on_a_tab_asks_to_rename_it_but_not_on_its_field() {
        let second_row = Point::new(10.0, 40.0);
        assert_eq!(double_click(second_row, None), vec![2], "its window index");
        assert!(double_click(second_row, Some(1)).is_empty());
        assert!(
            double_click(Point::new(10.0, 32.0), None).is_empty(),
            "the gap between rows is no tab"
        );
    }

    #[test]
    fn a_right_click_reports_the_menu_at_the_pointer_in_window_coordinates() {
        use iced::advanced::Shell;
        // Content coordinates: the list is scrolled by 400, so its rows and the pointer the
        // scrollable hands down are 400 below the window's.
        let rows = [
            Rectangle::new(Point::new(0.0, 400.0), Size::new(100.0, 30.0)),
            Rectangle::new(Point::new(0.0, 434.0), Size::new(100.0, 30.0)),
        ];
        let ids = [TabId::next(), TabId::next()];
        let strip = Strip {
            window: window::Id::unique(),
            ids: &ids,
            indices: &[0, 1],
            range: None,
            clamp: false,
            carry: None,
            modal: false,
            hole: Some(0),
            axis: Axis::Y,
            // The rows are measured inside a scrollable scrolled by 400: a pointer at
            // content y 440 is at window y 40.
            scroll: 400.0,
            renaming: None,
            first: 0,
        };
        let mut state = DragState::default();
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        let taken = drag_event(
            &strip,
            &mut state,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)),
            &rows,
            Rectangle::new(Point::new(0.0, 400.0), Size::new(100.0, 64.0)),
            mouse::Cursor::Available(Point::new(10.0, 440.0)),
            &mut shell,
        );
        assert!(taken);
        assert!(matches!(
            messages.as_slice(),
            [Message::TabBar(_, TabBarMsg::Menu { index: 1, at })] if *at == Point::new(10.0, 40.0)
        ));
    }
}
