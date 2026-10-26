//! Pure layout tree for split panes: no UI, no sessions, just geometry and tree surgery.
//!
//! A tab's panes form a binary tree. `Row` places children side by side, `Column` stacks them.
//! Between the two children of every split sits a divider of `DIVIDER` logical pixels.
//!
//! Items marked `allow(dead_code)` are wired in by the tile moves of a later tiling task (drop
//! targets, `insert`, `move_pane`); each attribute goes when its item gets a caller.

use std::sync::atomic::{AtomicU64, Ordering};

use iced::{Rectangle, Size};

/// Fewest columns a pane may be squeezed to.
pub const MIN_COLS: u16 = 20;
/// Fewest rows a pane may be squeezed to.
pub const MIN_ROWS: u16 = 5;
/// Logical pixels between two panes: the visible line is 1 px centred, the rest is hit area.
pub const DIVIDER: f32 = 4.0;

const MIN_RATIO: f32 = 0.05;
const MAX_RATIO: f32 = 0.95;
const EPS: f32 = 0.5;

/// Identifies one pane for the lifetime of the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct PaneId(pub u64);

impl PaneId {
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// `Row`: children side by side. `Column`: children stacked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Row,
    Column,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Left and Top come with tile moves
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// Where an inserted subtree lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Drop {
    /// Beside this pane, on the given side of it.
    Tile(PaneId, Side),
    /// Along the outer edge of the whole layout.
    Edge(Side),
}

/// From the root: `false` = first child, `true` = second. Names a split.
pub type Path = Vec<bool>;

