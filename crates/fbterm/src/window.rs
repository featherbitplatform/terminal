//! One window's view: tab bar, notices and the active tab's terminal.

use std::borrow::Cow;
use std::path::Path;

use fbterm_term::highlighted_terminal_view;
use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::widget::{button, column, container, row, scrollable, stack, text};
use iced::window::{self, Screenshot};
use iced::{
    Alignment, Background, Border, Color, Element, Event, Length, Padding, Pixels, Point,
    Rectangle, Size, Vector, mouse,
};

use crate::app::{App, Message, TabBarMsg};
use crate::chrome::{self, Chrome};
use crate::keys::{Action, Bindings};
use crate::palette;
use crate::shortcuts::{Mode, Shortcuts};
use crate::tab_look::{Surroundings, look};
use crate::tabs::{Pane, Tab, TabId, TabState};
use crate::theme::{Tokens, UI_FONT};
use crate::tooltip::{DemoTip, tip, tooltip_text};
use crate::ui::{ICON_PLUS, ICON_SETTINGS, ICON_USERS, ICON_X, icon};
use crate::windows::{Carry, DragMsg};

/// Gap between the top of the tab bar and the top of a tab.
const TAB_TOP_GAP: f32 = 4.0;
/// How far a pressed tab must travel before it is carried (less is a click).
const TAB_DRAG_THRESHOLD: f32 = 6.0;

pub fn view(app: &App, id: window::Id) -> Element<'_, Message> {
    let t = app.tokens;
    if let Some((eid, state)) = &app.editor
        && *eid == id
    {
        let degraded = app.fallback.as_ref().is_some_and(|f| f.degraded());
        let mut content = column![];
        if Chrome::current().draws_title_bar() {
            content = content.push(crate::ui::window_header(t, id));
        }
        let content = content.push(crate::editor::view::view(t, state, degraded));
        return Element::new(Shortcuts {
            content: content.into(),
            window: id,
            mode: Mode::Editor,
            bindings: &app.bindings,
        });
    }
    if let Some((sid, state)) = &app.settings_win
        && *sid == id
    {
        let mut content = column![];
        if Chrome::current().draws_title_bar() {
            content = content.push(crate::ui::window_header(t, id));
        }
        let notice = app.settings_notice.as_deref();
        let content = content.push(crate::settings_window::view::view(t, state, notice));
        return Element::new(Shortcuts {
            content: content.into(),
            window: id,
            mode: if state.recording.is_some() {
                Mode::Recording
            } else {
                Mode::Settings
            },
            bindings: &app.bindings,
        });
    }
    let Some(win) = app.windows.get(&id) else {
        return container(text("")).into();
    };
    // With the focused pane's find bar open, keys go to the bar (see `App::pane_style`).
    let find = win
        .active_tab()
        .and_then(|t| t.focused_pane().find.as_ref());
    let style = &app.style;
    // The default background and the chrome around it take the background opacity; text,
    // cursor, selection and cells with their own background stay opaque (the terminal widget
    // draws those and leaves its default background to the container below).
    let opacity = app.settings.background_opacity;
    let term_bg = to_color(style.scheme.background).scale_alpha(opacity);
    let bar_bg = t.surface.scale_alpha(opacity);
    let palette = app.palettes.get(&id);
    let find_box = app.find_box.as_ref().filter(|f| f.window == id);
    let dialog = app.prompts.front(id);
    let modal = palette.is_some() || find_box.is_some() || dialog.is_some();
    let carry = app.carry_in(id);
    let bar = TabBar {
        window: id,
        bar_bg,
        around: Surroundings {
            terminal: to_color(style.scheme.background),
            bar: t.surface,
            opacity,
        },
        scale: app.physical_scale(id),
        fullscreen: app.fullscreen.contains(&id),
        modal,
        demo_tip: app.demo_tip,
        bindings: &app.bindings,
        carry,
    };
    let mut body = column![tab_bar(t, bar, &win.tabs, win.active)];
    let notices = app.visible_notices();
    if !notices.is_empty() {
        body = body.push(notice_banner(t, bar_bg, notices));
    }
    if let Some(n) = app.export_notice.as_ref().filter(|n| n.window == id) {
        body = body.push(crate::export::notice_view(t, bar_bg, n));
    }
    // Only the active tab builds terminal views (one per pane on screen); background tabs
    // render nothing.
    let content: Element<'_, Message> = match win.active_tab() {
        Some(tab) => {
            // In the tab's own pane order, not the layout's: moving a tile then keeps each
            // pane's widget state (find field, selection drag) with its pane.
            let shown = tab.visible_panes();
            let panes = tab
                .panes
                .iter()
                .filter(|pane| shown.contains(&pane.id))
                .map(|pane| (pane.id, pane_view(app, id, tab, pane)))
                .collect();
            let incoming = match carry {
                Some(Carry::Tab(source)) => {
                    win.tabs
                        .iter()
                        .find(|t| t.id == source)
                        .map(|from| crate::tiles::Incoming {
                            tab: source,
                            mergeable: source != tab.id,
                            layout: from.layout.clone(),
                        })
                }
                _ => None,
            };
            // The minimum pane size is measured in the focused pane's cells, as for splits.
            let cell = app
                .grids
                .get(&tab.focused)
                .map(|g| Size::new(g.cell_width, g.cell_height));
            crate::tiles::tiles(t, id, tab, panes, !modal, incoming, cell)
        }
        None if win.tabs.is_empty() => container(
            text("No shells found. Add a profile in profiles.toml.")
                .font(UI_FONT)
                .size(13)
                .color(t.text_muted),
        )
        .center(Length::Fill)
        .into(),
        None => text("").into(),
    };
    body = body.push(
        container(content)
            .padding(Padding::from([4.0, 8.0]))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| container::Style {
                background: Some(term_bg.into()),
                ..container::Style::default()
            }),
    );
    // Under an overlay nothing reaches the terminal; only the palette and find-in-all-tabs
    // toggles still work, and not while a dialog waits for an answer.
    let covered = Mode::Blocked {
        toggles: dialog.is_none(),
    };
    // A carried tab follows the pointer over the whole window.
    let ghost = match carry {
        Some(Carry::Tab(source)) => win
            .tabs
            .iter()
            .find(|tab| tab.id == source)
            .map(|tab| tab.title().to_owned()),
        _ => None,
    };
    let body = Element::new(Carried {
        content: body.into(),
        ghost,
        tokens: t,
    });
    let base = Element::new(Shortcuts {
        content: body,
        window: id,
        mode: if modal {
            covered
        } else if find.is_some() {
            Mode::Find
        } else {
            Mode::Normal
        },
        bindings: &app.bindings,
    });
    let mut layers = stack![base];
    if let Some(state) = palette {
        layers = layers.push(Element::new(Shortcuts {
            content: palette::view(t, id, state, &app.bindings),
            window: id,
            mode: if dialog.is_some() {
                covered
            } else {
                Mode::Palette
            },
            bindings: &app.bindings,
        }));
    }
    if let Some(state) = find_box {
        layers = layers.push(Element::new(Shortcuts {
            content: crate::find::all_view::view(t, id, state),
            window: id,
            mode: if dialog.is_some() {
                covered
            } else {
                Mode::FindAll
            },
            bindings: &app.bindings,
        }));
    }
    if let Some(d) = dialog {
        layers = layers.push(Element::new(Shortcuts {
            content: crate::prompt_view::view(t, id, d),
            window: id,
            mode: Mode::Prompt,
            bindings: &app.bindings,
        }));
    }
    layers.into()
}

