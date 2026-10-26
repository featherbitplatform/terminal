//! The terminal widget: draws a terminal grid and turns mouse and keyboard into terminal input.
//!
//! The widget is drawn with iced's own primitives (quads and text runs, see `draw`). Input handling
//! lives in `input`. The terminal is locked only while building the draw list or handling an event.

mod colors;
mod draw;
mod geometry;
mod input;

use std::cell::RefCell;
use std::sync::Arc;

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
pub use geometry::{grid_size_from_bounds, pixel_to_cell, pixel_to_point};

/// How a terminal is drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalStyle {
    pub scheme: ColorScheme,
    pub font: Font,
    pub font_size: f32,
    /// When set, a 1 px border in this color (e.g. the active pane of a split).
    pub accent: Option<Rgb>,
    /// Keyboard input goes to the terminal and the cursor is solid only when focused.
    pub focused: bool,
    /// Opacity of the default background, `0.0` to `1.0`. Cells with their own background, the
    /// selection, the cursor and text stay opaque. At `0.0` the default background is not drawn
    /// at all, for a host that paints it behind the widget.
    pub background_opacity: f32,
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
}

/// A terminal view. Locks `term` only while drawing or handling an event.
pub fn terminal_view<'a, Message: 'a>(
    term: &'a SharedTerminal,
    style: &'a TerminalStyle,
    on_event: impl Fn(TerminalOutput) -> Message + 'a,
) -> Element<'a, Message> {
    highlighted_terminal_view(term, style, None, on_event)
}

/// A terminal view showing search matches (see [`Highlight`]).
pub fn highlighted_terminal_view<'a, Message: 'a>(
    term: &'a SharedTerminal,
    style: &'a TerminalStyle,
    highlight: Option<Highlight>,
    on_event: impl Fn(TerminalOutput) -> Message + 'a,
) -> Element<'a, Message> {
    Element::new(TerminalView {
        term,
        style,
        highlight,
        on_event: Box::new(on_event),
    })
}

struct TerminalView<'a, Message> {
    term: &'a SharedTerminal,
    style: &'a TerminalStyle,
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
}

/// Cell size measured for a font and size.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Measured {
    font: Font,
    font_size: f32,
    cell: Size,
}

#[derive(Default)]
struct State {
    measured: Option<Measured>,
    cache: RefCell<Option<(CacheKey, draw::DrawList)>>,
    /// Reads the rows in view while building the draw list; kept across frames so a view
    /// scrolled into the history reuses its scratch row.
    reader: RefCell<RowReader<Cell>>,
    /// Terminal and grid size last synced to the terminal, to avoid locking on every event.
    synced: Option<(usize, GridSize)>,
    input: input::InputState,
    /// Terminal the input state belongs to.
    input_term: Option<usize>,
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
    fn sync_size(&self, state: &mut State, bounds: Rectangle, shell: &mut Shell<'_, Message>) {
        let Some(m) = state.measured else { return };
        if bounds.width < 1.0 || bounds.height < 1.0 {
            return;
        }
        let grid = grid_size_from_bounds(bounds.size(), m.cell);
        let id = term_id(self.term);
        if state.synced == Some((id, grid)) {
            return;
        }
        state.synced = Some((id, grid));
        let mut term = self.term.lock();
        if term.size() != grid {
            term.resize(grid);
            drop(term);
            shell.publish((self.on_event)(TerminalOutput::Resized(grid)));
            shell.request_redraw();
        }
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
        let (font, font_size) = (self.style.font, self.style.font_size);
        if state
            .measured
            .is_none_or(|m| m.font != font || m.font_size != font_size)
        {
            state.measured = Some(Measured {
                font,
                font_size,
                cell: measure(font, font_size),
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
        let id = term_id(self.term);
        if state.input_term != Some(id) {
            state.input.reset();
            state.input_term = Some(id);
        }
        let bounds = layout.bounds();
        self.sync_size(state, bounds, shell);
        let Some(m) = state.measured else { return };
        let grid = grid_size_from_bounds(bounds.size(), m.cell);
        let mut out = Vec::new();
        let ctx = input::Context {
            term: self.term,
            focused: self.style.focused,
            bounds,
            grid,
            cursor,
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
            let key = CacheKey {
                term: term_id(self.term),
                generation: term.generation(),
                focused: self.style.focused,
                scheme: self.style.scheme,
                hover: state.input.hover().cloned(),
                highlight: self.highlight.clone(),
            };
            if cache.as_ref().is_none_or(|(k, _)| *k != key) {
                let list = draw::build(
                    &mut state.reader.borrow_mut(),
                    &term,
                    &self.style.scheme,
                    key.focused,
                    key.hover.as_ref(),
                    key.highlight.as_ref(),
                );
                *cache = Some((key, list));
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
                },
                self.style.accent,
                self.style.background_opacity,
            );
        }
    }
}
