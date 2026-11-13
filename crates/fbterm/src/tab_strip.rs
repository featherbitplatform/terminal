//! The tab bar's strip: where the tabs sit when they no longer fit, and how they are clipped.
//!
//! Tabs share the bar's room from [`TAB_MAX_WIDTH`] down to [`TAB_MIN_WIDTH`]; past that the
//! strip is narrower than its tabs and scrolls, while the buttons after it stay in view. The
//! layout here is pure: the window width, the chrome and the tab count decide everything.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector, mouse, window};

use crate::app::{App, Message};
use crate::chrome::{self, Chrome};
use crate::tab_sidebar::reveal;
use crate::tabs::TabId;
use crate::window::{BAR_SPACING, BUTTON_WIDTH, trailing_width};

/// A tab's widest and narrowest width; tabs share the bar's room between them.
pub(crate) const TAB_MAX_WIDTH: f32 = 196.0;
pub(crate) const TAB_MIN_WIDTH: f32 = 80.0;
/// The narrowest a tab gets in a bar too narrow for the chevrons: its dot and a few letters of
/// its name, no close button.
pub(crate) const TAB_TIGHT_WIDTH: f32 = 50.0;
/// The gap between two tabs.
pub(crate) const TAB_SPACING: f32 = 2.0;
/// The two chevron buttons and their spacing.
pub(crate) const CHEVRONS_WIDTH: f32 = 2.0 * (BUTTON_WIDTH + BAR_SPACING);
/// Empty bar kept free of tabs however many there are, so the window can always be dragged by it.
pub(crate) const DRAG_ROOM: f32 = 10.0;

/// Where the tabs sit in a bar of one window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StripLayout {
    /// Each tab's width.
    pub tab_w: f32,
    /// The strip's visible width.
    pub strip_w: f32,
    /// All the tabs' width side by side.
    pub content_w: f32,
    /// The tabs do not all fit in the strip: it scrolls.
    pub overflow: bool,
    /// The chevrons show beside the strip: it overflows and there is room for them and a
    /// whole [`TAB_MIN_WIDTH`] tab.
    pub chevrons: bool,
}

/// The strip for `count` tabs in a window `width` wide (`None`: not known yet, full-size tabs),
/// with `trailing` px reserved for the button group after it (see `window::trailing_width`).
pub(crate) fn strip_layout(
    chrome: Chrome,
    fullscreen: bool,
    width: Option<f32>,
    count: usize,
    trailing: f32,
) -> StripLayout {
    let Some(width) = width else {
        return StripLayout {
            tab_w: TAB_MAX_WIDTH,
            strip_w: content_width(TAB_MAX_WIDTH, count),
            content_w: content_width(TAB_MAX_WIDTH, count),
            overflow: false,
            chevrons: false,
        };
    };
    let controls = if chrome.draws_controls() {
        let setup = chrome::setup();
        chrome::controls_width(setup.look, setup.controls.order.len())
    } else {
        0.0
    };
    let room = width - chrome.leading_inset(fullscreen) - controls - trailing - DRAG_ROOM;
    let shared = room / count.max(1) as f32 - TAB_SPACING;
    // Half a pixel of slack: widths come out of float arithmetic.
    let fits = count as f32 * (TAB_MIN_WIDTH + TAB_SPACING) <= room + 0.5;
    let beside_chevrons = room - CHEVRONS_WIDTH - TAB_SPACING;
    let (tab_w, strip_w, chevrons) = if fits {
        let tab_w = shared.clamp(TAB_MIN_WIDTH, TAB_MAX_WIDTH);
        (tab_w, content_width(tab_w, count), false)
    } else if beside_chevrons >= TAB_MIN_WIDTH {
        (TAB_MIN_WIDTH, beside_chevrons, true)
    } else {
        // Too narrow for the chevrons and a whole tab: the strip takes the room, the tabs
        // shrink further to share it, never wider than the strip so the active one can show
        // whole, and the wheel still scrolls them.
        let strip_w = room.max(0.0);
        let tab_w = if strip_w > 0.0 {
            shared.clamp(TAB_TIGHT_WIDTH, TAB_MIN_WIDTH).min(strip_w)
        } else {
            TAB_TIGHT_WIDTH
        };
        (tab_w, strip_w, false)
    };
    let content_w = content_width(tab_w, count);
    let overflow = content_w > strip_w + 0.5;
    // A scrolling strip shows whole tabs only, so both its ends land on tab edges: at either
    // end no tab is cut, and the active one never keeps the strip from reaching it
    // (`keep_whole`). The rest of the room stays empty bar.
    let strip_w = if overflow && strip_w >= tab_w {
        let pitch = tab_w + TAB_SPACING;
        ((strip_w + TAB_SPACING) / pitch + 1e-3).floor() * pitch - TAB_SPACING
    } else {
        strip_w
    };
    StripLayout {
        tab_w,
        strip_w,
        content_w,
        overflow,
        chevrons,
    }
}

