//! The terminal widget: draws a terminal grid and turns mouse and keyboard into terminal input.
//!
//! The widget is drawn with iced's own primitives (quads and text runs, see `draw`). Input handling
//! lives in `input`. The terminal is locked only while building the draw list or handling an event.

mod colors;
mod draw;
mod features;
mod geometry;
mod images;
mod input;
mod shaped;

use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::{Arc, Weak};

use fbterm_config::Rgb;
use feathercritty::grid::RowReader;
use feathercritty::term::cell::{Cell, Hyperlink};
use iced::advanced::layout::{self, Layout};
use iced::advanced::text::{self, Paragraph as _};
use iced::advanced::widget::{self, Widget};
use iced::advanced::{Clipboard, Shell, renderer};
use iced::{Element, Event, Font, Length, Pixels, Rectangle, Size, mouse};

use crate::palette::ColorScheme;
use crate::{GridSize, SearchMatch, SearchPattern, SharedTerminal};

pub use colors::cell_colors;
pub use draw::{DrawList, build as build_draw_list};
pub use features::FontFeatures;
pub use geometry::{grid_size_from_bounds, pixel_to_cell, pixel_to_point};
pub use images::CellGeometry;
pub use input::paste_input;

/// How a terminal is drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalStyle {
    pub scheme: ColorScheme,
    pub font: Font,
    pub font_size: f32,
    /// Ligatures and other OpenType features for the grid text.
    pub features: FontFeatures,
    /// When set, a 1 px border in this color (e.g. the active pane of a split).
    pub accent: Option<Rgb>,
    /// Keyboard input goes to the terminal and the cursor is solid only when focused.
    pub focused: bool,
    /// Opacity of the default background, `0.0` to `1.0`. Cells with their own background, the
    /// selection, the cursor and text stay opaque. At `0.0` the default background is not drawn
    /// at all, for a host that paints it behind the widget.
    pub background_opacity: f32,
    /// Physical pixels per logical pixel of the window this terminal sits in. The grid the view
    /// reports carries it, so programs are told physical pixels; cells stay logical, which is what
    /// the drawing uses. `0.0` means the window has not reported a scale yet: a view given that
    /// reports no grid at all, because every pixel number in it would be wrong (see
    /// [`crate::Terminal::hold_until_sized`]).
    pub scale: f32,
}

impl<'a> From<&'a TerminalStyle> for Cow<'a, TerminalStyle> {
    fn from(style: &'a TerminalStyle) -> Self {
        Cow::Borrowed(style)
    }
}

impl From<TerminalStyle> for Cow<'_, TerminalStyle> {
    fn from(style: TerminalStyle) -> Self {
        Cow::Owned(style)
    }
}

/// Search matches to show: every match of `pattern` in view tinted with `match_bg`, the
/// `current` one drawn in `current_fg` on `current_bg`.
#[derive(Debug, Clone, PartialEq)]
pub struct Highlight {
    pub pattern: SearchPattern,
    pub current: Option<SearchMatch>,
    pub match_bg: Rgb,
    pub current_bg: Rgb,
    pub current_fg: Rgb,
}

/// What the widget asks of its host.
#[derive(Debug, Clone, PartialEq)]
pub enum TerminalOutput {
    /// Bytes to write to the session (keys, paste, mouse reports).
    Input(Vec<u8>),
    /// The grid size changed; the widget already resized the terminal, the host resizes the session.
    Resized(GridSize),
    /// Text was copied to the clipboard.
    Copy(String),
    /// An OSC 8 hyperlink was activated (Cmd+click on macOS, Ctrl+click elsewhere).
    OpenUrl(String),
    /// The user right-clicked (see [`RightClick`]): show the context menu at this point of
    /// the window, its Copy enabled when `can_copy` (a selection is there to copy).
    ContextMenu { at: iced::Point, can_copy: bool },
}

/// What a right-click the program does not take does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RightClick {
    /// Copy the selection and clear it; with nothing selected, open the context menu
    /// (Windows and Linux).
    CopyOrMenu,
    /// Open the context menu, keeping the selection (macOS).
    Menu,
}

/// This platform's right-click: the menu on macOS, copy-or-menu on Windows and Linux.
pub const RIGHT_CLICK: RightClick = if cfg!(target_os = "macos") {
    RightClick::Menu
} else {
    RightClick::CopyOrMenu
};

