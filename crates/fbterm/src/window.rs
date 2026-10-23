//! One window's view: tab bar, notices and the active tab's terminal.

use std::path::Path;

use fbterm_term::terminal_view;
use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::widget::{button, column, container, row, scrollable, stack, svg, text};
use iced::window::{self, Screenshot};
use iced::{
    Alignment, Background, Border, Color, Element, Event, Length, Padding, Rectangle, Size, Vector,
    keyboard, mouse,
};

use crate::app::{App, Message, TabBarMsg};
use crate::chrome::{self, Chrome};
use crate::editor::EditorMsg;
use crate::keys::{self, Action};
use crate::palette::{self, PaletteMsg};
use crate::prompts::PromptMsg;
use crate::tabs::{Tab, TabState};
use crate::theme::{Tokens, UI_FONT};

/// Gap between the top of the tab bar and the top of a tab.
const TAB_TOP_GAP: f32 = 4.0;

// Lucide icons (ISC License, Copyright (c) Lucide Contributors): "x" and "plus".
const ICON_X: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></svg>"#;
const ICON_PLUS: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12h14"/><path d="M12 5v14"/></svg>"#;

fn icon<'a>(bytes: &'static [u8], size: f32, color: Color) -> Element<'a, Message> {
    svg(svg::Handle::from_memory(bytes))
        .width(size)
        .height(size)
        .style(move |_, _| svg::Style { color: Some(color) })
        .into()
}

pub fn view(app: &App, id: window::Id) -> Element<'_, Message> {
    let t = app.tokens;
    if let Some((eid, state)) = &app.editor
        && *eid == id
    {
        let degraded = app.fallback.as_ref().is_some_and(|f| f.degraded());
        let mut content = column![];
        if Chrome::current().draws_title_bar() {
            content = content.push(editor_header(t, id));
        }
        let content = content.push(crate::editor::view::view(t, state, degraded));
        return Element::new(Shortcuts {
            content: content.into(),
            window: id,
            mode: Mode::Editor,
        });
    }
    let Some(win) = app.windows.get(&id) else {
        return container(text("")).into();
    };
    let style = if app.app_active && app.focused == Some(id) {
        &app.style
    } else {
        &app.style_unfocused
    };
    // The default background and the chrome around it take the background opacity; text,
    // cursor, selection and cells with their own background stay opaque (the terminal widget
    // draws those and leaves its default background to the container below).
    let opacity = app.settings.background_opacity;
    let term_bg = to_color(style.scheme.background).scale_alpha(opacity);
    let bar_bg = t.surface.scale_alpha(opacity);
    let palette = app.palettes.get(&id);
    let dialog = app.prompts.front(id);
    let bar = TabBar {
        window: id,
        bar_bg,
        active_bg: term_bg,
        fullscreen: app.fullscreen.contains(&id),
        modal: palette.is_some() || dialog.is_some(),
    };
    let mut body = column![tab_bar(t, bar, &win.tabs, win.active)];
    let notices = app.visible_notices();
    if !notices.is_empty() {
        body = body.push(notice_banner(t, bar_bg, notices));
    }
    // Only the active tab builds a terminal view; background tabs render nothing.
    let content: Element<'_, Message> = match win.active_tab().and_then(Tab::terminal) {
        Some(term) => terminal_view(term, style, move |o| Message::Terminal(id, o)),
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
    // Under an overlay nothing reaches the terminal; only the palette toggle still works,
    // and not while a dialog waits for an answer.
    let covered = if dialog.is_some() {
        Mode::Blocked {
            palette_toggle: false,
        }
    } else {
        Mode::Blocked {
            palette_toggle: true,
        }
    };
    let base = Element::new(Shortcuts {
        content: body.into(),
        window: id,
        mode: if palette.is_some() || dialog.is_some() {
            covered
        } else {
            Mode::Normal
        },
    });
    let mut layers = stack![base];
    if let Some(state) = palette {
        layers = layers.push(Element::new(Shortcuts {
            content: palette::view(t, id, state),
            window: id,
            mode: if dialog.is_some() {
                covered
            } else {
                Mode::Palette
            },
        }));
    }
    if let Some(d) = dialog {
        layers = layers.push(Element::new(Shortcuts {
            content: crate::prompt_view::view(t, id, d),
            window: id,
            mode: Mode::Prompt,
        }));
    }
    layers.into()
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
struct TabBar {
    window: window::Id,
    bar_bg: Color,
    /// The active tab's background (the terminal's).
    active_bg: Color,
    /// The window is in full screen: no room needed for the window buttons.
    fullscreen: bool,
    /// The palette or a dialog covers the window.
    modal: bool,
}