/// `count` tabs of `tab_w` side by side.
fn content_width(tab_w: f32, count: usize) -> f32 {
    count as f32 * tab_w + count.saturating_sub(1) as f32 * TAB_SPACING
}

/// `offset` kept within the strip: 0 to the hidden part of the tabs (0 when nothing scrolls).
pub(crate) fn clamp_offset(offset: f32, layout: &StripLayout) -> f32 {
    if layout.overflow {
        offset.clamp(0.0, (layout.content_w - layout.strip_w).max(0.0))
    } else {
        0.0
    }
}

/// `offset`, reached by scrolling `delta`, moved on so the tab `[start, start + len)` is whole
/// in view or out of it: a tab cut at the strip's edge gets no hole, so the bar's translucent
/// background would lie under its own fill. Cut on the side the strip is moving toward, it is
/// passed over when the strip's end allows; otherwise it is shown whole.
pub(crate) fn keep_whole(
    offset: f32,
    delta: f32,
    start: f32,
    len: f32,
    layout: &StripLayout,
) -> f32 {
    let viewport = layout.strip_w;
    let end = start + len;
    let shown = |o: f32| start >= o - 0.01 && end <= o + viewport + 0.01;
    let hidden = |o: f32| end <= o + 0.01 || start >= o + viewport - 0.01;
    if !layout.overflow || len > viewport || shown(offset) || hidden(offset) {
        return offset;
    }
    let cut_left = start < offset;
    if (delta > 0.0) == cut_left {
        let past = clamp_offset(
            if cut_left {
                end + TAB_SPACING
            } else {
                start - TAB_SPACING - viewport
            },
            layout,
        );
        if hidden(past) {
            return past;
        }
    }
    clamp_offset(if cut_left { start } else { end - viewport }, layout)
}

/// The strip itself: `content` (the tabs' row) laid out `offset` px to the left of the strip's
/// own left edge and clipped to a strip `width` wide. The content's layout is therefore in
/// window coordinates, so whatever reads the tabs' rectangles (the bar's drags, hover tints
/// and outline) needs no knowledge of the offset. Only a pointer inside the strip reaches the
/// content: a tab scrolled out of view cannot be hovered or pressed.
pub(crate) struct Viewport<'a> {
    content: Element<'a, Message>,
    width: f32,
    offset: f32,
}

impl<'a> Viewport<'a> {
    pub(crate) fn new(content: Element<'a, Message>, width: f32, offset: f32) -> Self {
        Self {
            content,
            width,
            offset,
        }
    }

    /// The pointer as the content may see it: gone while it is outside the strip.
    fn inside(layout: Layout<'_>, cursor: mouse::Cursor) -> mouse::Cursor {
        if cursor.is_over(layout.bounds()) {
            cursor
        } else {
            mouse::Cursor::Unavailable
        }
    }
}

