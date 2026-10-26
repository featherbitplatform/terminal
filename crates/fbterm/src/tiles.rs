//! The panes of one tab on screen: each pane's view laid out at its rectangle of the layout
//! tree, the dividers between them, and (with more than one pane) the focus outline and the
//! dimming of the other panes. Also turns presses into pane focus, divider drags and tile
//! moves: a tile is picked up by the grab handle shown at the top of the hovered pane, or
//! anywhere in a pane with the pick modifier held (Cmd on macOS, Ctrl+Shift elsewhere), and
//! dropped on a tile's edge or the outer edge of the content, with a preview of where it lands.
//! A tab carried from the tab bar over the content gets the same targets and preview, and a tile
//! carried out over the tab bar is reported to the app (`DragMsg::Tile`) so the bar can take it.
//!
//! A tab with one pane draws exactly its pane's view: no divider, outline, dimming or handle.

use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout as Placed};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::{
    Border, Color, Element, Event, Length, Pixels, Point, Rectangle, Size, Vector, keyboard, mouse,
    window,
};

use crate::app::Message;
use crate::layout::{Axis, DIVIDER, Drop, Layout, PaneId, Path, Side};
use crate::tabs::{Tab, TabId};
use crate::theme::Tokens;
use crate::ui::{ICON_GRIP, ICONS};
use crate::windows::DragMsg;

/// Extra grab room on each side of a divider, over the panes next to it.
const SLOP: f32 = 2.0;
/// Two presses on a divider within this time equalize its split.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Alpha of the terminal background laid over the panes that do not have focus.
const DIM: f32 = 0.35;
/// Depth of the band along the content's outer edge that drops a tile as a whole row or column.
const EDGE_BAND: f32 = 24.0;
/// How far the pointer must travel with the button held before a pick becomes a tile drag.
const PICK_SLOP: f32 = 4.0;
/// The grab handle at the top of a hovered pane: its size and its glyph's.
const HANDLE_W: f32 = 28.0;
const HANDLE_H: f32 = 14.0;
const GRIP: f32 = 12.0;

/// What a window's tiles ask of the app.
#[derive(Debug, Clone, PartialEq)]
pub enum TilesMsg {
    /// A press inside a pane other than the focused one.
    Focus(PaneId),
    /// A divider is being dragged: the split at `path` should give its first side `ratio`.
    DragDivider { path: Path, ratio: f32 },
    /// The divider drag ended.
    EndDrag,
    /// Double-click on a divider: share its split equally.
    Equalize(Path),
    /// The area the panes are laid out in changed size.
    Area(Size),
    /// A tile was dropped: move `pane` to `at`.
    DropTile { pane: PaneId, at: Drop },
}

/// What lies under a point of the tiles.
#[derive(Debug, Clone, PartialEq)]
pub enum Hit {
    Pane(PaneId),
    Divider(Path),
    None,
}

/// What lies under `point` when `layout` fills `area`. A divider's grab area reaches `SLOP`
/// into the panes beside it and wins over them.
pub fn hit(layout: &Layout, area: Rectangle, point: Point) -> Hit {
    if let Some(d) = layout
        .dividers(area)
        .into_iter()
        .find(|d| grab_area(d.rect, d.axis).contains(point))
    {
        return Hit::Divider(d.path);
    }
    layout
        .rects(area)
        .into_iter()
        .find(|(_, r)| r.contains(point))
        .map_or(Hit::None, |(id, _)| Hit::Pane(id))
}

/// Where a drop at `point` lands when `layout` fills `area`: within `EDGE_BAND` of the
/// content's outer edge, that whole edge; else inside a tile, the nearest of its four edges.
/// `dragged`: the pane being moved (a tile drag), never its own target; `None` when what is
/// dragged is not part of this layout (a whole tab).
pub fn drop_target(
    layout: &Layout,
    area: Rectangle,
    point: Point,
    dragged: Option<PaneId>,
) -> Option<Drop> {
    if !area.contains(point) {
        return None;
    }
    let (side, depth) = nearest_side(area, point);
    if depth <= EDGE_BAND {
        return Some(Drop::Edge(side));
    }
    let (id, rect) = layout
        .rects(area)
        .into_iter()
        .find(|(_, r)| r.contains(point))?;
    if Some(id) == dragged {
        return None;
    }
    Some(Drop::Tile(id, nearest_side(rect, point).0))
}

/// The side of `rect` nearest to `p` (the first in left, right, top, bottom order on a tie)
/// and the distance to it.
fn nearest_side(rect: Rectangle, p: Point) -> (Side, f32) {
    [
        (Side::Left, p.x - rect.x),
        (Side::Right, rect.x + rect.width - p.x),
        (Side::Top, p.y - rect.y),
        (Side::Bottom, rect.y + rect.height - p.y),
    ]
    .into_iter()
    .min_by(|a, b| a.1.total_cmp(&b.1))
    .expect("four sides")
}

/// Whether every pane of `layout` keeps the minimum size in `area` with cells of `cell`
/// (unknown before the first layout: anything goes, as for splits).
fn fits(layout: &Layout, area: Rectangle, cell: Option<Size>) -> bool {
    cell.is_none_or(|cell| layout.fits(area, cell))
}

/// The rectangle `pane` would get if dropped at `at`; `None` when the drop is impossible,
/// would leave the arrangement as it is, or would squeeze a pane below the minimum size.
fn preview(
    layout: &Layout,
    area: Rectangle,
    pane: PaneId,
    at: Drop,
    cell: Option<Size>,
) -> Option<Rectangle> {
    let mut moved = layout.clone();
    if !moved.move_pane(pane, at) || moved == *layout || !fits(&moved, area, cell) {
        return None;
    }
    moved
        .rects(area)
        .into_iter()
        .find(|(id, _)| *id == pane)
        .map(|(_, r)| snap(r))
}

/// The rectangle a whole tab (`subtree`, its layout) dropped at `at` takes, its panes keeping
/// their arrangement inside; `None` when a pane would end up below the minimum size.
fn incoming_preview(
    layout: &Layout,
    area: Rectangle,
    subtree: &Layout,
    at: Drop,
    cell: Option<Size>,
) -> Option<Rectangle> {
    let mut merged = layout.clone();
    merged.insert(subtree.clone(), at);
    if !fits(&merged, area, cell) {
        return None;
    }
    let ids = subtree.panes();
    merged
        .rects(area)
        .into_iter()
        .filter(|(id, _)| ids.contains(id))
        .map(|(_, r)| r)
        .reduce(|a, b| a.union(&b))
        .map(snap)
}