/// A terminal view. Locks `term` only while drawing or handling an event.
pub fn terminal_view<'a, Message: 'a>(
    term: &'a SharedTerminal,
    style: &'a TerminalStyle,
    on_event: impl Fn(TerminalOutput) -> Message + 'a,
) -> Element<'a, Message> {
    highlighted_terminal_view(term, style, None, on_event)
}

/// A terminal view showing search matches (see [`Highlight`]). `style` is borrowed
/// or owned (e.g. a per-pane style built for this view).
pub fn highlighted_terminal_view<'a, Message: 'a>(
    term: &'a SharedTerminal,
    style: impl Into<Cow<'a, TerminalStyle>>,
    highlight: Option<Highlight>,
    on_event: impl Fn(TerminalOutput) -> Message + 'a,
) -> Element<'a, Message> {
    Element::new(TerminalView {
        term,
        style: style.into(),
        highlight,
        on_event: Box::new(on_event),
    })
}

struct TerminalView<'a, Message> {
    term: &'a SharedTerminal,
    style: Cow<'a, TerminalStyle>,
    highlight: Option<Highlight>,
    on_event: Box<dyn Fn(TerminalOutput) -> Message + 'a>,
}

/// Inputs of a cached draw list; when unchanged the list is reused.
#[derive(PartialEq)]
struct CacheKey {
    term: usize,
    generation: u64,
    focused: bool,
    scheme: ColorScheme,
    hover: Option<Hyperlink>,
    highlight: Option<Highlight>,
    /// The cell geometry pictures are laid out with, as bits: a font resize bumps `generation`
    /// through the grid resize it triggers, but moving the window to another display changes only
    /// this scale — no relayout, no terminal event — and placements drawn at the old physical cell
    /// size would sit wrong on their cells until the next bump.
    cell_bits: (u32, u32),
    scale_bits: u32,
    /// [`crate::images::ImageStore::generation`] for this terminal's image owner: an eviction or a
    /// refusal bumps it and wakes the pane, and reusing the cached frame then would repaint — and
    /// `touch_drawn`-re-stamp — exactly the picture the store just freed, its cloned handle keeping
    /// those bytes alive outside the store's books. The key is read in the same lock section as the
    /// `get`/`touch_drawn` calls it must agree with.
    store_generation: u64,
}

/// Whether a cached frame may be reused for this redraw instead of being rebuilt.
///
/// Same key means nothing the drawer could see has changed — not the terminal, not the theme or
/// geometry, and not the store's epoch for its pictures (`CacheKey::store_generation`, which an
/// eviction or refusal bumps). A frame that left a picture `pending` is never reused: the answer
/// may have arrived, and only asking again draws it (or its placeholder).
fn frame_is_current(cached: &CacheKey, current: &CacheKey, pending: bool) -> bool {
    cached == current && !pending
}

/// Cell size measured for a font and size.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Measured {
    font: Font,
    font_size: f32,
    cell: Size,
    /// [`font_epoch`] when measured.
    epoch: u64,
}

/// Counts font loads after startup (see [`fonts_loaded`]).
static FONT_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many times fonts were added to the renderer after startup.
pub fn font_epoch() -> u64 {
    FONT_EPOCH.load(std::sync::atomic::Ordering::Relaxed)
}

