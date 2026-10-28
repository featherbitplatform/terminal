use fbterm_config::{ProfileId, TunnelKind};
use fbterm_session::ssh::tunnel::TunnelState;

use super::Group;
use crate::app::Message;
use crate::tunnels_tests::{World, key, panel_world, tunnels_of};

fn groups(w: &World, query: &str) -> Vec<Group> {
    super::groups(&w.app.profiles, &w.app.tunnels, query, None)
}

fn names(groups: &[crate::tunnels_panel::Group]) -> Vec<(String, Vec<String>)> {
    groups
        .iter()
        .map(|g| {
            (
                g.profile.clone(),
                g.rows.iter().map(|r| r.name.clone()).collect(),
            )
        })
        .collect()
}

#[test]
fn panel_lists_tunnels_grouped_by_profile() {
    use crate::tunnels_panel::Tone;
    let (mut w, _) = panel_world();
    let g = groups(&w, "");
    assert_eq!(
        names(&g),
        [
            ("srv".to_string(), vec!["web".to_string(), "db".to_string()]),
            (
                "warehouse".to_string(),
                vec!["hook".to_string(), "proxy".to_string()]
            ),
        ],
        "profiles without tunnels are left out; definition order within a profile"
    );
    let web = &g[0].rows[0];
    assert_eq!(web.address, "127.0.0.1:18080 \u{2192} db.internal:5432");
    assert_eq!(web.state, "Off");
    assert_eq!(web.tone, Tone::Off);
    assert!(!web.on && !web.retry && !web.restart_to_apply);
    // A with-the-shell tunnel waiting for its shell says so.
    assert_eq!(g[0].rows[1].state, "Starts with the shell");
    assert_eq!(
        g[1].rows[0].address,
        "remote 127.0.0.1:9000 \u{2192} localhost:3000 via bastion"
    );
    assert_eq!(g[1].rows[1].address, "SOCKS 127.0.0.1:1080 via bastion");

    // Every state, its label and tone.
    w.toggle("web");
    let row = |w: &World| groups(w, "")[0].rows[0].clone();
    assert_eq!(
        (row(&w).state.as_str(), row(&w).tone, row(&w).on),
        ("Starting", Tone::Busy, true)
    );
    let web_key = key(&w.srv, "web");
    for (state, label, tone) in [
        (TunnelState::Up { connections: 0 }, "Up", Tone::Up),
        (
            TunnelState::Up { connections: 1 },
            "Up \u{b7} 1 connection",
            Tone::Up,
        ),
        (
            TunnelState::Up { connections: 3 },
            "Up \u{b7} 3 connections",
            Tone::Up,
        ),
        (
            TunnelState::Reconnecting {
                attempt: 2,
                max: 10,
            },
            "Reconnecting (attempt 2 of 10)",
            Tone::Busy,
        ),
    ] {
        let _ = w.app.update(Message::Tunnel(web_key.clone(), state));
        assert_eq!((row(&w).state.as_str(), row(&w).tone), (label, tone));
    }
    // A changed definition marks the running tunnel.
    w.edit(|p| {
        if let TunnelKind::Local { host_port, .. } = &mut tunnels_of(p)[0].kind {
            *host_port = 5433;
        }
    });
    assert!(row(&w).restart_to_apply);
    // While it runs the address is what is actually listening, not the pending edit.
    assert_eq!(row(&w).address, "127.0.0.1:18080 \u{2192} db.internal:5432");
    // A failure: red, switched off, with Retry.
    let _ = w.app.update(Message::Tunnel(
        web_key,
        TunnelState::Failed("port 18080 already in use".into()),
    ));
    let r = row(&w);
    // Off, it shows the definition the next start uses.
    assert_eq!(r.address, "127.0.0.1:18080 \u{2192} db.internal:5433");
    assert_eq!(r.state, "Failed: port 18080 already in use");
    assert_eq!(r.tone, Tone::Failed);
    assert!(!r.on && r.retry && !r.restart_to_apply);

    // A with-the-shell tunnel switched off by hand while its shell is open is plainly off.
    let _ = w.open_shell();
    assert!(w.running("db"));
    w.toggle("db");
    assert_eq!(groups(&w, "")[0].rows[1].state, "Off");
}