fn tab_bar<'a>(
    t: &'static Tokens,
    bar: TabBar,
    tabs: &'a [Tab],
    active: usize,
) -> Element<'a, Message> {
    let TabBar {
        window,
        bar_bg,
        active_bg,
        fullscreen,
        modal,
    } = bar;
    let chrome = Chrome::current();
    let height = chrome.bar_height();
    let msg = move |m| Message::TabBar(window, m);
    let mut bar = row![].spacing(2).align_y(Alignment::End);
    for (i, tab) in tabs.iter().enumerate() {
        bar = bar.push(tab_button(
            t,
            window,
            tab,
            i,
            (i == active).then_some(active_bg),
            height - TAB_TOP_GAP,
            tab_radius(bar_bg),
        ));
    }
    let new_tab = button(icon(ICON_PLUS, 14.0, t.text_muted))
        .on_press(msg(TabBarMsg::New))
        .padding(7)
        .style(move |_, status| ghost(t, status));
    bar = bar.push(
        container(new_tab)
            .padding(Padding::from([0.0, 4.0]))
            .center_y(height),
    );
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
        content: bar.into(),
        color: bar_bg,
        hole: (active < tabs.len()).then_some(active),
        corner: tab_radius(bar_bg),
    });
    chrome::title_bar(bar, modal, move |a| Message::TitleBar(window, a))
}

/// Top corner radius of a tab.
const TAB_RADIUS: f32 = 6.0;

/// The tabs' top corner radius at `bar_bg`: square on a translucent bar, where the bar's
/// background behind a rounded corner would add to the tab's own alpha and show as a notch.
fn tab_radius(bar_bg: Color) -> f32 {
    if bar_bg.a < 1.0 { 0.0 } else { TAB_RADIUS }
}

/// The pieces of `outer` that cover everything but `hole`, plus the hole's top corner squares
/// (behind its rounded corners). With a translucent background the active tab must not be
/// drawn over the bar's background, or the two alphas would add up there.
fn around(outer: Rectangle, hole: Rectangle, corner: f32) -> Vec<Rectangle> {
    let hole = match outer.intersection(&hole) {
        Some(h) => h,
        None => return vec![outer],
    };
    let mut out = Vec::with_capacity(6);
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
    push(outer.x, outer.y, outer.width, hole.y - outer.y);
    push(
        outer.x,
        bottom,
        outer.width,
        outer.y + outer.height - bottom,
    );
    push(outer.x, hole.y, hole.x - outer.x, hole.height);
    push(right, hole.y, outer.x + outer.width - right, hole.height);
    let r = corner.min(hole.width / 2.0).min(hole.height);
    push(hole.x, hole.y, r, r);
    push(right - r, hole.y, r, r);
    out
}

/// Paints the tab bar's background everywhere except under the active tab (child `hole` of
/// the bar's row), which draws its own.
struct BarBackground<'a> {
    content: Element<'a, Message>,
    color: Color,
    hole: Option<usize>,
    /// Top corner radius of the tab in the hole.
    corner: f32,
}

impl Widget<Message, iced::Theme, iced::Renderer> for BarBackground<'_> {
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
        // container -> row -> tab
        let hole = self.hole.and_then(|i| {
            layout
                .children()
                .next()
                .and_then(|row| row.children().nth(i))
                .map(|tab| tab.bounds())
        });
        let pieces = match hole {
            Some(h) => around(bounds, h, self.corner),
            None => vec![bounds],
        };
        for piece in pieces {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: piece,
                    snap: true,
                    ..renderer::Quad::default()
                },
                self.color,
            );
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

/// The profile editor's own bar where the OS title bar is gone: room for the window buttons
/// and to move the window. It stays blank; the sidebar already titles the window.
fn editor_header<'a>(t: &'static Tokens, window: window::Id) -> Element<'a, Message> {
    let header = container(text(""))
        .width(Length::Fill)
        .height(Chrome::current().bar_height())
        .style(move |_| container::Style {
            background: Some(t.surface.into()),
            ..container::Style::default()
        });
    let rule = container(text(""))
        .width(Length::Fill)
        .height(1)
        .style(move |_| container::Style {
            background: Some(t.border.into()),
            ..container::Style::default()
        });
    chrome::title_bar(column![header, rule], false, move |a| {
        Message::TitleBar(window, a)
    })
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