/// Fonts were added to the renderer: every terminal view measures its cell again and drops
/// its shaped text at its next layout, since a family it names may now resolve to a real face
/// instead of the fallback. A view whose grid changes reports it as for a font change.
pub fn fonts_loaded() {
    FONT_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Whether a view measured as `measured` must measure again for `font` at `font_size`.
fn needs_measure(measured: Option<Measured>, font: Font, font_size: f32) -> bool {
    measured.is_none_or(|m| m.font != font || m.font_size != font_size || m.epoch != font_epoch())
}

#[derive(Default)]
struct State {
    measured: Option<Measured>,
    cache: RefCell<Option<(CacheKey, draw::DrawList)>>,
    /// Reads the rows in view while building the draw list; kept across frames so a view
    /// scrolled into the history reuses its scratch row.
    reader: RefCell<RowReader<Cell>>,
    /// Grid text shaped with its features, kept across frames.
    shapes: RefCell<shaped::ShapeCache>,
    /// Grid size last synced to the terminal, to avoid locking on every event.
    synced: Option<GridSize>,
    input: input::InputState,
    /// The terminal this state belongs to. A `Weak` keeps its allocation reserved, so a new
    /// terminal (a pane restart frees one and allocates the next, of the same size) can never
    /// take its address and inherit `synced`, the input state or the draw cache.
    held: Option<Weak<parking_lot::Mutex<crate::Terminal>>>,
}

impl State {
    /// Make `term` the state's terminal, forgetting everything kept for a previous one.
    fn hold(&mut self, term: &SharedTerminal) {
        if self
            .held
            .as_ref()
            .is_some_and(|w| std::ptr::eq(w.as_ptr(), Arc::as_ptr(term)))
        {
            return;
        }
        self.held = Some(Arc::downgrade(term));
        self.synced = None;
        self.input.reset();
        *self.cache.borrow_mut() = None;
    }
}

fn term_id(term: &SharedTerminal) -> usize {
    Arc::as_ptr(term) as usize
}

/// Cell size of `font`: advance of "M" and a 1.2 line height, both rounded to whole pixels.
fn measure(font: Font, font_size: f32) -> Size {
    const PROBE: &str = "MMMMMMMMMMMMMMMMMMMM";
    let p = <iced::Renderer as text::Renderer>::Paragraph::with_text(text::Text {
        content: PROBE,
        bounds: Size::INFINITE,
        size: Pixels(font_size),
        line_height: text::LineHeight::Relative(1.2),
        font,
        align_x: text::Alignment::Left,
        align_y: iced::alignment::Vertical::Top,
        shaping: text::Shaping::Advanced,
        wrapping: text::Wrapping::None,
    });
    let width = p.min_bounds().width / PROBE.len() as f32;
    Size::new(width.max(1.0), (font_size * 1.2).ceil().max(1.0))
}

impl<Message> TerminalView<'_, Message> {
    /// Keep the terminal's grid in step with the widget bounds.
    ///
    /// `style.scale` is `0.0` until the window has reported a scale (see [`TerminalStyle::scale`]).
    /// Until then there is no honest grid to hand over: its cells would tell programs half or twice
    /// the pixels they really have, and a picture parsed against them keeps the wrong span for good.
    /// So nothing is reported, and output held for this pane's first real grid keeps waiting (see
    /// [`crate::Terminal::hold_until_sized`]). Because no provisional grid is ever recorded as
    /// synced, the first pass that has a real scale always runs, whatever that scale turns out to
    /// be, 1.0 included: that one would otherwise look like nothing had changed.
    fn sync_size(&self, state: &mut State, bounds: Rectangle, shell: &mut Shell<'_, Message>) {
        let Some(m) = state.measured else { return };
        if bounds.width < 1.0 || bounds.height < 1.0 {
            return;
        }
        if self.style.scale <= 0.0 {
            return;
        }
        let grid = grid_size_from_bounds(bounds.size(), m.cell, self.style.scale);
        if state.synced == Some(grid) {
            return;
        }
        state.synced = Some(grid);
        let mut term = self.term.lock();
        // Resizing is also what parses the output held for this pane's first real grid. When the
        // terminal already has exactly this grid - its session started at the size the layout turned
        // out to need - nothing resizes, so `sized` releases the hold; parsing what waited changed
        // the screen, which is why that case reports and redraws too.
        let changed = if term.size() != grid {
            term.resize(grid);
            true
        } else {
            // The terminal already had this grid, so nothing resizes and `sized` is what releases a
            // hold armed for this pane's first real grid. Report that release even when nothing was
            // waiting to parse: it is the app's only notice that this pane laid out at all - its map
            // of reported grids is where the next new pane's cells come from, and without a report
            // nobody asks for what a release queued, so an answer parsed at this moment would sit in
            // the terminal until the next resize or feed.
            let released = term.holding();
            term.sized();
            released
        };
        drop(term);
        // Report only what changed: a view that agrees with its terminal says nothing, which is how
        // the Color schemes preview avoids rebuilding its sample every time it is laid out. A hold
        // that released here did change the screen, so it counts as changed and is reported too.
        if !changed {
            return;
        }
        shell.publish((self.on_event)(TerminalOutput::Resized(grid)));
        shell.request_redraw();
    }
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for TerminalView<'_, Message> {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<State>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(State::default())
    }

    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State>();
        state.hold(self.term);
        let (font, font_size) = (self.style.font, self.style.font_size);
        if needs_measure(state.measured, font, font_size) {
            let epoch = font_epoch();
            if state.measured.is_some_and(|m| m.epoch != epoch) {
                // New faces: text shaped with the fallback is stale.
                *state.shapes.borrow_mut() = shaped::ShapeCache::default();
                *state.cache.borrow_mut() = None;
            }
            state.measured = Some(Measured {
                font,
                font_size,
                cell: measure(font, font_size),
                epoch,
            });
            state.synced = None;
        }
        layout::Node::new(limits.max())
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        state.hold(self.term);
        let bounds = layout.bounds();
        self.sync_size(state, bounds, shell);
        let Some(m) = state.measured else { return };
        let grid = grid_size_from_bounds(bounds.size(), m.cell, self.style.scale);
        let mut out = Vec::new();
        let ctx = input::Context {
            term: self.term,
            focused: self.style.focused,
            bounds,
            grid,
            cursor,
            right_click: RIGHT_CLICK,
        };
        let effect = state.input.handle(&ctx, event, clipboard, &mut out);
        for o in out {
            shell.publish((self.on_event)(o));
        }
        if effect.redraw {
            shell.request_redraw();
        }
        if effect.capture {
            shell.capture_event();
        }
    }

    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if !cursor.is_over(layout.bounds()) {
            return mouse::Interaction::None;
        }
        let state = tree.state.downcast_ref::<State>();
        if state.input.link_armed() {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::Text
        }
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut iced::Renderer,
        _theme: &iced::Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let Some(m) = state.measured else { return };
        let mut cache = state.cache.borrow_mut();
        {
            let term = self.term.lock();
            // Pictures are laid out with the grid the terminal itself reports: its cell size and its
            // sanitized scale, which is what programs are told and what `grid_size_from_bounds`
            // settled (a bogus `style.scale` must not put images at different pixels than text).
            let grid = term.size();
            let geometry =
                images::CellGeometry::new(Size::new(grid.cell_width, grid.cell_height), grid.scale);
            let key = CacheKey {
                term: term_id(self.term),
                generation: term.generation(),
                focused: self.style.focused,
                scheme: self.style.scheme,
                hover: state.input.hover().cloned(),
                highlight: self.highlight.clone(),
                cell_bits: (
                    geometry.cell.width.to_bits(),
                    geometry.cell.height.to_bits(),
                ),
                scale_bits: geometry.scale.to_bits(),
                // Read here, in this same lock section as the `get`/`touch_drawn` calls below it:
                // an eviction or refusal bumps it (under the store's lock, before its wake goes
                // out), so the redraw that wake asks for rebuilds instead of repainting the frame
                // that holds the freed picture. Terminal-then-store is the allowed lock order.
                store_generation: crate::images::store().generation(term.image_owner()),
            };
            // A frame that left a picture waiting is rebuilt instead of reused: the store has had
            // its chance to answer since (its `wake` hook is what asked for this redraw).
            let stale = cache
                .as_ref()
                .is_none_or(|(k, l)| !frame_is_current(k, &key, l.images.pending));
            if stale {
                let list = draw::build(
                    &mut state.reader.borrow_mut(),
                    &term,
                    &self.style.scheme,
                    key.focused,
                    key.hover.as_ref(),
                    key.highlight.as_ref(),
                    crate::images::store(),
                    geometry,
                );
                *cache = Some((key, list));
            } else if let Some((_, list)) = cache.as_ref() {
                // Reused frame: the pictures it holds were not asked for this time, so tell the
                // store they are on screen — otherwise a static pane's images age out of the LRU
                // while plainly visible (see [`crate::images::ImageStore::touch_drawn`]).
                let keys: Vec<_> = list.images.drawn_keys().collect();
                crate::images::store().touch_drawn(term.image_owner(), &keys);
            }
        }
        if let Some((_, list)) = cache.as_ref() {
            draw::render(
                list,
                renderer,
                layout.bounds(),
                draw::Metrics {
                    cell: m.cell,
                    font: m.font,
                    font_size: m.font_size,
                    features: self.style.features.clone(),
                },
                self.style.accent,
                self.style.background_opacity,
                &mut state.shapes.borrow_mut(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term() -> SharedTerminal {
        let size = GridSize {
            cols: 80,
            rows: 24,
            cell_width: 8.0,
            cell_height: 16.0,
            scale: 1.0,
        };
        Arc::new(parking_lot::Mutex::new(crate::Terminal::new(size, 10)))
    }

    /// The frame-cache decision, as a pure function over two keys. A store eviction or refusal for
    /// this pane's images bumps `store_generation` and wakes the redraw; reusing the cached frame
    /// then would repaint — and `touch_drawn`-re-stamp — exactly the picture the store just freed.
    #[test]
    fn a_store_epoch_change_invalidates_the_frame_cache() {
        let key = |store_generation| CacheKey {
            term: 7,
            generation: 3,
            focused: true,
            scheme: ColorScheme::graphite(),
            hover: None,
            highlight: None,
            cell_bits: (8.0f32.to_bits(), 16.0f32.to_bits()),
            scale_bits: 1.0f32.to_bits(),
            store_generation,
        };
        assert!(
            frame_is_current(&key(4), &key(4), false),
            "nothing moved: the frame is reused"
        );
        assert!(
            !frame_is_current(&key(4), &key(5), false),
            "the store changed what this pane can show: rebuild, dropping the stale handles"
        );
        assert!(
            !frame_is_current(&key(4), &key(4), true),
            "a frame that left a picture waiting is never reused"
        );
    }

    /// A view given a new terminal forgets the size it synced to the old one, so the new
    /// terminal is resized to the view; the old allocation stays reserved until then, so the
    /// new terminal cannot share its address.
    #[test]
    fn a_new_terminal_is_synced_again() {
        let synced = GridSize {
            cols: 100,
            rows: 30,
            cell_width: 8.0,
            cell_height: 16.0,
            scale: 1.0,
        };
        let mut state = State::default();
        let first = term();
        state.hold(&first);
        state.synced = Some(synced);
        state.hold(&first);
        assert_eq!(state.synced, Some(synced), "same terminal: kept");
        let old = Arc::as_ptr(&first);
        drop(first);
        let second = term();
        assert!(
            !std::ptr::eq(old, Arc::as_ptr(&second)),
            "address still reserved"
        );
        state.hold(&second);
        assert_eq!(state.synced, None, "new terminal: synced again");
    }

    fn style_at(scale: f32) -> TerminalStyle {
        TerminalStyle {
            scheme: ColorScheme::graphite(),
            font: Font::with_name("Sample Mono"),
            font_size: 14.0,
            features: FontFeatures::default(),
            accent: None,
            focused: true,
            background_opacity: 1.0,
            scale,
        }
    }

    /// What `view` publishes for one `sync_size` pass at `bounds`.
    fn sync(
        view: &TerminalView<'_, TerminalOutput>,
        state: &mut State,
        bounds: Rectangle,
    ) -> Vec<TerminalOutput> {
        let mut out = Vec::new();
        let mut shell = Shell::new(&mut out);
        view.sync_size(state, bounds, &mut shell);
        out
    }

    /// A window moving to a display of another scale changes nothing the widget draws with: same
    /// bounds, same logical cells, so the same grid of cells. It changes what programs are told,
    /// so the grid must carry the new scale and reach the terminal.
    #[test]
    fn a_scale_change_makes_the_view_report_a_new_grid() {
        let term = term();
        let mut state = State::default();
        state.hold(&term);
        state.measured = Some(Measured {
            font: Font::with_name("Sample Mono"),
            font_size: 14.0,
            cell: Size::new(8.0, 16.0),
            epoch: font_epoch(),
        });
        // 100x30 cells of 8x16 logical px: bigger than the terminal's own 80x24, so the first
        // layout has something to report.
        let bounds = Rectangle::new(iced::Point::new(0.0, 0.0), Size::new(800.0, 480.0));

        let at_one = style_at(1.0);
        let view = TerminalView {
            term: &term,
            style: Cow::Borrowed(&at_one),
            highlight: None,
            on_event: Box::new(|out| out),
        };
        let out = sync(&view, &mut state, bounds);
        let [TerminalOutput::Resized(first)] = out[..] else {
            panic!("expected one resize, got {out:?}");
        };
        assert_eq!((first.cols, first.rows), (100, 30));
        assert_eq!(first.scale, 1.0);
        assert!(
            sync(&view, &mut state, bounds).is_empty(),
            "the same grid is not reported twice"
        );

        // The window moved to a 2x display: the layout re-runs with the new scale.
        let at_two = style_at(2.0);
        let view = TerminalView {
            term: &term,
            style: Cow::Borrowed(&at_two),
            highlight: None,
            on_event: Box::new(|out| out),
        };
        let out = sync(&view, &mut state, bounds);
        let [TerminalOutput::Resized(second)] = out[..] else {
            panic!("expected one resize for the scale change, got {out:?}");
        };
        assert_eq!(
            (second.cols, second.rows),
            (100, 30),
            "cells are logical: the grid of cells is unchanged"
        );
        assert_eq!(second.scale, 2.0);

        // The terminal took the new grid, and its cell pixels doubled with it.
        let mut t = term.lock();
        assert_eq!(t.size(), second);
        t.feed(b"\x1b[16t");
        let answer: String = String::from_utf8(
            t.take_events()
                .into_iter()
                .filter_map(|e| match e {
                    crate::TermEvent::PtyWrite(b) => Some(b),
                    _ => None,
                })
                .flatten()
                .collect::<Vec<u8>>(),
        )
        .unwrap();
        assert_eq!(answer, "\x1b[6;32;16t", "cells of 16x32 physical px");
    }

    /// The window's scale arrives after it opens, asynchronously. Until then every pixel number a
    /// grid could carry is wrong (a 2x display would be told 1x), so the view reports nothing at
    /// all: neither the terminal nor the app hears a provisional size, and output held for the pane's
    /// first real grid (`Terminal::hold_until_sized`) keeps waiting. The first pass that has a scale
    /// runs whatever it says - 1.0 included, which would otherwise look like nothing had changed.
    #[test]
    fn a_view_reports_nothing_until_the_window_reports_its_scale() {
        let term = Arc::new(parking_lot::Mutex::new(crate::Terminal::new(
            GridSize {
                cols: 80,
                rows: 24,
                cell_width: 8.0,
                cell_height: 16.0,
                scale: 1.0,
            },
            10,
        )));
        term.lock().hold_until_sized();
        let mut state = State::default();
        state.hold(&term);
        state.measured = Some(Measured {
            font: Font::with_name("Sample Mono"),
            font_size: 14.0,
            cell: Size::new(8.0, 16.0),
            epoch: font_epoch(),
        });
        let bounds = Rectangle::new(iced::Point::new(0.0, 0.0), Size::new(800.0, 480.0));
        term.lock().feed(b"ready");

        let unknown = style_at(0.0);
        let unknown_view = TerminalView {
            term: &term,
            style: Cow::Borrowed(&unknown),
            highlight: None,
            on_event: Box::new(|out| out),
        };
        assert!(
            sync(&unknown_view, &mut state, bounds).is_empty(),
            "a grid whose pixels are guesses is not worth reporting"
        );
        assert_eq!(
            term.lock().size().cols,
            80,
            "the terminal keeps the size it started at"
        );
        assert_eq!(first_cell(&term), ' ', "and its output still waits");

        // The window reports 2x. Cells stay cells; now the pixels are real, so this pass counts.
        let at_two = style_at(2.0);
        let two_view = TerminalView {
            term: &term,
            style: Cow::Borrowed(&at_two),
            highlight: None,
            on_event: Box::new(|out| out),
        };
        let out = sync(&two_view, &mut state, bounds);
        let [TerminalOutput::Resized(grid)] = &out[..] else {
            panic!("expected one resize once the scale is known, got {out:?}");
        };
        assert_eq!((grid.cols, grid.rows, grid.scale), (100, 30, 2.0));
        assert_eq!(
            first_cell(&term),
            'r',
            "the held output was parsed at these cells"
        );
        // And it is answered from them: a `CSI 16 t` after the release reports 16x32 px cells.
        let answer = ask(&term, b"\x1b[16t");
        assert_eq!(answer, "\x1b[6;32;16t");
    }

    /// A pane whose terminal was started at exactly the grid its first layout needs: nothing
    /// resizes, and a view that only resized on a change would hold its output forever. The first
    /// pass releases it and reports the size, so the app delivers what parsing produced.
    #[test]
    fn the_first_sync_releases_held_output_even_when_the_grid_matches() {
        let term = Arc::new(parking_lot::Mutex::new(crate::Terminal::new(
            GridSize {
                cols: 80,
                rows: 24,
                cell_width: 8.0,
                cell_height: 16.0,
                scale: 1.0,
            },
            10,
        )));
        term.lock().hold_until_sized();
        let mut state = State::default();
        state.hold(&term);
        state.measured = Some(Measured {
            font: Font::with_name("Sample Mono"),
            font_size: 14.0,
            cell: Size::new(8.0, 16.0),
            epoch: font_epoch(),
        });
        // Bounds that fit exactly the 80x24 grid the terminal already has.
        let bounds = Rectangle::new(iced::Point::new(0.0, 0.0), Size::new(640.0, 384.0));
        term.lock().feed(b"ready");

        let style = style_at(1.0);
        let view = TerminalView {
            term: &term,
            style: Cow::Borrowed(&style),
            highlight: None,
            on_event: Box::new(|out| out),
        };
        assert_eq!(term.lock().size(), grid_at(80, 24, 1.0));
        let out = sync(&view, &mut state, bounds);
        let [TerminalOutput::Resized(grid)] = &out[..] else {
            panic!("the size is unchanged but output landed: expected one report, got {out:?}");
        };
        assert_eq!(*grid, grid_at(80, 24, 1.0));
        assert_eq!(first_cell(&term), 'r');

        // Nothing new to say on the next pass at the same bounds and scale.
        assert!(sync(&view, &mut state, bounds).is_empty());
    }

    /// The equal-grid pass even with nothing waiting to parse is still reported, once. That report is
    /// how the app learns this pane's grid at all - its map of reported grids is where the next new
    /// pane's cells come from - while a terminal that never held anything (the Color schemes preview,
    /// whose sample is rebuilt at the grid it already has) still has nothing to say.
    #[test]
    fn a_release_with_nothing_waiting_is_still_reported_once() {
        let term = Arc::new(parking_lot::Mutex::new(crate::Terminal::new(
            GridSize {
                cols: 80,
                rows: 24,
                cell_width: 8.0,
                cell_height: 16.0,
                scale: 1.0,
            },
            10,
        )));
        term.lock().hold_until_sized();
        let mut state = State::default();
        state.hold(&term);
        state.measured = Some(Measured {
            font: Font::with_name("Sample Mono"),
            font_size: 14.0,
            cell: Size::new(8.0, 16.0),
            epoch: font_epoch(),
        });
        let bounds = Rectangle::new(iced::Point::new(0.0, 0.0), Size::new(640.0, 384.0));

        let style = style_at(1.0);
        let view = TerminalView {
            term: &term,
            style: Cow::Borrowed(&style),
            highlight: None,
            on_event: Box::new(|out| out),
        };
        let out = sync(&view, &mut state, bounds);
        let [TerminalOutput::Resized(grid)] = &out[..] else {
            panic!("a hold released at the same size is still this pane laying out, got {out:?}");
        };
        assert_eq!(*grid, grid_at(80, 24, 1.0));

        // Nothing new to say afterwards: the hold is gone, so an equal grid is silence again.
        assert!(sync(&view, &mut state, bounds).is_empty());
    }

    /// The first cell of screen row 0.
    fn first_cell(term: &SharedTerminal) -> char {
        use feathercritty::index::{Column, Line};
        term.lock().term().grid().screen()[Line(0)][Column(0)].c
    }

    /// What the terminal answers one query, as text.
    fn ask(term: &SharedTerminal, query: &[u8]) -> String {
        let mut t = term.lock();
        t.feed(query);
        let bytes: Vec<u8> = t
            .take_events()
            .into_iter()
            .filter_map(|e| match e {
                crate::TermEvent::PtyWrite(b) => Some(b),
                _ => None,
            })
            .flatten()
            .collect();
        String::from_utf8(bytes).unwrap()
    }

    fn grid_at(cols: u16, rows: u16, scale: f32) -> GridSize {
        GridSize {
            cols,
            rows,
            cell_width: 8.0,
            cell_height: 16.0,
            scale,
        }
    }

    #[test]
    fn a_font_load_makes_views_measure_again() {
        let font = Font::with_name("Sample Mono");
        let epoch = font_epoch();
        let m = Measured {
            font,
            font_size: 13.0,
            cell: Size::new(8.0, 16.0),
            epoch,
        };
        assert!(!needs_measure(Some(m), font, 13.0));
        assert!(needs_measure(None, font, 13.0));
        assert!(needs_measure(Some(m), Font::MONOSPACE, 13.0));
        assert!(needs_measure(Some(m), font, 14.0));
        fonts_loaded();
        assert!(font_epoch() > epoch);
        assert!(needs_measure(Some(m), font, 13.0), "same font, new faces");
    }
}