impl Widget<Message, iced::Theme, iced::Renderer> for Viewport<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::stateless()
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.width), Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let unbounded =
            layout::Limits::new(Size::ZERO, Size::new(f32::INFINITY, limits.max().height));
        let child = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, &unbounded)
            .move_to(Point::new(-self.offset, 0.0));
        let size = Size::new(self.width, child.size().height);
        layout::Node::with_children(size, vec![child])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        if let Some(child) = layout.children().next() {
            self.content
                .as_widget_mut()
                .operate(&mut tree.children[0], child, renderer, operation);
        }
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
        let Some(child) = layout.children().next() else {
            return;
        };
        let clip = layout.bounds().intersection(viewport).unwrap_or_default();
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            child,
            Self::inside(layout, cursor),
            renderer,
            clipboard,
            shell,
            &clip,
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
        let Some(child) = layout.children().next() else {
            return mouse::Interaction::None;
        };
        let clip = layout.bounds().intersection(viewport).unwrap_or_default();
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            child,
            Self::inside(layout, cursor),
            &clip,
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
        let Some(child) = layout.children().next() else {
            return;
        };
        let bounds = layout.bounds();
        let clip = bounds.intersection(viewport).unwrap_or_default();
        let cursor = Self::inside(layout, cursor);
        renderer.with_layer(bounds, |renderer| {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                child,
                cursor,
                &clip,
            );
        });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, iced::Theme, iced::Renderer>> {
        let child = layout.children().next()?;
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            child,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a> From<Viewport<'a>> for Element<'a, Message> {
    fn from(strip: Viewport<'a>) -> Self {
        Element::new(strip)
    }
}

/// How far one wheel line scrolls the strip.
pub(crate) const WHEEL_LINE_PX: f32 = 40.0;

impl App {
    /// The strip of window `wid`, from its size, its tabs and the buttons after it.
    pub(crate) fn strip_layout_of(&self, wid: window::Id) -> StripLayout {
        let count = self.windows.get(&wid).map_or(0, |w| w.tabs.len());
        let trailing = trailing_width(self.tunnels.up_count(), self.files_button(wid).is_some());
        strip_layout(
            Chrome::current(),
            self.fullscreen.contains(&wid),
            self.sizes.get(&wid).map(|&(w, _)| w),
            count,
            trailing,
        )
    }

    /// The window's strip offset as stored, maybe past the strip's end since the strip shrank:
    /// readers clamp it with the layout they have (`clamp_offset`).
    pub(crate) fn stored_offset(&self, wid: window::Id) -> f32 {
        self.strip_offset.get(&wid).copied().unwrap_or(0.0)
    }

    /// The window's strip offset as stored, kept within the strip as it is now.
    #[cfg(test)]
    pub(crate) fn offset_of(&self, wid: window::Id) -> f32 {
        clamp_offset(self.stored_offset(wid), &self.strip_layout_of(wid))
    }

    /// Scroll the strip by `delta` logical pixels, within its ends, never leaving the active tab
    /// cut at an edge (`keep_whole`).
    pub(crate) fn strip_scrolled_by(&mut self, wid: window::Id, delta: f32) {
        let layout = self.strip_layout_of(wid);
        let mut next = clamp_offset(
            clamp_offset(self.stored_offset(wid), &layout) + delta,
            &layout,
        );
        if let Some(w) = self.windows.get(&wid) {
            let start = w.active as f32 * (layout.tab_w + TAB_SPACING);
            next = keep_whole(next, delta, start, layout.tab_w, &layout);
        }
        self.strip_offset.insert(wid, next);
    }

    /// Show window `wid`'s active tab again after this update, as if it had just become
    /// active: scrolled into view in the bar or the sidebar, its group unfolded. For a key or
    /// palette action on the active tab (rename, copy password), which must not act on a tab
    /// scrolled out of sight by hand.
    pub(crate) fn reveal_active_tab(&mut self, wid: window::Id) {
        self.strip_seen.remove(&wid);
        self.sidebar_shown.remove(&wid);
    }

    /// A chevron: one tab toward `dir` (`-1` earlier tabs, `1` later).
    pub(crate) fn strip_stepped(&mut self, wid: window::Id, dir: i8) {
        let step = self.strip_layout_of(wid).tab_w + TAB_SPACING;
        self.strip_scrolled_by(wid, f32::from(dir) * step);
    }

    /// After every update, with tabs on top: a window whose active tab, its place among the
    /// tabs or the strip's room changed since the strip last showed the active tab scrolls just enough
    /// to show it; one whose tabs fit goes back to the start. Any other change (a tab opening,
    /// the buttons' width) only clamps the offset, and a scroll by hand stays. Never while a
    /// carry is on in that window: a hover selecting a half-shown tab would scroll the strip
    /// under a pointer that did not move, and the next dwell would walk on; the reveal comes
    /// once the carry ends. Until then a half-shown tab made active by a dwell stays clipped
    /// with no hole (`visible_hole`), a deliberate, short-lived look.
    pub(crate) fn reveal_strips(&mut self) {
        let changed: Vec<(window::Id, StripSeen, StripLayout)> = self
            .windows
            .iter()
            .filter(|(wid, _)| self.carry_in(**wid).is_none())
            .filter_map(|(wid, w)| {
                let tab = w.active_tab()?;
                let layout = self.strip_layout_of(*wid);
                let seen = StripSeen {
                    tab: tab.id,
                    index: w.active,
                    tab_w: layout.tab_w,
                    strip_w: layout.strip_w,
                };
                (self.strip_seen.get(wid) != Some(&seen)).then_some((*wid, seen, layout))
            })
            .collect();
        for (wid, seen, layout) in changed {
            self.strip_seen.insert(wid, seen);
            let start = seen.index as f32 * (layout.tab_w + TAB_SPACING);
            let offset = clamp_offset(self.stored_offset(wid), &layout);
            let next = if layout.overflow {
                reveal(start, layout.tab_w, offset, layout.strip_w, 0.0).unwrap_or(offset)
            } else {
                0.0
            };
            self.strip_offset.insert(wid, clamp_offset(next, &layout));
        }
    }
}

/// What the reveal of a window's active tab was last done for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StripSeen {
    tab: TabId,
    index: usize,
    /// The room the strip had: a resize, full screen or a button appearing (a tunnel count,
    /// the files button) changes it and can push the active tab out of view.
    tab_w: f32,
    strip_w: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::trailing_width;

    fn layout(chrome: Chrome, width: Option<f32>, count: usize) -> StripLayout {
        strip_layout(chrome, false, width, count, trailing_width(3, true))
    }

    #[test]
    fn everything_fits_without_overflow() {
        let l = layout(Chrome::Unified, Some(1600.0), 3);
        assert_eq!(l.tab_w, TAB_MAX_WIDTH);
        assert!(!l.overflow);
        assert_eq!(l.content_w, 3.0 * l.tab_w + 2.0 * TAB_SPACING);
        assert_eq!(l.strip_w, l.content_w);
    }

    #[test]
    fn tabs_shrink_to_eighty_then_overflow() {
        let few = layout(Chrome::Unified, Some(1000.0), 5);
        assert!(
            TAB_MIN_WIDTH < few.tab_w && few.tab_w < TAB_MAX_WIDTH,
            "{few:?}"
        );
        assert!(!few.overflow);
        let more = layout(Chrome::Unified, Some(1000.0), 8);
        assert!(more.tab_w < few.tab_w, "more tabs, narrower tabs");
        assert!(
            layout(Chrome::Unified, Some(1000.0), 5).tab_w
                > layout(Chrome::Unified, Some(700.0), 5).tab_w,
            "narrower window, narrower tabs"
        );

        let l = layout(Chrome::Unified, Some(600.0), 30);
        assert_eq!(l.tab_w, 80.0);
        assert!(l.overflow);
        // The room beside the chevrons, trimmed to whole tabs.
        let room = 600.0
            - Chrome::Unified.leading_inset(false)
            - trailing_width(3, true)
            - DRAG_ROOM
            - CHEVRONS_WIDTH
            - TAB_SPACING;
        let pitch = 80.0 + TAB_SPACING;
        assert_eq!(
            l.strip_w,
            ((room + TAB_SPACING) / pitch).floor() * pitch - TAB_SPACING
        );
        assert_eq!(l.content_w, 30.0 * 80.0 + 29.0 * TAB_SPACING);
    }

    #[test]
    fn the_overflow_threshold_is_exact() {
        let n = 10;
        let trailing = trailing_width(3, true);
        let inset = Chrome::Unified.leading_inset(false);
        // The room left of the group exactly holds `n` tabs of 80 px with their gaps.
        let exact = inset + trailing + DRAG_ROOM + n as f32 * (TAB_MIN_WIDTH + TAB_SPACING);
        let at = |width| strip_layout(Chrome::Unified, false, Some(width), n, trailing);
        assert!(!at(exact).overflow, "exactly fits");
        assert!(at(exact - 1.0).overflow, "a pixel less does not");
    }

    #[test]
    fn a_window_too_narrow_for_the_group_has_an_empty_strip() {
        let l = layout(Chrome::Unified, Some(10.0), 5);
        assert_eq!(l.strip_w, 0.0);
        assert_eq!(l.tab_w, TAB_TIGHT_WIDTH);
        assert!(l.overflow && !l.chevrons);
    }

    #[test]
    fn no_tabs_and_one_tab() {
        for count in [0, 1] {
            let l = layout(Chrome::Unified, Some(600.0), count);
            assert!(!l.overflow, "{count}: {l:?}");
            assert!(l.strip_w.is_finite() && l.strip_w >= 0.0);
            assert!(l.content_w.is_finite() && l.content_w >= 0.0);
        }
        assert_eq!(layout(Chrome::Unified, Some(600.0), 0).content_w, 0.0);
        for count in [0, 1, 50] {
            let l = layout(Chrome::Unified, None, count);
            assert_eq!(l.tab_w, TAB_MAX_WIDTH);
            assert!(!l.overflow);
        }
    }

    #[test]
    fn drawn_controls_and_full_screen_change_the_room() {
        let plain = layout(Chrome::Unified, Some(700.0), 30);
        let drawn = layout(Chrome::Drawn, Some(700.0), 30);
        let setup = chrome::setup();
        let controls = chrome::controls_width(setup.look, setup.controls.order.len());
        let inset_diff = Chrome::Unified.leading_inset(false) - Chrome::Drawn.leading_inset(false);
        // Each strip is trimmed to whole tabs, so the difference is the controls' within a tab.
        let pitch = plain.tab_w + TAB_SPACING;
        let diff = plain.strip_w - drawn.strip_w;
        assert!(
            (diff - (controls - inset_diff)).abs() < pitch,
            "{plain:?} {drawn:?}"
        );
        let full = strip_layout(
            Chrome::Unified,
            true,
            Some(700.0),
            30,
            trailing_width(3, true),
        );
        let gained = Chrome::Unified.leading_inset(false) - Chrome::Unified.leading_inset(true);
        assert!(
            (full.strip_w - plain.strip_w - gained).abs() < pitch,
            "{full:?} {plain:?}"
        );
    }

    #[test]
    fn clamp_offset_never_leaves_a_blank_strip() {
        let l = StripLayout {
            tab_w: 80.0,
            strip_w: 300.0,
            content_w: 420.0,
            overflow: true,
            chevrons: true,
        };
        assert_eq!(clamp_offset(500.0, &l), 120.0);
        assert_eq!(clamp_offset(-5.0, &l), 0.0);
        assert_eq!(clamp_offset(50.0, &l), 50.0);
        let fits = StripLayout {
            overflow: false,
            chevrons: false,
            strip_w: 420.0,
            ..l
        };
        assert_eq!(clamp_offset(50.0, &fits), 0.0);
    }

    #[test]
    fn reveal_without_padding_cases() {
        let reveal_span = |start, len, offset, viewport| reveal(start, len, offset, viewport, 0.0);
        // View [100, 400).
        assert_eq!(
            reveal_span(20.0, 80.0, 100.0, 300.0),
            Some(20.0),
            "left of it"
        );
        assert_eq!(
            reveal_span(450.0, 80.0, 100.0, 300.0),
            Some(230.0),
            "right of it"
        );
        assert_eq!(reveal_span(150.0, 80.0, 100.0, 300.0), None, "shown");
        assert_eq!(
            reveal_span(60.0, 80.0, 100.0, 300.0),
            Some(60.0),
            "half hidden left"
        );
        assert_eq!(
            reveal_span(360.0, 80.0, 100.0, 300.0),
            Some(140.0),
            "half hidden right"
        );
        assert_eq!(
            reveal_span(10.0, 500.0, 100.0, 300.0),
            Some(10.0),
            "longer than the view"
        );
        assert_eq!(
            reveal_span(0.0, 80.0, 100.0, 300.0),
            Some(0.0),
            "never negative"
        );
    }
}