/// One pane's terminal, with its find bar over its top right while open.
fn pane_view<'a>(
    app: &'a App,
    id: window::Id,
    tab: &'a Tab,
    pane: &'a Pane,
) -> Element<'a, Message> {
    let Some(term) = pane.terminal() else {
        return text("").into();
    };
    let find = pane.find.as_ref();
    let highlight = find.and_then(|bar| {
        let alt = term
            .lock()
            .mode()
            .contains(fbterm_term::TermMode::ALT_SCREEN);
        app.highlight(bar, alt)
    });
    let pid = pane.id;
    let view = highlighted_terminal_view(
        term,
        Cow::Owned(app.pane_style(id, tab, pane)),
        highlight,
        move |o| Message::Terminal(id, pid, o),
    );
    match find {
        Some(bar) => stack![
            view,
            crate::find::bar_view::view(app.tokens, id, bar, &app.bindings)
        ]
        .into(),
        None => view,
    }
}

fn to_color(c: fbterm_config::Rgb) -> Color {
    Color::from_rgb8(c.r, c.g, c.b)
}

fn state_color(t: &Tokens, state: &TabState) -> Color {
    match state {
        TabState::Connecting => t.warning,
        TabState::Connected => t.success,
        TabState::Disconnected(_) => t.danger,
        TabState::Exited(_) | TabState::NotConnected => t.text_muted,
    }
}

fn dot<'a>(color: Color) -> Element<'a, Message> {
    container(text(""))
        .width(7)
        .height(7)
        .style(move |_| container::Style {
            background: Some(color.into()),
            border: Border {
                radius: 4.0.into(),
                ..Border::default()
            },
            ..container::Style::default()
        })
        .into()
}

/// How a window's tab bar is drawn.
#[derive(Clone, Copy)]
struct TabBar<'b> {
    window: window::Id,
    bar_bg: Color,
    /// What the tabs are painted against.
    around: Surroundings,
    /// Physical pixels per logical pixel: tab corners are whole physical pixels.
    scale: f32,
    /// The window is in full screen: no room needed for the window buttons.
    fullscreen: bool,
    /// The palette or a dialog covers the window.
    modal: bool,
    /// `FBTERM_DEMO_TOOLTIP` (debug aid): the button whose tooltip always shows.
    demo_tip: Option<DemoTip>,
    /// The chords tooltips name.
    bindings: &'b Bindings,
    /// A tab or tile being dragged in this window.
    carry: Option<Carry>,
}

fn tab_bar<'a>(
    t: &'static Tokens,
    bar: TabBar<'_>,
    tabs: &'a [Tab],
    active: usize,
) -> Element<'a, Message> {
    let TabBar {
        window,
        bar_bg,
        fullscreen,
        modal,
        demo_tip,
        bindings,
        ..
    } = bar;
    let tabs_bar = bar;
    let chrome = Chrome::current();
    let height = chrome.bar_height();
    let msg = move |m| Message::TabBar(window, m);
    let mut bar = row![].spacing(2).align_y(Alignment::End);
    for (i, tab) in tabs.iter().enumerate() {
        bar = bar.push(tab_button(
            t,
            tabs_bar,
            tab,
            i,
            i == active,
            height - TAB_TOP_GAP,
        ));
    }
    let new_tab = button(icon(ICON_PLUS, 14.0, t.text_muted))
        .on_press(msg(TabBarMsg::New))
        .padding(7)
        .style(move |_, status| ghost(t, status));
    let profiles = button(icon(ICON_USERS, 14.0, t.text_muted))
        .on_press(msg(TabBarMsg::Profiles))
        .padding(7)
        .style(move |_, status| ghost(t, status));
    let settings = button(icon(ICON_SETTINGS, 14.0, t.text_muted))
        .on_press(msg(TabBarMsg::Settings))
        .padding(7)
        .style(move |_, status| ghost(t, status));
    let new_tab = tip(
        t,
        new_tab,
        tooltip_text("New tab", Action::NewTab, bindings),
        demo_tip == Some(DemoTip::New),
    );
    let profiles = tip(
        t,
        profiles,
        tooltip_text("Edit profiles", Action::EditProfiles, bindings),
        demo_tip == Some(DemoTip::Profiles),
    );
    let settings = tip(
        t,
        settings,
        tooltip_text("Settings", Action::Settings, bindings),
        demo_tip == Some(DemoTip::Settings),
    );
    bar = bar.push(
        container(new_tab)
            .padding(Padding::from([0.0, 4.0]))
            .center_y(height),
    );
    bar = bar.push(container(profiles).center_y(height));
    bar = bar.push(container(settings).center_y(height));
    let bar = container(bar)
        .padding(Padding {
            top: 0.0,
            right: 8.0,
            bottom: 0.0,
            left: chrome.leading_inset(fullscreen),
        })
        .width(Length::Fill)
        .height(height)
        .align_y(Alignment::End);
    let bar = Element::new(BarBackground {
        window,
        ids: tabs.iter().map(|t| t.id).collect(),
        carry: tabs_bar.carry,
        modal,
        content: bar.into(),
        color: bar_bg,
        hole: (active < tabs.len()).then_some(active),
        tokens: t,
        around: tabs_bar.around,
        tabs: tabs
            .iter()
            .map(|t| (t.accent(), t.accent_style()))
            .collect(),
        scale: tabs_bar.scale,
    });
    chrome::title_bar(bar, modal, move |a| Message::TitleBar(window, a))
}

