//! The panes of one tab on screen: each pane's view laid out at its rectangle of the layout
//! tree, the dividers between them, and (with more than one pane) the focus outline and the
//! dimming of the other panes. Also turns presses into pane focus and divider drags.
//!
//! A tab with one pane draws exactly its pane's view: no divider, outline or dimming.

use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout as Placed};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::{Color, Element, Event, Length, Point, Rectangle, Size, Vector, mouse, window};

use crate::app::Message;
use crate::layout::{Axis, DIVIDER, Layout, PaneId, Path};
use crate::tabs::Tab;
use crate::theme::Tokens;

/// Extra grab room on each side of a divider, over the panes next to it.
const SLOP: f32 = 2.0;
/// Two presses on a divider within this time equalize its split.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Alpha of the terminal background laid over the panes that do not have focus.
const DIM: f32 = 0.35;

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

/// Lays `panes` (the views of `tab.visible_panes()`, in that order) out at their rectangles
/// of the tab's layout. `focused_outline`: draw the focus outline (when the tab shows more
/// than one pane).
pub fn tiles<'a>(
    t: &'static Tokens,
    window: window::Id,
    tab: &'a Tab,
    panes: Vec<(PaneId, Element<'a, Message>)>,
    focused_outline: bool,
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
}

struct Drag {
    path: Path,
    /// Pointer offset from the divider's centre when it was grabbed.
    grab: f32,
}

#[derive(Default)]
struct State {
    drag: Option<Drag>,
    /// The last press on a divider, for double-click.
    last_press: Option<(Path, Instant)>,
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
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<State>();
        if state.area != Some(bounds.size()) {
            state.area = Some(bounds.size());
            self.publish(shell, TilesMsg::Area(bounds.size()));
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
                            Hit::Pane(id) if id != self.focused => {
                                // Not captured: the pane's terminal still gets the press.
                                self.publish(shell, TilesMsg::Focus(id));
                            }
                            Hit::Pane(_) | Hit::None => {}
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
        if !self.several() {
            return;
        }
        let bounds = layout.bounds();
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
        });
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
    fn a_drag_ends_when_the_pointer_or_focus_is_lost() {
        assert!(cancels_drag(&Event::Mouse(mouse::Event::CursorLeft)));
        assert!(cancels_drag(&Event::Window(window::Event::Unfocused)));
        assert!(!cancels_drag(&Event::Window(window::Event::Focused)));
        assert!(!cancels_drag(&Event::Mouse(mouse::Event::CursorEntered)));
        assert!(!cancels_drag(&Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left
        ))));
    }
}
