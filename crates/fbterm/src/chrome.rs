//! Window chrome: how a window's top edge is drawn on each platform.
//!
//! macOS uses a unified title bar: the OS title strip is transparent and hidden, the content
//! runs under it, and the tab bar (or the editor's header) takes its place with the native
//! window buttons drawn over its leading end. Empty space in that bar moves the window and a
//! double click zooms it.
//!
//! Linux and Windows keep the OS title bar and decorations for now. They are a separate
//! [`Chrome`] strategy so drawn window controls can be added there later without touching the
//! views.

use iced::advanced::layout::{self, Layout};
use iced::advanced::mouse::{self, click};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Size, Vector, window};

/// Whether a menu item with `action` and `key` (its key equivalent) is the app menu's Quit on
/// `Cmd+Q`: the item [`release_quit_key`] takes the key from.
#[cfg(any(test, target_os = "macos"))]
pub(crate) fn is_quit_key_item(action: Option<&str>, key: &str) -> bool {
    action == Some("terminate:") && key.eq_ignore_ascii_case("q")
}

/// Take `Cmd+Q` from the macOS app menu's Quit, so the key reaches the window and goes
/// through the app's own Quit (which asks first while transfers run). The menu item stays;
/// choosing it with the mouse still ends the app at once. Returns whether the key was found
/// and freed (the menu exists only once the app has launched); does nothing elsewhere.
#[cfg(target_os = "macos")]
pub(crate) fn release_quit_key() -> bool {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;
    // AppKit's menus belong to the main thread, where iced runs `update`.
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let Some(menu) = NSApplication::sharedApplication(mtm).mainMenu() else {
        return false;
    };
    let mut freed = false;
    for top in menu.itemArray().iter() {
        let Some(submenu) = top.submenu() else {
            continue;
        };
        for item in submenu.itemArray().iter() {
            let action = item
                .action()
                .map(|sel| sel.name().to_string_lossy().into_owned());
            if is_quit_key_item(action.as_deref(), &item.keyEquivalent().to_string()) {
                item.setKeyEquivalent(&NSString::from_str(""));
                freed = true;
            }
        }
    }
    if freed {
        tracing::info!("Cmd+Q now goes through the app's Quit");
    }
    freed
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn release_quit_key() -> bool {
    true
}

/// Height of the macOS title bar area in logical pixels (measured on macOS 27: the window
/// buttons are 14 px tall, centered 16 px from the top).
pub const MACOS_TITLEBAR_HEIGHT: f32 = 32.0;

/// Where content may start after the macOS window buttons (the zoom button ends at 69 px).
pub const MACOS_TRAFFIC_LIGHTS_INSET: f32 = 78.0;

/// Height of the tab bar where the OS draws its own title bar.
const NATIVE_BAR_HEIGHT: f32 = 36.0;

/// Leading padding of the tab bar where nothing is drawn over it.
const NATIVE_LEADING_INSET: f32 = 8.0;

/// How a platform draws the top edge of a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chrome {
    /// The tab bar is the title bar; the OS draws only its window buttons over it (macOS).
    Unified,
    /// The OS draws a title bar with its decorations above the content (Linux, Windows).
    Native,
}

/// What a press in the empty part of a title bar asks of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarAction {
    /// Move the window with the pointer.
    Drag,
    /// Zoom the window, or restore it.
    ToggleMaximize,
}