/// A tab; `active_bg` is set for the active tab, which takes the terminal's background so the
/// two read as one surface.
fn tab_button<'a>(
    t: &'static Tokens,
    window: window::Id,
    tab: &'a Tab,
    index: usize,
    active_bg: Option<Color>,
    height: f32,
    radius: f32,
) -> Element<'a, Message> {
    let active = active_bg.is_some();
    let stripe = container(text(""))
        .width(3)
        .height(14)
        .style(move |_| container::Style {
            background: tab.accent.map(|a| to_color(a).into()),
            border: Border {
                radius: 1.5.into(),
                ..Border::default()
            },
            ..container::Style::default()
        });
    let label = text(tab.title.as_str())
        .font(UI_FONT)
        .size(12.5)
        .wrapping(text::Wrapping::None)
        .color(if active { t.text } else { t.text_secondary });
    let close = button(icon(ICON_X, 12.0, t.text_muted))
        .on_press(Message::TabBar(window, TabBarMsg::Close(index)))
        .padding(3)
        .style(move |_, status| ghost(t, status));
    let inner = row![
        stripe,
        dot(state_color(t, &tab.state)),
        container(label).width(Length::Fill).clip(true),
        close
    ]
    .spacing(8)
    .height(Length::Fill)
    .align_y(Alignment::Center);
    let surface_bg = active_bg.map(Background::Color);
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
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: surface_bg.or(hovered.then_some(Background::Color(t.hover))),
                text_color: t.text,
                border: Border {
                    radius: iced::border::Radius::default().top(radius),
                    ..Border::default()
                },
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

/// How a [`Shortcuts`] wrapper treats keys.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Application shortcuts act; other keys pass on.
    Normal,
    /// Beneath an overlay: every key is dropped, so nothing reaches the terminal and no
    /// shortcut acts behind the overlay, except the palette toggle when `palette_toggle`.
    Blocked { palette_toggle: bool },
    /// The palette's own: Up/Down, Ctrl+P/N, Enter and Escape drive it; the rest goes on to the
    /// search box.
    Palette,
    /// A connection dialog: Escape cancels, Enter submits; other keys go to its field.
    Prompt,
    /// The profile editor window: Cmd+W closes it, Cmd+S saves; other shortcuts do nothing.
    Editor,
}