/// The grab handle of a pane at `rect`: centred on its top edge, just inside it.
fn handle_rect(rect: Rectangle) -> Rectangle {
    Rectangle {
        x: (rect.center_x() - HANDLE_W / 2.0).round(),
        y: rect.y.round(),
        width: HANDLE_W,
        height: HANDLE_H,
    }
}

/// The modifier that picks up a tile from anywhere in a pane: Cmd on macOS, Ctrl+Shift
/// elsewhere.
fn pick_modifier(m: keyboard::Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        m.logo() && !m.control() && !m.alt()
    } else {
        m.control() && m.shift() && !m.alt() && !m.logo()
    }
}

fn is_escape(key: &keyboard::Key) -> bool {
    matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape))
}

fn grab_area(rect: Rectangle, axis: Axis) -> Rectangle {
    match axis {
        Axis::Row => Rectangle {
            x: rect.x - SLOP,
            width: rect.width + 2.0 * SLOP,
            ..rect
        },
        Axis::Column => Rectangle {
            y: rect.y - SLOP,
            height: rect.height + 2.0 * SLOP,
            ..rect
        },
    }
}

/// Position of `p` along `axis` and the start and length of `rect` on it.
fn along(axis: Axis, rect: Rectangle, p: Point) -> (f32, f32, f32) {
    match axis {
        Axis::Row => (p.x, rect.x, rect.width),
        Axis::Column => (p.y, rect.y, rect.height),
    }
}

/// The ratio that puts the divider's centre at `pos` (minus the grab offset).
fn ratio_at(axis: Axis, split: Rectangle, p: Point, grab: f32) -> f32 {
    let (pos, start, total) = along(axis, split, p);
    let room = (total - DIVIDER).max(1.0);
    (pos - grab - start - DIVIDER / 2.0) / room
}

/// Events that end a divider drag without its button release: the pointer leaving the window
/// or the window losing focus.
fn cancels_drag(event: &Event) -> bool {
    matches!(
        event,
        Event::Mouse(mouse::Event::CursorLeft) | Event::Window(window::Event::Unfocused)
    )
}

/// Whole logical pixels, so the panes' text sits on the pixel grid like a lone pane's.
fn snap(r: Rectangle) -> Rectangle {
    let (x, y) = (r.x.round(), r.y.round());
    Rectangle {
        x,
        y,
        width: ((r.x + r.width).round() - x).max(0.0),
        height: ((r.y + r.height).round() - y).max(0.0),
    }
}

/// A tab carried from the tab bar while these tiles are shown.
#[derive(Debug, Clone, PartialEq)]
pub struct Incoming {
    pub tab: TabId,
    /// It is another tab than the one shown, so it can be dropped here.
    pub mergeable: bool,
    /// Its layout, which lands whole.
    pub layout: Layout,
}

/// Lays `panes` (the views of `tab.visible_panes()`, in any order) out at their rectangles
/// of the tab's layout. `focused_outline`: nothing modal covers the tiles, so (when the tab
/// shows more than one pane) the focus outline is drawn and tiles can be picked up.
/// `incoming`: a tab carried over the window, which may be dropped on these tiles.
pub fn tiles<'a>(
    t: &'static Tokens,
    window: window::Id,
    tab: &'a Tab,
    panes: Vec<(PaneId, Element<'a, Message>)>,
    focused_outline: bool,
    incoming: Option<Incoming>,
    cell: Option<Size>,
) -> Element<'a, Message> {
    let (ids, children) = panes.into_iter().unzip();
    Element::new(Tiles {
        t,
        window,
        layout: tab.shown_layout(),
        focused: tab.focused,
        ids,
        children,
        outline: focused_outline,
        incoming,
        cell,
    })
}

struct Tiles<'a> {
    t: &'static Tokens,
    window: window::Id,
    layout: Layout,
    focused: PaneId,
    ids: Vec<PaneId>,
    children: Vec<Element<'a, Message>>,
    outline: bool,
    incoming: Option<Incoming>,
    /// The cell size the minimum pane size is measured in (the focused pane's).
    cell: Option<Size>,
}

struct Drag {
    path: Path,
    /// Pointer offset from the divider's centre when it was grabbed.
    grab: f32,
}

/// What happens to an event after the tile-drag logic saw it.
#[derive(Debug, PartialEq)]
enum Flow {
    /// Consumed: the panes do not see it.
    Consumed,
    /// On to divider handling and the panes.
    PassOn,
    /// The release of a modifier click that never travelled: the panes get the held-back
    /// press at this point, then the release.
    Replay(Point),
}

/// A tile picked up: by its handle, or with the pick modifier from inside its pane.
struct TileDrag {
    pane: PaneId,
    origin: Point,
    /// Picked by the handle; else with the pick modifier, the press held back from the pane's
    /// view until it is clear whether it is a drag or a click.
    by_handle: bool,
    /// The pointer travelled past `PICK_SLOP`: the tile is being carried.
    carrying: bool,
    target: Option<Drop>,
    /// Escape dropped it: the rest of this press (moves and the release) is swallowed.
    cancelled: bool,
}

#[derive(Default)]
struct State {
    drag: Option<Drag>,
    tile: Option<TileDrag>,
    /// The pane under the pointer (whose handle shows) and whether the pointer is on the handle.
    hover: Option<(PaneId, bool)>,
    modifiers: keyboard::Modifiers,
    /// The Escape that cancelled a tile drag: its release is not passed on either.
    swallow_escape: bool,
    /// The last press on a divider, for double-click.
    last_press: Option<(Path, Instant)>,
    /// Where a carried tab would land, while it is over these tiles.
    incoming: Option<Drop>,
    /// The carried tile last reported to the app (`DragMsg::Tile`).
    reported: Option<PaneId>,
    /// The area last reported to the app.
    area: Option<Size>,
}