/// Top corner radius of a tab (the active one, a hovered one and a tinted one), and of the
/// flares at the active tab's bottom; rounded to whole physical pixels by [`tab_radius`].
const TAB_RADIUS: f32 = 6.0;

/// `v` logical pixels moved to the nearest whole physical pixel, as the renderer snaps quads.
fn whole_px(v: f32, scale: f32) -> f32 {
    (v * scale + 0.001).round() / scale
}

/// `r` with its edges on whole physical pixels, where the renderer draws a snapped quad.
fn snap_rect(r: Rectangle, scale: f32) -> Rectangle {
    let (x, y) = (whole_px(r.x, scale), whole_px(r.y, scale));
    Rectangle {
        x,
        y,
        width: whole_px(r.x + r.width, scale) - x,
        height: whole_px(r.y + r.height, scale) - y,
    }
}

/// The tab corner radius in window scale `scale`: a whole number of physical pixels, so the
/// tab's rounded corners, the bar's corner pieces and the flares stay concentric at any
/// scale (1.25 and 1.75 included).
fn tab_radius(scale: f32) -> f32 {
    whole_px(TAB_RADIUS, scale).max(1.0 / scale)
}

/// How far a concave corner piece or flare reaches under the antialiased edge it meets, in
/// physical pixels.
const CORNER_OVERLAP_PX: f32 = 0.14;

/// The pieces of `rect` that `hole` does not cover (at most four, disjoint).
fn subtract(rect: Rectangle, hole: Rectangle) -> Vec<Rectangle> {
    let Some(hole) = rect
        .intersection(&hole)
        .filter(|h| h.width > 0.0 && h.height > 0.0)
    else {
        return vec![rect];
    };
    let mut out = Vec::with_capacity(4);
    let mut push = |x: f32, y: f32, w: f32, h: f32| {
        if w > 0.0 && h > 0.0 {
            out.push(Rectangle {
                x,
                y,
                width: w,
                height: h,
            });
        }
    };
    let (right, bottom) = (hole.x + hole.width, hole.y + hole.height);
    push(rect.x, rect.y, rect.width, hole.y - rect.y);
    push(rect.x, bottom, rect.width, rect.y + rect.height - bottom);
    push(rect.x, hole.y, hole.x - rect.x, hole.height);
    push(right, hole.y, rect.x + rect.width - right, hole.height);
    out
}

/// The pieces of `outer` that none of `holes` cover, disjoint. The active tab and its flares
/// draw their own background there; with a translucent background nothing may lie under
/// them, or the two alphas would add up.
fn around(outer: Rectangle, holes: &[Rectangle]) -> Vec<Rectangle> {
    holes.iter().fold(vec![outer], |pieces, &hole| {
        pieces.into_iter().flat_map(|p| subtract(p, hole)).collect()
    })
}

/// A flare: the square beside a bottom corner of the active tab, split by a circle of
/// `radius` around `center` (the square's outer top corner). Outside the circle (toward the
/// tab) it takes the tab's surface, the terminal's background; inside it, the bar's.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Flare {
    /// The square, cut to the bar (clip bounds).
    square: Rectangle,
    center: iced::Point,
    radius: f32,
}

impl Flare {
    /// The disc quad (the bar's part).
    fn disc(&self) -> Rectangle {
        let r = self.radius;
        Rectangle::new(
            iced::Point::new(self.center.x - r, self.center.y - r),
            Size::new(2.0 * r, 2.0 * r),
        )
    }

    /// The ring quad (the tab's part): its border, as wide as the radius, starts at the circle.
    fn ring(&self) -> Rectangle {
        let r = self.radius;
        Rectangle::new(
            iced::Point::new(self.center.x - 2.0 * r, self.center.y - 2.0 * r),
            Size::new(4.0 * r, 4.0 * r),
        )
    }
}

/// The flares beside the bottom corners of `hole`, cut to `outer` (none where no room is left).
fn flares(outer: Rectangle, hole: Rectangle, radius: f32) -> Vec<Flare> {
    let r = radius.min(hole.height);
    let bottom = hole.y + hole.height;
    let right = hole.x + hole.width;
    [
        (
            Rectangle::new(iced::Point::new(hole.x - r, bottom - r), Size::new(r, r)),
            hole.x - r,
        ),
        (
            Rectangle::new(iced::Point::new(right, bottom - r), Size::new(r, r)),
            right + r,
        ),
    ]
    .into_iter()
    .filter_map(|(square, cx)| {
        let square = outer
            .intersection(&square)
            .filter(|s| s.width > 0.0 && s.height > 0.0)?;
        Some(Flare {
            square,
            center: iced::Point::new(cx, bottom - r),
            radius: r,
        })
    })
    .collect()
}

/// One concave corner piece: the part of a `radius` square in a top corner of the hole that
/// lies outside the tab's rounded corner, filled with the bar's background.
///
/// Drawn as a ring (a round quad whose border is as wide as `radius`, transparent inside)
/// clipped to the corner square: the inner edge of the border is a circle of `radius` around
/// the same center as the tab's corner, and the two antialiased edges are complementary, so
/// nothing is covered twice and no notch shows at any opacity.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Corner {
    /// The corner square (clip bounds).
    clip: Rectangle,
    /// The ring quad's bounds: a circle of `2 * radius` around the corner's center.
    ring: Rectangle,
    /// Border width (and inner radius) of the ring.
    radius: f32,
}