#[cfg(test)]
mod app_tests {
    use fbterm_config::keys::Action;
    use iced::window;

    use super::*;
    use crate::app::{App, Message, TabBarMsg};
    use crate::restore_tests::{paths_with, profile};
    use crate::tabs::{Pane, Tab, WindowState};

    fn app_with_tabs(n: usize, width: f32) -> (App, window::Id) {
        let profiles: Vec<_> = (0..n).map(|i| profile(&format!("t{i}"))).collect();
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
        app.sizes.insert(wid, (width, 600.0));
        (app, wid)
    }

    fn max(app: &App, wid: window::Id) -> f32 {
        let l = app.strip_layout_of(wid);
        l.content_w - l.strip_w
    }

    #[test]
    fn the_strip_offset_is_clamped() {
        let (mut app, wid) = app_with_tabs(40, 800.0);
        assert!(app.strip_layout_of(wid).overflow);
        app.strip_scrolled_by(wid, 1e6);
        assert_eq!(app.strip_offset[&wid], max(&app, wid));
        app.strip_scrolled_by(wid, -1e6);
        assert_eq!(app.strip_offset[&wid], 0.0);
        // Closed tabs shrink the strip: the stored offset is read back within it.
        app.strip_scrolled_by(wid, 1e6);
        for _ in 0..35 {
            let _ = app.close_tab(wid, 0);
        }
        assert_eq!(app.offset_of(wid), 0.0, "five tabs fit: nothing to scroll");
    }