/// The palette message a key means while the palette is up.
fn palette_key(key: &keyboard::Key, mods: keyboard::Modifiers) -> Option<PaletteMsg> {
    use keyboard::key::Named;
    match key {
        keyboard::Key::Named(Named::ArrowUp) => Some(PaletteMsg::Up),
        keyboard::Key::Named(Named::ArrowDown) => Some(PaletteMsg::Down),
        keyboard::Key::Named(Named::Enter) => Some(PaletteMsg::Submit),
        keyboard::Key::Named(Named::Escape) => Some(PaletteMsg::Close),
        keyboard::Key::Character(c) if mods.control() && !mods.shift() && !mods.logo() => {
            match c.as_str() {
                "p" => Some(PaletteMsg::Up),
                "n" => Some(PaletteMsg::Down),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Wraps a window's content and turns application shortcuts into messages before the terminal
/// sees the key, so Ctrl+Shift chords never reach the shell.
struct Shortcuts<'a> {
    content: Element<'a, Message>,
    window: window::Id,
    mode: Mode,
}

/// What a key press means to a [`Shortcuts`] wrapper in `mode` over window `w`:
/// `Some(Some(msg))` publish and stop it, `Some(None)` stop it, `None` let it through.
fn key_message(
    mode: Mode,
    w: window::Id,
    key: &keyboard::Key,
    mods: keyboard::Modifiers,
    repeat: bool,
) -> Option<Option<Message>> {
    use keyboard::key::Named;
    let shortcut = keys::shortcut(key, mods);
    match mode {
        Mode::Normal => shortcut.map(|action| {
            // Holding a key repeats tab switching only.
            (!repeat || matches!(action, Action::NextTab | Action::PrevTab))
                .then_some(Message::Shortcut(w, action))
        }),
        Mode::Blocked { palette_toggle } => shortcut.map(|action| {
            (palette_toggle && action == Action::Palette && !repeat)
                .then_some(Message::Shortcut(w, action))
        }),
        // Other shortcuts fall through to the layer beneath, which handles them.
        Mode::Palette => palette_key(key, mods).map(|m| Some(Message::Palette(w, m))),
        Mode::Prompt => match key {
            keyboard::Key::Named(Named::Escape) => {
                Some(Some(Message::Prompt(PromptMsg::Cancel(w))))
            }
            keyboard::Key::Named(Named::Enter) => Some(Some(Message::Prompt(PromptMsg::Submit(w)))),
            _ => shortcut.map(|_| None),
        },
        Mode::Editor => {
            if mods.command() && matches!(key, keyboard::Key::Character(c) if c.as_str() == "s") {
                return Some((!repeat).then_some(Message::Editor(EditorMsg::Save)));
            }
            shortcut.map(|action| match action {
                Action::CloseTab if !repeat => Some(Message::Editor(EditorMsg::Close)),
                Action::Quit if !repeat => Some(Message::Shortcut(w, action)),
                _ => None,
            })
        }
    }
}

impl Widget<Message, iced::Theme, iced::Renderer> for Shortcuts<'_> {
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
        if let Event::Keyboard(keyboard::Event::KeyPressed {
            key,
            modifiers,
            repeat,
            ..
        }) = event
            && let Some(msg) = key_message(self.mode, self.window, key, *modifiers, *repeat)
        {
            if let Some(msg) = msg {
                shell.publish(msg);
            }
            shell.capture_event();
            return;
        }
        if matches!(self.mode, Mode::Blocked { .. }) && matches!(event, Event::Keyboard(_)) {
            shell.capture_event();
            return;
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
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
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

    fn chord(c: &str, shift: bool) -> (keyboard::Key, keyboard::Modifiers) {
        let mut mods = if cfg!(target_os = "macos") {
            keyboard::Modifiers::LOGO
        } else {
            keyboard::Modifiers::CTRL | keyboard::Modifiers::SHIFT
        };
        if shift {
            mods |= keyboard::Modifiers::SHIFT;
        }
        (keyboard::Key::Character(c.into()), mods)
    }

    fn press(mode: Mode, c: &str, shift: bool) -> Option<Option<Message>> {
        let (key, mods) = chord(c, shift);
        key_message(mode, window::Id::unique(), &key, mods, false)
    }

    #[test]
    fn under_the_palette_only_the_toggle_acts() {
        let blocked = Mode::Blocked {
            palette_toggle: true,
        };
        for c in ["w", "n", "t", "1", "9"] {
            assert!(
                matches!(press(blocked, c, false), Some(None)),
                "{c} must be swallowed behind the palette"
            );
        }
        assert!(matches!(
            press(blocked, "p", true),
            Some(Some(Message::Shortcut(_, Action::Palette)))
        ));
        assert!(matches!(
            press(Mode::Normal, "w", false),
            Some(Some(Message::Shortcut(_, Action::CloseTab)))
        ));
    }

    #[test]
    fn the_bar_background_leaves_the_active_tab_to_itself() {
        let outer = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 600.0,
            height: 32.0,
        };
        let tab = Rectangle {
            x: 100.0,
            y: 4.0,
            width: 196.0,
            height: 28.0,
        };
        let pieces = around(outer, tab, 6.0);
        let area = |r: &Rectangle| r.width * r.height;
        let covered: f32 = pieces.iter().map(area).sum();
        // Everything but the tab, plus its two 6x6 corner squares.
        assert_eq!(covered, area(&outer) - area(&tab) + 2.0 * 36.0);
        // Nothing covers the middle of the tab.
        let inside = iced::Point::new(198.0, 20.0);
        assert!(pieces.iter().all(|r| !r.contains(inside)));
        // Square tabs (translucent bar) get no corner squares.
        let covered: f32 = around(outer, tab, 0.0).iter().map(area).sum();
        assert_eq!(covered, area(&outer) - area(&tab));
        assert_eq!(tab_radius(Color::WHITE), TAB_RADIUS);
        assert_eq!(tab_radius(Color::WHITE.scale_alpha(0.85)), 0.0);
        // With no overlap the whole bar is painted.
        let away = Rectangle { x: 900.0, ..tab };
        assert_eq!(around(outer, away, 6.0), vec![outer]);
    }

    #[test]
    fn under_a_dialog_nothing_acts() {
        let blocked = Mode::Blocked {
            palette_toggle: false,
        };
        assert!(matches!(press(blocked, "p", true), Some(None)));
        assert!(matches!(press(Mode::Prompt, "w", false), Some(None)));
        let esc = keyboard::Key::Named(keyboard::key::Named::Escape);
        assert!(matches!(
            key_message(
                Mode::Prompt,
                window::Id::unique(),
                &esc,
                keyboard::Modifiers::empty(),
                false
            ),
            Some(Some(Message::Prompt(PromptMsg::Cancel(_))))
        ));
        // Plain typing reaches the dialog's field.
        let a = keyboard::Key::Character("a".into());
        assert!(
            key_message(
                Mode::Prompt,
                window::Id::unique(),
                &a,
                keyboard::Modifiers::empty(),
                false
            )
            .is_none()
        );
    }
}
