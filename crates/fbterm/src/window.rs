//! One window's view: tab bar, notices and the active tab's terminal.

use std::borrow::Cow;
use std::path::Path;

use fbterm_term::highlighted_terminal_view;
use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::widget::{button, column, container, responsive, row, scrollable, stack, text};
use iced::window::{self, Screenshot};
use iced::{
    Alignment, Background, Border, Color, Element, Event, Length, Padding, Pixels, Point,
    Rectangle, Size, Vector, mouse,
};

use crate::app::{App, Message, TabBarMsg};
use crate::chrome::{self, Chrome};
use crate::file_panel::FilePanelMsg;
use crate::keys::{Action, Bindings};
use crate::palette;
use crate::shortcuts::{Mode, Shortcuts};
use crate::tab_drag::{Axis, DragState, Strip, drag_event};
use crate::tab_look::{Surroundings, look};
use crate::tab_sidebar::SIDEBAR_WIDTH;
use crate::tab_strip::{
    TAB_MIN_WIDTH, TAB_SPACING, Viewport, WHEEL_LINE_PX, clamp_offset, strip_layout,
};
use crate::tabs::{Page, Pane, Tab, TabId, TabState};
use crate::theme::{Tokens, UI_FONT};
use crate::tooltip::{DemoTip, tip, tooltip_text};
use crate::tunnels_panel::TunnelsMsg;
use crate::ui::{
    ICON_FOLDER, ICON_LEFT, ICON_PALETTE, ICON_PLUS, ICON_RIGHT, ICON_SETTINGS, ICON_TERMINAL,
    ICON_TUNNELS, ICON_USERS, ICON_X, icon,
};
use crate::windows::Carry;

/// Space around the terminal area. Where the chrome is drawn the window's resize band lies
/// within it, so a press on the terminal's text never resizes the window.
pub(crate) const TERMINAL_PADDING: Padding = Padding {
    top: 4.0,
    right: 8.0,
    bottom: 4.0,
    left: 8.0,
};

/// Gap between the top of the tab bar and the top of a tab.
const TAB_TOP_GAP: f32 = 4.0;

pub fn view(app: &App, id: window::Id) -> Element<'_, Message> {
    let content = window_view(app, id);
    // The main password dialog over a window other than a terminal window (those draw it
    // with their other dialogs).
    let content = match app.vault.dialog.as_ref().filter(|d| d.window == id) {
        Some(d) if !app.windows.contains_key(&id) => {
            stack![content, vault_layer(app, id, d)].into()
        }
        _ => content,
    };
    // Without OS decorations the window resizes from the band along its edges.
    if Chrome::current().draws_controls() {
        let fixed = app.maximized.contains(&id) || app.fullscreen.contains(&id);
        chrome::edges(content, fixed, move |a| Message::TitleBar(id, a))
    } else {
        content
    }
}

/// The main password dialog, taking the keys.
fn vault_layer<'a>(
    app: &'a App,
    id: window::Id,
    d: &'a crate::saved_secrets::VaultDialog,
) -> Element<'a, Message> {
    Element::new(Shortcuts {
        content: crate::saved_secrets::view::view(app.tokens, d),
        window: id,
        mode: Mode::Vault,
        bindings: &app.bindings,
    })
}

fn window_view(app: &App, id: window::Id) -> Element<'_, Message> {
    let t = app.tokens;
    if let Some((aid, state)) = &app.archive_win
        && *aid == id
    {
        let mut content = column![];
        if Chrome::current().draws_title_bar() {
            content = content.push(crate::ui::window_header(
                t,
                id,
                app.maximized.contains(&id),
                app.physical_scale(id),
            ));
        }
        let content = content.push(crate::archive_window::view::view(
            t,
            state,
            &app.profiles,
            app.profiles_error.is_some(),
        ));
        return Element::new(Shortcuts {
            content: content.into(),
            window: id,
            mode: Mode::Archive,
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
    // The default background and the chrome around it take the background opacity; text,
    // cursor, selection and cells with their own background stay opaque (the terminal widget
    // draws those and leaves its default background to the container below, and the tiles
    // to each pane's scheme where it differs). The container and the active tab's fade into
    // it take the focused pane's scheme.
    let opacity = app.settings.background_opacity;
    let focused_bg = win.active_tab().map_or(app.style.scheme.background, |tab| {
        app.pane_background(tab.focused_pane())
    });
    let term_bg = to_color(focused_bg).scale_alpha(opacity);
    let bar_bg = t.surface.scale_alpha(opacity);
    let palette = app.palettes.get(&id);
    let find_box = app.find_box.as_ref().filter(|f| f.window == id);
    let dialog = app.prompts.front(id);
    let vault = app.vault.dialog.as_ref().filter(|d| d.window == id);
    let asking = dialog.is_some() || vault.is_some();
    let menu = app.context_menu.as_ref().filter(|m| m.window == id);
    let modal = palette.is_some() || find_box.is_some() || asking || menu.is_some();
    let carry = app.carry_in(id);
    // The file panel or the tunnels panel docks at the active tab's right edge, outside the
    // tiles: they lay out in what is left, as they would in a narrower window.
    let panel = docked(app, id);
    // The panel's rule outlines the content's right side; the bar carries that line on along
    // the content's top and around the active tab, ending where the rule's right edge is.
    // The outline along the content's top and around the active tab always shows: with a panel
    // docked it ends where the panel's rule starts, else at the window's right edge.
    // Tabs on the left: the sidebar takes its width (less in a narrow window) and a rule at
    // the content's left edge; the dock lays out in what is left.
    let hairline = crate::ui::hairline(app.physical_scale(id));
    let sidebar = app.settings.tabs_left().then(|| {
        let total = app.sizes.get(&id).map_or(SIDEBAR_WIDTH, |&(w, _)| w);
        crate::tab_sidebar::sidebar_width(app.sidebar_width_of(id), total, hairline)
    });
    let left = sidebar.map_or(0.0, |w| w + hairline);
    let outline_end = app.sizes.get(&id).map(|&(total, _)| match panel.as_ref() {
        Some(p) => {
            let avail = total - left;
            left + avail - docked_width(p.width, avail) + hairline
        }
        None => total,
    });
    let bar = TabBar {
        window: id,
        bar_bg,
        around: Surroundings {
            terminal: to_color(focused_bg),
            bar: t.surface,
            opacity,
        },
        scale: app.physical_scale(id),
        chrome: Chrome::current(),
        fullscreen: app.fullscreen.contains(&id),
        maximized: app.maximized.contains(&id),
        modal,
        demo_tip: app.demo_tip,
        bindings: &app.bindings,
        carry,
        tunnels_up: app.tunnels.up_count(),
        tunnels_open: app.tunnels_button_active(id),
        files: app.files_button(id),
        update_dot: app.update_dot(),
        outline_end,
        outline_to_edge: panel.is_none(),
        width: app.sizes.get(&id).map(|&(w, _)| w),
        renaming: app.renaming.as_ref().filter(|r| r.window == id),
        tabs_in_bar: sidebar.is_none(),
        outline_start: sidebar.unwrap_or(0.0),
        strip_offset: app.stored_offset(id),
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
    let page = win.active_tab().and_then(|t| t.page);
    let content: Element<'_, Message> = match (win.active_tab(), page) {
        (Some(_), Some(page)) => page_view_min(app, id, page),
        (Some(tab), None) => {
            // In the tab's own pane order, not the layout's: moving a tile then keeps each
            // pane's widget state (find field, selection drag) with its pane.
            let shown = tab.visible_panes();
            let panes = tab
                .panes
                .iter()
                .filter(|pane| shown.contains(&pane.id))
                .map(|pane| {
                    let bg = to_color(app.pane_background(pane));
                    (
                        pane.id,
                        pane_view(app, id, tab, pane),
                        bg,
                        pane.accent.map(to_color),
                    )
                })
                .collect();
            let incoming = match carry {
                Some(Carry::Tab(source)) => {
                    win.tabs
                        .iter()
                        .find(|t| t.id == source)
                        .map(|from| crate::tiles::Incoming {
                            tab: source,
                            mergeable: source != tab.id && from.page.is_none(),
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
            let ground = crate::tiles::Ground {
                under: to_color(focused_bg),
                opacity,
            };
            crate::tiles::tiles(t, id, tab, panes, ground, !modal, incoming, cell)
        }
        (None, _) if win.tabs.is_empty() => container(
            text("No shells found. Add a profile in profiles.toml.")
                .font(UI_FONT)
                .size(13)
                .color(t.text_muted),
        )
        .center(Length::Fill)
        .into(),
        (None, _) => text("").into(),
    };
    let terminal_area: Element<'_, Message> = container(content)
        .padding(if page.is_some() {
            Padding::ZERO
        } else {
            TERMINAL_PADDING
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(term_bg.into()),
            ..container::Style::default()
        })
        .into();
    let panel_mode = panel
        .as_ref()
        .filter(|p| p.focused)
        .map(|p| match p.content {
            DockContent::Files => Mode::Panel,
            DockContent::Tunnels => Mode::TunnelsPanel,
        });
    let docked_area = dock(terminal_area, panel);
    // What the sidebar lists: rows, under group headers while folding is on.
    let items = crate::tab_sidebar::items(&app.sections_of(id), win.active, app.groups_on());
    body = body.push(match sidebar {
        Some(width) => {
            let tile_group = match carry {
                Some(Carry::Tile(pane)) => win.locate(pane).map(|(t, p)| {
                    crate::tab_groups::group_of_profile(win.tabs[t].panes[p].profile, &app.profiles)
                }),
                _ => None,
            };
            let side = crate::tab_sidebar::Sidebar {
                items: &items,
                tile_group: tile_group.as_ref(),
                window: id,
                tokens: t,
                around: Surroundings {
                    terminal: to_color(focused_bg),
                    bar: t.surface,
                    opacity,
                },
                bar_bg,
                bindings: &app.bindings,
                carry,
                modal,
                scale: app.physical_scale(id),
                width,
                tips: carry.is_none() && !modal && !app.sidebar_resizing.contains(&id),
                renaming: app.renaming.as_ref().filter(|r| r.window == id),
            };
            let beside = row![
                crate::tab_sidebar::view(side, &win.tabs, win.active),
                docked_area
            ]
            .height(Length::Fill);
            crate::sidebar_resize::resizable(id, beside.into(), width, hairline, modal, t)
        }
        None => docked_area,
    });
    // Under an overlay nothing reaches the terminal; only the palette and find-in-all-tabs
    // toggles still work, and not while a dialog waits for an answer.
    let covered = Mode::Blocked {
        toggles: !asking && menu.is_none(),
    };
    // A carried tab follows the pointer over the whole window.
    let ghost = match carry {
        Some(Carry::Tab(source)) => win
            .tabs
            .iter()
            .find(|tab| tab.id == source)
            .map(|tab| tab.shown_name().to_owned()),
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
        } else {
            let recording = app
                .settings_win
                .as_ref()
                .is_some_and(|(_, s)| s.recording.is_some());
            let schemes = app.schemes_win.as_ref().map(|(_, s)| s);
            key_mode(
                page,
                recording,
                schemes,
                panel_mode,
                find.is_some(),
                app.renaming_in(id),
            )
        },
        bindings: &app.bindings,
    });
    let mut layers = stack![base];
    if let Some(state) = palette {
        layers = layers.push(Element::new(Shortcuts {
            content: palette::view(t, id, state, &app.bindings),
            window: id,
            mode: if asking { covered } else { Mode::Palette },
            bindings: &app.bindings,
        }));
    }
    if let Some(state) = find_box {
        layers = layers.push(Element::new(Shortcuts {
            content: crate::find::all_view::view(t, id, state),
            window: id,
            mode: if asking { covered } else { Mode::FindAll },
            bindings: &app.bindings,
        }));
    }
    if let Some(d) = dialog {
        layers = layers.push(Element::new(Shortcuts {
            content: crate::prompt_view::view(t, id, d, app.storage()),
            window: id,
            mode: if vault.is_some() {
                covered
            } else {
                Mode::Prompt
            },
            bindings: &app.bindings,
        }));
    }
    if let Some(d) = vault {
        layers = layers.push(vault_layer(app, id, d));
    }
    if let Some(m) = menu {
        let size = app.sizes.get(&id).map(|&(w, h)| Size::new(w, h));
        layers = layers.push(Element::new(Shortcuts {
            content: crate::context_menu::view(t, id, m, size),
            window: id,
            mode: Mode::Menu,
            bindings: &app.bindings,
        }));
    }
    layers.into()
}

fn page_min_width(page: Page) -> f32 {
    match page {
        Page::Settings | Page::Welcome => 520.0,
        // The width the Color schemes window had as its minimum.
        Page::Schemes => 760.0,
        // Sidebar 260 plus the footer's buttons and an "Unsaved changes" note.
        _ => 770.0,
    }
}

/// `page_view`, scrolling sideways below [`page_min_width`] with the app scrollbar, as the
/// pages' own vertical one.
fn page_view_min(app: &App, id: window::Id, page: Page) -> Element<'_, Message> {
    responsive(move |size| {
        let body = page_view(app, id, page);
        let min = page_min_width(page);
        if size.width >= min {
            return body;
        }
        scrollable(container(body).width(min).height(Length::Fill))
            .direction(scrollable::Direction::Horizontal(
                // Embedded: the bar takes its own row under the page instead of floating over the footer.
                crate::ui::edge_scrollbar(crate::chrome::Chrome::current().draws_controls())
                    .spacing(0.0),
            ))
            .style(move |theme, status| crate::ui::scroll_style(app.tokens, theme, status))
            .into()
    })
    .into()
}

/// The body of a page tab: the profile editor or the Settings, drawn where the terminals go.
fn page_view(app: &App, id: window::Id, page: Page) -> Element<'_, Message> {
    let t = app.tokens;
    match page {
        Page::Profiles => match &app.editor {
            Some((_, state)) => crate::editor::view::view(
                t,
                state,
                app.scheme_choices(true),
                app.storage(),
                app.editor_job.is_some(),
                app.physical_scale(id),
            ),
            None => text("").into(),
        },
        Page::Settings => match &app.settings_win {
            Some((_, state)) => crate::settings_window::view::view(
                t,
                state,
                app.scheme_choices(false),
                app.settings_notice.as_deref(),
                app.secrets_section(),
                app.chrome_fell_back,
                app.opaque_window_open(),
            ),
            None => text("").into(),
        },
        Page::Welcome => crate::welcome::view::view(t, &app.settings),
        Page::Schemes => match &app.schemes_win {
            Some((_, state)) => crate::schemes_window::view::view(
                t,
                state,
                &app.style,
                &app.schemes,
                app.physical_scale(id),
            ),
            None => text("").into(),
        },
    }
}

/// The narrowest the terminal area gets beside the file panel.
pub(crate) const MIN_TERMINAL_WIDTH: f32 = 200.0;

/// How wide the band at the file panel's left edge is that a press grabs to resize it. It lies
/// on the panel's side of the edge, so it never takes a press meant for the terminal.
pub(crate) const DOCK_GRAB: f32 = 6.0;

/// How wide a panel asking for `width` is docked in a window `total` wide: it never squeezes
/// the terminal below [`MIN_TERMINAL_WIDTH`].
fn docked_width(width: f32, total: f32) -> f32 {
    width.min(total - MIN_TERMINAL_WIDTH).max(0.0).round()
}

/// Which panel the right edge shows: one at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DockContent {
    Files,
    Tunnels,
}

impl DockContent {
    /// Dragging the panel's edge: its new width.
    fn resize(self, width: f32) -> Message {
        match self {
            Self::Files => Message::FilePanel(FilePanelMsg::Resize(width)),
            Self::Tunnels => Message::Tunnels(TunnelsMsg::Resize(width)),
        }
    }

    /// The drag ended.
    fn resize_end(self) -> Message {
        match self {
            Self::Files => Message::FilePanel(FilePanelMsg::ResizeEnd),
            Self::Tunnels => Message::Tunnels(TunnelsMsg::ResizeEnd),
        }
    }

    /// The panel takes the keyboard (`true`) or gives it back to the content.
    fn focus(self, window: window::Id, focused: bool) -> Message {
        match self {
            Self::Files => Message::FilePanel(FilePanelMsg::Focus(window, focused)),
            Self::Tunnels => Message::Tunnels(TunnelsMsg::Focus(window, focused)),
        }
    }
}

/// A panel as docked beside a window's content.
pub(crate) struct Docked<'a> {
    pub panel: Element<'a, Message>,
    pub width: f32,
    pub window: window::Id,
    /// The panel has the keyboard: key presses go to it, never to the content.
    pub focused: bool,
    /// Which panel it is: where the dock reports drags and clicks.
    pub content: DockContent,
}