/// The two concave corner pieces beside a hole with top corner radius `radius`.
fn corners(hole: Rectangle, radius: f32) -> [Corner; 2] {
    let r = radius.min(hole.width / 2.0).min(hole.height);
    let piece = |corner_x: f32, center_x: f32| Corner {
        clip: Rectangle {
            x: corner_x,
            y: hole.y,
            width: r,
            height: r,
        },
        ring: Rectangle {
            x: center_x - 2.0 * r,
            y: hole.y + r - 2.0 * r,
            width: 4.0 * r,
            height: 4.0 * r,
        },
        radius: r,
    };
    [
        piece(hole.x, hole.x + r),
        piece(hole.x + hole.width - r, hole.x + hole.width - r),
    ]
}

/// Paints the tab bar's background everywhere except under the active tab (child `hole` of
/// the bar's row), which draws its own, and its flares; and the inactive tabs' backgrounds
/// (hover, tint), kept off the flares.
///
/// It also turns a press on a tab that travels `TAB_DRAG_THRESHOLD` into a tab drag (a shorter
/// one stays a click), reports the tab a carried tab hovers, and takes a tile dropped on the
/// bar as a new tab at the gap nearest the pointer.
struct BarBackground<'a> {
    window: window::Id,
    /// Each tab's id, in row order.
    ids: Vec<TabId>,
    carry: Option<Carry>,
    /// The palette or a dialog covers the window: no tab drags.
    modal: bool,
    content: Element<'a, Message>,
    color: Color,
    hole: Option<usize>,
    tokens: &'static Tokens,
    around: Surroundings,
    /// Each tab's accent and how it shows, in row order.
    tabs: Vec<(Option<fbterm_config::Rgb>, fbterm_config::AccentStyle)>,
    /// Physical pixels per logical pixel.
    scale: f32,
}

/// A press on a tab button, until its release.
struct TabPress {
    index: usize,
    origin: Point,
    /// Travelled past the threshold: the tab is carried, the release is no click.
    dragging: bool,
}

#[derive(Default)]
struct BarState {
    press: Option<TabPress>,
    /// The tab button a carried tab hovers, as last reported.
    over: Option<usize>,
    /// Where a carried tile would become a tab, as last drawn.
    gap: Option<usize>,
}

/// The tab buttons' bounds: the first `n` children of the bar's row (container -> row).
fn tab_rects(layout: Layout<'_>, n: usize) -> Vec<Rectangle> {
    layout
        .children()
        .next()
        .map(|row| row.children().take(n).map(|tab| tab.bounds()).collect())
        .unwrap_or_default()
}

/// Where a tile dropped at `p` becomes a tab: before the tab whose left half is under `p`,
/// after the one whose right half is, else at the nearest gap. Over its own tab (`own`):
/// nowhere.
fn tab_gap(buttons: &[Rectangle], own: Option<usize>, p: Point) -> Option<usize> {
    if let Some(i) = buttons.iter().position(|r| r.contains(p)) {
        if Some(i) == own {
            return None;
        }
        return Some(if p.x < buttons[i].center_x() {
            i
        } else {
            i + 1
        });
    }
    Some(
        buttons
            .iter()
            .position(|r| p.x < r.center_x())
            .unwrap_or(buttons.len()),
    )
}