    #[test]
    fn a_rename_or_copy_password_by_key_scrolls_the_active_tab_back_into_view() {
        use crate::app::TabBarMsg;
        use crate::keys::Action;
        for action in [Action::RenameTab, Action::CopyPassword] {
            let (mut app, wid) = app_with_tabs(40, 800.0);
            app.windows.get_mut(&wid).unwrap().active = 39;
            let _ = app.update(Message::TabBar(wid, TabBarMsg::StripScroll(0.0)));
            assert_eq!(app.offset_of(wid), max(&app, wid), "the last tab is shown");
            let _ = app.update(Message::TabBar(wid, TabBarMsg::StripScroll(-1e6)));
            assert_eq!(app.offset_of(wid), 0.0, "{action:?}: scrolled away by hand");
            let _ = app.update(Message::Shortcut(wid, action));
            assert_eq!(app.offset_of(wid), max(&app, wid), "{action:?}");
        }
    }

    #[test]
    fn a_chevron_step_moves_one_tab() {
        let (mut app, wid) = app_with_tabs(40, 800.0);
        let step = app.strip_layout_of(wid).tab_w + TAB_SPACING;
        app.strip_stepped(wid, 1);
        assert_eq!(app.strip_offset[&wid], step);
        app.strip_stepped(wid, -1);
        app.strip_stepped(wid, -1);
        assert_eq!(app.strip_offset[&wid], 0.0, "never before the first tab");
    }