/// The panel docked in window `id`, if any: the tunnels panel or the file panel (never
/// both), only while the window has an active tab.
pub(crate) fn docked(app: &App, id: window::Id) -> Option<Docked<'_>> {
    app.windows.get(&id)?.active_tab()?;
    debug_assert!(
        !(app.tunnels_panels.is_open(id) && app.file_panels.is_open(id)),
        "one panel at a time at a window's edge"
    );
    if let Some((panel, width)) = crate::tunnels_panel::view::panel(app, id) {
        return Some(Docked {
            panel,
            width,
            window: id,
            focused: app.tunnels_panels.has_focus(id),
            content: DockContent::Tunnels,
        });
    }
    crate::file_panel::view::panel(app, id).map(|(panel, width)| Docked {
        panel,
        width,
        window: id,
        focused: app.file_panels.has_focus(id),
        content: DockContent::Files,
    })
}

/// `content` with a panel (and its width) docked at its right edge. Without a panel
/// the content is laid out exactly as it would be on its own. The same widget is used either
/// way, so showing or hiding the panel keeps the content's widget state (terminal views, the
/// find bar's focus).
pub(crate) fn dock<'a>(
    content: Element<'a, Message>,
    panel: Option<Docked<'a>>,
) -> Element<'a, Message> {
    let (panel, width, window, focused, kind) = match panel {
        Some(d) => (Some(d.panel), d.width, Some(d.window), d.focused, d.content),
        None => (None, 0.0, None, false, DockContent::Files),
    };
    Element::new(Dock {
        content,
        panel,
        width,
        window,
        focused,
        kind,
    })
}

/// The active tab's content and, when shown, the file panel or the tunnels panel beside it,
/// whose left edge drags to resize it (reported to that panel as `Resize`, then `ResizeEnd`;
/// a drag also ends when the pointer leaves the window or the window loses focus).
///
/// It also decides who has the keyboard: a press in the panel gives it to the panel, a press
/// in the content gives it back (that panel's `Focus`); key presses and releases go only to
/// the side that has it, so the terminal never sees the panel's keys and the panel never
/// sees the terminal's.
struct Dock<'a> {
    content: Element<'a, Message>,
    panel: Option<Element<'a, Message>>,
    width: f32,
    window: Option<window::Id>,
    focused: bool,
    /// Which panel `panel` is.
    kind: DockContent,
}

#[derive(Default)]
struct DockState {
    dragging: bool,
}

impl Dock<'_> {
    /// Where a press grabs the panel's edge; `None` without a panel.
    fn edge(layout: Layout<'_>) -> Option<Rectangle> {
        let bounds = layout.bounds();
        let panel = layout.children().nth(1)?.bounds();
        Some(Rectangle {
            x: panel.x,
            width: DOCK_GRAB,
            ..bounds
        })
    }

    fn children(&self) -> impl Iterator<Item = &Element<'_, Message>> {
        std::iter::once(&self.content).chain(self.panel.as_ref())
    }
}