impl BarBackground<'_> {
    fn publish(&self, shell: &mut Shell<'_, Message>, msg: DragMsg) {
        shell.publish(Message::Drag(self.window, msg));
    }

    /// The tab-drag side of `event`. Returns `true` when the content must not see where the
    /// pointer is (the release that ends a tab drag is no click on a button).
    fn drag_event(
        &self,
        state: &mut BarState,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        shell: &mut Shell<'_, Message>,
    ) -> bool {
        let bounds = layout.bounds();
        let buttons = tab_rects(layout, self.ids.len());
        let under = |p: Point| buttons.iter().position(|r| r.contains(p));
        let tile = matches!(self.carry, Some(Carry::Tile(_)));
        // A tile drag that ended elsewhere (Escape) leaves no gap behind.
        if !tile && state.gap.take().is_some() {
            shell.request_redraw();
        }
        let gap = |p: Option<Point>| {
            p.filter(|_| tile)
                .and_then(|p| tab_gap(&buttons, self.hole, p))
        };
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                state.press = cursor
                    .position_over(bounds)
                    .filter(|_| !self.modal)
                    .and_then(|p| {
                        under(p).map(|index| TabPress {
                            index,
                            origin: p,
                            dragging: false,
                        })
                    });
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let (Some(press), Some(p)) = (&mut state.press, cursor.position()) {
                    if !press.dragging && p.distance(press.origin) >= TAB_DRAG_THRESHOLD {
                        press.dragging = true;
                        if let Some(id) = self.ids.get(press.index) {
                            shell.publish(Message::Drag(self.window, DragMsg::StartTab(*id)));
                        }
                    }
                    if press.dragging && matches!(self.carry, Some(Carry::Tab(_))) {
                        let over = cursor
                            .position_over(bounds)
                            .and_then(under)
                            .filter(|i| *i != press.index);
                        if over != state.over {
                            state.over = over;
                            self.publish(shell, DragMsg::OverTab(over));
                        }
                    }
                }
                let at = gap(cursor.position_over(bounds));
                if at != state.gap {
                    state.gap = at;
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if state.gap.take().is_some() {
                    shell.request_redraw();
                }
                if let Some(TabPress { dragging: true, .. }) = state.press.take() {
                    state.over = None;
                    self.publish(shell, DragMsg::End);
                    return true;
                }
                if let (Some(Carry::Tile(pane)), Some(index)) =
                    (self.carry, gap(cursor.position_over(bounds)))
                {
                    self.publish(shell, DragMsg::TileToTab { pane, index });
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) | Event::Window(window::Event::Unfocused) => {
                if let Some(TabPress { dragging: true, .. }) = state.press.take() {
                    self.publish(shell, DragMsg::End);
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
}

impl Widget<Message, iced::Theme, iced::Renderer> for BarBackground<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<BarState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(BarState::default())
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
        let state = tree.state.downcast_mut::<BarState>();
        // The button pressed at the start of a drag is let go without a click: it sees the
        // release with no pointer over it.
        let cursor = if self.drag_event(state, event, layout, cursor, shell) {
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
        let bounds = layout.bounds();
        let buttons = tab_rects(layout, self.tabs.len());
        let tab_bounds: Vec<Rectangle> = buttons
            .iter()
            .map(|tab| snap_rect(*tab, self.scale))
            .collect();
        let hole = self
            .hole
            .and_then(|i| tab_bounds.get(i).copied())
            .filter(|h| h.width > 0.0 && h.height > 0.0);
        let radius = tab_radius(self.scale);
        let flares = hole.map_or_else(Vec::new, |h| flares(bounds, h, radius));
        let mut holes: Vec<Rectangle> = hole.into_iter().collect();
        holes.extend(flares.iter().map(|f| f.square));
        let quad = |bounds: Rectangle, border: Border| renderer::Quad {
            bounds,
            border,
            snap: true,
            ..renderer::Quad::default()
        };
        for piece in around(bounds, &holes) {
            renderer.fill_quad(quad(piece, Border::default()), self.color);
        }
        // A ring quad with a transparent inside: its border, as wide as `width`, starts at
        // the circle of `ring.width / 4` around the ring's center. A hair wider than the
        // radius, so two translucent antialiased edges leave no faint lighter arc (measured:
        // alpha within 5% along it).
        let scale = self.scale;
        let ring = |renderer: &mut iced::Renderer, clip: Rectangle, ring: Rectangle, color| {
            let r = ring.width / 4.0;
            renderer.with_layer(clip, |renderer| {
                renderer.fill_quad(
                    quad(
                        ring,
                        Border {
                            color,
                            width: r + CORNER_OVERLAP_PX / scale,
                            radius: (2.0 * r).into(),
                        },
                    ),
                    Color::TRANSPARENT,
                );
            });
        };
        let terminal = self.around.terminal.scale_alpha(self.around.opacity);
        if let Some(h) = hole {
            for c in corners(h, radius) {
                ring(renderer, c.clip, c.ring, self.color);
            }
        }
        for f in &flares {
            let disc = quad(
                f.disc(),
                Border {
                    radius: f.radius.into(),
                    ..Border::default()
                },
            );
            renderer.with_layer(f.square, |renderer| {
                renderer.fill_quad(disc, self.color);
            });
            ring(renderer, f.square, f.ring(), terminal);
        }
        // Inactive tabs: hover and tint, never over a flare's tab-colored part (a neighbour
        // of the active tab only reaches into the flare's disc).
        for (i, (&tab, &(accent, style))) in tab_bounds.iter().zip(&self.tabs).enumerate() {
            if Some(i) == self.hole {
                continue;
            }
            let hovered = cursor.is_over(tab);
            let Some(background) =
                look(self.tokens, accent, style, false, hovered, self.around).background
            else {
                continue;
            };
            let shape = renderer::Quad {
                bounds: tab,
                border: Border {
                    radius: iced::border::Radius::default().top(radius),
                    ..Border::default()
                },
                snap: true,
                ..renderer::Quad::default()
            };
            let touching: Vec<&Flare> = flares
                .iter()
                .filter(|f| f.square.intersection(&tab).is_some_and(|s| s.width > 0.0))
                .collect();
            if touching.is_empty() {
                renderer.fill_quad(shape, background);
                continue;
            }
            let squares: Vec<Rectangle> = touching.iter().map(|f| f.square).collect();
            for piece in around(tab, &squares) {
                renderer.with_layer(piece, |renderer| renderer.fill_quad(shape, background));
            }
            for f in touching {
                let Some(clip) = f.square.intersection(&tab) else {
                    continue;
                };
                let disc = quad(
                    f.disc(),
                    Border {
                        radius: f.radius.into(),
                        ..Border::default()
                    },
                );
                renderer.with_layer(clip, |renderer| renderer.fill_quad(disc, background));
            }
        }
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        // A carried tile: a violet bar in the gap where it would become a tab.
        let state = tree.state.downcast_ref::<BarState>();
        if let (Some(Carry::Tile(_)), Some(gap)) = (self.carry, state.gap) {
            let x = match buttons.get(gap) {
                Some(next) => next.x - 2.0,
                None => buttons.last().map_or(bounds.x, |r| r.x + r.width),
            };
            let height = buttons.first().map_or(bounds.height, |r| r.height);
            let marker = snap_rect(
                Rectangle {
                    x,
                    y: bounds.y + bounds.height - height,
                    width: 2.0,
                    height,
                },
                self.scale,
            );
            renderer.with_layer(bounds, |renderer| {
                renderer.fill_quad(quad(marker, Border::default()), self.tokens.accent);
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

/// The window's content; while a tab is carried, its title follows the pointer.
struct Carried<'a> {
    content: Element<'a, Message>,
    /// The carried tab's title.
    ghost: Option<String>,
    tokens: &'static Tokens,
}

/// The carried tab's size, and where it hangs from the pointer.
const GHOST_SIZE: Size = Size::new(180.0, 28.0);
const GHOST_GRIP: Vector = Vector::new(-24.0, -14.0);

impl Widget<Message, iced::Theme, iced::Renderer> for Carried<'_> {
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
        // The ghost follows the pointer: a move redraws while a tab is carried.
        if self.ghost.is_some() && matches!(event, Event::Mouse(mouse::Event::CursorMoved { .. })) {
            shell.request_redraw();
        }
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
        if self.ghost.is_some() {
            return mouse::Interaction::Grabbing;
        }
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
        use iced::advanced::text::{self, Renderer as _};
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        let (Some(title), Some(p)) = (&self.ghost, cursor.position()) else {
            return;
        };
        let t = self.tokens;
        let bounds = layout.bounds();
        let r = Rectangle::new(p + GHOST_GRIP, GHOST_SIZE);
        renderer.with_layer(bounds, |renderer| {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: r,
                    border: Border {
                        color: t.accent,
                        width: 1.0,
                        radius: TAB_RADIUS.into(),
                    },
                    snap: true,
                    ..renderer::Quad::default()
                },
                t.raised,
            );
            let inner = Rectangle {
                x: r.x + 12.0,
                width: r.width - 24.0,
                ..r
            };
            renderer.fill_text(
                text::Text {
                    content: title.clone(),
                    bounds: inner.size(),
                    size: Pixels(12.5),
                    line_height: text::LineHeight::default(),
                    font: UI_FONT,
                    align_x: text::Alignment::Left,
                    align_y: iced::alignment::Vertical::Center,
                    shaping: text::Shaping::Advanced,
                    wrapping: text::Wrapping::None,
                },
                Point::new(inner.x, inner.center_y()),
                t.text,
                inner,
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
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

fn ghost(t: &Tokens, status: button::Status) -> button::Style {
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
}

/// A tab; the active one takes the terminal's background so the two read as one surface,
/// and the profile accent shows as a stripe or tints the whole tab (`tab_look`).
fn tab_button<'a>(
    t: &'static Tokens,
    bar: TabBar<'_>,
    tab: &'a Tab,
    index: usize,
    active: bool,
    height: f32,
) -> Element<'a, Message> {
    let window = bar.window;
    let demo_tip = index == 0 && bar.demo_tip == Some(DemoTip::Close);
    let (accent, accent_style, around) = (tab.accent(), tab.accent_style(), bar.around);
    let radius = tab_radius(bar.scale);
    let idle = look(t, accent, accent_style, active, false, around);
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
    let label = text(tab.title())
        .font(UI_FONT)
        .size(12.5)
        .wrapping(text::Wrapping::None)
        .color(idle.label);
    let close = button(icon(ICON_X, 12.0, idle.icon))
        .on_press(Message::TabBar(window, TabBarMsg::Close(index)))
        .padding(3)
        .style(move |_, status| ghost(t, status));
    // The shortcut closes the active tab only, so only its button names it.
    let close = tip(
        t,
        close,
        if active {
            tooltip_text("Close tab", Action::CloseTab, bar.bindings)
        } else {
            "Close tab".to_owned()
        },
        demo_tip,
    );
    let mut inner = row![stripe, dot(state_color(t, tab.state()))];
    // A background pane that lost its session: a second dot, the theme's warning color.
    if tab.trouble() {
        inner = inner.push(dot(t.warning));
    }
    let inner = inner
        .push(container(label).width(Length::Fill).clip(true))
        .push(close)
        .spacing(8)
        .height(Length::Fill)
        .align_y(Alignment::Center);
    button(inner)
        .on_press(Message::TabBar(window, TabBarMsg::Select(index)))
        .width(Length::Fixed(196.0))
        .height(height)
        // The tab starts TAB_TOP_GAP below the top of the bar; the same space below its
        // content centers the label on the bar (and on the window buttons).
        .padding(Padding {
            top: 0.0,
            right: 6.0,
            bottom: TAB_TOP_GAP,
            left: 8.0,
        })
        .style(move |_, status| {
            // Inactive tabs' hover and tint are painted by the bar (`BarBackground`), which
            // keeps them off the active tab's flares.
            let _ = status;
            button::Style {
                background: if active { idle.background } else { None },
                text_color: t.text,
                border: Border {
                    radius: iced::border::Radius::default().top(radius),
                    ..Border::default()
                },
                // Snapped like the bar's pieces and corners, so their edges meet exactly.
                snap: true,
                ..button::Style::default()
            }
        })
        .into()
}

fn notice_banner<'a>(
    t: &'static Tokens,
    base: Color,
    notices: Vec<String>,
) -> Element<'a, Message> {
    let mono = iced::Font::with_name("Geist Mono");
    let lines = notices.into_iter().fold(column![].spacing(2), |c, n| {
        c.push(text(n).font(mono).size(12).color(t.text))
    });
    let dismiss = button(text("Dismiss").font(UI_FONT).size(12))
        .on_press(Message::DismissNotices)
        .padding(Padding::from([3.0, 10.0]))
        .style(move |_, status| ghost(t, status));
    let banner = container(
        row![
            scrollable(lines).height(Length::Shrink).width(Length::Fill),
            dismiss
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([6.0, 12.0]))
    .width(Length::Fill)
    .style(move |_| container::Style {
        background: Some(t.danger_soft.into()),
        ..container::Style::default()
    });
    // The tint sits on the chrome's background, so it reads the same at any opacity.
    container(banner)
        .width(Length::Fill)
        .style(move |_| container::Style {
            background: Some(base.into()),
            ..container::Style::default()
        })
        .into()
}

/// Write a window screenshot as PNG (the `FBTERM_SCREENSHOT` debug aid).
pub fn save_png(path: &Path, shot: &Screenshot) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(file),
        shot.size.width,
        shot.size.height,
    );
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(&shot.rgba).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTER: Rectangle = Rectangle {
        x: 0.0,
        y: 0.0,
        width: 600.0,
        height: 32.0,
    };
    const TAB: Rectangle = Rectangle {
        x: 100.0,
        y: 4.0,
        width: 196.0,
        height: 28.0,
    };

    #[test]
    fn the_bar_background_leaves_the_active_tab_to_itself() {
        let pieces = around(OUTER, &[TAB]);
        let area = |r: &Rectangle| r.width * r.height;
        let covered: f32 = pieces.iter().map(area).sum();
        // Everything but the tab, with nothing behind its corners.
        assert_eq!(covered, area(&OUTER) - area(&TAB));
        for p in [
            iced::Point::new(198.0, 20.0),
            iced::Point::new(101.0, 5.0),
            iced::Point::new(295.0, 5.0),
        ] {
            assert!(pieces.iter().all(|r| !r.contains(p)), "{p:?} covered");
        }
        // With no overlap the whole bar is painted.
        let away = Rectangle { x: 900.0, ..TAB };
        assert_eq!(around(OUTER, &[away]), vec![OUTER]);
    }

    fn area(rects: &[Rectangle]) -> f32 {
        rects.iter().map(|r| r.width * r.height).sum()
    }

    #[test]
    fn flares_sit_beside_the_tabs_bottom_corners_and_leave_the_bar_disjoint() {
        let r = TAB_RADIUS;
        let fl = flares(OUTER, TAB, r);
        assert_eq!(fl.len(), 2);
        let (left, right) = (fl[0], fl[1]);
        assert_eq!(
            left.square,
            Rectangle::new((94.0, 26.0).into(), Size::new(r, r))
        );
        assert_eq!(
            right.square,
            Rectangle::new((296.0, 26.0).into(), Size::new(r, r))
        );
        // The circle's center is each square's outer top corner, so the part outside the
        // circle is the corner that touches the tab and the terminal below.
        assert_eq!(left.center, iced::Point::new(94.0, 26.0));
        assert_eq!(right.center, iced::Point::new(302.0, 26.0));
        assert_eq!(left.disc().center(), left.center);
        assert_eq!(left.ring().center(), left.center);
        assert_eq!(
            left.ring().width / 4.0,
            r,
            "the ring's border starts at the circle"
        );
        // The bar's pieces, the tab and the flare squares tile the bar exactly once.
        let holes = [TAB, left.square, right.square];
        let pieces = around(OUTER, &holes);
        assert!((area(&pieces) + area(&holes) - area(&[OUTER])).abs() < 1e-3);
        for (i, a) in pieces.iter().enumerate() {
            for b in &pieces[i + 1..] {
                assert!(a.intersection(b).is_none_or(|x| x.width * x.height == 0.0));
            }
            for h in holes {
                assert!(a.intersection(&h).is_none_or(|x| x.width * x.height == 0.0));
            }
        }
    }

    #[test]
    fn flares_are_cut_at_the_bars_edges() {
        // The first tab against the bar's left edge (full screen, no window buttons): no
        // room for its left flare.
        let first = Rectangle { x: 0.0, ..TAB };
        let fl = flares(OUTER, first, TAB_RADIUS);
        assert_eq!(fl.len(), 1);
        assert_eq!(fl[0].square.x, 196.0);
        // Partly out of the bar: the square is cut, the circle stays where it was.
        let near = Rectangle { x: 3.0, ..TAB };
        let fl = flares(OUTER, near, TAB_RADIUS);
        assert_eq!(
            fl[0].square,
            Rectangle::new((0.0, 26.0).into(), Size::new(3.0, 6.0))
        );
        assert_eq!(fl[0].center, iced::Point::new(-3.0, 26.0));
    }

    #[test]
    fn at_fractional_scales_corners_and_flares_are_whole_physical_pixels() {
        for scale in [1.0_f32, 1.25, 1.5, 1.75, 2.0] {
            let whole = |v: f32| ((v * scale) - (v * scale).round()).abs() < 1e-3;
            let r = tab_radius(scale);
            assert!(whole(r), "radius at {scale}");
            assert!(
                (r - TAB_RADIUS).abs() <= 0.5 / scale + 1e-4,
                "close to 6 at {scale}"
            );
            // A tab laid out at fractional logical positions, as the renderer snaps it.
            let tab = snap_rect(
                Rectangle {
                    x: 100.3,
                    y: 4.0,
                    width: 196.0,
                    height: 27.6,
                },
                scale,
            );
            for v in [tab.x, tab.y, tab.x + tab.width, tab.y + tab.height] {
                assert!(whole(v), "tab edge {v} at {scale}");
            }
            for c in corners(tab, r) {
                for v in [c.clip.x, c.clip.y, c.ring.x, c.ring.y, c.ring.width] {
                    assert!(whole(v), "corner {v} at {scale}");
                }
                // Concentric with the tab's own corner: ring center = tab corner +/- r.
                let center = c.ring.center();
                assert!((center.y - (tab.y + r)).abs() < 1e-3);
                assert!(
                    (center.x - (tab.x + r)).abs() < 1e-3
                        || (center.x - (tab.x + tab.width - r)).abs() < 1e-3
                );
            }
            for f in flares(OUTER, tab, r) {
                for v in [f.square.x, f.square.y, f.center.x, f.center.y, f.ring().x] {
                    assert!(whole(v), "flare {v} at {scale}");
                }
                assert!((f.square.y + f.square.height - (tab.y + tab.height)).abs() < 1e-3);
            }
        }
    }

    mod press_path {
        use iced::advanced::clipboard;
        use iced::advanced::widget::Tree;
        use iced::{Pixels, Point};

        use super::*;
        use crate::layout::PaneId;
        use crate::windows::{Carry, DragMsg};

        const SIZE: Size = Size::new(1000.0, 40.0);

        fn press() -> Event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        }

        fn release() -> Event {
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
        }

        fn moved(p: Point) -> Event {
            Event::Mouse(mouse::Event::CursorMoved { position: p })
        }

        fn tabs() -> Vec<Tab> {
            ["one", "two", "three"]
                .iter()
                .map(|n| Tab::single(Pane::new(fbterm_config::ProfileId::new(), *n, None)))
                .collect()
        }

        /// Runs `steps` (an event and the pointer position) through a tab bar of `tabs` (the
        /// first active) while `carry` is under way; `steps` gets each tab button's bounds.
        /// Returns the messages published.
        fn drive(
            tabs: &[Tab],
            carry: Option<Carry>,
            steps: impl FnOnce(&[Rectangle]) -> Vec<(Event, Point)>,
        ) -> Vec<Message> {
            let bindings = Bindings::defaults(cfg!(target_os = "macos"));
            let t = &crate::theme::DARK;
            let bar = TabBar {
                window: window::Id::unique(),
                bar_bg: t.surface,
                around: Surroundings {
                    terminal: Color::BLACK,
                    bar: t.surface,
                    opacity: 1.0,
                },
                scale: 1.0,
                fullscreen: false,
                modal: false,
                demo_tip: None,
                bindings: &bindings,
                carry,
            };
            let mut element = tab_bar(t, bar, tabs, 0);
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
            // title bar -> background -> container -> row -> tabs
            let buttons: Vec<Rectangle> = Layout::new(&node)
                .children()
                .next()
                .expect("the row")
                .children()
                .take(tabs.len())
                .map(|l| l.bounds())
                .collect();
            let viewport = Rectangle::with_size(SIZE);
            let mut messages = Vec::new();
            for (event, at) in steps(&buttons) {
                let mut shell = Shell::new(&mut messages);
                element.as_widget_mut().update(
                    &mut tree,
                    &event,
                    Layout::new(&node),
                    mouse::Cursor::Available(at),
                    &renderer,
                    &mut clipboard::Null,
                    &mut shell,
                    &viewport,
                );
            }
            messages
        }

        fn selects(messages: &[Message]) -> Vec<usize> {
            messages
                .iter()
                .filter_map(|m| match m {
                    Message::TabBar(_, TabBarMsg::Select(i)) => Some(*i),
                    _ => None,
                })
                .collect()
        }

        fn drags(messages: &[Message]) -> Vec<DragMsg> {
            messages
                .iter()
                .filter_map(|m| match m {
                    Message::Drag(_, d) => Some(d.clone()),
                    _ => None,
                })
                .collect()
        }

        #[test]
        fn tab_click_still_selects() {
            let tabs = tabs();
            let messages = drive(&tabs, None, |b| {
                let at = b[1].center();
                // A hair of travel is still a click.
                let near = Point::new(at.x + 3.0, at.y + 2.0);
                vec![
                    (moved(at), at),
                    (press(), at),
                    (moved(near), near),
                    (release(), near),
                ]
            });
            assert_eq!(selects(&messages), [1]);
            assert!(drags(&messages).is_empty(), "{messages:?}");
            assert!(
                !messages.iter().any(|m| matches!(m, Message::TitleBar(..))),
                "a press on a tab never drags the window"
            );
        }

        #[test]
        fn tab_drag_starts_after_six_pixels() {
            let tabs = tabs();
            let id = tabs[1].id;
            let messages = drive(&tabs, None, |b| {
                let at = b[1].center();
                let near = Point::new(at.x, at.y + 5.9);
                let far = Point::new(at.x, at.y + 6.0);
                vec![
                    (press(), at),
                    (moved(near), near),
                    (moved(far), far),
                    (release(), at),
                ]
            });
            // Exactly 6 px starts it (5.9 did not: it is the first drag message).
            assert_eq!(drags(&messages)[0], DragMsg::StartTab(id));
            assert_eq!(drags(&messages).len(), 2, "start and end only");
            assert!(selects(&messages).is_empty(), "a drag is not a click");

            // While carried, the tab under the pointer is reported (not the carried one).
            let messages = drive(&tabs, Some(Carry::Tab(id)), |b| {
                let at = b[1].center();
                let over = b[2].center();
                vec![
                    (press(), at),
                    (
                        moved(Point::new(at.x + 10.0, at.y)),
                        Point::new(at.x + 10.0, at.y),
                    ),
                    (moved(over), over),
                    (release(), over),
                ]
            });
            let drags = drags(&messages);
            assert!(drags.contains(&DragMsg::OverTab(Some(2))), "{drags:?}");
            assert!(!drags.contains(&DragMsg::OverTab(Some(1))), "{drags:?}");
            assert_eq!(drags.last(), Some(&DragMsg::End));
            assert!(
                selects(&messages).is_empty(),
                "dropping on a tab is not a click"
            );
        }

        #[test]
        fn a_tile_dropped_on_the_bar_becomes_a_tab_there() {
            let tabs = tabs();
            let pane = PaneId(77);
            let dropped = |x: fn(&[Rectangle]) -> Point| {
                drags(&drive(&tabs, Some(Carry::Tile(pane)), |b| {
                    let p = x(b);
                    vec![(moved(p), p), (release(), p)]
                }))
            };
            // The right half of tab 1: after it.
            assert_eq!(
                dropped(|b| Point::new(b[1].x + b[1].width * 0.75, b[1].center_y())),
                [DragMsg::TileToTab { pane, index: 2 }]
            );
            // The left half of tab 2: before it.
            assert_eq!(
                dropped(|b| Point::new(b[2].x + 5.0, b[2].center_y())),
                [DragMsg::TileToTab { pane, index: 2 }]
            );
            // Past the last tab: at the end.
            assert_eq!(
                dropped(|b| Point::new(b[2].x + b[2].width + 200.0, b[2].center_y())),
                [DragMsg::TileToTab { pane, index: 3 }]
            );
            // Its own tab (the active one): nothing.
            assert_eq!(dropped(|b| b[0].center()), []);
        }
    }

    #[test]
    fn concave_corners_fill_exactly_outside_the_tabs_rounding() {
        let [left, right] = corners(TAB, TAB_RADIUS);
        let r = TAB_RADIUS;
        assert_eq!(
            left.clip,
            Rectangle::new((100.0, 4.0).into(), Size::new(r, r))
        );
        assert_eq!(
            right.clip,
            Rectangle::new((290.0, 4.0).into(), Size::new(r, r))
        );
        for (c, center) in [(left, (106.0, 10.0)), (right, (290.0, 10.0))] {
            // The ring is centered on the tab corner's circle ...
            assert_eq!(c.ring.center(), iced::Point::new(center.0, center.1));
            // ... its inner edge (outer radius minus border) is that circle ...
            assert_eq!(c.ring.width / 2.0 - c.radius, r);
            // ... and its outer edge covers the whole corner square.
            let far = [c.clip.x, c.clip.x + c.clip.width]
                .iter()
                .flat_map(|&x| [(x, c.clip.y), (x, c.clip.y + c.clip.height)])
                .map(|(x, y)| ((x - center.0).powi(2) + (y - center.1).powi(2)).sqrt())
                .fold(0.0_f32, f32::max);
            assert!(far <= c.ring.width / 2.0);
        }
        // A tab narrower than two radii keeps its corners inside itself.
        let thin = Rectangle { width: 8.0, ..TAB };
        let [l, r] = corners(thin, TAB_RADIUS);
        assert_eq!(l.radius, 4.0);
        assert!(l.clip.x + l.clip.width <= r.clip.x);
    }
}