    #[test]
    fn selecting_a_hidden_tab_scrolls_it_into_view() {
        let (mut app, wid) = app_with_tabs(40, 800.0);
        let _ = app.update(Message::SelectTab(0));
        let _ = app.update(Message::SelectTab(39));
        let l = app.strip_layout_of(wid);
        let start = 39.0 * (l.tab_w + TAB_SPACING);
        let offset = app.offset_of(wid);
        assert!(offset <= start, "{offset} {start}");
        assert!(
            start + l.tab_w <= offset + l.strip_w + 0.5,
            "{offset} {start} {l:?}"
        );
        let _ = app.update(Message::SelectTab(0));
        assert_eq!(app.offset_of(wid), 0.0);
    }

    #[test]
    fn a_manual_scroll_is_kept_until_something_changes() {
        let (mut app, wid) = app_with_tabs(40, 800.0);
        let _ = app.update(Message::SelectTab(20));
        let at = app.offset_of(wid);
        app.strip_scrolled_by(wid, -100.0);
        let moved = app.offset_of(wid);
        assert_ne!(at, moved);
        // An unrelated update with the same active tab and layout leaves it.
        let _ = app.update(Message::TabBar(wid, TabBarMsg::StripScroll(0.0)));
        assert_eq!(app.offset_of(wid), moved);
        // A resized window shows the active tab again.
        app.sizes.insert(wid, (700.0, 600.0));
        let _ = app.update(Message::TabBar(wid, TabBarMsg::StripScroll(0.0)));
        let l = app.strip_layout_of(wid);
        let start = 20.0 * (l.tab_w + TAB_SPACING);
        let offset = app.offset_of(wid);
        assert!(offset <= start && start + l.tab_w <= offset + l.strip_w + 0.5);
    }

    #[test]
    fn tabs_on_the_left_leave_the_strip_alone() {
        let (mut app, wid) = app_with_tabs(40, 800.0);
        let _ = app.update(Message::SelectTab(30));
        assert!(app.strip_offset[&wid] > 0.0);
        let _ = app.update(Message::Shortcut(wid, Action::VerticalTabs));
        assert!(app.settings.tabs_left());
        assert!(!app.strip_offset.contains_key(&wid));
        assert!(!app.strip_seen.contains_key(&wid));
        let _ = app.update(Message::SelectTab(10));
        assert!(!app.strip_offset.contains_key(&wid), "no strip to scroll");
    }