#[test]
fn search_filters_by_tunnel_and_profile() {
    let (w, _) = panel_world();
    // A tunnel name: only that row, under its profile.
    assert_eq!(
        names(&groups(&w, "proxy")),
        [("warehouse".to_string(), vec!["proxy".to_string()])]
    );
    // A profile name: every tunnel of that profile.
    assert_eq!(
        names(&groups(&w, "wareh")),
        [(
            "warehouse".to_string(),
            vec!["hook".to_string(), "proxy".to_string()]
        )]
    );
    // Fuzzy, like the palette, and words may span profile and tunnel.
    assert_eq!(
        names(&groups(&w, "srv db")),
        [("srv".to_string(), vec!["db".to_string()])]
    );
    assert_eq!(
        names(&groups(&w, "wb")),
        [("srv".to_string(), vec!["web".to_string()])]
    );
    assert!(groups(&w, "nothing-like-this").is_empty());
    // Addresses are not searched.
    assert!(groups(&w, "1080").is_empty());
    assert_eq!(groups(&w, "  ").len(), 2, "a blank query lists everything");
}

fn highlighted(w: &World, query: &str, profile: Option<ProfileId>) -> Vec<(String, bool)> {
    super::groups(&w.app.profiles, &w.app.tunnels, query, profile)
        .into_iter()
        .map(|g| (g.profile, g.highlighted))
        .collect()
}

fn id_of(w: &World, name: &str) -> ProfileId {
    w.app
        .profiles
        .profiles
        .iter()
        .find(|p| p.name == name)
        .expect("profile")
        .id
}

fn s(a: &str, h: bool) -> (String, bool) {
    (a.to_string(), h)
}

#[test]
fn highlighted_group_comes_first_others_keep_order() {
    let (w, _) = panel_world();
    let wh = id_of(&w, "warehouse");
    assert_eq!(
        highlighted(&w, "", Some(wh)),
        [s("warehouse", true), s("srv", false)]
    );
    let g = super::groups(&w.app.profiles, &w.app.tunnels, "", Some(wh));
    assert_eq!(g[0].profile_id, wh);
    assert_eq!(g[0].rows.len(), 2);
}

#[test]
fn highlighted_middle_group_moves_first_the_others_keep_order() {
    let (mut w, warehouse) = panel_world();
    // A third profile with tunnels, after the other two.
    let zeta = fbterm_config::Profile {
        id: ProfileId::new(),
        name: "zeta".into(),
        ..warehouse
    };
    w.app.profiles.profiles.push(zeta);
    assert_eq!(
        highlighted(&w, "", None),
        [s("srv", false), s("warehouse", false), s("zeta", false)]
    );
    let wh = id_of(&w, "warehouse");
    assert_eq!(
        highlighted(&w, "", Some(wh)),
        [s("warehouse", true), s("srv", false), s("zeta", false)]
    );
    let z = id_of(&w, "zeta");
    assert_eq!(
        highlighted(&w, "", Some(z)),
        [s("zeta", true), s("srv", false), s("warehouse", false)]
    );
}

#[test]
fn no_highlight_keeps_profile_order() {
    let (w, _) = panel_world();
    assert_eq!(
        highlighted(&w, "", None),
        [s("srv", false), s("warehouse", false)]
    );
    let srv = id_of(&w, "srv");
    assert_eq!(
        highlighted(&w, "", Some(srv)),
        [s("srv", true), s("warehouse", false)]
    );
}

#[test]
fn filtered_out_highlight_highlights_nothing() {
    let (w, _) = panel_world();
    let wh = id_of(&w, "warehouse");
    assert_eq!(highlighted(&w, "web", Some(wh)), [s("srv", false)]);
}