#[derive(Debug, Clone, PartialEq)]
pub struct Divider {
    pub path: Path,
    pub axis: Axis,
    pub rect: Rectangle,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Layout {
    Pane(PaneId),
    Split {
        axis: Axis,
        /// Share of `first`, in `0.0..1.0`.
        ratio: f32,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

impl Side {
    fn axis(self) -> Axis {
        match self {
            Side::Left | Side::Right => Axis::Row,
            Side::Top | Side::Bottom => Axis::Column,
        }
    }

    /// Whether the inserted subtree comes first (left or top).
    fn leads(self) -> bool {
        matches!(self, Side::Left | Side::Top)
    }
}

fn join(axis: Axis, ratio: f32, first: Layout, second: Layout) -> Layout {
    Layout::Split {
        axis,
        ratio,
        first: Box::new(first),
        second: Box::new(second),
    }
}

/// Places `subtree` beside `existing` on `side`, with `subtree` taking `share` of the axis.
fn beside(existing: Layout, subtree: Layout, side: Side, share: f32) -> Layout {
    if side.leads() {
        join(side.axis(), share, subtree, existing)
    } else {
        join(side.axis(), 1.0 - share, existing, subtree)
    }
}

impl Layout {
    /// Panes in reading order (first child before second, depth first).
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Layout::Pane(id) => out.push(*id),
            Layout::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        match self {
            Layout::Pane(id) => *id == pane,
            Layout::Split { first, second, .. } => first.contains(pane) || second.contains(pane),
        }
    }

    /// Splits `target` in half, putting `new` on `side` of it. No-op if `target` is absent.
    pub fn split(&mut self, target: PaneId, side: Side, new: PaneId) {
        self.insert_at_leaf(target, Layout::Pane(new), side);
    }

    fn insert_at_leaf(&mut self, target: PaneId, subtree: Layout, side: Side) -> Option<Layout> {
        match self {
            Layout::Pane(id) if *id == target => {
                let old = std::mem::replace(self, Layout::Pane(target));
                *self = beside(old, subtree, side, 0.5);
                None
            }
            Layout::Pane(_) => Some(subtree),
            Layout::Split { first, second, .. } => {
                let left = if first.contains(target) {
                    first.insert_at_leaf(target, subtree, side)
                } else {
                    Some(subtree)
                }?;
                second.insert_at_leaf(target, left, side)
            }
        }
    }

    /// Inserts a whole subtree. A `Tile` target that is absent falls back to the matching edge.
    #[allow(dead_code)]
    pub fn insert(&mut self, subtree: Layout, at: Drop) {
        match at {
            Drop::Tile(target, side) if self.contains(target) => {
                self.insert_at_leaf(target, subtree, side);
            }
            Drop::Tile(_, side) | Drop::Edge(side) => {
                let n = self.top_level_siblings(side.axis());
                let share = 1.0 / (n as f32 + 1.0);
                let old = std::mem::replace(self, Layout::Pane(PaneId(0)));
                *self = beside(old, subtree, side, share);
            }
        }
    }

    /// How many tiles sit side by side along `axis` at the top of the tree.
    #[allow(dead_code)]
    fn top_level_siblings(&self, axis: Axis) -> usize {
        match self {
            Layout::Split {
                axis: a,
                first,
                second,
                ..
            } if *a == axis => first.top_level_siblings(axis) + second.top_level_siblings(axis),
            _ => 1,
        }
    }

    /// Removes a pane; its sibling takes the parent's place. Returns the removed leaf, or `None`
    /// if the pane is absent or is the only pane (removing the last pane is the caller's job).
    pub fn remove(&mut self, pane: PaneId) -> Option<Layout> {
        let Layout::Split { first, second, .. } = self else {
            return None;
        };
        if matches!(**first, Layout::Pane(id) if id == pane) {
            let rest = std::mem::replace(&mut **second, Layout::Pane(pane));
            *self = rest;
            return Some(Layout::Pane(pane));
        }
        if matches!(**second, Layout::Pane(id) if id == pane) {
            let rest = std::mem::replace(&mut **first, Layout::Pane(pane));
            *self = rest;
            return Some(Layout::Pane(pane));
        }
        first.remove(pane).or_else(|| second.remove(pane))
    }

    /// Moves a pane to a drop target. False (and no change) if the move is impossible.
    #[allow(dead_code)]
    pub fn move_pane(&mut self, pane: PaneId, at: Drop) -> bool {
        if !self.contains(pane) || self.panes().len() < 2 {
            return false;
        }
        if let Drop::Tile(target, _) = at
            && (target == pane || !self.contains(target))
        {
            return false;
        }
        let Some(leaf) = self.remove(pane) else {
            return false;
        };
        self.insert(leaf, at);
        true
    }

    /// Pane rectangles inside `area`, in reading order.
    pub fn rects(&self, area: Rectangle) -> Vec<(PaneId, Rectangle)> {
        let mut out = Vec::new();
        self.walk(area, &mut Vec::new(), &mut |node| {
            if let Node::Pane(id, rect) = node {
                out.push((id, rect));
            }
        });
        out
    }

    /// One divider per split.
    pub fn dividers(&self, area: Rectangle) -> Vec<Divider> {
        let mut out = Vec::new();
        self.walk(area, &mut Vec::new(), &mut |node| {
            if let Node::Divider(path, axis, rect) = node {
                out.push(Divider { path, axis, rect });
            }
        });
        out
    }

    fn walk(&self, area: Rectangle, path: &mut Path, f: &mut dyn FnMut(Node)) {
        match self {
            Layout::Pane(id) => f(Node::Pane(*id, area)),
            Layout::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let (a, d, b) = cut(area, *axis, *ratio);
                f(Node::Divider(path.clone(), *axis, d));
                path.push(false);
                first.walk(a, path, f);
                path.pop();
                path.push(true);
                second.walk(b, path, f);
                path.pop();
            }
        }
    }

    fn split_at_mut(&mut self, path: &Path) -> Option<&mut f32> {
        let mut node = self;
        for &second in path {
            let Layout::Split {
                first, second: s, ..
            } = node
            else {
                return None;
            };
            node = if second { s } else { first };
        }
        match node {
            Layout::Split { ratio, .. } => Some(ratio),
            Layout::Pane(_) => None,
        }
    }

    /// Sets a split's ratio, clamped to `0.05..=0.95`. The caller checks `fits` afterwards.
    pub fn set_ratio(&mut self, path: &Path, ratio: f32) {
        if let Some(r) = self.split_at_mut(path) {
            *r = ratio.clamp(MIN_RATIO, MAX_RATIO);
        }
    }

    pub fn equalize(&mut self, path: &Path) {
        self.set_ratio(path, 0.5);
    }

    /// The axis and the rectangle of the split at `path`, laid out in `area`.
    pub fn split_area(&self, area: Rectangle, path: &Path) -> Option<(Axis, Rectangle)> {
        let mut node = self;
        let mut area = area;
        for &second in path {
            let Layout::Split {
                axis,
                ratio,
                first,
                second: s,
            } = node
            else {
                return None;
            };
            let (a, _, b) = cut(area, *axis, *ratio);
            (node, area) = if second { (&**s, b) } else { (&**first, a) };
        }
        match node {
            Layout::Split { axis, .. } => Some((*axis, area)),
            Layout::Pane(_) => None,
        }
    }

    /// The pane next to `from` in `dir`: overlapping on the other axis, nearest edge first;
    /// ties go to the earliest in `recent`, then reading order.
    pub fn neighbour(
        &self,
        area: Rectangle,
        from: PaneId,
        dir: Dir,
        recent: &[PaneId],
    ) -> Option<PaneId> {
        let rects = self.rects(area);
        let origin = rects.iter().find(|(id, _)| *id == from)?.1;
        let mut best: Option<(f32, usize, PaneId)> = None;
        for (id, r) in rects.iter().filter(|(id, _)| *id != from) {
            let (gap, overlap) = match dir {
                Dir::Right => (
                    r.x - (origin.x + origin.width),
                    span(origin.y, origin.height, r.y, r.height),
                ),
                Dir::Left => (
                    origin.x - (r.x + r.width),
                    span(origin.y, origin.height, r.y, r.height),
                ),
                Dir::Down => (
                    r.y - (origin.y + origin.height),
                    span(origin.x, origin.width, r.x, r.width),
                ),
                Dir::Up => (
                    origin.y - (r.y + r.height),
                    span(origin.x, origin.width, r.x, r.width),
                ),
            };
            if gap < -EPS || overlap <= EPS {
                continue;
            }
            let rank = recent.iter().position(|p| p == id).unwrap_or(usize::MAX);
            let better = match best {
                None => true,
                Some((g, rk, _)) => gap < g - EPS || (gap <= g + EPS && rank < rk),
            };
            if better {
                best = Some((gap, rank, *id));
            }
        }
        best.map(|(_, _, id)| id)
    }

    /// Whether every pane keeps at least `MIN_COLS` x `MIN_ROWS` cells.
    pub fn fits(&self, area: Rectangle, cell: Size) -> bool {
        let min_w = f32::from(MIN_COLS) * cell.width;
        let min_h = f32::from(MIN_ROWS) * cell.height;
        self.rects(area)
            .iter()
            .all(|(_, r)| r.width >= min_w && r.height >= min_h)
    }
}

enum Node {
    Pane(PaneId, Rectangle),
    Divider(Path, Axis, Rectangle),
}

/// Overlap length of two 1-D spans.
fn span(a: f32, alen: f32, b: f32, blen: f32) -> f32 {
    (a + alen).min(b + blen) - a.max(b)
}

/// Cuts `area` into first, divider and second.
fn cut(area: Rectangle, axis: Axis, ratio: f32) -> (Rectangle, Rectangle, Rectangle) {
    let total = match axis {
        Axis::Row => area.width,
        Axis::Column => area.height,
    };
    let room = (total - DIVIDER).max(0.0);
    let first = room * ratio;
    let second = room - first;
    let d = DIVIDER.min(total);
    match axis {
        Axis::Row => (
            Rectangle {
                width: first,
                ..area
            },
            Rectangle {
                x: area.x + first,
                width: d,
                ..area
            },
            Rectangle {
                x: area.x + first + d,
                width: second,
                ..area
            },
        ),
        Axis::Column => (
            Rectangle {
                height: first,
                ..area
            },
            Rectangle {
                y: area.y + first,
                height: d,
                ..area
            },
            Rectangle {
                y: area.y + first + d,
                height: second,
                ..area
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(n: u64) -> PaneId {
        PaneId(n)
    }

    fn pane(n: u64) -> Layout {
        Layout::Pane(p(n))
    }

    fn area() -> Rectangle {
        Rectangle {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 600.0,
        }
    }

    fn rect_of(l: &Layout, a: Rectangle, id: u64) -> Rectangle {
        l.rects(a).into_iter().find(|(i, _)| *i == p(id)).unwrap().1
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn split_right_halves_the_pane() {
        let mut l = pane(1);
        l.split(p(1), Side::Right, p(2));
        let rects = l.rects(area());
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].0, p(1));
        assert!(near(rects[0].1.width, 498.0) && near(rects[0].1.height, 600.0));
        assert!(near(rects[1].1.width, 498.0) && near(rects[1].1.x, 502.0));
        let d = l.dividers(area());
        assert_eq!(d.len(), 1);
        assert!(near(d[0].rect.x, 498.0) && near(d[0].rect.width, DIVIDER));
        assert_eq!(d[0].axis, Axis::Row);
        assert!(d[0].path.is_empty());
    }

    #[test]
    fn split_down_stacks() {
        let mut l = pane(1);
        l.split(p(1), Side::Bottom, p(2));
        assert_eq!(l.panes(), vec![p(1), p(2)]);
        let r1 = rect_of(&l, area(), 1);
        let r2 = rect_of(&l, area(), 2);
        assert!(near(r1.height, 298.0) && near(r1.width, 1000.0));
        assert!(near(r2.y, 302.0));
        let mut t = pane(1);
        t.split(p(1), Side::Left, p(2));
        assert_eq!(t.panes(), vec![p(2), p(1)]);
    }

    #[test]
    fn remove_gives_space_to_sibling() {
        let mut l = pane(1);
        l.split(p(1), Side::Right, p(2));
        assert_eq!(l.remove(p(1)), Some(pane(1)));
        assert_eq!(l, pane(2));
        assert_eq!(rect_of(&l, area(), 2), area());
        assert_eq!(l.remove(p(2)), None, "last pane is the caller's job");
        assert_eq!(l.remove(p(9)), None);
    }

    #[test]
    fn insert_on_tile_edge_splits_that_tile() {
        let mut l = pane(1);
        l.split(p(1), Side::Right, p(2));
        l.insert(pane(3), Drop::Tile(p(2), Side::Bottom));
        assert_eq!(l.panes(), vec![p(1), p(2), p(3)]);
        let r1 = rect_of(&l, area(), 1);
        let r2 = rect_of(&l, area(), 2);
        let r3 = rect_of(&l, area(), 3);
        assert!(near(r1.height, 600.0));
        assert!(near(r2.width, r3.width) && near(r2.height, r3.height));
        assert!(r3.y > r2.y);
    }

    #[test]
    fn insert_on_outer_edge_takes_whole_column() {
        let mut l = pane(1);
        l.split(p(1), Side::Right, p(2));
        l.split(p(1), Side::Bottom, p(3));
        // Now (1 over 3) | 2: root is a row with two tiles, so N gets a third.
        let mut two = l.clone();
        two.insert(pane(9), Drop::Edge(Side::Right));
        let n = rect_of(&two, area(), 9);
        assert!(near(n.height, 600.0));
        assert!(n.width > 330.0 && n.width < 336.0, "{}", n.width);

        // A over C stacked, B beside A: A|B over C.
        let mut l = pane(1);
        l.split(p(1), Side::Right, p(2));
        l.split(p(1), Side::Bottom, p(3));
        let mut l = Layout::Split {
            axis: Axis::Column,
            ratio: 0.5,
            first: Box::new(join(Axis::Row, 0.5, pane(1), pane(2))),
            second: Box::new(pane(3)),
        };
        l.insert(pane(9), Drop::Edge(Side::Right));
        let n = rect_of(&l, area(), 9);
        assert!(near(n.height, 600.0));
        assert!(near(n.width, 498.0), "{}", n.width);

        let mut row = join(
            Axis::Row,
            0.5,
            pane(1),
            join(Axis::Row, 0.5, pane(2), pane(3)),
        );
        row.insert(pane(9), Drop::Edge(Side::Right));
        let n = rect_of(&row, area(), 9);
        assert!(near(n.width, (1000.0 - DIVIDER) / 4.0), "{}", n.width);

        let mut left = pane(1);
        left.insert(pane(9), Drop::Edge(Side::Left));
        assert_eq!(left.panes(), vec![p(9), p(1)]);
    }

    #[test]
    fn move_pane_keeps_other_ratios() {
        let mut l = join(
            Axis::Row,
            0.3,
            pane(1),
            join(Axis::Column, 0.7, pane(2), pane(3)),
        );
        assert!(l.move_pane(p(1), Drop::Tile(p(3), Side::Right)));
        assert_eq!(l.panes().len(), 3);
        let Layout::Split { axis, ratio, .. } = &l else {
            panic!("split expected")
        };
        assert_eq!((*axis, *ratio), (Axis::Column, 0.7));
        assert!(!l.move_pane(p(2), Drop::Tile(p(2), Side::Left)));
        assert!(!l.move_pane(p(9), Drop::Edge(Side::Left)));
        assert!(!pane(1).move_pane(p(1), Drop::Edge(Side::Left)));
    }

    #[test]
    fn neighbour_prefers_overlap_then_recent() {
        // 1 | (2 over 3), then 4 right of the whole thing, spanning full height.
        let l = join(
            Axis::Row,
            0.5,
            pane(1),
            join(Axis::Column, 0.5, pane(2), pane(3)),
        );
        let a = area();
        assert_eq!(l.neighbour(a, p(1), Dir::Right, &[]), Some(p(2)));
        assert_eq!(l.neighbour(a, p(1), Dir::Right, &[p(3)]), Some(p(3)));
        assert_eq!(l.neighbour(a, p(2), Dir::Left, &[]), Some(p(1)));
        assert_eq!(l.neighbour(a, p(2), Dir::Down, &[]), Some(p(3)));
        assert_eq!(l.neighbour(a, p(3), Dir::Up, &[]), Some(p(2)));
        assert_eq!(l.neighbour(a, p(1), Dir::Left, &[]), None);
        assert_eq!(l.neighbour(a, p(2), Dir::Right, &[]), None);
        // Nearest edge beats recency: 2 is farther than 3 only if geometry says so.
        let l = join(
            Axis::Row,
            0.5,
            pane(1),
            join(Axis::Row, 0.5, pane(2), pane(3)),
        );
        assert_eq!(l.neighbour(a, p(1), Dir::Right, &[p(3)]), Some(p(2)));
    }

    #[test]
    fn split_area_follows_the_path() {
        let l = join(
            Axis::Row,
            0.5,
            pane(1),
            join(Axis::Column, 0.5, pane(2), pane(3)),
        );
        assert_eq!(l.split_area(area(), &vec![]), Some((Axis::Row, area())));
        let (axis, r) = l.split_area(area(), &vec![true]).unwrap();
        assert_eq!(axis, Axis::Column);
        assert!(near(r.x, 502.0) && near(r.width, 498.0) && near(r.height, 600.0));
        assert_eq!(l.split_area(area(), &vec![false]), None);
    }

    #[test]
    fn equalize_sets_half() {
        let mut l = join(Axis::Row, 0.8, pane(1), pane(2));
        l.equalize(&vec![]);
        assert!(matches!(l, Layout::Split { ratio, .. } if ratio == 0.5));
    }

    #[test]
    fn ratios_are_clamped() {
        let mut l = join(
            Axis::Row,
            0.5,
            pane(1),
            join(Axis::Column, 0.5, pane(2), pane(3)),
        );
        l.set_ratio(&vec![], 2.0);
        l.set_ratio(&vec![true], -1.0);
        let Layout::Split { ratio, second, .. } = &l else {
            panic!()
        };
        assert_eq!(*ratio, 0.95);
        assert!(matches!(**second, Layout::Split { ratio, .. } if ratio == 0.05));
        // A path that names no split changes nothing.
        let before = l.clone();
        l.set_ratio(&vec![false], 0.2);
        assert_eq!(l, before);
    }

    #[test]
    fn split_refused_below_minimum() {
        let small = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 300.0,
            height: 100.0,
        };
        let cell = Size::new(8.0, 16.0);
        let single = pane(1);
        assert!(single.fits(small, cell));
        let mut l = pane(1);
        l.split(p(1), Side::Right, p(2));
        assert!(!l.fits(small, cell));
        assert!(l.fits(area(), cell));
    }

    #[test]
    fn rects_cover_area_without_overlap() {
        let trees = [
            pane(1),
            join(Axis::Row, 0.5, pane(1), pane(2)),
            join(
                Axis::Row,
                0.3,
                pane(1),
                join(Axis::Column, 0.7, pane(2), pane(3)),
            ),
            join(
                Axis::Column,
                0.4,
                join(Axis::Row, 0.25, pane(1), pane(2)),
                join(
                    Axis::Row,
                    0.9,
                    join(Axis::Column, 0.5, pane(3), pane(4)),
                    pane(5),
                ),
            ),
        ];
        let a = Rectangle {
            x: 10.0,
            y: 20.0,
            width: 1000.0,
            height: 600.0,
        };
        for t in &trees {
            let mut all: Vec<Rectangle> = t.rects(a).into_iter().map(|(_, r)| r).collect();
            all.extend(t.dividers(a).into_iter().map(|d| d.rect));
            let total: f32 = all.iter().map(|r| r.width * r.height).sum();
            assert!(near(total, a.width * a.height) || (total - a.width * a.height).abs() < 1.0);
            for (i, r) in all.iter().enumerate() {
                assert!(r.x >= a.x - 0.01 && r.y >= a.y - 0.01);
                assert!(r.x + r.width <= a.x + a.width + 0.01);
                assert!(r.y + r.height <= a.y + a.height + 0.01);
                for s in &all[i + 1..] {
                    let w = span(r.x, r.width, s.x, s.width);
                    let h = span(r.y, r.height, s.y, s.height);
                    assert!(w <= 0.01 || h <= 0.01, "{r:?} overlaps {s:?}");
                }
            }
        }
    }
}