impl Chrome {
    /// The strategy of the platform this build runs on.
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Unified
        } else {
            Self::Native
        }
    }

    /// Height of the bar at the top of a window (the tab bar, the editor's header).
    pub fn bar_height(self) -> f32 {
        match self {
            Self::Unified => MACOS_TITLEBAR_HEIGHT,
            Self::Native => NATIVE_BAR_HEIGHT,
        }
    }

    /// Space before the first tab: clear of the window buttons where they sit in the bar. In
    /// full screen the OS hides them, so the tabs start at the edge.
    pub fn leading_inset(self, fullscreen: bool) -> f32 {
        match self {
            Self::Unified if !fullscreen => MACOS_TRAFFIC_LIGHTS_INSET,
            Self::Unified | Self::Native => NATIVE_LEADING_INSET,
        }
    }

    /// Whether this window has a bar of its own above the content (the editor's header): only
    /// where the OS title bar is gone.
    pub fn draws_title_bar(self) -> bool {
        self == Self::Unified
    }

    /// What a left press in the bar means. `captured` is whether a widget in the bar (a tab, a
    /// button) took it; only presses on empty space act on the window. `modal` is whether the
    /// palette or a dialog covers the window: then the bar does nothing.
    pub fn bar_press(self, captured: bool, modal: bool, kind: click::Kind) -> Option<BarAction> {
        if captured || modal || self == Self::Native {
            return None;
        }
        match kind {
            click::Kind::Single => Some(BarAction::Drag),
            click::Kind::Double => Some(BarAction::ToggleMaximize),
            click::Kind::Triple => None,
        }
    }

    /// `base` with this platform's title-bar treatment applied.
    pub fn window_settings(self, base: window::Settings) -> window::Settings {
        #[cfg(target_os = "macos")]
        if self == Self::Unified {
            return window::Settings {
                platform_specific: window::settings::PlatformSpecific {
                    title_hidden: true,
                    titlebar_transparent: true,
                    fullsize_content_view: true,
                },
                ..base
            };
        }
        base
    }
}

/// How a terminal window is created for a background opacity and blur setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backdrop {
    /// The window can show what is behind it. Fixed when the window opens.
    pub transparent: bool,
    /// What shows through is blurred. Fixed when the window opens.
    pub blur: bool,
}

impl Chrome {
    /// The backdrop of a terminal window opened now. On macOS windows are always created
    /// transparent and paint an opaque background at full opacity, so a changed opacity applies
    /// to open windows at once; elsewhere only a window opened while the opacity is below 1 is
    /// transparent. Blur only matters, and is only asked for, below full opacity.
    pub fn backdrop(self, opacity: f32, blur: bool) -> Backdrop {
        let translucent = opacity < 1.0;
        Backdrop {
            transparent: translucent || self == Self::Unified,
            blur: blur && translucent,
        }
    }
}

/// The task for a bar action on window `id`.
pub fn perform<T>(id: window::Id, action: BarAction) -> iced::Task<T> {
    match action {
        BarAction::Drag => window::drag(id),
        BarAction::ToggleMaximize => window::toggle_maximize(id),
    }
}

/// Wraps a title bar: left presses its children leave alone become [`BarAction`]s.
/// `modal`: the palette or a dialog covers the window, so presses leave the window alone.
pub fn title_bar<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    modal: bool,
    on_action: impl Fn(BarAction) -> Message + 'a,
) -> Element<'a, Message> {
    Element::new(TitleBar {
        content: content.into(),
        on_action: Box::new(on_action),
        chrome: Chrome::current(),
        modal,
    })
}

struct TitleBar<'a, Message> {
    content: Element<'a, Message>,
    on_action: Box<dyn Fn(BarAction) -> Message + 'a>,
    chrome: Chrome,
    modal: bool,
}