impl<'a> Widget<Message, iced::Theme, iced::Renderer> for Dock<'a> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<DockState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(DockState::default())
    }

    fn children(&self) -> Vec<Tree> {
        Dock::children(self).map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        // The content is always the first child: its state survives the panel coming and
        // going.
        let children: Vec<&Element<'_, Message>> = Dock::children(self).collect();
        tree.diff_children(&children);
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
        let max = limits.max();
        let Some(panel) = &mut self.panel else {
            let content =
                self.content
                    .as_widget_mut()
                    .layout(&mut tree.children[0], renderer, limits);
            return layout::Node::with_children(content.size(), vec![content]);
        };
        let width = docked_width(self.width, max.width);
        let content_width = max.width - width;
        let content = self.content.as_widget_mut().layout(
            &mut tree.children[0],
            renderer,
            &layout::Limits::new(Size::ZERO, Size::new(content_width, max.height)),
        );
        let panel = panel
            .as_widget_mut()
            .layout(
                &mut tree.children[1],
                renderer,
                &layout::Limits::new(Size::ZERO, Size::new(width, max.height)),
            )
            .move_to(Point::new(content_width, 0.0));
        layout::Node::with_children(max, vec![content, panel])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for ((child, state), layout) in std::iter::once(&mut self.content)
                .chain(self.panel.as_mut())
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
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<DockState>();
        if state.dragging && self.panel.is_none() {
            state.dragging = false;
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if Self::edge(layout).is_some_and(|edge| cursor.is_over(edge)) =>
            {
                state.dragging = true;
                shell.capture_event();
                return;
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) if state.dragging => {
                let width = bounds.x + bounds.width - position.x;
                shell.publish(self.kind.resize(width));
                shell.capture_event();
                return;
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.dragging => {
                state.dragging = false;
                shell.publish(self.kind.resize_end());
                shell.capture_event();
                return;
            }
            // The release may never come (the pointer left, another app took focus): the drag
            // ends here, keeping the width it reached.
            e if state.dragging && crate::tiles::cancels_drag(e) => {
                state.dragging = false;
                shell.publish(self.kind.resize_end());
            }
            _ => {}
        }
        // A press moves the keyboard to the side it lands on.
        if let (Event::Mouse(mouse::Event::ButtonPressed(_)), Some(window), Some(panel)) =
            (event, self.window, layout.children().nth(1))
            && let Some(at) = cursor.position()
        {
            let in_panel = panel.bounds().contains(at);
            let in_content = layout
                .children()
                .next()
                .is_some_and(|c| c.bounds().contains(at));
            if in_panel != self.focused && (in_panel || in_content) {
                shell.publish(self.kind.focus(window, in_panel));
            }
        }
        let keys = matches!(
            event,
            Event::Keyboard(
                iced::keyboard::Event::KeyPressed { .. }
                    | iced::keyboard::Event::KeyReleased { .. }
            )
        );
        let panel_has_keys = self.panel.is_some() && self.focused;
        for (i, ((child, state), layout)) in std::iter::once(&mut self.content)
            .chain(self.panel.as_mut())
            .zip(&mut tree.children)
            .zip(layout.children())
            .enumerate()
        {
            if keys && (i == 1) != panel_has_keys {
                continue;
            }
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let dragging = self.panel.is_some() && tree.state.downcast_ref::<DockState>().dragging;
        if dragging || Self::edge(layout).is_some_and(|edge| cursor.is_over(edge)) {
            return mouse::Interaction::ResizingHorizontally;
        }
        Dock::children(self)
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
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        for ((child, state), layout) in Dock::children(self)
            .zip(&tree.children)
            .zip(layout.children())
        {
            child
                .as_widget()
                .draw(state, renderer, theme, style, layout, cursor, viewport);
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
        let (first, rest) = tree.children.split_first_mut()?;
        let mut layouts = layout.children();
        let content = self.content.as_widget_mut().overlay(
            first,
            layouts.next()?,
            renderer,
            viewport,
            translation,
        );
        let panel = match (self.panel.as_mut(), rest.first_mut(), layouts.next()) {
            (Some(panel), Some(state), Some(layout)) => {
                panel
                    .as_widget_mut()
                    .overlay(state, layout, renderer, viewport, translation)
            }
            _ => None,
        };
        match (content, panel) {
            (Some(a), Some(b)) => Some(overlay::Group::with_children(vec![a, b]).overlay()),
            (a, b) => a.or(b),
        }
    }
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
        app.highlight(bar, alt, app.pane_background(pane))
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

pub(crate) fn state_color(t: &Tokens, state: &TabState) -> Color {
    match state {
        TabState::Connecting => t.warning,
        TabState::Connected => t.success,
        TabState::Disconnected(_) => t.danger,
        TabState::Exited(_) | TabState::NotConnected => t.text_muted,
    }
}

pub(crate) fn dot<'a>(color: Color) -> Element<'a, Message> {
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
    /// How the bar is drawn.
    chrome: Chrome,
    /// The window is in full screen: no room needed for the window buttons.
    fullscreen: bool,
    /// The window is maximized: the drawn maximize control restores.
    maximized: bool,
    /// The palette or a dialog covers the window.
    modal: bool,
    /// `FBTERM_DEMO_TOOLTIP` (debug aid): the button whose tooltip always shows.
    demo_tip: Option<DemoTip>,
    /// The chords tooltips name.
    bindings: &'b Bindings,
    /// A tab or tile being dragged in this window.
    carry: Option<Carry>,
    /// How many tunnels are up (the indicator's count).
    tunnels_up: usize,
    /// The window's tunnels panel is open: the button shows its active style.
    tunnels_open: bool,
    /// The files button: `None` hidden, else whether the window's file panel is open.
    files: Option<bool>,
    /// A newer release is out: a red dot on the settings button (see [`crate::update`]).
    update_dot: bool,
    /// Where the outline along the content's top and around the active tab ends (window x):
    /// the docked panel's left rule, else the window's right edge. `None` (size not known
    /// yet) draws no outline.
    outline_end: Option<f32>,
    /// The outline reaches the window's right edge (no panel docked), so it passes under
    /// drawn controls on the right.
    outline_to_edge: bool,
    /// The window's width, to size the tabs (`None`: full size).
    width: Option<f32>,
    /// The rename under way in this window, if any: its tab shows the field.
    renaming: Option<&'b crate::tab_rename::Renaming>,
    /// The tabs sit on the bar (tabs on top); with tabs on the left the bar holds only its
    /// buttons and paints plain.
    tabs_in_bar: bool,
    /// Where the outline along the content's top starts (window x): the sidebar's rule
    /// with tabs on the left, else the window's left edge.
    outline_start: f32,
    /// How far the tab strip is scrolled (see `tab_strip`), as the app keeps it.
    strip_offset: f32,
}

pub(crate) const BUTTON_WIDTH: f32 = 28.0;
pub(crate) const BAR_SPACING: f32 = 2.0;
const BAR_RIGHT_PADDING: f32 = 8.0;

pub(crate) fn trailing_width(tunnels_up: usize, files: bool) -> f32 {
    let count = crate::tunnels_panel::indicator_count(tunnels_up)
        .map_or(0.0, |n| 5.0 + 8.0 * n.len() as f32);
    let buttons = [
        BUTTON_WIDTH + 8.0,
        BUTTON_WIDTH,
        BUTTON_WIDTH + count,
        if files { BUTTON_WIDTH } else { 0.0 },
        BUTTON_WIDTH,
    ];
    let shown = buttons.iter().filter(|w| **w > 0.0).count() as f32;
    buttons.iter().sum::<f32>() + shown * BAR_SPACING + BAR_RIGHT_PADDING
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
        chrome,
        fullscreen,
        maximized,
        modal,
        demo_tip,
        bindings,
        tunnels_up,
        tunnels_open,
        files,
        update_dot,
        ..
    } = bar;
    let tabs_bar = bar;
    let height = chrome.bar_height();
    let msg = move |m| Message::TabBar(window, m);
    let mut bar = row![].spacing(2).align_y(Alignment::End);
    // With tabs on the left the bar lists none: no buttons, no hole, nothing to drag.
    let tabs: &[Tab] = if tabs_bar.tabs_in_bar { tabs } else { &[] };
    let trailing = trailing_width(tunnels_up, files.is_some());
    let strip = strip_layout(chrome, fullscreen, tabs_bar.width, tabs.len(), trailing);
    let offset = clamp_offset(tabs_bar.strip_offset, &strip);
    // The tabs sit in a clipped strip, the first child of the bar's row (`tab_rects`).
    if !tabs.is_empty() {
        let mut tab_row = row![].spacing(TAB_SPACING).align_y(Alignment::End);
        for (i, tab) in tabs.iter().enumerate() {
            tab_row = tab_row.push(tab_button(
                t,
                tabs_bar,
                tab,
                i,
                i == active,
                height - TAB_TOP_GAP,
                strip.tab_w,
            ));
        }
        bar = bar.push(Viewport::new(tab_row.into(), strip.strip_w, offset));
    }
    if strip.chevrons {
        let max = strip.content_w - strip.strip_w;
        let chevron = |glyph: &'static str, dir: i8, enabled: bool| {
            let color = if enabled {
                t.text_muted
            } else {
                t.text_muted.scale_alpha(0.35)
            };
            let button = button(icon(glyph, 14.0, color))
                .on_press_maybe(enabled.then(|| msg(TabBarMsg::StripStep(dir))))
                .padding(7)
                .style(move |_, status| ghost(t, status));
            container(button).center_y(height)
        };
        bar = bar.push(chevron(ICON_LEFT, -1, offset > 0.5));
        bar = bar.push(chevron(ICON_RIGHT, 1, offset < max - 0.5));
    }
    let new_tab = button(icon(ICON_PLUS, 14.0, t.text_muted))
        .on_press(msg(TabBarMsg::New))
        .padding(7)
        .style(move |_, status| ghost(t, status));
    let profiles = button(icon(ICON_USERS, 14.0, t.text_muted))
        .on_press(msg(TabBarMsg::Profiles))
        .padding(7)
        .style(move |_, status| ghost(t, status));
    // Active while the window's panel is open, as the files button.
    let tunnels_color = if tunnels_open { t.accent } else { t.text_muted };
    let mut indicator = row![icon(ICON_TUNNELS, 14.0, tunnels_color)]
        .spacing(5)
        .align_y(Alignment::Center);
    if let Some(n) = crate::tunnels_panel::indicator_count(tunnels_up) {
        indicator = indicator.push(
            text(n)
                .font(UI_FONT)
                .size(12)
                .line_height(text::LineHeight::Absolute(14.0.into()))
                .color(t.success),
        );
    }
    let tunnels = button(indicator)
        .on_press(msg(TabBarMsg::Tunnels))
        .padding(7)
        .style(move |_, status| ghost(t, status));
    let files = files.map(|open| {
        let color = if open { t.accent } else { t.text_muted };
        let button = button(icon(ICON_FOLDER, 14.0, color))
            .on_press(msg(TabBarMsg::Files))
            .padding(7)
            .style(move |_, status| ghost(t, status));
        tip(
            t,
            button,
            tooltip_text("Files", Action::FilePanel, bindings),
            demo_tip == Some(DemoTip::Files),
        )
    });
    let gear = icon(ICON_SETTINGS, 14.0, t.text_muted);
    let gear: Element<'_, Message> = if update_dot {
        // A badge over the gear's top-right corner, never a change of the icon itself.
        stack![
            gear,
            container(crate::ui::dot(t.danger, 6.0))
                .width(14)
                .height(14)
                .align_right(14)
                .align_top(14),
        ]
        .into()
    } else {
        gear
    };
    let settings = button(gear)
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
        tooltip_text("Profiles", Action::EditProfiles, bindings),
        demo_tip == Some(DemoTip::Profiles),
    );
    let tunnels = tip(
        t,
        tunnels,
        tooltip_text("Tunnels", Action::Tunnels, bindings),
        demo_tip == Some(DemoTip::Tunnels),
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
    // Every button comes after the strip: `BarBackground` finds the tabs in the row's first
    // child (`strip_and_tabs`).
    bar = bar.push(container(tunnels).center_y(height));
    if let Some(files) = files {
        bar = bar.push(container(files).center_y(height));
    }
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
        renaming: tabs_bar.renaming.map(|r| r.tab),
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
        outline_end: tabs_bar.outline_end,
        outline_start: tabs_bar.outline_start,
    });
    // Drawn controls sit beside the tabs' row, on a plate of the bar's color: beside the
    // background, never under it (a translucent bar would show twice), and never in the row,
    // whose first children `BarBackground` measures as the tabs. The plate is drawn after the
    // background, so it would cover the content outline's row along the bar's bottom edge:
    // the controls stop short of that row, and the strip under them shows the outline where
    // it reaches the window's edge. With a panel docked the outline ends at the panel's rule
    // and the strip keeps the plate's color, flush with the panel's surface below it.
    let bar = if chrome.draws_controls() {
        let setup = chrome::setup();
        let width = chrome::controls_width(setup.look, setup.controls.order.len());
        let strip_height = outline_width(tabs_bar.scale);
        let controls_height = height - strip_height;
        let outline_to_edge = tabs_bar.outline_to_edge;
        let strip = container(iced::widget::Space::new())
            .width(width)
            .height(strip_height)
            .style(move |_| container::Style {
                background: outline_to_edge.then_some(Background::Color(t.border)),
                snap: true,
                ..container::Style::default()
            });
        let controls = container(crate::ui::window_controls(
            t,
            window,
            maximized,
            controls_height,
        ))
        .height(controls_height)
        .align_y(Alignment::Center);
        let plate = container(column![controls, strip])
            .height(height)
            .style(move |_| container::Style {
                background: Some(bar_bg.into()),
                ..container::Style::default()
            });
        match chrome::setup().controls.side {
            chrome::Side::Left => row![plate, bar].into(),
            chrome::Side::Right => row![bar, plate].into(),
        }
    } else {
        bar
    };
    chrome::title_bar(chrome, bar, modal, move |a| Message::TitleBar(window, a))
}