    #[test]
    fn closing_the_window_forgets_the_strip() {
        let (mut app, wid) = app_with_tabs(40, 800.0);
        let _ = app.update(Message::SelectTab(30));
        assert!(app.strip_offset.contains_key(&wid) && app.strip_seen.contains_key(&wid));
        let _ = app.update(Message::WindowClosed(wid));
        assert!(!app.strip_offset.contains_key(&wid));
        assert!(!app.strip_seen.contains_key(&wid));
    }

    #[test]
    fn a_scroll_by_hand_survives_changes_that_are_not_the_active_tab_or_the_width() {
        let (mut app, wid) = app_with_tabs(40, 800.0);
        let _ = app.update(Message::SelectTab(20));
        app.strip_scrolled_by(wid, -1000.0);
        let moved = app.offset_of(wid);
        // Another tab opens at the end: the layout changes, the active tab does not.
        let extra = profile("extra");
        {
            let w = app.windows.get_mut(&wid).unwrap();
            w.push(Tab::single(Pane::for_profile(&extra)));
            // `push` shows the new tab; the active one stays here.
            w.active = 20;
        }
        let _ = app.update(Message::TabBar(wid, TabBarMsg::StripScroll(0.0)));
        assert_eq!(app.offset_of(wid), moved, "left where it was put");
    }

    #[test]
    fn a_narrower_strip_reveals_the_active_tab_again() {
        let (mut app, wid) = app_with_tabs(40, 800.0);
        let _ = app.update(Message::SelectTab(20));
        // Scroll so the active tab is the last one shown, then take room from the strip (here
        // by a resize; a tunnel count or the files button does it through `strip_w` the same way).
        let l = app.strip_layout_of(wid);
        let start = 20.0 * (l.tab_w + TAB_SPACING);
        app.strip_offset.insert(wid, start + l.tab_w - l.strip_w);
        app.sizes.insert(wid, (700.0, 600.0));
        let _ = app.update(Message::TabBar(wid, TabBarMsg::StripScroll(0.0)));
        let l = app.strip_layout_of(wid);
        let offset = app.offset_of(wid);
        assert!(
            offset <= start && start + l.tab_w <= offset + l.strip_w + 0.5,
            "{offset} {l:?}"
        );
    }

    #[test]
    fn overflowing_tabs_leave_room_to_drag_the_window() {
        let l = strip_layout(Chrome::Unified, false, Some(800.0), 40, 0.0);
        assert!(l.overflow);
        let used = Chrome::Unified.leading_inset(false) + l.strip_w + TAB_SPACING + CHEVRONS_WIDTH;
        assert!(800.0 - used >= DRAG_ROOM, "{l:?}");
    }

    #[test]
    fn a_scroll_by_hand_never_leaves_the_active_tab_cut() {
        for active in [0, 1, 7, 20, 38, 39] {
            let (mut app, wid) = app_with_tabs(40, 800.0);
            let _ = app.update(Message::SelectTab(active));
            let l = app.strip_layout_of(wid);
            let start = active as f32 * (l.tab_w + TAB_SPACING);
            let end = start + l.tab_w;
            for delta in [13.0, 40.0, -13.0, -40.0] {
                for _ in 0..200 {
                    app.strip_scrolled_by(wid, delta);
                    let o = app.offset_of(wid);
                    let shown = start >= o - 0.01 && end <= o + l.strip_w + 0.01;
                    let hidden = end <= o + 0.01 || start >= o + l.strip_w - 0.01;
                    assert!(shown || hidden, "tab {active}, delta {delta}, offset {o}");
                }
            }
            // The strip still reaches both ends exactly, by the wheel and by the chevrons.
            let max = l.content_w - l.strip_w;
            app.strip_scrolled_by(wid, 1e6);
            assert_eq!(app.offset_of(wid), max, "tab {active}");
            app.strip_scrolled_by(wid, -1e6);
            assert_eq!(app.offset_of(wid), 0.0, "tab {active}");
            for (dir, end) in [(1, max), (-1, 0.0)] {
                for _ in 0..50 {
                    app.strip_stepped(wid, dir);
                }
                assert_eq!(app.offset_of(wid), end, "tab {active}, chevron {dir}");
            }
        }
    }