#[derive(Default)]
struct State {
    previous: Option<mouse::Click>,
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for TitleBar<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
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
        let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event else {
            return;
        };
        let Some(position) = cursor.position_over(layout.bounds()) else {
            return;
        };
        let state = tree.state.downcast_mut::<State>();
        let captured = shell.is_event_captured();
        let click = mouse::Click::new(position, mouse::Button::Left, state.previous);
        // A press on a tab breaks a double click on empty space.
        state.previous = (!captured).then_some(click);
        if let Some(action) = self.chrome.bar_press(captured, self.modal, click.kind()) {
            shell.publish((self.on_action)(action));
            shell.capture_event();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_press_on_empty_bar_space_moves_the_window() {
        let c = Chrome::Unified;
        assert_eq!(
            c.bar_press(false, false, click::Kind::Single),
            Some(BarAction::Drag)
        );
        assert_eq!(
            c.bar_press(false, false, click::Kind::Double),
            Some(BarAction::ToggleMaximize)
        );
        assert_eq!(c.bar_press(false, false, click::Kind::Triple), None);
    }

    #[test]
    fn a_press_a_tab_or_button_took_leaves_the_window_alone() {
        let c = Chrome::Unified;
        for kind in [click::Kind::Single, click::Kind::Double] {
            assert_eq!(c.bar_press(true, false, kind), None);
        }
    }

    #[test]
    fn under_the_palette_or_a_dialog_the_bar_does_nothing() {
        let c = Chrome::Unified;
        for kind in [click::Kind::Single, click::Kind::Double] {
            assert_eq!(c.bar_press(false, true, kind), None);
        }
    }

    #[test]
    fn tab_bar_buttons_never_drag_or_zoom() {
        // The "+" and profiles buttons capture their press, so the bar sees `captured`.
        for kind in [
            click::Kind::Single,
            click::Kind::Double,
            click::Kind::Triple,
        ] {
            assert_eq!(Chrome::Unified.bar_press(true, false, kind), None);
        }
    }

    #[test]
    fn os_title_bars_keep_moving_the_window_themselves() {
        let c = Chrome::Native;
        for captured in [false, true] {
            assert_eq!(c.bar_press(captured, false, click::Kind::Single), None);
            assert_eq!(c.bar_press(captured, false, click::Kind::Double), None);
        }
    }

    #[test]
    fn only_the_unified_bar_makes_room_for_the_window_buttons() {
        assert_eq!(
            Chrome::Unified.leading_inset(false),
            MACOS_TRAFFIC_LIGHTS_INSET
        );
        assert_eq!(Chrome::Unified.bar_height(), MACOS_TITLEBAR_HEIGHT);
        assert!(Chrome::Native.leading_inset(false) < MACOS_TRAFFIC_LIGHTS_INSET);
        assert!(!Chrome::Native.draws_title_bar());
        let expected = if cfg!(target_os = "macos") {
            Chrome::Unified
        } else {
            Chrome::Native
        };
        assert_eq!(Chrome::current(), expected);
    }

    #[test]
    fn full_screen_drops_the_room_for_the_window_buttons() {
        assert_eq!(Chrome::Unified.leading_inset(true), NATIVE_LEADING_INSET);
        assert_eq!(
            Chrome::Native.leading_inset(true),
            Chrome::Native.leading_inset(false)
        );
    }

    #[test]
    fn windows_are_transparent_when_translucent_and_always_on_macos() {
        let opaque = Backdrop {
            transparent: false,
            blur: false,
        };
        assert_eq!(Chrome::Native.backdrop(1.0, true), opaque);
        assert_eq!(
            Chrome::Native.backdrop(0.85, true),
            Backdrop {
                transparent: true,
                blur: true
            }
        );
        assert_eq!(
            Chrome::Native.backdrop(0.85, false),
            Backdrop {
                transparent: true,
                blur: false
            }
        );
        assert_eq!(
            Chrome::Unified.backdrop(1.0, true),
            Backdrop {
                transparent: true,
                blur: false
            }
        );
    }

    #[test]
    fn only_the_menus_quit_on_cmd_q_is_picked() {
        assert!(is_quit_key_item(Some("terminate:"), "q"));
        assert!(is_quit_key_item(Some("terminate:"), "Q"));
        assert!(!is_quit_key_item(Some("terminate:"), ""));
        assert!(!is_quit_key_item(Some("hide:"), "h"));
        assert!(!is_quit_key_item(Some("performClose:"), "q"));
        assert!(!is_quit_key_item(None, "q"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_windows_hide_the_title_strip() {
        let s = Chrome::Unified.window_settings(window::Settings::default());
        assert!(s.platform_specific.title_hidden);
        assert!(s.platform_specific.titlebar_transparent);
        assert!(s.platform_specific.fullsize_content_view);
        let n = Chrome::Native.window_settings(window::Settings::default());
        assert!(!n.platform_specific.fullsize_content_view);
    }
}