/// Top corner radius of a tab (the active one, a hovered one and a tinted one), and of the
/// flares at the active tab's bottom; rounded to whole physical pixels by [`tab_radius`].
const TAB_RADIUS: f32 = 6.0;

/// `v` logical pixels moved to the nearest whole physical pixel, as the renderer snaps quads.
fn whole_px(v: f32, scale: f32) -> f32 {
    (v * scale + 0.001).round() / scale
}

/// `r` with its edges on whole physical pixels, where the renderer draws a snapped quad.
pub(crate) fn snap_rect(r: Rectangle, scale: f32) -> Rectangle {
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

/// One piece of the content outline: a straight run, or a quarter arc drawn as a ring quad
/// (transparent inside, its border `width` wide at the outer edge) clipped to `clip`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Stroke {
    Line(Rectangle),
    Arc {
        clip: Rectangle,
        ring: Rectangle,
        width: f32,
    },
}

/// The outline's width in window scale `scale`: one logical pixel, on whole physical pixels.
fn outline_width(scale: f32) -> f32 {
    crate::ui::hairline(scale)
}

/// The content outline's pieces in the tab bar, `width` wide: along the bar's bottom edge
/// `bottom` from `start` to `end`, and with an active tab `hole` (top corner radius
/// `radius`) around it instead of under it: through the left flare, up its side, over its
/// rounded top corners, down the other side and through the right flare. Every piece lies
/// outside the tab and outside the flares' tab-colored part, so the tab and the content read
/// as one outlined shape and nothing is drawn under the tab.
fn outline(
    start: f32,
    end: f32,
    bottom: f32,
    hole: Option<Rectangle>,
    radius: f32,
    width: f32,
) -> Vec<Stroke> {
    let w = width;
    let mut out = Vec::new();
    let mut line = |x0: f32, y0: f32, x1: f32, y1: f32| {
        if x1 > x0 && y1 > y0 {
            out.push(Stroke::Line(Rectangle::new(
                Point::new(x0, y0),
                Size::new(x1 - x0, y1 - y0),
            )));
        }
    };
    let Some(h) = hole else {
        line(start, bottom - w, end, bottom);
        return out;
    };
    let (right, tab_bottom) = (h.x + h.width, h.y + h.height);
    // The same radii as the flares and the corner pieces.
    let rf = radius.min(h.height);
    let rc = radius.min(h.width / 2.0).min(h.height);
    line(start, bottom - w, (h.x - rf).min(end), bottom);
    line((right + rf).max(start), bottom - w, end, bottom);
    line(h.x - w, h.y + rc, h.x, tab_bottom - rf);
    line(right, h.y + rc, right + w, tab_bottom - rf);
    line(h.x + rc, h.y - w, right - rc, h.y);
    let arc = |clip: Rectangle, center: Point, outer: f32| Stroke::Arc {
        clip,
        ring: Rectangle::new(
            Point::new(center.x - outer, center.y - outer),
            Size::new(2.0 * outer, 2.0 * outer),
        ),
        width: w,
    };
    // Flares: just inside the bar's disc, along its circle.
    for (x, cx) in [(h.x - rf, h.x - rf), (right, right + rf)] {
        out.push(arc(
            Rectangle::new(Point::new(x, tab_bottom - rf), Size::new(rf, rf)),
            Point::new(cx, tab_bottom - rf),
            rf,
        ));
    }
    // Top corners: just outside the tab's rounding.
    for (x, cx) in [(h.x - w, h.x + rc), (right - rc, right - rc)] {
        out.push(arc(
            Rectangle::new(Point::new(x, h.y - w), Size::new(rc + w, rc + w)),
            Point::new(cx, h.y + rc),
            rc + w,
        ));
    }
    out
}

/// Paints the tab bar's background everywhere except under the active tab (child `hole` of
/// the bar's row), which draws its own, and its flares; and the inactive tabs' backgrounds
/// (hover, tint), kept off the flares.
///
/// It also turns a press on a tab that travels `tab_drag::DRAG_THRESHOLD` into a tab drag (a shorter
/// one stays a click), reports the tab a carried tab hovers, and takes a tile dropped on the
/// bar as a new tab at the gap nearest the pointer.
struct BarBackground<'a> {
    window: window::Id,
    /// Each tab's id, in row order.
    ids: Vec<TabId>,
    carry: Option<Carry>,
    /// The palette or a dialog covers the window: no tab drags.
    modal: bool,
    /// The tab showing the rename field (see `tab_drag::Strip::renaming`).
    renaming: Option<TabId>,
    content: Element<'a, Message>,
    color: Color,
    hole: Option<usize>,
    tokens: &'static Tokens,
    around: Surroundings,
    /// Each tab's accent and how it shows, in row order.
    tabs: Vec<(Option<fbterm_config::Rgb>, fbterm_config::AccentStyle)>,
    /// Physical pixels per logical pixel.
    scale: f32,
    /// See [`TabBar::outline_end`].
    outline_end: Option<f32>,
    /// See [`TabBar::outline_start`].
    outline_start: f32,
}

/// The glyph a page tab shows in place of a session's state dot: the icon of the button
/// that opens it, or the app's own for the Welcome tab.
pub(crate) fn page_icon(page: Page) -> &'static str {
    match page {
        Page::Settings => ICON_SETTINGS,
        Page::Profiles => ICON_USERS,
        Page::Welcome => ICON_TERMINAL,
        Page::Schemes => ICON_PALETTE,
    }
}

/// How close to either end of the strip a drag scrolls it, and the most one pointer move
/// scrolls: a move scrolls as far as the pointer travelled, so the speed follows the hand, not
/// the device's event rate.
const EDGE_SCROLL_BAND: f32 = 24.0;
const EDGE_SCROLL_STEP: f32 = 24.0;

/// The active tab's rectangle when the strip shows all of it: the bar's hole and flares are cut
/// around a whole tab only. Cut at the strip's edge or scrolled out, the outline runs on.
fn visible_hole(tab: Rectangle, strip: Rectangle) -> Option<Rectangle> {
    let inside = tab.x >= strip.x - 0.01 && tab.x + tab.width <= strip.x + strip.width + 0.01;
    (tab.width > 0.0 && tab.height > 0.0 && inside).then_some(tab)
}

/// Where the violet gap marker stands for a carried tab or tile that would land before `rects[gap]`
/// (after the last one when `gap` is past the end), kept inside the strip.
fn marker_x(rects: &[Rectangle], gap: usize, strip: Rectangle) -> f32 {
    let x = match rects.get(gap) {
        Some(next) => next.x - 2.0,
        None => rects.last().map_or(strip.x, |r| r.x + r.width),
    };
    x.min(strip.x + strip.width - 2.0).max(strip.x)
}

/// The strip's bounds and its `n` tab buttons' bounds as laid out, scrolled out or not, in one
/// walk of the layout: the strip is the bar's row's first child, the tabs its row's children
/// (container -> row -> strip -> row).
fn strip_and_tabs(layout: Layout<'_>, n: usize) -> (Rectangle, Vec<Rectangle>) {
    let Some(strip) = layout
        .children()
        .next()
        .and_then(|row| row.children().next())
    else {
        return (Rectangle::default(), Vec::new());
    };
    let tabs = match strip.children().next() {
        Some(row) if n > 0 => row.children().take(n).map(|tab| tab.bounds()).collect(),
        _ => Vec::new(),
    };
    (strip.bounds(), tabs)
}

/// The tabs the strip `view` shows of `full` (see [`strip_and_tabs`]), whole or cut at its
/// edge: the index of the first one and their bounds clipped to the strip, in order (the shown
/// tabs are always a run). Tabs scrolled out have no place on screen and are left out.
fn shown_tabs(view: Rectangle, full: &[Rectangle]) -> (usize, Vec<Rectangle>) {
    let mut first = None;
    let mut shown = Vec::new();
    for (i, tab) in full.iter().enumerate() {
        if let Some(r) = tab.intersection(&view).filter(|r| r.width > 0.0) {
            first.get_or_insert(i);
            shown.push(r);
        }
    }
    (first.unwrap_or(0), shown)
}

/// Whether tabs are scrolled out of the strip on its left and on its right: a scroll toward a
/// side with nothing hidden would change nothing, so it is never sent.
fn hidden_sides(strip: Rectangle, full: &[Rectangle]) -> (bool, bool) {
    (
        full.first().is_some_and(|r| r.x < strip.x - 0.5),
        full.last()
            .is_some_and(|r| r.x + r.width > strip.x + strip.width + 0.5),
    )
}