#[test]
fn highlight_of_a_missing_profile_is_ignored() {
    let (w, _) = panel_world();
    assert_eq!(
        highlighted(&w, "", Some(ProfileId::new())),
        [s("srv", false), s("warehouse", false)]
    );
}

#[test]
fn local_profile_highlights_nothing() {
    let (w, _) = panel_world();
    let local = id_of(&w, "local");
    assert_eq!(
        highlighted(&w, "", Some(local)),
        [s("srv", false), s("warehouse", false)]
    );
    let bastion = id_of(&w, "bastion");
    assert_eq!(
        highlighted(&w, "", Some(bastion)),
        [s("srv", false), s("warehouse", false)]
    );
}

// The panel as drawn in the dock.

fn renderer() -> iced::Renderer {
    iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        iced::Font::DEFAULT,
        iced::Pixels(16.0),
    ))
}

/// A tab of `profile` (not connected) in `w`'s window, focused.
fn focus_tab_of(w: &mut World, profile: &fbterm_config::Profile) {
    use crate::tabs::{Pane, Tab};
    w.app
        .windows
        .get_mut(&w.wid)
        .unwrap()
        .push(Tab::single(Pane::for_profile(profile)));
}

/// Every text the docked panel of `w`'s window draws, laid out `size`.
fn drawn_texts(w: &World, size: iced::Size) -> Vec<String> {
    use iced::advanced::layout::{Layout, Limits};
    use iced::advanced::widget::{Id, Operation, Tree};
    struct Texts(Vec<String>);
    impl Operation for Texts {
        fn text(&mut self, _id: Option<&Id>, _bounds: iced::Rectangle, text: &str) {
            self.0.push(text.to_owned());
        }
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
    }
    let renderer = renderer();
    let docked = crate::window::docked(&w.app, w.wid).expect("a docked panel");
    let mut element = docked.panel;
    let mut tree = Tree::new(&element);
    let node =
        element
            .as_widget_mut()
            .layout(&mut tree, &renderer, &Limits::new(iced::Size::ZERO, size));
    let mut texts = Texts(Vec::new());
    element
        .as_widget_mut()
        .operate(&mut tree, Layout::new(&node), &renderer, &mut texts);
    texts.0
}

#[test]
fn panel_draws_title_and_the_focused_profile_first() {
    let (mut w, warehouse) = panel_world();
    focus_tab_of(&mut w, &warehouse);
    let wid = w.wid;
    let _ = w
        .app
        .update(Message::Shortcut(wid, crate::keys::Action::Tunnels));
    let content = crate::window::docked(&w.app, wid).expect("docked").content;
    assert_eq!(content, crate::window::DockContent::Tunnels);
    let texts = drawn_texts(&w, iced::Size::new(300.0, 700.0));
    assert_eq!(texts.first().map(String::as_str), Some("Tunnels"));
    let at = |s: &str| texts.iter().position(|t| t == s);
    assert!(
        at("warehouse") < at("srv"),
        "the focused profile first: {texts:?}"
    );
    assert!(at("hook").is_some() && at("web").is_some());
    assert_eq!(w.app.tunnels_highlight(wid), Some(warehouse.id));

    // A search that hides every tunnel says so; with no tunnels at all, where to add them.
    let _ = w.app.update(Message::Tunnels(super::TunnelsMsg::Query(
        wid,
        "zzzz".into(),
    )));
    assert!(drawn_texts(&w, iced::Size::new(300.0, 700.0)).contains(&super::NO_MATCHES.into()));
}