    #[test]
    fn a_scrolling_strip_shows_whole_tabs_only() {
        for width in [400.0, 640.0, 800.0, 1013.0, 1500.0] {
            for count in [12, 40, 90] {
                let l = strip_layout(Chrome::Unified, false, Some(width), count, 0.0);
                if !l.overflow {
                    continue;
                }
                let tabs = (l.strip_w + TAB_SPACING) / (l.tab_w + TAB_SPACING);
                assert!((tabs - tabs.round()).abs() < 1e-3, "{width} {count} {l:?}");
                let end = (l.content_w - l.strip_w) / (l.tab_w + TAB_SPACING);
                assert!((end - end.round()).abs() < 1e-3, "{width} {count} {l:?}");
            }
        }
    }

    #[test]
    fn a_bar_too_narrow_for_the_chevrons_shrinks_its_tabs() {
        // 30 tabs leave about 105 px: no chevrons, the strip takes it all and scrolls.
        let l = strip_layout(
            Chrome::Unified,
            false,
            Some(400.0),
            30,
            trailing_width(3, true),
        );
        assert!(l.overflow && !l.chevrons, "{l:?}");
        assert!(
            l.strip_w >= TAB_MIN_WIDTH && l.tab_w == TAB_TIGHT_WIDTH,
            "{l:?}"
        );
        // Two tabs in about 118 px share it without scrolling.
        let room = 118.0;
        let width = room + Chrome::Unified.leading_inset(false) + DRAG_ROOM;
        let l = strip_layout(Chrome::Unified, false, Some(width), 2, 0.0);
        assert!(!l.overflow && !l.chevrons, "{l:?}");
        assert!(
            l.content_w <= l.strip_w + 0.5 && l.tab_w < TAB_MIN_WIDTH,
            "{l:?}"
        );
        // Under 48 px of room, one tab: it shows whole, the strip's width.
        let width = 48.0 + Chrome::Unified.leading_inset(false) + DRAG_ROOM;
        let l = strip_layout(Chrome::Unified, false, Some(width), 1, 0.0);
        assert!(!l.overflow && !l.chevrons, "{l:?}");
        assert!(
            l.tab_w > 0.0 && l.tab_w <= l.strip_w && l.strip_w <= 48.0,
            "{l:?}"
        );
        // No room at all: nothing shows, nothing is zero wide.
        let l = strip_layout(Chrome::Unified, false, Some(10.0), 3, 0.0);
        assert_eq!(l.strip_w, 0.0);
        assert!(l.tab_w > 0.0 && !l.chevrons);
    }

    #[test]
    fn a_hover_switch_while_carrying_leaves_the_strip_still() {
        use crate::windows::DragMsg;
        let (mut app, wid) = app_with_tabs(40, 800.0);
        let _ = app.update(Message::SelectTab(0));
        let carried = app.windows[&wid].tabs[0].id;
        let _ = app.update(Message::Drag(wid, DragMsg::StartTab(carried)));
        let _ = app.update(Message::Drag(wid, DragMsg::OverTab(Some(39))));
        let generation = app.hover_generation;
        let _ = app.update(Message::Drag(wid, DragMsg::HoverElapsed(generation)));
        assert_eq!(app.windows[&wid].active, 39);
        assert_eq!(app.offset_of(wid), 0.0, "no scroll under the carried tab");
        // The carry ends on the tab shown: now it is revealed.
        app.carry = None;
        let _ = app.update(Message::TabBar(wid, TabBarMsg::StripScroll(0.0)));
        assert!(app.offset_of(wid) > 0.0);
    }

    #[test]
    fn moving_the_active_tab_reveals_it_again() {
        let (mut app, wid) = app_with_tabs(40, 800.0);
        let _ = app.update(Message::SelectTab(0));
        assert_eq!(app.offset_of(wid), 0.0);
        // The active tab is reordered to the end: same tab, new index.
        {
            let w = app.windows.get_mut(&wid).unwrap();
            w.tabs.swap(0, 39);
            w.active = 39;
        }
        let _ = app.update(Message::TabBar(wid, TabBarMsg::StripScroll(0.0)));
        let l = app.strip_layout_of(wid);
        let start = 39.0 * (l.tab_w + TAB_SPACING);
        let offset = app.offset_of(wid);
        assert!(
            offset <= start && start + l.tab_w <= offset + l.strip_w + 0.5,
            "{offset}"
        );
    }
}