impl Widget<Message, iced::Theme, iced::Renderer> for BarBackground<'_> {
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
        // The wheel over the strip scrolls it sideways, a vertical wheel too: the bar has no
        // other axis. Scrolling down or right moves toward later tabs. Nothing is sent toward
        // a side with no hidden tabs.
        let (view, full) = strip_and_tabs(layout, self.ids.len());
        if let Event::Mouse(mouse::Event::WheelScrolled { delta }) = event
            && !self.modal
            && !self.ids.is_empty()
            && cursor.is_over(view)
        {
            let (x, y) = match *delta {
                mouse::ScrollDelta::Lines { x, y } => (x * WHEEL_LINE_PX, y * WHEEL_LINE_PX),
                mouse::ScrollDelta::Pixels { x, y } => (x, y),
            };
            let along = if x.abs() > y.abs() { -x } else { -y };
            let (hidden_left, hidden_right) = hidden_sides(view, &full);
            if (along < 0.0 && hidden_left) || (along > 0.0 && hidden_right) {
                shell.publish(Message::TabBar(self.window, TabBarMsg::StripScroll(along)));
            }
            return;
        }
        let state = tree.state.downcast_mut::<DragState>();
        // The bar lists every tab: rows and window indices are the same.
        let indices: Vec<usize> = (0..self.ids.len()).collect();
        let (first, rects) = shown_tabs(view, &full);
        let strip = Strip {
            window: self.window,
            ids: &self.ids,
            indices: &indices,
            range: None,
            clamp: false,
            carry: self.carry,
            modal: self.modal,
            hole: self.hole,
            axis: Axis::X,
            scroll: 0.0,
            renaming: self.renaming,
            first,
        };
        // A tab or tile dragged near either end of the strip, or past it anywhere over the bar
        // (a fast pointer overshoots the band), scrolls it while tabs are hidden on that side,
        // so it can be dropped further along: by how far the pointer moved toward that edge
        // (at most `EDGE_SCROLL_STEP`; a move along the band or back toward the middle scrolls
        // nothing), never a timer.
        let dx = match event {
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let from = state.last_pointer.replace(*position);
                from.map_or(0.0, |from| {
                    (position.x - from.x).clamp(-EDGE_SCROLL_STEP, EDGE_SCROLL_STEP)
                })
            }
            _ => 0.0,
        };
        if let Event::Mouse(mouse::Event::CursorMoved { position }) = event
            && dx.abs() >= 0.5
            && !self.modal
            && !self.ids.is_empty()
            && (state.press.as_ref().is_some_and(|p| p.dragging)
                || matches!(self.carry, Some(Carry::Tile(_) | Carry::Tab(_))))
        {
            let bar = layout.bounds();
            let (hidden_left, hidden_right) = hidden_sides(view, &full);
            let over = position.y >= bar.y && position.y <= bar.y + bar.height;
            let left = over && dx < 0.0 && hidden_left && position.x <= view.x + EDGE_SCROLL_BAND;
            let right = over
                && dx > 0.0
                && hidden_right
                && position.x >= view.x + view.width - EDGE_SCROLL_BAND;
            if left || right {
                shell.publish(Message::TabBar(self.window, TabBarMsg::StripScroll(dx)));
            }
        }
        // The button pressed at the start of a drag is let go without a click: it sees the
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
        let bounds = layout.bounds();
        let (view, full) = strip_and_tabs(layout, self.tabs.len());
        let (first, buttons) = shown_tabs(view, &full);
        let tab_bounds: Vec<Rectangle> = buttons
            .iter()
            .map(|tab| snap_rect(*tab, self.scale))
            .collect();
        // The hole (and the flares beside it) only around a tab the strip shows whole.
        let strip = snap_rect(view, self.scale);
        let hole = self
            .hole
            .and_then(|i| full.get(i).copied())
            .map(|tab| snap_rect(tab, self.scale))
            .and_then(|tab| visible_hole(tab, strip));
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
        let shown = self.tabs.iter().skip(first);
        for (i, (&tab, &(accent, style))) in tab_bounds.iter().zip(shown).enumerate() {
            if Some(first + i) == self.hole {
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
        // With a panel docked, its rule's line carries on along the content's top
        // and around the active tab. From the window's left edge, over drawn controls on the
        // left too (drawn before the bar).
        if let Some(end) = self.outline_end {
            let bottom = whole_px(bounds.y + bounds.height, scale);
            let width = outline_width(scale);
            let color = self.tokens.border;
            let start = whole_px(self.outline_start, scale);
            for stroke in outline(start, whole_px(end, scale), bottom, hole, radius, width) {
                match stroke {
                    Stroke::Line(r) => renderer.fill_quad(quad(r, Border::default()), color),
                    Stroke::Arc { clip, ring, width } => {
                        let edge = quad(
                            ring,
                            Border {
                                color,
                                width,
                                radius: (ring.width / 2.0).into(),
                            },
                        );
                        renderer.with_layer(clip, |renderer| {
                            renderer.fill_quad(edge, Color::TRANSPARENT);
                        });
                    }
                }
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
        // A carried tile or tab: a violet bar in the gap where it would land.
        let state = tree.state.downcast_ref::<DragState>();
        if let (Some(Carry::Tile(_) | Carry::Tab(_)), Some(gap)) = (self.carry, state.gap) {
            let x = marker_x(&buttons, gap.saturating_sub(first), view);
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
    width: f32,
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
    let label: Element<'a, Message> = match bar.renaming.filter(|r| r.tab == tab.id) {
        Some(r) => crate::tab_rename::field(t, r, 12.5),
        None => text(tab.shown_name())
            .font(UI_FONT)
            .size(12.5)
            .wrapping(text::Wrapping::None)
            .color(idle.label)
            .into(),
    };
    let close = button(icon(ICON_X, 12.0, idle.icon))
        .on_press(Message::TabBar(window, TabBarMsg::Close(index)))
        .padding(3)
        .style(move |_, status| ghost(t, status));
    // The shortcut closes the active tab only, so only its button names it, and only while the
    // tab is one pane: in a split tab the chord closes the focused pane, the button the tab.
    let close = tip(
        t,
        close,
        if active && tab.panes.len() == 1 {
            tooltip_text("Close tab", Action::CloseTab, bar.bindings)
        } else {
            "Close tab".to_owned()
        },
        demo_tip,
    );
    let mut inner = row![stripe];
    // A page tab has no session to show: it carries the icon of the button that opens it.
    inner = match tab.page {
        None => inner.push(dot(state_color(t, tab.state()))),
        Some(page) => inner.push(icon(page_icon(page), 14.0, idle.icon)),
    };
    // A background pane that lost its session: a second dot, the theme's warning color.
    if tab.trouble() {
        inner = inner.push(dot(t.warning));
    }
    let inner = inner.push(container(label).width(Length::Fill).clip(true));
    // A tab shrunk below the usual minimum (a bar too narrow for the chevrons) has no room for
    // the close button; the shortcut and the tab menu still close it.
    let inner = if width >= TAB_MIN_WIDTH {
        inner.push(close)
    } else {
        inner
    };
    let inner = inner
        .spacing(8)
        .height(Length::Fill)
        .align_y(Alignment::Center);
    button(inner)
        .on_press(Message::TabBar(window, TabBarMsg::Select(index)))
        .width(Length::Fixed(width))
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
            scrollable(lines)
                .direction(scrollable::Direction::Vertical(crate::ui::edge_scrollbar(
                    false
                ),))
                .style(move |theme, status| crate::ui::scroll_style(t, theme, status))
                .height(Length::Shrink)
                .width(Length::Fill),
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

/// How the window's keys are handled (outside modals): a page tab's own mode, except that a
/// panel docked beside it has the keyboard while it is focused. Recording a shortcut in
/// Settings, and a question the Color schemes tab asks, keep the keys from the panel. A tab's
/// rename field takes them before all of these.
fn key_mode(
    page: Option<Page>,
    recording: bool,
    schemes: Option<&crate::schemes_window::SchemesState>,
    panel: Option<Mode>,
    find: bool,
    renaming: bool,
) -> Mode {
    // A tab's rename field has the keys, whatever the active tab shows.
    if renaming {
        return Mode::Rename;
    }
    let question = match (page, schemes) {
        (Some(Page::Schemes), Some(s)) if s.confirm.is_some() => Some(Mode::SchemesConfirm),
        (Some(Page::Schemes), Some(s)) if s.import.is_some() => Some(Mode::SchemesImport),
        (Some(Page::Schemes), Some(s)) if s.pending_update.is_some() => Some(Mode::SchemesUpdate),
        _ => None,
    };
    if let Some(mode) = question {
        return mode;
    }
    match (page, panel) {
        (Some(Page::Settings), _) if recording => Mode::Recording,
        (_, Some(panel)) => panel,
        (Some(Page::Profiles), _) => Mode::Editor,
        (Some(Page::Settings), _) => Mode::Settings,
        (Some(Page::Welcome), _) => Mode::Welcome,
        (Some(Page::Schemes), _) => Mode::Schemes,
        (None, _) if find => Mode::Find,
        (None, _) => Mode::Normal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_welcome_tab_has_its_own_keys() {
        assert!(matches!(
            key_mode(Some(Page::Welcome), false, None, None, false, false),
            Mode::Welcome
        ));
    }

    #[test]
    fn the_schemes_tab_has_its_keys_and_its_questions_keep_them() {
        let dir = std::env::temp_dir().join(format!(
            "fbterm-schemes-keys-{}",
            fbterm_config::ProfileId::new()
        ));
        let (set, _) = fbterm_config::schemes::SchemeSet::load(&dir);
        let mut s = crate::schemes_window::SchemesState::new(&set, Some("graphite"), Vec::new());
        let page = Some(Page::Schemes);
        let panel = Some(Mode::TunnelsPanel);
        assert!(matches!(
            key_mode(page, false, Some(&s), None, false, false),
            Mode::Schemes
        ));
        assert!(matches!(
            key_mode(page, false, Some(&s), panel, false, false),
            Mode::TunnelsPanel
        ));
        s.pending_update = Some("meadow".into());
        assert!(matches!(
            key_mode(page, false, Some(&s), panel, false, false),
            Mode::SchemesUpdate
        ));
        s.import = Some(crate::schemes_window::ImportAsk {
            scheme: set.all()[0].clone(),
            existing: "X".into(),
        });
        assert!(matches!(
            key_mode(page, false, Some(&s), None, false, false),
            Mode::SchemesImport
        ));
        s.confirm = Some(crate::schemes_window::DeleteAsk {
            id: "x".into(),
            name: "X".into(),
            profiles: 0,
        });
        assert!(matches!(
            key_mode(page, false, Some(&s), None, false, false),
            Mode::SchemesConfirm
        ));
        // Another page tab active: the Color schemes question is not its keys'.
        assert!(matches!(
            key_mode(Some(Page::Settings), false, Some(&s), None, false, false),
            Mode::Settings
        ));
    }

    #[test]
    fn a_focused_panel_keeps_its_keys_beside_a_page_tab() {
        let panel = Some(Mode::TunnelsPanel);
        for page in [Page::Settings, Page::Profiles, Page::Welcome] {
            assert!(matches!(
                key_mode(Some(page), false, None, panel, false, false),
                Mode::TunnelsPanel
            ));
        }
        assert!(matches!(
            key_mode(Some(Page::Settings), false, None, None, false, false),
            Mode::Settings
        ));
        assert!(matches!(
            key_mode(Some(Page::Profiles), false, None, None, false, false),
            Mode::Editor
        ));
        // Recording a shortcut still wins.
        assert!(matches!(
            key_mode(Some(Page::Settings), true, None, panel, false, false),
            Mode::Recording
        ));
        assert!(matches!(
            key_mode(None, false, None, None, true, false),
            Mode::Find
        ));
        assert!(matches!(
            key_mode(None, false, None, None, false, false),
            Mode::Normal
        ));
        // A rename field takes the keys before a panel, a find bar or a page tab's own.
        for page in [None, Some(Page::Settings)] {
            assert!(matches!(
                key_mode(page, true, None, panel, true, true),
                Mode::Rename
            ));
        }
    }

    const OUTER: Rectangle = Rectangle {
        x: 0.0,
        y: 0.0,
        width: 600.0,
        height: 32.0,
    };
    /// A bar wide enough for every tab the outline tests place.
    const OUTER_WIDE: Rectangle = Rectangle {
        x: 0.0,
        y: 0.0,
        width: 1000.0,
        height: 32.0,
    };
    const TAB: Rectangle = Rectangle {
        x: 100.0,
        y: 4.0,
        width: 196.0,
        height: 28.0,
    };

    #[test]
    fn only_a_tab_fully_in_the_strip_keeps_its_hole() {
        let strip = Rectangle::new(Point::new(100.0, 0.0), Size::new(300.0, 40.0));
        let tab = |x: f32, w: f32| Rectangle::new(Point::new(x, 4.0), Size::new(w, 36.0));
        assert_eq!(
            visible_hole(tab(150.0, 90.0), strip),
            Some(tab(150.0, 90.0))
        );
        assert_eq!(
            visible_hole(tab(100.0, 90.0), strip),
            Some(tab(100.0, 90.0)),
            "flush"
        );
        assert_eq!(
            visible_hole(tab(80.0, 90.0), strip),
            None,
            "cut on the left"
        );
        assert_eq!(
            visible_hole(tab(350.0, 90.0), strip),
            None,
            "cut on the right"
        );
        assert_eq!(visible_hole(tab(500.0, 90.0), strip), None, "scrolled out");
        assert_eq!(visible_hole(tab(150.0, 0.0), strip), None, "nothing of it");
    }

    #[test]
    fn the_gap_marker_never_leaves_the_strip() {
        let strip = Rectangle::new(Point::new(100.0, 0.0), Size::new(300.0, 40.0));
        let rect = |x: f32, w: f32| Rectangle::new(Point::new(x, 4.0), Size::new(w, 36.0));
        // Two hidden tabs left of the strip (zero wide at its edge), two visible, one hidden
        // right of it.
        let rects = [
            rect(100.0, 0.0),
            rect(100.0, 0.0),
            rect(100.0, 80.0),
            rect(182.0, 80.0),
            rect(400.0, 0.0),
        ];
        assert_eq!(marker_x(&rects, 0, strip), 100.0, "before the hidden ones");
        assert_eq!(
            marker_x(&rects, 3, strip),
            180.0,
            "between two visible tabs"
        );
        assert_eq!(
            marker_x(&rects, 4, strip),
            398.0,
            "kept inside the strip's end"
        );
        assert_eq!(marker_x(&rects, 5, strip), 398.0, "after the last tab");
        assert_eq!(
            marker_x(&[], 0, Rectangle::default()),
            0.0,
            "no tabs, no panic"
        );
    }

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
        use crate::chrome::BarAction;
        use crate::layout::PaneId;
        use crate::tab_strip::WHEEL_LINE_PX;
        use crate::windows::{Carry, DragMsg};

        const SIZE: Size = Size::new(1000.0, 40.0);

        /// Every tab's bounds clipped to the strip, zero wide at the strip's edge on their
        /// side when scrolled out: the tests address tabs by index.
        fn tab_rects(layout: Layout<'_>, n: usize) -> Vec<Rectangle> {
            let (view, full) = strip_and_tabs(layout, n);
            full.into_iter()
                .map(|b| {
                    b.intersection(&view).unwrap_or(Rectangle {
                        x: if b.x < view.x {
                            view.x
                        } else {
                            view.x + view.width
                        },
                        width: 0.0,
                        ..b
                    })
                })
                .collect()
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
            drive_with(Chrome::Unified, tabs, carry, |b, _| steps(b))
        }

        /// The drawn controls as laid out: their plate, the strip under them and each
        /// button's bounds (none without drawn chrome). Reads as the buttons' slice.
        #[derive(Debug, Default)]
        struct DrawnControls {
            plate: Option<Rectangle>,
            strip: Option<Rectangle>,
            buttons: Vec<Rectangle>,
        }

        impl std::ops::Deref for DrawnControls {
            type Target = [Rectangle];
            fn deref(&self) -> &[Rectangle] {
                &self.buttons
            }
        }

        /// [`drive`] with the bar drawn by `chrome`; `steps` also gets the drawn controls'
        /// bounds (none without them).
        fn drive_with(
            chrome: Chrome,
            tabs: &[Tab],
            carry: Option<Carry>,
            steps: impl FnOnce(&[Rectangle], &DrawnControls) -> Vec<(Event, Point)>,
        ) -> Vec<Message> {
            drive_full(chrome, tabs, carry, None, 0.0, |p| {
                steps(&p.tabs, &p.controls)
            })
        }

        /// What a bar's layout holds, for [`drive_full`]'s steps.
        struct Parts {
            /// Each tab's bounds, clipped to the strip (zero wide when hidden).
            tabs: Vec<Rectangle>,
            /// The strip's bounds.
            strip: Rectangle,
            /// The row's children after the strip: chevrons, then the buttons.
            rest: Vec<Rectangle>,
            controls: DrawnControls,
        }

        /// [`drive_with`] in a window `width` wide (`None`: the harness's 1000 px, tabs at full
        /// size) with the strip scrolled by `strip_offset`.
        fn drive_full(
            chrome: Chrome,
            tabs: &[Tab],
            carry: Option<Carry>,
            width: Option<f32>,
            strip_offset: f32,
            steps: impl FnOnce(&Parts) -> Vec<(Event, Point)>,
        ) -> Vec<Message> {
            let size = Size::new(width.unwrap_or(SIZE.width), SIZE.height);
            let bindings = Bindings::defaults(cfg!(target_os = "macos"));
            let t = crate::theme::graphite_tokens();
            let bar = TabBar {
                window: window::Id::unique(),
                bar_bg: t.surface,
                around: Surroundings {
                    terminal: Color::BLACK,
                    bar: t.surface,
                    opacity: 1.0,
                },
                scale: 1.0,
                chrome,
                fullscreen: false,
                maximized: false,
                modal: false,
                demo_tip: None,
                bindings: &bindings,
                carry,
                // The indicator with a count sits after the tabs; tabs are measured as before.
                tunnels_up: 3,
                tunnels_open: true,
                // So is the files button.
                files: Some(true),
                // And the settings button with its update dot.
                update_dot: true,
                // And the outline a docked file panel brings.
                outline_end: Some(500.0),
                outline_to_edge: false,
                width,
                renaming: None,
                tabs_in_bar: true,
                outline_start: 0.0,
                strip_offset,
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
                &layout::Limits::new(Size::ZERO, size),
            );
            // title bar -> background -> container -> row -> tabs; with drawn controls
            // (on the right in tests) title bar -> row -> [background, plate -> column ->
            // [container -> controls, strip]].
            let root = Layout::new(&node);
            let (background, controls) = if chrome.draws_controls() {
                let mut parts = root.children();
                let background = parts.next().expect("the background");
                let plate = parts.next().expect("the plate");
                let mut column = plate.children().next().expect("the column").children();
                let buttons: Vec<Rectangle> = column
                    .next()
                    .expect("the controls' container")
                    .children()
                    .next()
                    .expect("the controls")
                    .children()
                    .map(|l| l.bounds())
                    .collect();
                let strip = column.next().expect("the strip").bounds();
                let controls = DrawnControls {
                    plate: Some(plate.bounds()),
                    strip: Some(strip),
                    buttons,
                };
                (background, controls)
            } else {
                (root, DrawnControls::default())
            };
            let parts = Parts {
                tabs: tab_rects(background, tabs.len()),
                strip: strip_and_tabs(background, 0).0,
                rest: background
                    .children()
                    .next()
                    .expect("the row")
                    .children()
                    .skip(usize::from(!tabs.is_empty()))
                    .map(|l| l.bounds())
                    .collect(),
                controls,
            };
            let viewport = Rectangle::with_size(size);
            let mut messages = Vec::new();
            for (event, at) in steps(&parts) {
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
                !messages.iter().any(|m| matches!(
                    m,
                    Message::TitleBar(_, BarAction::Drag | BarAction::ToggleMaximize)
                )),
                "a press on a tab never drags the window"
            );
        }

        fn many_tabs(n: usize) -> Vec<Tab> {
            (0..n)
                .map(|i| {
                    Tab::single(Pane::new(
                        fbterm_config::ProfileId::new(),
                        format!("tab {i}"),
                        None,
                    ))
                })
                .collect()
        }

        fn click(p: Point) -> Vec<(Event, Point)> {
            vec![(moved(p), p), (press(), p), (release(), p)]
        }

        fn strip_steps(messages: &[Message]) -> Vec<i8> {
            messages
                .iter()
                .filter_map(|m| match m {
                    Message::TabBar(_, TabBarMsg::StripStep(d)) => Some(*d),
                    _ => None,
                })
                .collect()
        }

        #[test]
        fn buttons_stay_inside_the_window_with_many_tabs() {
            let tabs = many_tabs(40);
            let _ = drive_full(Chrome::Unified, &tabs, None, Some(800.0), 0.0, |p| {
                assert_eq!(
                    p.rest.len(),
                    7,
                    "two chevrons and five buttons: {:?}",
                    p.rest
                );
                let last = p.rest.last().unwrap();
                assert!(
                    last.x + last.width <= 800.0 - BAR_RIGHT_PADDING + 0.5,
                    "{last:?}"
                );
                assert!(p.strip.x + p.strip.width <= p.rest[0].x, "{:?}", p.strip);
                vec![]
            });
        }

        #[test]
        fn buttons_follow_the_last_tab_until_the_end() {
            let at = |n: usize, width: f32| {
                let tabs = many_tabs(n);
                let mut found = (0.0, 0.0);
                let _ = drive_full(Chrome::Unified, &tabs, None, Some(width), 0.0, |p| {
                    let last = p.tabs.last().unwrap();
                    found = (last.x + last.width, p.rest[p.rest.len() - 5].x);
                    vec![]
                });
                found
            };
            let (tab_end, plus) = at(3, 1600.0);
            assert_eq!(plus, tab_end + TAB_SPACING, "right after the last tab");
            let (_, many) = at(40, 800.0);
            let (_, more) = at(80, 800.0);
            assert_eq!(many, more, "stopped at the end of the free space");
        }

        #[test]
        fn no_chevrons_while_everything_fits() {
            let _ = drive_full(
                Chrome::Unified,
                &many_tabs(3),
                None,
                Some(1600.0),
                0.0,
                |p| {
                    assert_eq!(p.rest.len(), 5, "{:?}", p.rest);
                    vec![]
                },
            );
        }

        #[test]
        fn a_tab_scrolled_out_cannot_be_clicked() {
            let tabs = many_tabs(40);
            let messages = drive_full(Chrome::Unified, &tabs, None, Some(800.0), 500.0, |p| {
                assert_eq!(p.tabs[0].width, 0.0, "the first tab is scrolled out");
                // Where the hidden tab would be: left of the strip.
                let mut steps = click(Point::new(p.strip.x - 10.0, 20.0));
                let k = p
                    .tabs
                    .iter()
                    .position(|r| r.width >= 80.0)
                    .expect("a visible tab");
                steps.extend(click(p.tabs[k].center()));
                steps
            });
            let first_visible = (500.0_f32 / (TAB_MIN_WIDTH + TAB_SPACING)).ceil() as usize;
            assert_eq!(selects(&messages), [first_visible], "only the visible tab");
        }

        #[test]
        fn the_pointer_over_a_button_never_reaches_a_tab_under_it() {
            let tabs = many_tabs(40);
            let messages = drive_full(Chrome::Unified, &tabs, None, Some(800.0), 0.0, |p| {
                click(p.rest[2].center())
            });
            assert!(selects(&messages).is_empty(), "{messages:?}");
        }

        #[test]
        fn the_left_chevron_ignores_presses_at_the_start_and_the_right_at_the_end() {
            let tabs = many_tabs(40);
            let l = strip_layout(
                Chrome::Unified,
                false,
                Some(800.0),
                40,
                trailing_width(3, true),
            );
            let end = l.content_w - l.strip_w;
            let messages = drive_full(Chrome::Unified, &tabs, None, Some(800.0), 0.0, |p| {
                let mut s = click(p.rest[0].center());
                s.extend(click(p.rest[1].center()));
                s
            });
            assert_eq!(
                strip_steps(&messages),
                [1],
                "only the right one at the start"
            );
            let messages = drive_full(Chrome::Unified, &tabs, None, Some(800.0), end, |p| {
                let mut s = click(p.rest[0].center());
                s.extend(click(p.rest[1].center()));
                s
            });
            assert_eq!(strip_steps(&messages), [-1], "only the left one at the end");
        }

        fn strip_scrolls(messages: &[Message]) -> Vec<f32> {
            messages
                .iter()
                .filter_map(|m| match m {
                    Message::TabBar(_, TabBarMsg::StripScroll(d)) => Some(*d),
                    _ => None,
                })
                .collect()
        }

        fn wheel(delta: mouse::ScrollDelta) -> Event {
            Event::Mouse(mouse::Event::WheelScrolled { delta })
        }

        #[test]
        fn a_wheel_over_the_strip_scrolls_it_sideways() {
            let tabs = many_tabs(40);
            let messages = drive_full(Chrome::Unified, &tabs, None, Some(800.0), 300.0, |p| {
                let at = p.strip.center();
                vec![
                    (wheel(mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 }), at),
                    (wheel(mouse::ScrollDelta::Lines { x: 1.0, y: 0.0 }), at),
                    (wheel(mouse::ScrollDelta::Pixels { x: 0.0, y: -30.0 }), at),
                ]
            });
            assert_eq!(
                strip_scrolls(&messages),
                [WHEEL_LINE_PX, -WHEEL_LINE_PX, 30.0]
            );
        }

        #[test]
        fn a_wheel_toward_a_side_with_nothing_hidden_sends_nothing() {
            let toward_start = |p: &Parts| {
                let at = p.strip.center();
                vec![(wheel(mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 }), at)]
            };
            let toward_end = |p: &Parts| {
                let at = p.strip.center();
                vec![(wheel(mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 }), at)]
            };
            let tabs = many_tabs(40);
            let l = strip_layout(
                Chrome::Unified,
                false,
                Some(800.0),
                40,
                trailing_width(3, true),
            );
            let end = l.content_w - l.strip_w;
            let at_start = drive_full(Chrome::Unified, &tabs, None, Some(800.0), 0.0, toward_start);
            assert!(strip_scrolls(&at_start).is_empty(), "{at_start:?}");
            let at_end = drive_full(Chrome::Unified, &tabs, None, Some(800.0), end, toward_end);
            assert!(strip_scrolls(&at_end).is_empty(), "{at_end:?}");
            let few = many_tabs(3);
            let fit = drive_full(Chrome::Unified, &few, None, Some(800.0), 0.0, toward_start);
            assert!(strip_scrolls(&fit).is_empty(), "{fit:?}");
            let fit = drive_full(Chrome::Unified, &few, None, Some(800.0), 0.0, toward_end);
            assert!(strip_scrolls(&fit).is_empty(), "{fit:?}");
        }

        #[test]
        fn a_wheel_off_the_strip_does_nothing() {
            let tabs = many_tabs(40);
            let messages = drive_full(Chrome::Unified, &tabs, None, Some(800.0), 0.0, |p| {
                let down = wheel(mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 });
                vec![
                    // A chevron, the `+` button and the empty padding right of the buttons.
                    (down.clone(), p.rest[0].center()),
                    (down.clone(), p.rest[2].center()),
                    (down, Point::new(799.0, 20.0)),
                ]
            });
            assert!(strip_scrolls(&messages).is_empty(), "{messages:?}");
        }

        #[test]
        fn dragging_near_the_strip_edge_scrolls_it() {
            let tabs = many_tabs(40);
            let mut edges = (0.0, 0.0);
            // A tab dragged: past the threshold, then to the right edge, then the left, then
            // the middle.
            let messages = drive_full(Chrome::Unified, &tabs, None, Some(800.0), 300.0, |p| {
                edges = (p.strip.x, p.strip.x + p.strip.width);
                let k = p.tabs.iter().position(|r| r.width >= 80.0).unwrap() + 1;
                let from = p.tabs[k].center();
                let mid = Point::new(p.strip.center().x, from.y);
                let right = Point::new(p.strip.x + p.strip.width - 5.0, from.y);
                let left = Point::new(p.strip.x + 5.0, from.y);
                vec![
                    (moved(from), from),
                    (press(), from),
                    (moved(mid), mid),
                    (moved(right), right),
                    (moved(left), left),
                    (moved(mid), mid),
                ]
            });
            assert!(edges.1 > edges.0);
            assert_eq!(
                strip_scrolls(&messages),
                [EDGE_SCROLL_STEP, -EDGE_SCROLL_STEP],
                "{messages:?}"
            );
        }

        #[test]
        fn a_drag_past_the_band_keeps_scrolling_and_stops_at_the_ends() {
            let tabs = many_tabs(40);
            let carried = Some(Carry::Tile(PaneId(7)));
            let mut to_chevron = 0.0;
            let past = |p: &Parts| {
                // Above the tabs at the right edge, then over the right chevron, then left of
                // the strip.
                let above = Point::new(p.strip.x + p.strip.width - 5.0, p.strip.y - 4.0);
                let chevron = Point::new(p.rest[1].center().x, p.strip.center().y);
                let left = Point::new(p.strip.x - 1.0, p.strip.center().y);
                to_chevron = (chevron.x - above.x).min(EDGE_SCROLL_STEP);
                // A move to measure from first: the strip's middle scrolls nothing.
                let start = p.strip.center();
                vec![
                    (moved(start), start),
                    (moved(above), above),
                    (moved(chevron), chevron),
                    (moved(left), left),
                ]
            };
            let messages = drive_full(Chrome::Unified, &tabs, carried, Some(800.0), 0.0, past);
            assert!(to_chevron >= 0.5, "the chevron sits right of the strip");
            assert_eq!(
                strip_scrolls(&messages),
                [EDGE_SCROLL_STEP, to_chevron],
                "nothing hidden on the left at the start"
            );
            // Tabs that fit: nothing to scroll anywhere.
            let few = many_tabs(3);
            let messages = drive_full(Chrome::Unified, &few, carried, Some(800.0), 0.0, |p| {
                let at = Point::new(p.strip.x + p.strip.width - 5.0, p.strip.center().y);
                vec![(moved(at), at)]
            });
            assert!(strip_scrolls(&messages).is_empty(), "{messages:?}");
        }

        #[test]
        fn edge_scroll_goes_by_the_distance_moved_not_the_event_count() {
            let tabs = many_tabs(40);
            let carried = Some(Carry::Tile(PaneId(7)));
            // Twenty 1 px moves in the band (a fast mouse) scroll 20 px in all, one long move
            // at most one step, and a pointer that stays put nothing.
            let messages = drive_full(Chrome::Unified, &tabs, carried, Some(800.0), 0.0, |p| {
                let y = p.strip.center().y;
                let x0 = p.strip.x + p.strip.width - 22.0;
                (0..=20)
                    .map(|i| Point::new(x0 + i as f32, y))
                    .chain([Point::new(x0 + 20.0, y)])
                    .map(|at| (moved(at), at))
                    .collect()
            });
            let scrolls = strip_scrolls(&messages);
            assert_eq!(scrolls.len(), 20, "{scrolls:?}");
            assert!(
                scrolls.iter().all(|d| (d - 1.0).abs() < 0.01),
                "{scrolls:?}"
            );
        }

        #[test]
        fn edge_scroll_follows_only_the_outward_horizontal_move() {
            let tabs = many_tabs(40);
            let carried = Some(Carry::Tile(PaneId(7)));
            // In the right band: 6 px down, 3 px back toward the middle, then 4 px right and
            // 8 px down at once. Only the last scrolls, and by its 4 px outward.
            let messages = drive_full(Chrome::Unified, &tabs, carried, Some(800.0), 0.0, |p| {
                let (x, y) = (p.strip.x + p.strip.width - 10.0, p.strip.y + 2.0);
                [
                    (x, y),
                    (x, y + 6.0),
                    (x - 3.0, y + 6.0),
                    (x + 1.0, y + 14.0),
                ]
                .map(|(x, y)| Point::new(x, y))
                .into_iter()
                .map(|at| (moved(at), at))
                .collect()
            });
            assert_eq!(strip_scrolls(&messages), [4.0], "{messages:?}");
        }

        #[test]
        fn a_carried_tile_near_the_strip_edge_scrolls_it_and_nothing_carried_does_not() {
            let tabs = many_tabs(40);
            let near_right = |p: &Parts| {
                let from = Point::new(p.strip.x + p.strip.width - 15.0, 20.0);
                let at = Point::new(p.strip.x + p.strip.width - 5.0, 20.0);
                vec![(moved(from), from), (moved(at), at)]
            };
            let carried = Some(Carry::Tile(PaneId(7)));
            let messages = drive_full(
                Chrome::Unified,
                &tabs,
                carried,
                Some(800.0),
                0.0,
                near_right,
            );
            // The first move has nothing to measure from; the second went 10 px.
            assert_eq!(strip_scrolls(&messages), [10.0]);
            // Nothing carried and nothing dragged: a pointer at the edge does nothing.
            let messages = drive_full(Chrome::Unified, &tabs, None, Some(800.0), 0.0, near_right);
            assert!(strip_scrolls(&messages).is_empty(), "{messages:?}");
        }

        #[test]
        fn a_drop_past_the_strip_lands_where_the_marker_shows() {
            let tabs = many_tabs(40);
            let pane = PaneId(9);
            let drop = |at: fn(&Parts) -> Point| {
                let mut visible = (0, 0);
                let messages = drive_full(
                    Chrome::Unified,
                    &tabs,
                    Some(Carry::Tile(pane)),
                    Some(800.0),
                    500.0,
                    |p| {
                        let first = p.tabs.iter().position(|r| r.width > 0.0).unwrap();
                        let last = p.tabs.iter().rposition(|r| r.width > 0.0).unwrap();
                        visible = (first, last);
                        let at = at(p);
                        vec![(moved(at), at), (release(), at)]
                    },
                );
                (drags(&messages), visible)
            };
            // Over a chevron: after the last visible tab, not after the last hidden one.
            let (dropped, (_, last)) = drop(|p| p.rest[0].center());
            assert_eq!(
                dropped,
                [DragMsg::TileToTab {
                    pane,
                    index: last + 1
                }]
            );
            // Left of the strip: before the first visible tab.
            let (dropped, (first, _)) = drop(|p| Point::new(p.strip.x - 4.0, 20.0));
            assert_eq!(dropped, [DragMsg::TileToTab { pane, index: first }]);
        }

        fn pointer_reports(messages: &[Message]) -> Vec<bool> {
            messages
                .iter()
                .filter_map(|m| match m {
                    Message::TitleBar(_, BarAction::PointerOverWidget(over)) => Some(*over),
                    _ => None,
                })
                .collect()
        }

        #[test]
        fn the_unified_bar_reports_the_pointer_onto_and_off_a_tab() {
            let tabs = tabs();
            let messages = drive(&tabs, None, |b| {
                let tab = b[1].center();
                let along = Point::new(tab.x + 4.0, tab.y);
                // Far right of the bar: empty space.
                let empty = Point::new(SIZE.width - 20.0, tab.y);
                vec![
                    (moved(empty), empty),
                    (moved(tab), tab),
                    (moved(along), along),
                    (moved(empty), empty),
                    (moved(tab), tab),
                    (Event::Mouse(mouse::Event::CursorLeft), tab),
                    (moved(tab), tab),
                    (Event::Window(window::Event::Unfocused), tab),
                    (moved(along), along),
                ]
            });
            // Empty first, onto the tab once (not again along it), off, on, out of the window,
            // back, and once more after focus changed (the app made the window movable).
            assert_eq!(
                pointer_reports(&messages),
                [false, true, false, true, false, true, true]
            );
        }

        #[test]
        fn a_tab_drag_ending_off_the_tabs_reports_the_pointer_off() {
            let tabs = tabs();
            let id = tabs[1].id;
            let messages = drive(&tabs, Some(Carry::Tab(id)), |b| {
                let at = b[1].center();
                let below = Point::new(at.x, SIZE.height + 100.0);
                vec![
                    (moved(at), at),
                    (press(), at),
                    (moved(below), below),
                    (release(), below),
                ]
            });
            assert_eq!(pointer_reports(&messages).last(), Some(&false));
        }

        #[test]
        fn drawn_bars_never_report_the_pointer() {
            let tabs = tabs();
            let messages = drive_with(Chrome::Drawn, &tabs, None, |b, _| {
                let tab = b[1].center();
                let empty = Point::new(SIZE.width / 2.0, tab.y);
                vec![
                    (moved(tab), tab),
                    (moved(empty), empty),
                    (Event::Mouse(mouse::Event::CursorLeft), empty),
                ]
            });
            assert!(pointer_reports(&messages).is_empty(), "{messages:?}");
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
        fn a_carried_tab_dropped_in_a_gap_asks_to_move_there() {
            let tabs = tabs();
            let id = tabs[0].id;
            // Tab 0 released on the right half of tab 1: the gap after it, index 2.
            let messages = drive(&tabs, Some(Carry::Tab(id)), |b| {
                let at = b[0].center();
                let away = Point::new(at.x + 10.0, at.y);
                let over = Point::new(b[1].center_x() + 10.0, at.y);
                vec![
                    (press(), at),
                    (moved(away), away),
                    (moved(over), over),
                    (release(), over),
                ]
            });
            assert_eq!(
                drags(&messages)
                    .iter()
                    .filter(|d| matches!(d, DragMsg::ReorderTab { .. }))
                    .collect::<Vec<_>>(),
                [&DragMsg::ReorderTab { tab: id, index: 2 }],
                "{messages:?}"
            );
            assert_eq!(drags(&messages).last(), Some(&DragMsg::End));

            // Released on the left half of the next tab: the gap right after itself, no move.
            let messages = drive(&tabs, Some(Carry::Tab(id)), |b| {
                let at = b[0].center();
                let away = Point::new(at.x + 10.0, at.y);
                let over = Point::new(b[1].center_x() - 10.0, at.y);
                vec![
                    (press(), at),
                    (moved(away), away),
                    (moved(over), over),
                    (release(), over),
                ]
            });
            assert!(
                !drags(&messages)
                    .iter()
                    .any(|d| matches!(d, DragMsg::ReorderTab { .. })),
                "{messages:?}"
            );
        }

        fn bar_actions(messages: &[Message]) -> Vec<BarAction> {
            messages
                .iter()
                .filter_map(|m| match m {
                    Message::TitleBar(_, a) => Some(*a),
                    _ => None,
                })
                .collect()
        }

        #[test]
        fn drawn_controls_act_on_the_window_and_tabs_still_work() {
            let tabs = tabs();
            let click = |p: Point| vec![(moved(p), p), (press(), p), (release(), p)];
            // Each control, left to right: minimize, maximize, close.
            let messages = drive_with(Chrome::Drawn, &tabs, None, |_, c| {
                assert_eq!(c.len(), 3, "{c:?}");
                c.iter().flat_map(|r| click(r.center())).collect()
            });
            assert_eq!(
                bar_actions(&messages),
                [
                    BarAction::Minimize,
                    BarAction::ToggleMaximize,
                    BarAction::Close
                ]
            );
            // The controls are at the bar's right end, clear of the tabs.
            let messages = drive_with(Chrome::Drawn, &tabs, None, |b, c| {
                assert!(c[0].x > b[2].x + b[2].width);
                // Flush with the edge (Windows) or one gap from it (Linux).
                let from_edge = SIZE.width - (c[2].x + c[2].width);
                assert!((-0.5..=8.5).contains(&from_edge), "{c:?}");
                click(b[1].center())
            });
            assert_eq!(selects(&messages), [1]);
            assert!(bar_actions(&messages).is_empty());
            // Empty bar space still moves the window.
            let messages = drive_with(Chrome::Drawn, &tabs, None, |_, c| {
                let gap = Point::new(c[0].x - 10.0, 20.0);
                vec![(moved(gap), gap), (press(), gap)]
            });
            assert_eq!(bar_actions(&messages), [BarAction::Drag]);
            // A tab drag works as without drawn controls.
            let id = tabs[1].id;
            let messages = drive_with(Chrome::Drawn, &tabs, None, |b, _| {
                let at = b[1].center();
                let far = Point::new(at.x, at.y + 6.0);
                vec![(press(), at), (moved(far), far), (release(), at)]
            });
            assert_eq!(drags(&messages)[0], DragMsg::StartTab(id));
        }

        #[test]
        fn drawn_controls_end_above_the_content_outline() {
            // The plate under the controls is drawn after the bar's background, so it would
            // cover the outline's row along the bar's bottom edge: the controls stop one
            // outline width short of the bottom, and the strip under them is that row (the
            // plate still fills the bar, so nothing behind it shows through).
            let tabs = tabs();
            drive_with(Chrome::Drawn, &tabs, None, |_, c| {
                assert_eq!(c.len(), 3, "{c:?}");
                let bar = Chrome::Drawn.bar_height();
                let limit = bar - outline_width(1.0);
                let plate = c.plate.expect("the plate");
                assert_eq!(plate.y + plate.height, bar, "{plate:?}");
                for r in c.iter() {
                    assert!(
                        r.y + r.height <= limit + 0.001,
                        "{r:?} reaches below {limit}"
                    );
                }
                let strip = c.strip.expect("the strip");
                assert_eq!((strip.y, strip.y + strip.height), (limit, bar), "{strip:?}");
                assert_eq!((strip.x, strip.width), (plate.x, plate.width), "{strip:?}");
                Vec::new()
            });
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

    /// `v` lies on a whole physical pixel at `scale`.
    fn on_px(v: f32, scale: f32) -> bool {
        (v * scale - (v * scale).round()).abs() < 1e-3
    }

    /// `a` and `b` agree up to float rounding.
    fn same(a: Rectangle, b: Rectangle) -> bool {
        [
            (a.x, b.x),
            (a.y, b.y),
            (a.width, b.width),
            (a.height, b.height),
        ]
        .iter()
        .all(|(p, q)| (p - q).abs() < 1e-3)
    }

    fn rect_on_px(r: Rectangle, scale: f32) -> bool {
        [r.x, r.y, r.x + r.width, r.y + r.height]
            .iter()
            .all(|&v| on_px(v, scale))
    }

    #[test]
    fn the_outline_goes_around_the_active_tab_on_whole_pixels() {
        for (scale, px) in [(1.0, 1.0), (2.0, 2.0), (1.25, 1.0)] {
            // A tab laid out off the pixel grid, snapped as the bar snaps it.
            let raw = Rectangle::new((100.3, 4.1).into(), Size::new(196.45, 27.9));
            let h = snap_rect(raw, scale);
            let r = tab_radius(scale);
            let w = outline_width(scale);
            assert_eq!(w * scale, px, "one logical pixel at {scale}");
            let bottom = h.y + h.height;
            let end = whole_px(700.4, scale);
            let strokes = outline(0.0, end, bottom, Some(h), r, w);
            let mut lines = Vec::new();
            let mut arcs = Vec::new();
            for s in &strokes {
                match *s {
                    Stroke::Line(l) => lines.push(l),
                    Stroke::Arc { clip, ring, width } => arcs.push((clip, ring, width)),
                }
            }
            assert_eq!((lines.len(), arcs.len()), (5, 4), "{scale}");
            for &l in &lines {
                assert!(rect_on_px(l, scale), "{l:?} at {scale}");
                // Never under the tab.
                assert!(l.intersection(&h).is_none_or(|i| i.width * i.height < 1e-4));
            }
            for &(clip, ring, width) in &arcs {
                assert!(
                    rect_on_px(clip, scale) && rect_on_px(ring, scale),
                    "{scale}"
                );
                assert_eq!(width, w);
            }
            let right = h.x + h.width;
            let line = |x: f32, y: f32, x1: f32, y1: f32| {
                Rectangle::new((x, y).into(), Size::new(x1 - x, y1 - y))
            };
            // Along the bar's bottom up to the flares, then from them to the end.
            assert!(
                same(lines[0], line(0.0, bottom - w, h.x - r, bottom)),
                "{:?} at {scale}",
                lines[0]
            );
            assert!(
                same(lines[1], line(right + r, bottom - w, end, bottom)),
                "{:?} at {scale}",
                lines[1]
            );
            // The sides, just outside the tab, between its top corners and the flares.
            assert!(
                same(lines[2], line(h.x - w, h.y + r, h.x, bottom - r)),
                "{:?} at {scale}",
                lines[2]
            );
            assert!(
                same(lines[3], line(right, h.y + r, right + w, bottom - r)),
                "{:?} at {scale}",
                lines[3]
            );
            // The top, just above it, between its corners.
            assert!(
                same(lines[4], line(h.x + r, h.y - w, right - r, h.y)),
                "{:?} at {scale}",
                lines[4]
            );
            // Flares: the flare squares, along the inside of the bar's disc (outer radius r,
            // centered where the flare's circle is), so never on the tab's part.
            let fl = flares(OUTER_WIDE, h, r);
            for (i, f) in fl.iter().enumerate() {
                let (clip, ring, _) = arcs[i];
                assert!(same(clip, f.square), "{:?} at {scale}", clip);
                assert!(same(ring, f.disc()), "{:?} at {scale}", ring);
            }
            // Top corners: centered on the tab's corner circles, starting where they end.
            let tab_corners = corners(h, r);
            for (i, c) in tab_corners.iter().enumerate() {
                let (clip, ring, width) = arcs[2 + i];
                assert!(ring.center().distance(c.ring.center()) < 1e-3);
                assert!((ring.width / 2.0 - width - r).abs() < 1e-3);
                // The clip reaches out by the outline's width, over and beside the tab.
                assert!((clip.y - (h.y - w)).abs() < 1e-3);
                assert!((clip.width - (r + w)).abs() < 1e-3);
            }
            assert!((arcs[2].0.x - (h.x - w)).abs() < 1e-3);
            assert!((arcs[3].0.x - (right - r)).abs() < 1e-3);
        }
    }

    #[test]
    fn the_outline_without_an_active_tab_is_one_line_and_stops_at_the_panel() {
        let strokes = outline(0.0, 500.0, 32.0, None, TAB_RADIUS, 1.0);
        assert_eq!(
            strokes,
            [Stroke::Line(Rectangle::new(
                (0.0, 31.0).into(),
                Size::new(500.0, 1.0)
            ))]
        );
        // An active tab past the panel's rule: the bottom line still ends at the rule.
        let far = Rectangle { x: 600.0, ..TAB };
        let strokes = outline(0.0, 500.0, 32.0, Some(far), TAB_RADIUS, 1.0);
        let bottoms: Vec<Rectangle> = strokes
            .iter()
            .filter_map(|s| match s {
                Stroke::Line(l) if l.y == 31.0 => Some(*l),
                _ => None,
            })
            .collect();
        assert_eq!(
            bottoms,
            [Rectangle::new((0.0, 31.0).into(), Size::new(500.0, 1.0))]
        );
    }
}

#[cfg(test)]
mod tab_width_tests {
    use super::*;

    #[test]
    fn the_trailing_room_follows_the_files_button_and_the_tunnel_count() {
        let base = trailing_width(0, false);
        assert!(trailing_width(0, true) > base, "files button");
        assert!(trailing_width(3, false) > base, "a count");
        assert!(
            trailing_width(12, false) > trailing_width(3, false),
            "two digits"
        );
    }
}