#[test]
fn panel_width_is_clamped_like_the_file_panel() {
    use iced::advanced::layout::{Layout, Limits};
    use iced::advanced::widget::Tree;
    use iced::widget::Space;
    use iced::{Element, Length, Size};

    use super::state::{MAX_WIDTH, MIN_WIDTH};
    let (mut w, _) = panel_world();
    let _ = w.open_shell();
    let wid = w.wid;
    let _ = w
        .app
        .update(Message::Shortcut(wid, crate::keys::Action::Tunnels));
    // The panel docks at its shared width, clamped to the file panel's limits.
    let _ = w
        .app
        .update(Message::Tunnels(super::TunnelsMsg::Resize(5000.0)));
    let width = |w: &World| crate::window::docked(&w.app, wid).expect("docked").width;
    assert_eq!(width(&w), MAX_WIDTH);
    assert_eq!(
        (MIN_WIDTH, MAX_WIDTH),
        (
            crate::file_panel::state::MIN_WIDTH,
            crate::file_panel::state::MAX_WIDTH
        )
    );
    let _ = w
        .app
        .update(Message::Tunnels(super::TunnelsMsg::Resize(1.0)));
    assert_eq!(width(&w), MIN_WIDTH);

    // A narrow window keeps the terminal usable, as beside the file panel.
    let _ = w
        .app
        .update(Message::Tunnels(super::TunnelsMsg::Resize(5000.0)));
    let docked = crate::window::docked(&w.app, wid).unwrap();
    let content: Element<'_, Message> =
        Space::new().width(Length::Fill).height(Length::Fill).into();
    let mut element = crate::window::dock(content, Some(docked));
    let mut tree = Tree::new(&element);
    let narrow = Size::new(600.0, 400.0);
    let renderer = renderer();
    let node =
        element
            .as_widget_mut()
            .layout(&mut tree, &renderer, &Limits::new(Size::ZERO, narrow));
    let layout = Layout::new(&node);
    let mut children = layout.children();
    let content = children.next().unwrap().bounds();
    let panel = children.next().unwrap().bounds();
    assert!(content.width >= crate::window::MIN_TERMINAL_WIDTH);
    assert_eq!(content.width + panel.width, narrow.width);
}

#[test]
fn dock_reports_resize_and_focus_to_the_tunnels_panel() {
    use iced::advanced::layout::{Layout, Limits};
    use iced::advanced::widget::Tree;
    use iced::advanced::{Shell, clipboard};
    use iced::widget::Space;
    use iced::{Element, Event, Length, Point, Rectangle, Size, mouse};
    const WINDOW: Size = Size::new(1000.0, 600.0);
    let fill = || -> Element<'static, Message> {
        Space::new().width(Length::Fill).height(Length::Fill).into()
    };
    let wid = iced::window::Id::unique();
    let mut element = crate::window::dock(
        fill(),
        Some(crate::window::Docked {
            panel: fill(),
            width: 300.0,
            window: wid,
            focused: false,
            content: crate::window::DockContent::Tunnels,
        }),
    );
    let renderer = renderer();
    let mut tree = Tree::new(&element);
    let node =
        element
            .as_widget_mut()
            .layout(&mut tree, &renderer, &Limits::new(Size::ZERO, WINDOW));
    let mut messages = Vec::new();
    let edge = Point::new(702.0, 300.0);
    let inside = Point::new(800.0, 300.0);
    for (event, at) in [
        (
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            inside,
        ),
        (
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            edge,
        ),
        (
            Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(600.0, 300.0),
            }),
            Point::new(600.0, 300.0),
        ),
        (
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            edge,
        ),
    ] {
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &event,
            Layout::new(&node),
            mouse::Cursor::Available(at),
            &renderer,
            &mut clipboard::Null,
            &mut shell,
            &Rectangle::with_size(WINDOW),
        );
    }
    let seen: Vec<String> = messages
        .iter()
        .map(|m| match m {
            Message::Tunnels(super::TunnelsMsg::Focus(id, on)) if *id == wid => {
                format!("focus {on}")
            }
            Message::Tunnels(super::TunnelsMsg::Resize(w)) => format!("resize {w}"),
            Message::Tunnels(super::TunnelsMsg::ResizeEnd) => "end".into(),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(seen, ["focus true", "resize 400", "end"]);
}