impl Tiles<'_> {
    fn several(&self) -> bool {
        self.ids.len() > 1
    }

    fn publish(&self, shell: &mut Shell<'_, Message>, msg: TilesMsg) {
        shell.publish(Message::Tiles(self.window, msg));
    }

    /// Whether tiles can be picked up: more than one pane and nothing modal over them.
    fn movable(&self) -> bool {
        self.several() && self.outline
    }

    /// The pane under `p` and whether `p` is on its grab handle.
    fn hover_at(&self, bounds: Rectangle, p: Point) -> Option<(PaneId, bool)> {
        let (id, rect) = self
            .layout
            .rects(bounds)
            .into_iter()
            .find(|(_, r)| r.contains(p))?;
        Some((id, handle_rect(snap(rect)).contains(p)))
    }

    /// Handles the tile-drag side of `event`: what then happens to it.
    fn tile_event(
        &self,
        state: &mut State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
        shell: &mut Shell<'_, Message>,
    ) -> Flow {
        // A drag of a pane no longer shown (closed, or the tab is down to one pane) is over.
        if state
            .tile
            .as_ref()
            .is_some_and(|t| !self.movable() || !self.ids.contains(&t.pane))
        {
            state.tile = None;
            state.swallow_escape = false;
        }
        if !self.several() {
            state.hover = None;
            return Flow::PassOn;
        }
        match event {
            Event::Keyboard(keyboard::Event::KeyPressed { key, .. })
                if is_escape(key) && state.tile.as_ref().is_some_and(|t| !t.cancelled) =>
            {
                if let Some(tile) = &mut state.tile {
                    tile.cancelled = true;
                    tile.carrying = false;
                    tile.target = None;
                }
                state.swallow_escape = true;
                shell.request_redraw();
                shell.capture_event();
                Flow::Consumed
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.tile.is_some() => {
                let tile = state.tile.as_mut().expect("checked");
                if let (false, Some(p)) = (tile.cancelled, cursor.position()) {
                    if !tile.carrying && p.distance(tile.origin) > PICK_SLOP {
                        tile.carrying = true;
                        shell.request_redraw();
                    }
                    if tile.carrying {
                        // A drop that would leave the arrangement as it is is no target.
                        let target =
                            drop_target(&self.layout, bounds, p, Some(tile.pane)).filter(|at| {
                                preview(&self.layout, bounds, tile.pane, *at, self.cell).is_some()
                            });
                        if target != tile.target {
                            tile.target = target;
                            shell.request_redraw();
                        }
                    }
                }
                shell.capture_event();
                Flow::Consumed
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if state.tile.is_some() =>
            {
                let tile = state.tile.take().expect("checked");
                if let (true, false, Some(at)) = (tile.carrying, tile.cancelled, tile.target) {
                    self.publish(
                        shell,
                        TilesMsg::DropTile {
                            pane: tile.pane,
                            at,
                        },
                    );
                }
                state.hover = cursor
                    .position_over(bounds)
                    .and_then(|p| self.hover_at(bounds, p));
                shell.request_redraw();
                if !tile.by_handle && !tile.carrying && !tile.cancelled {
                    // A modifier click that never travelled: the pane's view gets it after
                    // all (a link opens, a program reading the mouse sees its click).
                    return Flow::Replay(tile.origin);
                }
                shell.capture_event();
                Flow::Consumed
            }
            e if (state.tile.is_some() || state.swallow_escape) && cancels_drag(e) => {
                state.tile = None;
                state.hover = None;
                state.swallow_escape = false;
                shell.request_redraw();
                Flow::PassOn
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let hover = cursor
                    .position_over(bounds)
                    .and_then(|p| self.hover_at(bounds, p));
                if hover != state.hover {
                    state.hover = hover;
                    shell.request_redraw();
                }
                Flow::PassOn
            }
            Event::Mouse(mouse::Event::CursorLeft) => {
                if state.hover.take().is_some() {
                    shell.request_redraw();
                }
                Flow::PassOn
            }
            _ => Flow::PassOn,
        }
    }

    /// A tab carried from the tab bar: the preview follows the pointer over the tiles, a
    /// release drops it, Escape (or losing the pointer or focus) ends it. The panes see none
    /// of it. Returns whether the event was consumed.
    fn incoming_event(
        &self,
        state: &mut State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
        shell: &mut Shell<'_, Message>,
    ) -> bool {
        let Some(incoming) = self.incoming.as_ref() else {
            if state.incoming.take().is_some() {
                shell.request_redraw();
            }
            return false;
        };
        state.tile = None;
        // A target where a pane would end up below the minimum size is no target.
        let target = |p: Option<Point>| {
            p.filter(|_| incoming.mergeable)
                .and_then(|p| drop_target(&self.layout, bounds, p, None))
                .filter(|at| {
                    incoming_preview(&self.layout, bounds, &incoming.layout, *at, self.cell)
                        .is_some()
                })
        };
        let end = |state: &mut State, shell: &mut Shell<'_, Message>| {
            state.incoming = None;
            shell.publish(Message::Drag(self.window, DragMsg::End));
            shell.request_redraw();
        };
        match event {
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let at = target(cursor.position());
                if at != state.incoming {
                    state.incoming = at;
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                // Where the pointer is now: a hover may have switched the tab shown.
                state.incoming = None;
                if let Some(at) = target(cursor.position()) {
                    shell.publish(Message::Drag(
                        self.window,
                        DragMsg::DropTab {
                            tab: incoming.tab,
                            at,
                        },
                    ));
                }
                shell.request_redraw();
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) if is_escape(key) => {
                state.swallow_escape = true;
                end(state, shell);
            }
            e if cancels_drag(e) => {
                end(state, shell);
                return false;
            }
            _ => return false,
        }
        shell.capture_event();
        true
    }

    /// Everything `Widget::update` does but reporting a carried tile.
    #[allow(clippy::too_many_arguments)]
    fn update_inner(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Placed<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<State>();
        if state.area != Some(bounds.size()) {
            state.area = Some(bounds.size());
            self.publish(shell, TilesMsg::Area(bounds.size()));
        }
        if let Event::Keyboard(keyboard::Event::ModifiersChanged(m)) = event {
            state.modifiers = *m;
        }
        // The Escape that cancelled a drag: its release is not passed on either.
        if let Event::Keyboard(keyboard::Event::KeyReleased { key, .. }) = event
            && is_escape(key)
            && state.swallow_escape
        {
            state.swallow_escape = false;
            shell.capture_event();
            return;
        }
        if self.incoming_event(state, event, bounds, cursor, shell) {
            return;
        }
        match self.tile_event(state, event, bounds, cursor, shell) {
            Flow::Consumed => return,
            Flow::PassOn => {}
            Flow::Replay(origin) => {
                let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
                self.update_children(
                    tree,
                    &press,
                    layout,
                    mouse::Cursor::Available(origin),
                    renderer,
                    clipboard,
                    shell,
                    viewport,
                );
                self.update_children(
                    tree, event, layout, cursor, renderer, clipboard, shell, viewport,
                );
                return;
            }
        }
        if self.several() {
            match event {
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                    if let Some(p) = cursor.position_over(bounds) {
                        match hit(&self.layout, bounds, p) {
                            Hit::Divider(path) => {
                                let now = Instant::now();
                                let double = state.last_press.as_ref().is_some_and(|(q, at)| {
                                    *q == path && now.duration_since(*at) < DOUBLE_CLICK
                                });
                                if double {
                                    state.last_press = None;
                                    self.publish(shell, TilesMsg::Equalize(path));
                                } else if let Some((axis, split)) =
                                    self.layout.split_area(bounds, &path)
                                {
                                    let centre = self
                                        .layout
                                        .dividers(bounds)
                                        .into_iter()
                                        .find(|d| d.path == path)
                                        .map(|d| d.rect.center());
                                    let grab = centre.map_or(0.0, |c| {
                                        along(axis, split, p).0 - along(axis, split, c).0
                                    });
                                    state.last_press = Some((path.clone(), now));
                                    state.drag = Some(Drag { path, grab });
                                }
                                shell.capture_event();
                                return;
                            }
                            Hit::Pane(id) => {
                                if id != self.focused {
                                    // Not captured: the pane's terminal still gets the press.
                                    self.publish(shell, TilesMsg::Focus(id));
                                }
                                let on_handle = self.movable()
                                    && state.drag.is_none()
                                    && self.hover_at(bounds, p) == Some((id, true));
                                if on_handle {
                                    state.tile = Some(TileDrag {
                                        pane: id,
                                        origin: p,
                                        by_handle: true,
                                        carrying: false,
                                        target: None,
                                        cancelled: false,
                                    });
                                    shell.request_redraw();
                                    shell.capture_event();
                                    return;
                                }
                                if self.movable() && pick_modifier(state.modifiers) {
                                    // Held back from the pane's view: travelling past the slop
                                    // carries the tile, a click without travel is replayed to
                                    // the view on release (links, programs reading the mouse).
                                    state.tile = Some(TileDrag {
                                        pane: id,
                                        origin: p,
                                        by_handle: false,
                                        carrying: false,
                                        target: None,
                                        cancelled: false,
                                    });
                                    shell.capture_event();
                                    return;
                                }
                            }
                            Hit::None => {}
                        }
                    }
                }
                Event::Mouse(mouse::Event::CursorMoved { .. }) if state.drag.is_some() => {
                    if let (Some(drag), Some(p)) = (&state.drag, cursor.position())
                        && let Some((axis, split)) = self.layout.split_area(bounds, &drag.path)
                    {
                        let ratio = ratio_at(axis, split, p, drag.grab);
                        self.publish(
                            shell,
                            TilesMsg::DragDivider {
                                path: drag.path.clone(),
                                ratio,
                            },
                        );
                    }
                    shell.capture_event();
                    return;
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                    if state.drag.is_some() =>
                {
                    state.drag = None;
                    self.publish(shell, TilesMsg::EndDrag);
                    shell.capture_event();
                    return;
                }
                // The release can go missing (the pointer left the window, or the window lost
                // focus mid-drag): end the drag so the resizes flush. Not captured: the panes
                // see these events too.
                e if state.drag.is_some() && cancels_drag(e) => {
                    state.drag = None;
                    self.publish(shell, TilesMsg::EndDrag);
                }
                _ => {}
            }
        }
        self.update_children(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }
    /// Passes `event` on to the panes' views (the arguments of `Widget::update`).
    #[allow(clippy::too_many_arguments)]
    fn update_children(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Placed<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        for ((child, state), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }
    }
}

impl Tiles<'_> {
    /// Where a tab carried over the tiles would land.
    fn draw_incoming(&self, tree: &Tree, renderer: &mut iced::Renderer, bounds: Rectangle) {
        use iced::advanced::Renderer as _;
        let state = tree.state.downcast_ref::<State>();
        let Some(r) = self.incoming.as_ref().and_then(|incoming| {
            let at = state.incoming?;
            incoming_preview(&self.layout, bounds, &incoming.layout, at, self.cell)
        }) else {
            return;
        };
        renderer.with_layer(bounds.expand(1.0), |renderer| {
            self.draw_preview(renderer, r);
        });
    }

    /// The violet preview of where a dropped tile or tab lands.
    fn draw_preview(&self, renderer: &mut iced::Renderer, r: Rectangle) {
        use iced::advanced::Renderer as _;
        renderer.fill_quad(
            renderer::Quad {
                bounds: r,
                border: Border {
                    color: self.t.accent,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                snap: true,
                ..renderer::Quad::default()
            },
            self.t.accent_soft,
        );
    }

    fn draw_handle(&self, renderer: &mut iced::Renderer, r: Rectangle, hot: bool) {
        use iced::advanced::Renderer as _;
        use iced::advanced::text::{self, Renderer as _};
        renderer.fill_quad(
            renderer::Quad {
                bounds: r,
                border: Border {
                    color: self.t.border,
                    width: 1.0,
                    radius: 4.0.into(),
                },
                snap: true,
                ..renderer::Quad::default()
            },
            self.t.raised,
        );
        renderer.fill_text(
            text::Text {
                content: ICON_GRIP.to_string(),
                bounds: Size::new(GRIP, GRIP),
                size: Pixels(GRIP),
                line_height: text::LineHeight::Absolute(Pixels(GRIP)),
                font: ICONS,
                align_x: text::Alignment::Center,
                align_y: iced::alignment::Vertical::Center,
                shaping: text::Shaping::Basic,
                wrapping: text::Wrapping::None,
            },
            r.center(),
            if hot {
                self.t.text
            } else {
                self.t.text_secondary
            },
            r,
        );
    }
}

impl Widget<Message, iced::Theme, iced::Renderer> for Tiles<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.max();
        let rects = self.layout.rects(Rectangle::with_size(size));
        let several = self.several();
        let nodes = self
            .ids
            .iter()
            .zip(&mut self.children)
            .zip(&mut tree.children)
            .map(|((id, child), tree)| {
                let r = rects
                    .iter()
                    .find(|(p, _)| p == id)
                    .map_or(Rectangle::default(), |(_, r)| *r);
                let r = if several { snap(r) } else { r };
                child
                    .as_widget_mut()
                    .layout(tree, renderer, &layout::Limits::new(Size::ZERO, r.size()))
                    .move_to(r.position())
            })
            .collect();
        layout::Node::with_children(size, nodes)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Placed<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for ((child, state), layout) in self
                .children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child
                    .as_widget_mut()
                    .operate(state, layout, renderer, operation);
            }
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Placed<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.update_inner(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
        // Tell the app when a tile starts or stops being carried: the tab bar takes a tile
        // dropped on it.
        let state = tree.state.downcast_mut::<State>();
        let carried = state
            .tile
            .as_ref()
            .filter(|t| t.carrying && !t.cancelled)
            .map(|t| t.pane);
        if carried != state.reported {
            state.reported = carried;
            shell.publish(Message::Drag(self.window, DragMsg::Tile(carried)));
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Placed<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let bounds = layout.bounds();
        if self.several() {
            let state = tree.state.downcast_ref::<State>();
            if state.tile.as_ref().is_some_and(|t| t.carrying) {
                return mouse::Interaction::Grabbing;
            }
            if state.drag.is_none()
                && self.movable()
                && matches!(state.hover, Some((_, true)))
                && cursor.is_over(bounds)
            {
                return mouse::Interaction::Grab;
            }
            let path = match (&state.drag, cursor.position_over(bounds)) {
                (Some(drag), _) => Some(drag.path.clone()),
                (None, Some(p)) => match hit(&self.layout, bounds, p) {
                    Hit::Divider(path) => Some(path),
                    _ => None,
                },
                (None, None) => None,
            };
            if let Some((axis, _)) = path.and_then(|p| self.layout.split_area(bounds, &p)) {
                return match axis {
                    Axis::Row => mouse::Interaction::ResizingHorizontally,
                    Axis::Column => mouse::Interaction::ResizingVertically,
                };
            }
        }
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Placed<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        for ((child, state), layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
        {
            child
                .as_widget()
                .draw(state, renderer, theme, style, layout, cursor, viewport);
        }
        let bounds = layout.bounds();
        if !self.several() {
            self.draw_incoming(tree, renderer, bounds);
            return;
        }
        let fill = |renderer: &mut iced::Renderer, bounds: Rectangle, color: Color| {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    snap: true,
                    ..renderer::Quad::default()
                },
                color,
            );
        };
        // Over the panes' text, so in a layer of its own; one pixel wider for the outline.
        renderer.with_layer(bounds.expand(1.0), |renderer| {
            let dim = self.t.terminal.scale_alpha(DIM);
            let mut focused = None;
            for (id, placed) in self.ids.iter().zip(layout.children()) {
                if *id == self.focused {
                    focused = Some(placed.bounds());
                } else {
                    fill(renderer, placed.bounds(), dim);
                }
            }
            for d in self.layout.dividers(bounds) {
                let c = d.rect.center();
                let line = match d.axis {
                    Axis::Row => Rectangle {
                        x: (c.x - 0.5).floor(),
                        width: 1.0,
                        ..d.rect
                    },
                    Axis::Column => Rectangle {
                        y: (c.y - 0.5).floor(),
                        height: 1.0,
                        ..d.rect
                    },
                };
                fill(renderer, line, self.t.border);
            }
            // The outline sits just outside the focused pane: in the divider gap or the
            // window padding, never over its text.
            if self.outline
                && let Some(f) = focused
            {
                let o = f.expand(1.0);
                let accent = self.t.accent;
                fill(renderer, Rectangle { height: 1.0, ..o }, accent);
                fill(
                    renderer,
                    Rectangle {
                        y: o.y + o.height - 1.0,
                        height: 1.0,
                        ..o
                    },
                    accent,
                );
                fill(renderer, Rectangle { width: 1.0, ..o }, accent);
                fill(
                    renderer,
                    Rectangle {
                        x: o.x + o.width - 1.0,
                        width: 1.0,
                        ..o
                    },
                    accent,
                );
            }
            let state = tree.state.downcast_ref::<State>();
            match &state.tile {
                Some(TileDrag {
                    pane,
                    carrying: true,
                    target: Some(at),
                    ..
                }) => {
                    if let Some(r) = preview(&self.layout, bounds, *pane, *at, self.cell) {
                        self.draw_preview(renderer, r);
                    }
                }
                None if state.drag.is_none() && self.movable() => {
                    if let Some((id, hot)) = state.hover
                        && let Some((_, rect)) = layout
                            .children()
                            .zip(&self.ids)
                            .map(|(placed, id)| (*id, placed.bounds()))
                            .find(|(p, _)| *p == id)
                    {
                        self.draw_handle(renderer, handle_rect(rect), hot);
                    }
                }
                _ => {}
            }
        });
        self.draw_incoming(tree, renderer, bounds);
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Placed<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, iced::Theme, iced::Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Side;

    fn area() -> Rectangle {
        Rectangle {
            x: 8.0,
            y: 4.0,
            width: 1000.0,
            height: 600.0,
        }
    }

    fn two() -> Layout {
        let mut l = Layout::Pane(PaneId(1));
        l.split(PaneId(1), Side::Right, PaneId(2));
        l
    }

    #[test]
    fn hit_divider_wins_within_hit_area() {
        let l = two();
        // The divider spans x 506..510 (8 + 498); its grab area reaches 2 px into each pane.
        for x in [504.0, 505.5, 506.0, 508.0, 509.9, 511.5] {
            assert_eq!(
                hit(&l, area(), Point::new(x, 300.0)),
                Hit::Divider(vec![]),
                "{x}"
            );
        }
        let mut stacked = Layout::Pane(PaneId(1));
        stacked.split(PaneId(1), Side::Bottom, PaneId(2));
        // 4 + 298 = 302: the divider spans y 302..306.
        assert_eq!(
            hit(&stacked, area(), Point::new(100.0, 301.0)),
            Hit::Divider(vec![])
        );
    }

    #[test]
    fn hit_pane_elsewhere() {
        let l = two();
        assert_eq!(
            hit(&l, area(), Point::new(100.0, 100.0)),
            Hit::Pane(PaneId(1))
        );
        assert_eq!(
            hit(&l, area(), Point::new(503.0, 100.0)),
            Hit::Pane(PaneId(1))
        );
        assert_eq!(
            hit(&l, area(), Point::new(513.0, 100.0)),
            Hit::Pane(PaneId(2))
        );
        assert_eq!(
            hit(&l, area(), Point::new(900.0, 590.0)),
            Hit::Pane(PaneId(2))
        );
        assert_eq!(hit(&l, area(), Point::new(2.0, 100.0)), Hit::None);
        let single = Layout::Pane(PaneId(7));
        assert_eq!(
            hit(&single, area(), Point::new(500.0, 300.0)),
            Hit::Pane(PaneId(7))
        );
    }

    #[test]
    fn dragging_puts_the_divider_under_the_pointer() {
        let split = area();
        // Pointer at the divider's current centre (508) with no grab offset: ratio stays 0.5.
        let r = ratio_at(Axis::Row, split, Point::new(508.0, 0.0), 0.0);
        assert!((r - 0.5).abs() < 1e-4, "{r}");
        let r = ratio_at(Axis::Row, split, Point::new(260.0, 0.0), 0.0);
        let mut l = two();
        l.set_ratio(&vec![], r);
        let d = &l.dividers(split)[0];
        assert!((d.rect.center().x - 260.0).abs() < 0.01);
    }

    #[test]
    fn drop_target_outer_edge() {
        let l = two();
        let at = |x, y, dragged| drop_target(&l, area(), Point::new(x, y), dragged);
        // The content spans x 8..1008 and y 4..604; the band is 24 px deep.
        assert_eq!(
            at(10.0, 300.0, Some(PaneId(2))),
            Some(Drop::Edge(Side::Left))
        );
        assert_eq!(
            at(1000.0, 300.0, Some(PaneId(1))),
            Some(Drop::Edge(Side::Right))
        );
        assert_eq!(
            at(300.0, 10.0, Some(PaneId(2))),
            Some(Drop::Edge(Side::Top))
        );
        assert_eq!(
            at(700.0, 600.0, Some(PaneId(1))),
            Some(Drop::Edge(Side::Bottom))
        );
        // Over the dragged tile itself the band still counts.
        assert_eq!(
            at(20.0, 300.0, Some(PaneId(1))),
            Some(Drop::Edge(Side::Left))
        );
        // A corner goes to the nearer edge.
        assert_eq!(at(12.0, 6.0, Some(PaneId(2))), Some(Drop::Edge(Side::Top)));
        // Just past the band: the tile under the pointer.
        assert_eq!(
            at(33.0, 300.0, Some(PaneId(2))),
            Some(Drop::Tile(PaneId(1), Side::Left))
        );
        // Outside the content: nothing.
        assert_eq!(at(2.0, 300.0, Some(PaneId(2))), None);
    }

    #[test]
    fn drop_target_tile_edge_nearest_side() {
        let l = two();
        let at = |x, y, dragged| drop_target(&l, area(), Point::new(x, y), dragged);
        // Pane 2 spans x 510..1008, y 4..604.
        let tile = |side| Some(Drop::Tile(PaneId(2), side));
        assert_eq!(at(540.0, 300.0, Some(PaneId(1))), tile(Side::Left));
        assert_eq!(at(900.0, 300.0, Some(PaneId(1))), tile(Side::Right));
        assert_eq!(at(760.0, 60.0, Some(PaneId(1))), tile(Side::Top));
        assert_eq!(at(760.0, 560.0, Some(PaneId(1))), tile(Side::Bottom));
        // A whole-tab drag (nothing of this layout dragged) may target any tile.
        assert_eq!(
            at(100.0, 300.0, None),
            Some(Drop::Tile(PaneId(1), Side::Left))
        );
    }

    #[test]
    fn drop_on_itself_is_none() {
        let l = two();
        assert_eq!(
            drop_target(&l, area(), Point::new(250.0, 300.0), Some(PaneId(1))),
            None
        );
        // The divider gap is no tile.
        assert_eq!(
            drop_target(&l, area(), Point::new(508.0, 300.0), Some(PaneId(1))),
            None
        );
    }

    #[test]
    fn preview_is_the_rect_the_tile_would_get() {
        let l = two();
        // Pane 1 dropped along the bottom: a full-width row under pane 2.
        let r = preview(&l, area(), PaneId(1), Drop::Edge(Side::Bottom), None).unwrap();
        assert_eq!((r.x, r.width), (8.0, 1000.0));
        assert_eq!(r.y + r.height, 604.0);
        assert!((r.height - 298.0).abs() <= 1.0, "{r:?}");
        // Back where it is (beside the other pane on the same side): no preview.
        assert_eq!(
            preview(
                &l,
                area(),
                PaneId(1),
                Drop::Tile(PaneId(2), Side::Left),
                None
            ),
            None
        );
        // On its own tile: no move, no preview.
        assert_eq!(
            preview(
                &l,
                area(),
                PaneId(1),
                Drop::Tile(PaneId(1), Side::Left),
                None
            ),
            None
        );
    }

    #[test]
    fn no_preview_where_a_pane_would_not_fit() {
        let l = two();
        // 30 px cells: a pane needs 600 px, so a full-width row fits and a half does not.
        let wide = Some(Size::new(30.0, 20.0));
        assert!(preview(&l, area(), PaneId(1), Drop::Edge(Side::Bottom), wide).is_some());
        assert_eq!(
            preview(
                &l,
                area(),
                PaneId(1),
                Drop::Tile(PaneId(2), Side::Right),
                wide
            ),
            None
        );
        // 20 px cells: a pane needs 400 px.
        let cell = Some(Size::new(20.0, 20.0));
        // A carried tab of two panes side by side: along the bottom it fits, beside a tile not.
        let tab = Layout::Split {
            axis: Axis::Row,
            ratio: 0.5,
            first: Box::new(Layout::Pane(PaneId(7))),
            second: Box::new(Layout::Pane(PaneId(8))),
        };
        let r = incoming_preview(&l, area(), &tab, Drop::Edge(Side::Bottom), cell).unwrap();
        assert_eq!((r.x, r.width), (8.0, 1000.0), "the whole subtree's room");
        assert_eq!(
            incoming_preview(&l, area(), &tab, Drop::Tile(PaneId(2), Side::Right), cell),
            None
        );
        assert!(
            incoming_preview(&l, area(), &tab, Drop::Tile(PaneId(2), Side::Right), None).is_some()
        );
    }

    #[test]
    fn handle_sits_centred_on_the_top_edge() {
        let pane = Rectangle {
            x: 510.0,
            y: 4.0,
            width: 498.0,
            height: 600.0,
        };
        let h = handle_rect(pane);
        assert_eq!((h.y, h.width, h.height), (4.0, HANDLE_W, HANDLE_H));
        assert!((h.center_x() - pane.center_x()).abs() <= 0.5);
    }

    #[test]
    fn a_drag_ends_when_the_pointer_or_focus_is_lost() {
        assert!(cancels_drag(&Event::Mouse(mouse::Event::CursorLeft)));
        assert!(cancels_drag(&Event::Window(window::Event::Unfocused)));
        assert!(!cancels_drag(&Event::Window(window::Event::Focused)));
        assert!(!cancels_drag(&Event::Mouse(mouse::Event::CursorEntered)));
        assert!(!cancels_drag(&Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left
        ))));
    }

    /// The real press path: `Tiles::update` over two terminal views, headless.
    mod press_path {
        use std::sync::Arc;

        use fbterm_term::{SharedTerminal, Terminal, TerminalOutput, TerminalStyle};
        use iced::advanced::clipboard;
        use iced::keyboard::key::{Code, Named, Physical};
        use parking_lot::Mutex;

        use super::*;

        const SIZE: Size = Size::new(1000.0, 600.0);

        fn pick_mods() -> keyboard::Modifiers {
            if cfg!(target_os = "macos") {
                keyboard::Modifiers::LOGO
            } else {
                keyboard::Modifiers::CTRL | keyboard::Modifiers::SHIFT
            }
        }

        fn mods(m: keyboard::Modifiers) -> Event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(m))
        }

        fn press() -> Event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        }

        fn release() -> Event {
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
        }

        fn moved(x: f32, y: f32) -> Event {
            Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(x, y),
            })
        }

        fn escape(pressed: bool) -> Event {
            let key = keyboard::Key::Named(Named::Escape);
            let physical_key = Physical::Code(Code::Escape);
            Event::Keyboard(if pressed {
                keyboard::Event::KeyPressed {
                    key: key.clone(),
                    modified_key: key,
                    physical_key,
                    location: keyboard::Location::Standard,
                    modifiers: keyboard::Modifiers::empty(),
                    text: None,
                    repeat: false,
                }
            } else {
                keyboard::Event::KeyReleased {
                    key: key.clone(),
                    modified_key: key,
                    physical_key,
                    location: keyboard::Location::Standard,
                    modifiers: keyboard::Modifiers::empty(),
                }
            })
        }

        fn term() -> SharedTerminal {
            Arc::new(Mutex::new(Terminal::new(crate::app::DEFAULT_GRID, 100)))
        }

        /// What one event did: the messages published and whether it was captured.
        struct Step {
            messages: Vec<Message>,
            captured: bool,
        }

        /// Runs `steps` (an event and the pointer position) through tiles of two panes side by
        /// side (pane 1 over x 0..498, pane 2 over x 502..1000) showing `terms`. `setup` runs
        /// after a first event has sized the terminals to their panes.
        fn drive(
            terms: &[SharedTerminal; 2],
            setup: impl FnOnce(),
            steps: &[(Event, Point)],
        ) -> Vec<Step> {
            drive_with(terms, None, setup, steps)
        }

        /// `drive`, with a tab carried over the tiles.
        fn drive_with(
            terms: &[SharedTerminal; 2],
            incoming: Option<Incoming>,
            setup: impl FnOnce(),
            steps: &[(Event, Point)],
        ) -> Vec<Step> {
            let style: TerminalStyle =
                crate::settings_window::terminal_style(&fbterm_config::Settings::default());
            let wid = window::Id::unique();
            let ids = vec![PaneId(1), PaneId(2)];
            let children = ids
                .iter()
                .zip(terms)
                .map(|(id, t)| {
                    let id = *id;
                    fbterm_term::terminal_view(t, &style, move |o| Message::Terminal(wid, id, o))
                })
                .collect();
            let mut element: Element<'_, Message> = Element::new(Tiles {
                t: &crate::theme::DARK,
                window: wid,
                layout: two(),
                focused: PaneId(1),
                ids,
                children,
                outline: true,
                incoming,
                cell: None,
            });
            let renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
                iced::Font::DEFAULT,
                Pixels(16.0),
            ));
            let mut tree = Tree::new(&element);
            let node = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, SIZE),
            );
            let viewport = Rectangle::with_size(SIZE);
            let mut run = |event: &Event, at: Point| {
                let mut messages = Vec::new();
                let mut shell = Shell::new(&mut messages);
                element.as_widget_mut().update(
                    &mut tree,
                    event,
                    Placed::new(&node),
                    mouse::Cursor::Available(at),
                    &renderer,
                    &mut clipboard::Null,
                    &mut shell,
                    &viewport,
                );
                let captured = shell.is_event_captured();
                Step { messages, captured }
            };
            run(
                &Event::Mouse(mouse::Event::CursorEntered),
                Point::new(-10.0, -10.0),
            );
            setup();
            steps.iter().map(|(e, at)| run(e, *at)).collect()
        }

        fn drops(steps: &[Step]) -> Vec<(PaneId, Drop)> {
            steps
                .iter()
                .flat_map(|s| &s.messages)
                .filter_map(|m| match m {
                    Message::Tiles(_, TilesMsg::DropTile { pane, at }) => Some((*pane, *at)),
                    _ => None,
                })
                .collect()
        }

        fn urls(steps: &[Step]) -> Vec<String> {
            steps
                .iter()
                .flat_map(|s| &s.messages)
                .filter_map(|m| match m {
                    Message::Terminal(_, _, TerminalOutput::OpenUrl(u)) => Some(u.clone()),
                    _ => None,
                })
                .collect()
        }

        fn resized(messages: &[Message], pane: PaneId) -> Option<fbterm_term::GridSize> {
            messages.iter().rev().find_map(|m| match m {
                Message::Terminal(_, p, TerminalOutput::Resized(g)) if *p == pane => Some(*g),
                _ => None,
            })
        }

        /// After a drop the tiles lay the moved pane out at its new rectangle, and its view
        /// reports the new grid (which the app hands to the session through `pane_resized`).
        #[test]
        fn a_moved_tile_reports_its_new_grid() {
            let terms = [term(), term()];
            let style: TerminalStyle =
                crate::settings_window::terminal_style(&fbterm_config::Settings::default());
            let wid = window::Id::unique();
            let renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
                iced::Font::DEFAULT,
                Pixels(16.0),
            ));
            let tiles = |layout: Layout| -> Element<'_, Message> {
                let ids = vec![PaneId(1), PaneId(2)];
                let children = ids
                    .iter()
                    .zip(&terms)
                    .map(|(id, t)| {
                        let id = *id;
                        fbterm_term::terminal_view(t, &style, move |o| {
                            Message::Terminal(wid, id, o)
                        })
                    })
                    .collect();
                Element::new(Tiles {
                    t: &crate::theme::DARK,
                    window: wid,
                    layout,
                    focused: PaneId(1),
                    ids,
                    children,
                    outline: true,
                    incoming: None,
                    cell: None,
                })
            };
            let viewport = Rectangle::with_size(SIZE);
            let mut tree = None::<Tree>;
            let mut show = |layout: Layout| {
                let mut element = tiles(layout);
                let tree = match &mut tree {
                    Some(t) => {
                        t.diff(&element);
                        t
                    }
                    None => tree.insert(Tree::new(&element)),
                };
                let node = element.as_widget_mut().layout(
                    tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, SIZE),
                );
                let mut messages = Vec::new();
                let mut shell = Shell::new(&mut messages);
                element.as_widget_mut().update(
                    tree,
                    &moved(-10.0, -10.0),
                    Placed::new(&node),
                    mouse::Cursor::Unavailable,
                    &renderer,
                    &mut clipboard::Null,
                    &mut shell,
                    &viewport,
                );
                messages
            };
            let before = resized(&show(two()), PaneId(1)).expect("sized to its pane");
            let mut after_layout = two();
            assert!(after_layout.move_pane(PaneId(1), Drop::Edge(Side::Bottom)));
            let after = resized(&show(after_layout), PaneId(1)).expect("resized after the move");
            // From the left half to the bottom row: about twice as wide, about half as tall.
            assert!(
                after.cols > before.cols && after.rows < before.rows,
                "{before:?} -> {after:?}"
            );
            assert_eq!(
                terms[0].lock().size(),
                after,
                "the terminal took the new grid"
            );
        }

        #[test]
        fn modifier_drag_over_a_shell_moves_the_tile() {
            let terms = [term(), term()];
            terms[0].lock().feed(b"$ ls\r\nsome output\r\n$ ");
            let (from, to) = (Point::new(100.0, 300.0), Point::new(900.0, 300.0));
            let steps = drive(
                &terms,
                || {},
                &[
                    (mods(pick_mods()), from),
                    (moved(from.x, from.y), from),
                    (press(), from),
                    (moved(102.0, 300.0), Point::new(102.0, 300.0)),
                    (moved(to.x, to.y), to),
                    (release(), to),
                ],
            );
            assert!(
                steps[2].captured,
                "the press is held back from the terminal"
            );
            assert_eq!(
                drops(&steps),
                vec![(PaneId(1), Drop::Tile(PaneId(2), Side::Right))]
            );
            assert!(
                terms[0].lock().selection().is_none(),
                "no text was selected"
            );
        }

        #[test]
        fn a_carried_tab_drops_on_the_tiles() {
            let terms = [term(), term()];
            let tab = TabId::next();
            let carried = |mergeable| {
                Some(Incoming {
                    tab,
                    mergeable,
                    layout: Layout::Pane(PaneId(9)),
                })
            };
            let steps = [
                (moved(700.0, 300.0), Point::new(700.0, 300.0)),
                (moved(560.0, 300.0), Point::new(560.0, 300.0)),
                (release(), Point::new(560.0, 300.0)),
            ];
            let out = drive_with(&terms, carried(true), || {}, &steps);
            assert!(out.iter().all(|s| s.captured), "the panes see none of it");
            let dropped: Vec<_> = out
                .iter()
                .flat_map(|s| &s.messages)
                .filter_map(|m| match m {
                    Message::Drag(_, DragMsg::DropTab { tab: t, at }) => Some((*t, *at)),
                    _ => None,
                })
                .collect();
            assert_eq!(dropped, [(tab, Drop::Tile(PaneId(2), Side::Left))]);

            // Over the tab's own content: nothing to drop on.
            let out = drive_with(&terms, carried(false), || {}, &steps);
            assert!(
                out.iter()
                    .flat_map(|s| &s.messages)
                    .all(|m| !matches!(m, Message::Drag(_, DragMsg::DropTab { .. })))
            );

            // Escape ends it, and neither the press nor the release reaches the terminal.
            let out = drive_with(
                &terms,
                carried(true),
                || {},
                &[
                    (escape(true), Point::new(700.0, 300.0)),
                    (escape(false), Point::new(700.0, 300.0)),
                ],
            );
            assert!(out.iter().all(|s| s.captured));
            assert!(
                out[0]
                    .messages
                    .iter()
                    .any(|m| matches!(m, Message::Drag(_, DragMsg::End)))
            );
        }

        #[test]
        fn a_carried_tile_is_reported_until_dropped() {
            let terms = [term(), term()];
            let carried = |steps: &[Step]| -> Vec<Option<PaneId>> {
                steps
                    .iter()
                    .flat_map(|s| &s.messages)
                    .filter_map(|m| match m {
                        Message::Drag(_, DragMsg::Tile(p)) => Some(*p),
                        _ => None,
                    })
                    .collect()
            };
            // Carried out over the tab bar (above the tiles) and released there: no drop here.
            let out = drive(
                &terms,
                || {},
                &[
                    (mods(pick_mods()), Point::new(200.0, 300.0)),
                    (press(), Point::new(200.0, 300.0)),
                    (moved(200.0, 100.0), Point::new(200.0, 100.0)),
                    (moved(200.0, -10.0), Point::new(200.0, -10.0)),
                    (release(), Point::new(200.0, -10.0)),
                ],
            );
            assert_eq!(carried(&out), [Some(PaneId(1)), None]);
            assert!(drops(&out).is_empty());
        }

        #[test]
        fn modifier_click_without_travel_reaches_the_terminal() {
            let terms = [term(), term()];
            let at = Point::new(3.0, 3.0);
            let near = Point::new(5.0, 4.0);
            let steps = drive(
                &terms,
                || {
                    terms[0]
                        .lock()
                        .feed(b"\x1b]8;;https://example.com/\x1b\\link\x1b]8;;\x1b\\ text");
                },
                &[
                    (mods(pick_mods()), at),
                    (press(), at),
                    // Within the slop: still a click.
                    (moved(near.x, near.y), near),
                    (release(), near),
                ],
            );
            assert!(drops(&steps).is_empty());
            assert_eq!(urls(&steps), vec!["https://example.com/".to_string()]);
        }

        #[test]
        fn plain_drag_still_selects() {
            let terms = [term(), term()];
            let (from, to) = (Point::new(20.0, 10.0), Point::new(300.0, 60.0));
            let steps = drive(
                &terms,
                || {
                    terms[0]
                        .lock()
                        .feed(b"first line of text\r\nsecond line of text")
                },
                &[(press(), from), (moved(to.x, to.y), to), (release(), to)],
            );
            assert!(drops(&steps).is_empty());
            assert!(
                terms[0].lock().selection().is_some(),
                "the drag selected text"
            );
        }

        #[test]
        fn escape_cancels_a_tile_drag_and_never_reaches_the_terminal() {
            let terms = [term(), term()];
            let (from, to) = (Point::new(100.0, 300.0), Point::new(900.0, 300.0));
            let steps = drive(
                &terms,
                || {},
                &[
                    (mods(pick_mods()), from),
                    (press(), from),
                    (moved(to.x, to.y), to),
                    (escape(true), to),
                    (escape(false), to),
                    (moved(800.0, 300.0), Point::new(800.0, 300.0)),
                    (release(), Point::new(800.0, 300.0)),
                ],
            );
            assert!(drops(&steps).is_empty(), "nothing moved");
            for i in [3, 4] {
                assert!(steps[i].captured, "Escape step {i} was swallowed");
                assert!(
                    !steps[i]
                        .messages
                        .iter()
                        .any(|m| matches!(m, Message::Terminal(_, _, TerminalOutput::Input(_)))),
                    "no Escape bytes for the terminal"
                );
            }
            assert!(terms[0].lock().selection().is_none());
        }

        #[test]
        fn handle_drag_moves_the_tile_and_a_handle_click_does_not() {
            let terms = [term(), term()];
            // Pane 1's handle: centred on x 249, top 0..14.
            let grip = Point::new(249.0, 6.0);
            let below = Point::new(249.0, 580.0);
            let steps = drive(
                &terms,
                || {},
                &[
                    (moved(grip.x, grip.y), grip),
                    (press(), grip),
                    (release(), grip),
                    (press(), grip),
                    (moved(below.x, below.y), below),
                    (release(), below),
                ],
            );
            assert!(steps[1].captured && steps[2].captured);
            assert_eq!(drops(&steps), vec![(PaneId(1), Drop::Edge(Side::Bottom))]);
        }
    }
}
