//! Tooltips on chrome buttons: what the button does and its shortcut on this platform, in a
//! small Graphite bubble shown after a short hover.

use std::time::Duration;

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::widget::{container, text, tooltip};
use iced::{
    Border, Color, Element, Event, Length, Padding, Rectangle, Shadow, Size, Vector, mouse,
};

use crate::app::Message;
use crate::keys::{self, Action};
use crate::theme::{Tokens, UI_FONT};

/// How long the pointer rests on a button before its tooltip shows.
pub const DELAY: Duration = Duration::from_millis(500);

/// The tooltip text for a button doing `action`: `New tab (⌘T)` on macOS, `New tab
/// (Ctrl+Shift+T)` elsewhere; just `label` when the action has no chord.
pub fn tooltip_text(label: &str, action: Action, mac: bool) -> String {
    match keys::chord_for(mac, action) {
        Some(c) => format!("{label} ({})", c.label(mac)),
        None => label.to_owned(),
    }
}

/// `content` with a tooltip below it saying `label` and `action`'s shortcut. The content still
/// receives every press, so the title bar never sees one on a button.
pub fn tip<'a>(
    t: &'static Tokens,
    content: impl Into<Element<'a, Message>>,
    label: &str,
    action: Action,
    force: bool,
) -> Element<'a, Message> {
    let bubble = container(
        text(tooltip_text(label, action, cfg!(target_os = "macos")))
            .font(UI_FONT)
            .size(12)
            .color(t.text)
            .wrapping(text::Wrapping::None),
    )
    .padding(Padding::from([4.0, 8.0]))
    .style(move |_| bubble_style(t));
    let tip = tooltip(content, bubble, tooltip::Position::Bottom)
        .gap(6)
        .padding(0)
        .delay(DELAY);
    if force {
        Element::new(ForceHover {
            content: tip.into(),
        })
    } else {
        tip.into()
    }
}

/// Graphite tooltip surface: raised, 1 px border, radius 6.
pub fn bubble_style(t: &Tokens) -> container::Style {
    container::Style {
        background: Some(t.raised.into()),
        text_color: Some(t.text),
        border: Border {
            color: t.border,
            width: 1.0,
            radius: 6.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.3),
            offset: Vector::new(0.0, 4.0),
            blur_radius: 12.0,
        },
        ..container::Style::default()
    }
}

/// Debug aid (`FBTERM_DEMO_TOOLTIP`): its content always sees the pointer resting on its
/// middle, so a tooltip inside shows without a real pointer (for screenshots).
struct ForceHover<'a> {
    content: Element<'a, Message>,
}

fn hovering(layout: Layout<'_>) -> mouse::Cursor {
    mouse::Cursor::Available(layout.bounds().center())
}

impl Widget<Message, iced::Theme, iced::Renderer> for ForceHover<'_> {
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
        _cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            hovering(layout),
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
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            hovering(layout),
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

/// Which tab-bar button shows its tooltip for `FBTERM_DEMO_TOOLTIP=new|profiles|close`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemoTip {
    New,
    Profiles,
    /// The first tab's close button.
    Close,
}

impl DemoTip {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "new" => Some(Self::New),
            "profiles" => Some(Self::Profiles),
            "close" => Some(Self::Close),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltips_name_the_platform_chord() {
        assert_eq!(
            tooltip_text("New tab", Action::NewTab, true),
            "New tab (⌘T)"
        );
        assert_eq!(
            tooltip_text("Edit profiles", Action::EditProfiles, true),
            "Edit profiles (⇧⌘,)"
        );
        assert_eq!(
            tooltip_text("Close tab", Action::CloseTab, true),
            "Close tab (⌘W)"
        );
        assert_eq!(
            tooltip_text("New tab", Action::NewTab, false),
            "New tab (Ctrl+Shift+T)"
        );
        assert_eq!(
            tooltip_text("Close tab", Action::CloseTab, false),
            "Close tab (Ctrl+Shift+W)"
        );
        assert_eq!(
            tooltip_text("Reveal", Action::RevealConfigFolder, true),
            "Reveal"
        );
    }

    #[test]
    fn demo_tip_names() {
        assert_eq!(DemoTip::parse("new"), Some(DemoTip::New));
        assert_eq!(DemoTip::parse("close"), Some(DemoTip::Close));
        assert_eq!(DemoTip::parse("profiles"), Some(DemoTip::Profiles));
        assert_eq!(DemoTip::parse("x"), None);
    }
}
