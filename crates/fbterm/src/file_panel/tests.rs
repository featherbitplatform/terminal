//! The file panel inside the app: toggling, following the focused pane, placeholders, each
//! profile's folder, the remembered width and the docked layout. A fake backend stands in for
//! SFTP, so no network is needed: the tests answer its requests by hand.

use std::sync::Arc;

use fbterm_config::{AuthMethod, Paths, Profile, ProfileId, ProfileKind, SavedState, SshProfile};
use fbterm_session::SessionHandle;
use fbterm_session::ssh::sftp::{Entry, EntryKind, Sftp, SftpError};
use iced::window;
use parking_lot::Mutex;

use super::{Backend, Body, FilePanelMsg, ListRequest, OpenRequest, Opened, PanelEvent, Remote};
use crate::app::{App, DEFAULT_GRID, Delivered, Message};
use crate::keys::Action;
use crate::layout::{PaneId, Side};
use crate::restore_tests::{paths_with, profile};
use crate::tabs::{Pane, Tab, TabState, WindowState};
use crate::tiles::TilesMsg;

/// Records every request; answers nothing by itself. With `runtime`, each open is a task
/// that never finishes on it, so tests can see whether it was aborted.
#[derive(Default)]
struct Fake {
    opens: Mutex<Vec<(ProfileId, window::Id, u64)>>,
    lists: Mutex<Vec<(ProfileId, String, u64)>>,
    runtime: Option<tokio::runtime::Runtime>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl Backend for Fake {
    fn open(
        &self,
        request: OpenRequest,
        _sink: super::PanelSink,
    ) -> Option<tokio::task::AbortHandle> {
        self.opens
            .lock()
            .push((request.profile, request.window, request.token));
        let task = self.runtime.as_ref()?.spawn(std::future::pending());
        let abort = task.abort_handle();
        self.tasks.lock().push(task);
        Some(abort)
    }

    fn list(&self, request: ListRequest, _sink: super::PanelSink) {
        self.lists
            .lock()
            .push((request.profile, request.folder, request.token));
    }
}

/// A session with nothing behind it (the backend is the fake).
struct NoRemote;

#[async_trait::async_trait]
impl Remote for NoRemote {
    async fn list(&self, _path: &str) -> Result<Vec<Entry>, SftpError> {
        Ok(Vec::new())
    }

    fn sftp(&self) -> Option<Arc<Sftp>> {
        None
    }
}

fn ssh(name: &str) -> Profile {
    Profile {
        kind: ProfileKind::Ssh(SshProfile {
            host: "127.0.0.1".into(),
            port: 1,
            user: "user".into(),
            auth: AuthMethod::Agent,
            jump: vec![],
            tunnels: vec![],
        }),
        ..profile(name)
    }
}

fn file(name: &str) -> Entry {
    Entry {
        name: name.into(),
        kind: EntryKind::File,
        size: 1,
        modified: None,
        mode: None,
    }
}

fn folder(name: &str) -> Entry {
    Entry {
        kind: EntryKind::Dir,
        ..file(name)
    }
}

struct World {
    app: App,
    fake: Arc<Fake>,
    wid: window::Id,
    db: Profile,
    web: Profile,
    local: Profile,
}

fn world() -> World {
    let (db, web, local) = (ssh("db-host"), ssh("web-host"), profile("local"));
    let paths = paths_with(&[local.clone(), db.clone(), web.clone()]);
    world_at(paths, db, web, local)
}

fn world_at(paths: Paths, db: Profile, web: Profile, local: Profile) -> World {
    let (mut app, _) = App::build(paths, true);
    app.state_saving_enabled = false;
    let fake = Arc::new(Fake::default());
    app.file_panel_work.backend = fake.clone();
    let wid = window::Id::unique();
    app.windows.clear();
    app.windows.insert(wid, WindowState::new());
    app.focused = Some(wid);
    World {
        app,
        fake,
        wid,
        db,
        web,
        local,
    }
}

impl World {
    fn window(&mut self) -> &mut WindowState {
        self.app.windows.get_mut(&self.wid).unwrap()
    }

    /// A new tab of `p`; its shell connected unless `p` is local.
    fn open_tab(&mut self, p: &Profile) -> PaneId {
        let pane = Pane::for_profile(p);
        let id = pane.id;
        self.window().push(Tab::single(pane));
        if matches!(p.kind, ProfileKind::Ssh(_)) {
            self.connect(id);
        }
        id
    }

    /// A new tab split between `left` and `right`, both connected, `left` focused.
    fn open_split(&mut self, left: &Profile, right: &Profile) -> (PaneId, PaneId) {
        let (a, b) = (Pane::for_profile(left), Pane::for_profile(right));
        let (ia, ib) = (a.id, b.id);
        let mut tab = Tab::single(a);
        tab.layout.split(ia, Side::Right, ib);
        tab.panes.push(b);
        self.window().push(tab);
        self.connect(ia);
        self.connect(ib);
        (ia, ib)
    }

    fn connect(&mut self, pane: PaneId) {
        let handle = SessionHandle::inert(self.app.new_terminal(DEFAULT_GRID));
        let _ = self.app.update(Message::Connected(
            pane,
            Delivered::new(handle),
            DEFAULT_GRID,
        ));
        let w = &self.app.windows[&self.wid];
        let (t, p) = w.locate(pane).unwrap();
        assert_eq!(w.tabs[t].panes[p].state, TabState::Connected);
    }

    fn toggle(&mut self) {
        let _ = self
            .app
            .update(Message::Shortcut(self.wid, Action::FilePanel));
    }

    fn focus(&mut self, pane: PaneId) {
        let _ = self
            .app
            .update(Message::Tiles(self.wid, TilesMsg::Focus(pane)));
    }

    fn event(&mut self, event: PanelEvent) {
        let _ = self
            .app
            .update(Message::FilePanel(FilePanelMsg::Event(Delivered::new(
                event,
            ))));
    }

    /// The last open request for `p`: its window and token.
    fn last_open(&self, p: &Profile) -> Option<(window::Id, u64)> {
        self.fake
            .opens
            .lock()
            .iter()
            .rev()
            .find(|(id, _, _)| *id == p.id)
            .map(|(_, w, t)| (*w, *t))
    }

    fn opens(&self, p: &Profile) -> usize {
        self.fake
            .opens
            .lock()
            .iter()
            .filter(|(id, _, _)| *id == p.id)
            .count()
    }

    /// The last folder read requested for `p`, with its token.
    fn last_list(&self, p: &Profile) -> Option<(String, u64)> {
        self.fake
            .lists
            .lock()
            .iter()
            .rev()
            .find(|(id, _, _)| *id == p.id)
            .map(|(_, f, t)| (f.clone(), *t))
    }

    /// Answer `p`'s pending open: the session is up, its home is `home`.
    fn answer_open(&mut self, p: &Profile, home: &str) {
        let (_, token) = self.last_open(p).expect("an open was requested");
        self.event(PanelEvent::Opened {
            profile: p.id,
            token,
            result: Ok(Opened {
                remote: Arc::new(NoRemote),
                home: Ok(home.into()),
            }),
        });
    }

    /// Answer `p`'s pending folder read with `entries`.
    fn answer_list(&mut self, p: &Profile, entries: Vec<Entry>) {
        let (folder, token) = self.last_list(p).expect("a read was requested");
        self.event(PanelEvent::Listed {
            profile: p.id,
            token,
            folder,
            result: Ok(entries),
        });
    }

    fn body(&self) -> Option<Body<'_>> {
        self.app.file_panel_body(self.wid)
    }

    /// What the panel shows, in words: `loading`, `note: ...`, `error: ...` or the rows'
    /// names (`..` first when there is a folder above).
    fn shows(&self) -> Option<String> {
        Some(match self.body()? {
            Body::Loading => "loading".into(),
            Body::Note(line) => format!("note: {line}"),
            Body::Error(line) => format!("error: {line}"),
            Body::Entries(panel) => panel.row_names().collect::<Vec<_>>().join(" "),
        })
    }

    fn msg(&mut self, msg: FilePanelMsg) -> iced::Task<Message> {
        self.app.update(Message::FilePanel(msg))
    }

    fn key(&mut self, key: super::PanelKey) {
        let wid = self.wid;
        let _ = self.msg(FilePanelMsg::Key(wid, key));
    }

    /// A connected `db` tab with the panel open on `/home/user` listing `entries`.
    fn browsing(entries: Vec<Entry>) -> World {
        let mut w = world();
        let db = w.db.clone();
        w.open_tab(&db);
        w.toggle();
        w.answer_open(&db, "/home/user");
        w.answer_list(&db, entries);
        w
    }

    fn panel(&self) -> &super::ProfilePanel {
        &self.app.file_panels.by_profile[&self.db.id]
    }

    fn folder_of(&self, p: &Profile) -> String {
        self.app.file_panels.by_profile[&p.id].folder.clone()
    }
}

#[test]
fn toggle_opens_and_closes_panel() {
    let mut w = world();
    w.open_tab(&w.db.clone());
    assert!(!w.app.file_panels.is_open(w.wid));
    assert!(w.body().is_none(), "a closed panel shows nothing");
    w.toggle();
    assert!(w.app.file_panels.is_open(w.wid));
    assert!(w.body().is_some());
    w.toggle();
    assert!(!w.app.file_panels.is_open(w.wid));
    assert!(w.body().is_none());
    // The palette lists it under its title.
    let items = w.app.palette_items();
    assert!(items.iter().any(|i| matches!(
        i,
        crate::palette::PaletteItem::Action { action: Action::FilePanel, label } if label == "File panel"
    )));
}

#[test]
fn panel_follows_focused_pane_profile() {
    let mut w = world();
    let (db, web) = (w.db.clone(), w.web.clone());
    let (left, right) = w.open_split(&db, &web);
    w.toggle();
    // Opening on a connected SSH pane opens SFTP for its profile, asking over this window.
    assert_eq!(w.last_open(&db).map(|(win, _)| win), Some(w.wid));
    assert_eq!(w.opens(&web), 0, "only the focused pane's profile");
    assert_eq!(w.shows().as_deref(), Some("loading"));
    w.answer_open(&db, "/home/user");
    assert_eq!(
        w.last_list(&db).map(|(f, _)| f).as_deref(),
        Some("/home/user")
    );
    w.answer_list(&db, vec![file("app.log"), folder("backups")]);
    assert_eq!(
        w.shows().as_deref(),
        Some(".. backups app.log"),
        "folders first"
    );

    // Focus moves to the other profile's pane: the panel switches to its server.
    w.focus(right);
    assert_eq!(w.last_open(&web).map(|(win, _)| win), Some(w.wid));
    assert_eq!(w.shows().as_deref(), Some("loading"));
    w.answer_open(&web, "/srv");
    w.answer_list(&web, vec![file("index.html")]);
    assert_eq!(w.shows().as_deref(), Some(".. index.html"));

    // And back: db's listing is still there, nothing is opened again.
    w.focus(left);
    assert_eq!(w.shows().as_deref(), Some(".. backups app.log"));
    assert_eq!(w.opens(&db), 1);

    // A late answer for a superseded read is ignored.
    w.event(PanelEvent::Listed {
        profile: db.id,
        token: 0,
        folder: "/home/user".into(),
        result: Ok(vec![file("stale")]),
    });
    assert_eq!(w.shows().as_deref(), Some(".. backups app.log"));
}

#[test]
fn local_pane_shows_placeholder() {
    let mut w = world();
    let local = w.local.clone();
    w.open_tab(&local);
    w.toggle();
    assert_eq!(w.shows().as_deref(), Some("note: Local session"));
    assert!(w.fake.opens.lock().is_empty());

    // An SSH pane that is not connected.
    let db = w.db.clone();
    let pane = Pane::for_profile(&db);
    // Pushing a tab makes it the active one.
    w.window().push(Tab::single(pane));
    let _ = w.app.update(Message::DismissNotices);
    assert_eq!(w.shows().as_deref(), Some("note: Not connected"));
    assert!(w.fake.opens.lock().is_empty());

    // A server that refuses SFTP: the reason, inline.
    let mut w = world();
    let db = w.db.clone();
    w.open_tab(&db);
    w.toggle();
    let (_, token) = w.last_open(&db).unwrap();
    w.event(PanelEvent::Opened {
        profile: db.id,
        token,
        result: Err("the server refused the sftp subsystem".into()),
    });
    assert_eq!(
        w.shows().as_deref(),
        Some("error: SFTP is not available: the server refused the sftp subsystem")
    );
}

#[test]
fn folder_is_kept_per_profile() {
    let mut w = world();
    let (db, web) = (w.db.clone(), w.web.clone());
    let (_, right) = w.open_split(&db, &web);
    w.toggle();
    w.answer_open(&db, "/home/user");
    w.answer_list(&db, vec![file("app.log")]);
    // The user went to another folder.
    let wid = w.wid;
    let _ = w.msg(FilePanelMsg::Navigate(wid, "/var/log".into()));
    w.focus(right);
    w.answer_open(&web, "/srv");
    assert_eq!(w.folder_of(&web), "/srv");
    assert_eq!(w.folder_of(&db), "/var/log");

    // Hiding the panel lets the sessions go; showing it again reopens one but keeps the
    // folder rather than going home.
    w.toggle();
    assert!(
        w.app
            .file_panels
            .by_profile
            .values()
            .all(|p| p.sftp.is_none()),
        "no session is held while no panel is open"
    );
    let (left, _) = {
        let tab = w.app.windows[&w.wid].active_tab().unwrap();
        (tab.panes[0].id, tab.panes[1].id)
    };
    w.focus(left);
    w.toggle();
    assert_eq!(w.opens(&db), 2);
    w.answer_open(&db, "/home/user");
    assert_eq!(w.folder_of(&db), "/var/log");
    assert_eq!(
        w.last_list(&db).map(|(f, _)| f).as_deref(),
        Some("/var/log")
    );
}

#[test]
fn width_is_restored() {
    let (db, web, local) = (ssh("db-host"), ssh("web-host"), profile("local"));
    let paths = paths_with(&[local.clone(), db.clone(), web.clone()]);
    SavedState {
        file_panel_width: Some(410.0),
        ..Default::default()
    }
    .save(&paths.state())
    .unwrap();
    let mut w = world_at(paths.clone(), db, web, local);
    assert_eq!(w.app.file_panels.width, 410.0);

    // A drag changes it; the end of the drag remembers it.
    w.app.state_saving_enabled = true;
    let _ = w
        .app
        .update(Message::FilePanel(FilePanelMsg::Resize(5000.0)));
    assert_eq!(w.app.file_panels.width, super::state::MAX_WIDTH);
    let _ = w
        .app
        .update(Message::FilePanel(FilePanelMsg::Resize(360.0)));
    assert_eq!(w.app.file_panels.width, 360.0);
    let _ = w.app.update(Message::FilePanel(FilePanelMsg::ResizeEnd));
    let saved = SavedState::load_or_default(&paths.state());
    assert_eq!(saved.file_panel_width, Some(360.0));
}

/// `panel` docked `width` wide, without the keyboard.
fn side(panel: iced::Element<'_, Message>, width: f32) -> crate::window::Docked<'_> {
    crate::window::Docked {
        panel,
        width,
        window: window::Id::unique(),
        focused: false,
    }
}

#[test]
fn single_pane_tab_layout_unchanged_when_closed() {
    use iced::advanced::layout::{Layout, Limits};
    use iced::advanced::widget::Tree;
    use iced::widget::Space;
    use iced::{Element, Length, Pixels, Rectangle, Size};

    const WINDOW: Size = Size::new(1000.0, 600.0);
    let renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        iced::Font::DEFAULT,
        Pixels(16.0),
    ));
    // The bounds of the content (the tab's tiles) and of the panel, docked in WINDOW.
    let lay_out = |panel: Option<f32>| -> (Rectangle, Option<Rectangle>) {
        let content: Element<'_, Message> =
            Space::new().width(Length::Fill).height(Length::Fill).into();
        let docked = panel.map(|width| {
            let panel: Element<'_, Message> =
                Space::new().width(Length::Fill).height(Length::Fill).into();
            (panel, width)
        });
        let mut element = crate::window::dock(content, docked.map(|(p, w)| side(p, w)));
        let mut tree = Tree::new(&element);
        let node =
            element
                .as_widget_mut()
                .layout(&mut tree, &renderer, &Limits::new(Size::ZERO, WINDOW));
        let layout = Layout::new(&node);
        let mut children = layout.children();
        match children.next() {
            Some(first) => (first.bounds(), children.next().map(|l| l.bounds())),
            None => (layout.bounds(), None),
        }
    };
    let (closed, none) = lay_out(None);
    assert_eq!(
        closed,
        Rectangle::with_size(WINDOW),
        "the whole area, as before"
    );
    assert_eq!(none, None);

    let (content, panel) = lay_out(Some(300.0));
    let panel = panel.expect("the panel is laid out");
    assert_eq!(panel.width, 300.0);
    assert_eq!(
        panel.x + panel.width,
        WINDOW.width,
        "docked at the right edge"
    );
    assert_eq!(content.width + panel.width, WINDOW.width);
    assert_eq!(content.height, WINDOW.height);

    // A narrow window keeps room for the terminal.
    let (content, _) = {
        let content: Element<'_, Message> =
            Space::new().width(Length::Fill).height(Length::Fill).into();
        let panel: Element<'_, Message> =
            Space::new().width(Length::Fill).height(Length::Fill).into();
        let mut element = crate::window::dock(content, Some(side(panel, 700.0)));
        let mut tree = Tree::new(&element);
        let narrow = Size::new(600.0, 400.0);
        let node =
            element
                .as_widget_mut()
                .layout(&mut tree, &renderer, &Limits::new(Size::ZERO, narrow));
        let layout = Layout::new(&node);
        let mut children = layout.children();
        let first = children.next().unwrap().bounds();
        (first, children.next().map(|l| l.bounds()))
    };
    assert!(content.width >= crate::window::MIN_TERMINAL_WIDTH);
}

mod dock {
    use iced::advanced::layout::{Layout, Limits, Node};
    use iced::advanced::widget::Tree;
    use iced::advanced::{Shell, clipboard};
    use iced::widget::{Space, text_input};
    use iced::{Element, Event, Length, Pixels, Point, Rectangle, Size, mouse};

    use crate::app::Message;
    use crate::file_panel::FilePanelMsg;

    const WINDOW: Size = Size::new(1000.0, 600.0);
    /// Where the panel's left edge is with a 300 px panel in WINDOW.
    const EDGE_X: f32 = 700.0;

    fn renderer() -> iced::Renderer {
        iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::DEFAULT,
            Pixels(16.0),
        ))
    }

    fn fill<'a>() -> Element<'a, Message> {
        Space::new().width(Length::Fill).height(Length::Fill).into()
    }

    fn docked<'a>() -> Element<'a, Message> {
        crate::window::dock(fill(), Some(super::side(fill(), 300.0)))
    }

    /// Runs `events` (each with the pointer position) through a docked panel; returns the
    /// file-panel messages published.
    fn drive(events: &[(Event, Point)]) -> Vec<FilePanelMsg> {
        let renderer = renderer();
        let mut element = docked();
        let mut tree = Tree::new(&element);
        let node: Node =
            element
                .as_widget_mut()
                .layout(&mut tree, &renderer, &Limits::new(Size::ZERO, WINDOW));
        let viewport = Rectangle::with_size(WINDOW);
        let mut messages = Vec::new();
        for (event, at) in events {
            let mut shell = Shell::new(&mut messages);
            element.as_widget_mut().update(
                &mut tree,
                event,
                Layout::new(&node),
                mouse::Cursor::Available(*at),
                &renderer,
                &mut clipboard::Null,
                &mut shell,
                &viewport,
            );
        }
        messages
            .into_iter()
            .filter_map(|m| match m {
                Message::FilePanel(m) => Some(m),
                _ => None,
            })
            .collect()
    }

    fn press(at: Point) -> (Event, Point) {
        (
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            at,
        )
    }

    fn moved(at: Point) -> (Event, Point) {
        (Event::Mouse(mouse::Event::CursorMoved { position: at }), at)
    }

    fn summary(messages: &[FilePanelMsg]) -> Vec<String> {
        messages
            .iter()
            .map(|m| match m {
                FilePanelMsg::Resize(w) => format!("resize {w}"),
                FilePanelMsg::ResizeEnd => "end".into(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn edge_drag_ends_when_the_window_loses_focus_or_the_pointer_leaves() {
        let grab = Point::new(EDGE_X + 2.0, 300.0);
        for cancel in [
            Event::Window(iced::window::Event::Unfocused),
            Event::Mouse(mouse::Event::CursorLeft),
        ] {
            let messages = drive(&[
                press(grab),
                moved(Point::new(650.0, 300.0)),
                (cancel.clone(), Point::new(650.0, 300.0)),
                // No button is held any more: moving does not resize.
                moved(Point::new(500.0, 300.0)),
            ]);
            assert_eq!(
                summary(&messages),
                ["resize 350", "end"],
                "the drag ends on {cancel:?} and the width is still saved"
            );
        }
    }

    #[test]
    fn the_edge_grab_takes_no_press_on_the_terminal_side() {
        let messages = drive(&[
            press(Point::new(EDGE_X - 2.0, 300.0)),
            moved(Point::new(650.0, 300.0)),
        ]);
        assert!(messages.is_empty(), "{:?}", summary(&messages));
        let messages = drive(&[
            press(Point::new(EDGE_X + 1.0, 300.0)),
            moved(Point::new(650.0, 300.0)),
        ]);
        assert_eq!(summary(&messages), ["resize 350"]);
    }

    /// A window with a focused field in the content and one in the panel, the panel having
    /// the keyboard or not: what typing `a` and pressing Down publish.
    fn type_into(panel_focused: bool) -> Vec<Message> {
        type Paragraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;
        let renderer = renderer();
        let wid = iced::window::Id::unique();
        let bindings = crate::keys::Bindings::current(&Default::default());
        let field = |name: &'static str| -> Element<'static, Message> {
            text_input(name, "")
                .on_input(move |s| {
                    Message::Palette(
                        iced::window::Id::unique(),
                        crate::palette::PaletteMsg::Query(format!("{name}:{s}")),
                    )
                })
                .width(Length::Fill)
                .into()
        };
        let docked = crate::window::dock(
            field("terminal"),
            Some(crate::window::Docked {
                panel: field("panel"),
                width: 300.0,
                window: wid,
                focused: panel_focused,
            }),
        );
        let mut element = Element::new(crate::shortcuts::Shortcuts {
            content: docked,
            window: wid,
            mode: if panel_focused {
                crate::shortcuts::Mode::Panel { editing: false }
            } else {
                crate::shortcuts::Mode::Normal
            },
            bindings: &bindings,
        });
        let mut tree = Tree::new(&element);
        for side in &mut tree.children[0].children {
            side.state
                .downcast_mut::<text_input::State<Paragraph>>()
                .focus();
        }
        let node =
            element
                .as_widget_mut()
                .layout(&mut tree, &renderer, &Limits::new(Size::ZERO, WINDOW));
        let mut messages = Vec::new();
        let key = |key: iced::keyboard::Key, text: Option<&str>| {
            Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key,
                physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::KeyA),
                location: iced::keyboard::Location::Standard,
                modifiers: iced::keyboard::Modifiers::empty(),
                text: text.map(Into::into),
                repeat: false,
            })
        };
        for event in [
            key(iced::keyboard::Key::Character("a".into()), Some("a")),
            key(
                iced::keyboard::Key::Named(iced::keyboard::key::Named::ArrowDown),
                None,
            ),
        ] {
            let mut shell = Shell::new(&mut messages);
            element.as_widget_mut().update(
                &mut tree,
                &event,
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut clipboard::Null,
                &mut shell,
                &Rectangle::with_size(WINDOW),
            );
        }
        messages
    }

    fn typed(messages: &[Message]) -> Vec<String> {
        messages
            .iter()
            .map(|m| match m {
                Message::Palette(_, crate::palette::PaletteMsg::Query(q)) => q.clone(),
                Message::FilePanel(FilePanelMsg::Key(_, k)) => format!("key {k:?}"),
                _ => "other".into(),
            })
            .collect()
    }

    #[test]
    fn keys_go_only_to_the_side_that_has_the_keyboard() {
        assert_eq!(typed(&type_into(true)), ["panel:a", "key Down"]);
        assert_eq!(typed(&type_into(false)), ["terminal:a"]);
    }

    #[test]
    fn a_press_moves_the_keyboard_to_its_side() {
        let focus = |messages: Vec<FilePanelMsg>| -> Vec<String> {
            messages
                .into_iter()
                .filter_map(|m| match m {
                    FilePanelMsg::Focus(_, on) => Some(format!("focus {on}")),
                    _ => None,
                })
                .collect()
        };
        // `drive` docks a panel without the keyboard.
        assert_eq!(
            focus(drive(&[press(Point::new(800.0, 300.0))])),
            ["focus true"]
        );
        assert!(focus(drive(&[press(Point::new(300.0, 300.0))])).is_empty());
    }

    #[test]
    fn toggling_the_panel_keeps_the_content_state() {
        type Paragraph = <iced::Renderer as iced::advanced::text::Renderer>::Paragraph;
        let input = || -> Element<'static, Message> { text_input("Find", "").into() };
        let closed = crate::window::dock(input(), None);
        let mut tree = Tree::new(&closed);
        tree.children[0]
            .state
            .downcast_mut::<text_input::State<Paragraph>>()
            .focus();
        // The panel opens: the same widget, with the panel added after the content.
        let open = crate::window::dock(input(), Some(super::side(fill(), 300.0)));
        tree.diff(&open);
        assert_eq!(tree.children.len(), 2);
        let focused = |tree: &Tree| {
            tree.children[0]
                .state
                .downcast_ref::<text_input::State<Paragraph>>()
                .is_focused()
        };
        assert!(
            focused(&tree),
            "the find bar keeps focus when the panel opens"
        );
        let closed = crate::window::dock(input(), None);
        tree.diff(&closed);
        assert_eq!(tree.children.len(), 1);
        assert!(focused(&tree), "and when it closes");
    }
}

#[test]
fn hiding_the_panel_aborts_an_open_in_flight() {
    let mut w = world();
    let fake = Arc::new(Fake {
        runtime: Some(
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap(),
        ),
        ..Fake::default()
    });
    w.app.file_panel_work.backend = fake.clone();
    let db = w.db.clone();
    w.open_tab(&db);
    w.toggle();
    assert_eq!(fake.opens.lock().len(), 1);
    // Hidden before the session opened: the open stops, so it can raise no question later.
    w.toggle();
    let task = fake.tasks.lock().pop().unwrap();
    let rt = fake.runtime.as_ref().unwrap();
    let outcome =
        rt.block_on(async { tokio::time::timeout(std::time::Duration::from_secs(2), task).await });
    assert!(
        matches!(outcome, Ok(Err(ref e)) if e.is_cancelled()),
        "the open was aborted: {outcome:?}"
    );
}

#[test]
fn without_a_runtime_the_panel_says_so_instead_of_loading_forever() {
    // The real backend, outside any runtime.
    let mut w = world();
    w.app.file_panel_work = super::Work::new(w.app.pool.clone());
    let mut rx = w.app.file_panel_work.receiver.0.lock().take().unwrap();
    let db = w.db.clone();
    w.open_tab(&db);
    w.toggle();
    let event = rx.try_recv().expect("the open reports at once");
    w.event(event);
    let shows = w.shows().unwrap();
    assert!(
        shows.starts_with("error: SFTP is not available: "),
        "{shows}"
    );
}

fn sized(name: &str, size: u64) -> Entry {
    Entry { size, ..file(name) }
}

fn link(name: &str, target: &str, to: Option<EntryKind>) -> Entry {
    Entry {
        kind: EntryKind::Link {
            target: target.into(),
            to: to.map(Box::new),
        },
        ..file(name)
    }
}

#[test]
fn folders_first_then_files() {
    let w = World::browsing(vec![
        file("b.txt"),
        folder("zeta"),
        file("A.txt"),
        link("current", "releases/42", Some(EntryKind::Dir)),
        link("latest.log", "app.log", Some(EntryKind::File)),
        folder("alpha"),
    ]);
    // Folders (a link to one opens as a folder) first, then files, each by name, any case.
    assert_eq!(
        w.shows().as_deref(),
        Some(".. alpha current zeta A.txt b.txt latest.log")
    );
}

#[test]
fn sort_toggles_direction() {
    use fbterm_session::ssh::sftp::SortKey;
    let mut w = World::browsing(vec![
        sized("big", 300),
        folder("logs"),
        sized("small", 1),
        sized("mid", 20),
    ]);
    let wid = w.wid;
    let _ = w.msg(FilePanelMsg::Sort(wid, SortKey::Size));
    assert_eq!(w.shows().as_deref(), Some(".. logs small mid big"));
    // The same header again reverses; folders stay first.
    let _ = w.msg(FilePanelMsg::Sort(wid, SortKey::Size));
    assert_eq!(w.shows().as_deref(), Some(".. logs big mid small"));
    // Another header sorts by it, ascending.
    let _ = w.msg(FilePanelMsg::Sort(wid, SortKey::Name));
    assert_eq!(w.shows().as_deref(), Some(".. logs big mid small"));
    let _ = w.msg(FilePanelMsg::Sort(wid, SortKey::Name));
    assert_eq!(w.shows().as_deref(), Some(".. logs small mid big"));
    // A new listing keeps the panel's order.
    let db = w.db.clone();
    let _ = w.msg(FilePanelMsg::Retry(wid));
    w.answer_list(&db, vec![sized("a", 1), sized("b", 2)]);
    assert_eq!(w.shows().as_deref(), Some(".. b a"));
}

#[test]
fn filter_is_substring_case_insensitive() {
    let mut w = World::browsing(vec![
        file("app.log"),
        folder("Logs"),
        file("catalog.txt"),
        file("README.md"),
    ]);
    let wid = w.wid;
    let _ = w.msg(FilePanelMsg::Filter(wid, "LOG".into()));
    assert_eq!(w.shows().as_deref(), Some(".. Logs app.log catalog.txt"));
    let _ = w.msg(FilePanelMsg::Filter(wid, "readme".into()));
    assert_eq!(w.shows().as_deref(), Some(".. README.md"));
    // Escape clears it.
    w.key(super::PanelKey::Escape);
    assert_eq!(w.panel().filter, "");
    assert_eq!(
        w.shows().as_deref(),
        Some(".. Logs app.log catalog.txt README.md")
    );
    // Typing while the list has the keyboard types in the filter.
    w.key(super::PanelKey::Type("cat".into()));
    assert_eq!(w.shows().as_deref(), Some(".. catalog.txt"));
    // A new folder starts unfiltered.
    let _ = w.msg(FilePanelMsg::Activate(wid, "Logs".into()));
    assert_eq!(w.panel().filter, "");
    // The filter matches what is shown: control characters as `?`.
    let db = w.db.clone();
    w.answer_list(&db, vec![file("a\u{1b}b"), file("ab")]);
    let _ = w.msg(FilePanelMsg::Filter(wid, "a?b".into()));
    assert_eq!(w.shows().as_deref(), Some(".. a\u{1b}b"));
}

#[test]
fn hidden_toggle_hides_dotfiles() {
    let (db, web, local) = (ssh("db-host"), ssh("web-host"), profile("local"));
    let paths = paths_with(&[local.clone(), db.clone(), web.clone()]);
    let entries = || {
        vec![
            file(".env"),
            folder(".config"),
            file("app.log"),
            folder("logs"),
        ]
    };
    let mut w = world_at(paths.clone(), db.clone(), web.clone(), local.clone());
    assert!(w.app.file_panels.show_hidden, "shown by default");
    w.open_tab(&db);
    w.toggle();
    w.answer_open(&db, "/home/user");
    w.answer_list(&db, entries());
    assert_eq!(w.shows().as_deref(), Some(".. .config logs .env app.log"));
    w.app.state_saving_enabled = true;
    let _ = w.msg(FilePanelMsg::ShowHidden(false));
    assert_eq!(w.shows().as_deref(), Some(".. logs app.log"));
    // Remembered app-wide, in state.toml next to the width.
    let saved = SavedState::load_or_default(&paths.state());
    assert_eq!(saved.file_panel_show_hidden, Some(false));
    let w2 = world_at(paths.clone(), db, web, local);
    assert!(!w2.app.file_panels.show_hidden);
    let _ = w.msg(FilePanelMsg::ShowHidden(true));
    assert_eq!(w.shows().as_deref(), Some(".. .config logs .env app.log"));
    let saved = SavedState::load_or_default(&paths.state());
    assert_eq!(
        saved.file_panel_show_hidden, None,
        "the default is not written"
    );
}

#[test]
fn path_bar_navigates() {
    let mut w = World::browsing(vec![folder("logs"), file("app.log")]);
    let (wid, db) = (w.wid, w.db.clone());
    // A segment.
    let _ = w.msg(FilePanelMsg::Navigate(wid, "/home".into()));
    assert_eq!(w.folder_of(&db), "/home");
    assert_eq!(w.last_list(&db).unwrap().0, "/home");
    assert_eq!(w.shows().as_deref(), Some("loading"));
    w.answer_list(&db, vec![folder("user")]);

    // The empty end makes it a text field; Enter goes where it says.
    let _ = w.msg(FilePanelMsg::EditPath(wid));
    assert_eq!(
        w.app.file_panels.ui[&wid].editing.as_deref(),
        Some("/home"),
        "starts with the folder"
    );
    let _ = w.msg(FilePanelMsg::PathInput(wid, "/var/log/".into()));
    let _ = w.msg(FilePanelMsg::PathSubmit(wid));
    assert_eq!(w.folder_of(&db), "/var/log");
    assert_eq!(w.app.file_panels.ui[&wid].editing, None);
    w.answer_list(&db, vec![folder("app"), file("syslog")]);

    // A key press on the panel types a path too: `/` or `~` starts one.
    w.key(super::PanelKey::Type("~".into()));
    assert_eq!(w.app.file_panels.ui[&wid].editing.as_deref(), Some("~"));
    let _ = w.msg(FilePanelMsg::PathInput(wid, "~/logs".into()));
    w.key(super::PanelKey::Enter);
    assert_eq!(w.folder_of(&db), "/home/user/logs");
    w.answer_list(&db, vec![file("a.log")]);
    // Escape leaves the field without going anywhere.
    let _ = w.msg(FilePanelMsg::EditPath(wid));
    let _ = w.msg(FilePanelMsg::PathInput(wid, "/etc".into()));
    w.key(super::PanelKey::Escape);
    assert_eq!(w.app.file_panels.ui[&wid].editing, None);
    assert_eq!(w.folder_of(&db), "/home/user/logs");

    // Backspace and the `..` row go up, selecting the folder left.
    w.key(super::PanelKey::Backspace);
    assert_eq!(w.folder_of(&db), "/home/user");
    w.answer_list(
        &db,
        vec![folder("logs"), folder("music"), file("notes.txt")],
    );
    assert_eq!(w.panel().selected.as_deref(), Some("logs"));
    // Arrows move the one selection; Enter opens a folder, not a file.
    w.key(super::PanelKey::Down);
    assert_eq!(w.panel().selected.as_deref(), Some("music"));
    w.key(super::PanelKey::Down);
    assert_eq!(w.panel().selected.as_deref(), Some("notes.txt"));
    w.key(super::PanelKey::Enter);
    assert_eq!(w.folder_of(&db), "/home/user");
    w.key(super::PanelKey::Up);
    w.key(super::PanelKey::Enter);
    assert_eq!(w.folder_of(&db), "/home/user/music");
    w.answer_list(&db, vec![]);
    assert_eq!(w.shows().as_deref(), Some(".."));
    // A double click on `..`.
    let _ = w.msg(FilePanelMsg::Activate(wid, super::state::PARENT.into()));
    assert_eq!(w.folder_of(&db), "/home/user");
    // A click selects one item at a time.
    w.answer_list(&db, vec![folder("logs"), file("notes.txt")]);
    let _ = w.msg(FilePanelMsg::Select(wid, "notes.txt".into()));
    assert_eq!(w.panel().selected.as_deref(), Some("notes.txt"));
    let _ = w.msg(FilePanelMsg::Select(wid, "logs".into()));
    assert_eq!(w.panel().selected.as_deref(), Some("logs"));
    // A double click on a folder opens it; on a file it does nothing (downloads come later).
    let _ = w.msg(FilePanelMsg::Activate(wid, "notes.txt".into()));
    assert_eq!(w.folder_of(&db), "/home/user");
    let _ = w.msg(FilePanelMsg::Activate(wid, "logs".into()));
    assert_eq!(w.folder_of(&db), "/home/user/logs");
    // At the top there is nothing above.
    let _ = w.msg(FilePanelMsg::Navigate(wid, "/".into()));
    w.answer_list(&db, vec![folder("etc")]);
    assert_eq!(w.shows().as_deref(), Some("etc"));
    w.key(super::PanelKey::Backspace);
    assert_eq!(w.folder_of(&db), "/");
}

#[test]
fn error_shows_inline() {
    let mut w = World::browsing(vec![folder("root")]);
    let (wid, db) = (w.wid, w.db.clone());
    let _ = w.msg(FilePanelMsg::Activate(wid, "root".into()));
    let (folder, token) = w.last_list(&db).unwrap();
    w.event(PanelEvent::Listed {
        profile: db.id,
        token,
        folder,
        result: Err(SftpError::Denied(
            "Permission denied: /home/user/root.".into(),
        )),
    });
    assert_eq!(
        w.shows().as_deref(),
        Some("error: Permission denied: /home/user/root.")
    );
    // Retry reads the folder again on the same session.
    let lists = w.fake.lists.lock().len();
    let _ = w.msg(FilePanelMsg::Retry(wid));
    assert_eq!(w.fake.lists.lock().len(), lists + 1);
    assert_eq!(w.opens(&db), 1);
    assert_eq!(w.shows().as_deref(), Some("loading"));

    // The session is gone: Retry opens a new one, asking over the window it was pressed in,
    // here a second window rather than the first one that showed the profile.
    let second = window::Id::unique();
    let mut win = WindowState::new();
    let pane = Pane::for_profile(&db);
    let pane_id = pane.id;
    win.push(Tab::single(pane));
    w.app.windows.insert(second, win);
    let handle = SessionHandle::inert(w.app.new_terminal(DEFAULT_GRID));
    let _ = w.app.update(Message::Connected(
        pane_id,
        Delivered::new(handle),
        DEFAULT_GRID,
    ));
    let _ = w.app.update(Message::Shortcut(second, Action::FilePanel));
    let (folder, token) = w.last_list(&db).unwrap();
    w.event(PanelEvent::Listed {
        profile: db.id,
        token,
        folder,
        result: Err(SftpError::Unavailable("The connection was lost.".into())),
    });
    assert_eq!(
        w.shows().as_deref(),
        Some("error: The connection was lost.")
    );
    assert_eq!(w.opens(&db), 1, "the error stays until Retry");
    let _ = w.msg(FilePanelMsg::Retry(second));
    assert_eq!(w.opens(&db), 2);
    assert_eq!(w.last_open(&db).unwrap().0, second);
    w.answer_open(&db, "/home/user");
    assert_eq!(
        w.last_list(&db).unwrap().0,
        "/home/user/root",
        "the folder is kept"
    );
}

#[test]
fn control_chars_in_names_are_neutralized() {
    let evil = "evil\u{1b}[31m\nname\u{7}";
    let w = World::browsing(vec![
        file(evil),
        link("ln\u{1b}", "\u{1b}]0;title\u{7}", Some(EntryKind::File)),
    ]);
    let Some(Body::Entries(panel)) = w.body() else {
        panic!("a listing");
    };
    let Listing::Loaded(entries) = &panel.listing else {
        panic!("loaded");
    };
    let shown: Vec<_> = entries
        .iter()
        .map(|e| super::view::row_text(e, 2026))
        .collect();
    let names: Vec<&str> = shown.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains(&"evil?[31m?name?"), "{names:?}");
    assert!(names.contains(&"ln?"), "{names:?}");
    let target = shown.iter().find_map(|r| r.target.clone()).unwrap();
    assert_eq!(target, "?]0;title?");
    for r in &shown {
        assert!(
            !r.name
                .chars()
                .chain(r.target.iter().flat_map(|t| t.chars()))
                .any(char::is_control),
            "{r:?}"
        );
    }
    // The path bar too.
    let segments = super::view::path_labels("/srv/a\u{1b}b");
    assert_eq!(segments, ["/", "srv", "a?b"]);
    // Bidi overrides and line separators cannot reorder or break a name.
    assert_eq!(
        super::plain_name("invoice\u{202E}fdp.exe"),
        "invoice?fdp.exe"
    );
    assert_eq!(super::plain_name("a\u{2028}b\u{2029}c"), "a?b?c");
    assert_eq!(
        super::plain_name(
            "\u{202A}\u{2066}\u{2069}\u{200E}\u{200F}\u{061C}\u{200B}\u{2060}\u{FEFF}"
        ),
        "?????????"
    );
    // Joiners stay: names need them.
    assert_eq!(
        super::plain_name("a\u{200C}b\u{200D}c"),
        "a\u{200C}b\u{200D}c"
    );

    // The inline error: paths and server text are shown plain too.
    let mut w = World::browsing(vec![]);
    let (wid, db) = (w.wid, w.db.clone());
    let _ = w.msg(FilePanelMsg::Navigate(
        wid,
        "/srv/x\u{1b}[2J\u{202E}y".into(),
    ));
    let (folder, token) = w.last_list(&db).unwrap();
    w.event(PanelEvent::Listed {
        profile: db.id,
        token,
        folder,
        result: Err(SftpError::Denied(
            "Permission denied for /srv/x\u{1b}[2J\u{202E}y.\nsecond line".into(),
        )),
    });
    assert_eq!(
        w.shows().as_deref(),
        Some("error: Permission denied for /srv/x?[2J?y.?second line")
    );
    let mut w = world();
    let db = w.db.clone();
    w.open_tab(&db);
    w.toggle();
    let (_, token) = w.last_open(&db).unwrap();
    w.event(PanelEvent::Opened {
        profile: db.id,
        token,
        result: Err("refused\u{1b}]0;x\u{7}".into()),
    });
    assert_eq!(
        w.shows().as_deref(),
        Some("error: SFTP is not available: refused?]0;x?")
    );
}

use super::Listing;

#[test]
fn panel_focus_takes_keys_from_the_terminal() {
    let mut w = World::browsing(vec![folder("logs")]);
    let wid = w.wid;
    let pane_focused = |w: &World| {
        let tab = w.app.windows[&w.wid].active_tab().unwrap();
        w.app.pane_style(w.wid, tab, tab.focused_pane()).focused
    };
    w.app.app_active = true;
    // Opening the panel gives it the keyboard; the terminal is drawn unfocused and gets no keys.
    assert!(w.app.file_panels.has_focus(wid));
    assert!(!pane_focused(&w));
    // A click in the terminal gives it back.
    let _ = w.msg(FilePanelMsg::Focus(wid, false));
    assert!(!w.app.file_panels.has_focus(wid));
    assert!(pane_focused(&w));
    // While the terminal has the keyboard, panel keys do nothing.
    let db = w.db.clone();
    w.key(super::PanelKey::Type("x".into()));
    w.key(super::PanelKey::Down);
    assert_eq!(w.panel().filter, "");
    assert_eq!(w.panel().selected, None);
    let _ = w.msg(FilePanelMsg::Focus(wid, true));
    w.key(super::PanelKey::Down);
    assert_eq!(w.panel().selected.as_deref(), Some(".."));
    // Escape gives the keyboard back.
    w.key(super::PanelKey::Escape);
    assert!(!w.app.file_panels.has_focus(wid));
    assert_eq!(w.folder_of(&db), "/home/user");
}

#[test]
fn large_folders_build_only_the_rows_on_screen() {
    let entries: Vec<Entry> = (0..100_000).map(|i| file(&format!("f{i:06}"))).collect();
    let w = World::browsing(entries);
    assert_eq!(w.panel().shown.len(), 100_000);
    // 100001 rows with `..`; a 600 px list scrolled to the middle builds about 600/ROW rows.
    let range = super::view::visible_rows(100_001, 1_000_000.0, 600.0);
    assert!(
        range.len() <= (600.0 / super::view::ROW_HEIGHT) as usize + 3,
        "{range:?}"
    );
    assert_eq!(
        range.start,
        (1_000_000.0 / super::view::ROW_HEIGHT) as usize
    );
    // Unknown height: still a bounded number of rows.
    assert!(super::view::visible_rows(100_001, 0.0, 0.0).len() <= 120);
    // Near the end it stops at the last row.
    assert_eq!(super::view::visible_rows(10, 0.0, 600.0), 0..10);
}

#[test]
fn the_keyboard_goes_back_when_focus_moves_or_nothing_is_browsed() {
    let mut w = world();
    let (db, web, local) = (w.db.clone(), w.web.clone(), w.local.clone());
    let (_, right) = w.open_split(&db, &web);
    w.toggle();
    assert!(w.app.file_panels.has_focus(w.wid));
    // Another pane takes focus: the terminal gets the keys again.
    w.focus(right);
    assert!(!w.app.file_panels.has_focus(w.wid));
    // A local pane's panel has nothing to browse, so it never keeps the keyboard.
    w.open_tab(&local);
    let wid = w.wid;
    let _ = w.msg(FilePanelMsg::Focus(wid, true));
    assert!(!w.app.file_panels.has_focus(w.wid));
}

/// Every text `wid`'s panel draws, laid out in a `size` panel.
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
    let renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        iced::Font::DEFAULT,
        iced::Pixels(16.0),
    ));
    let (mut element, _) = super::view::panel(&w.app, w.wid).expect("the panel is open");
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
fn shrinking_rows_never_blank_the_list() {
    let size = iced::Size::new(300.0, 700.0);
    let entries: Vec<Entry> = (0..5000).map(|i| file(&format!("f{i:06}"))).collect();
    let mut w = World::browsing(entries);
    let (wid, db) = (w.wid, w.db.clone());
    let scroll_far = |w: &mut World| {
        let _ = w.msg(FilePanelMsg::Scrolled(
            wid,
            4000.0 * super::view::ROW_HEIGHT,
            500.0,
        ));
    };
    scroll_far(&mut w);
    let texts = drawn_texts(&w, size);
    assert!(
        texts.iter().any(|t| t == "f003999"),
        "rows near the offset are built"
    );
    assert!(!texts.iter().any(|t| t == "f000001"));

    // A filter that leaves a few rows: they are drawn, from the top.
    let _ = w.msg(FilePanelMsg::Filter(wid, "f00000".into()));
    assert_eq!(w.app.file_panels.ui[&wid].offset, 0.0);
    assert!(drawn_texts(&w, size).iter().any(|t| t == "f000003"));

    // A stale offset alone (no reset) still draws the rows: the list clamps it.
    w.app.file_panels.ui.get_mut(&wid).unwrap().offset = 4000.0 * super::view::ROW_HEIGHT;
    assert!(drawn_texts(&w, size).iter().any(|t| t == "f000003"));

    // Hidden files off, a shorter re-list: back at the top each time.
    let _ = w.msg(FilePanelMsg::Filter(wid, String::new()));
    scroll_far(&mut w);
    let _ = w.msg(FilePanelMsg::ShowHidden(false));
    assert_eq!(w.app.file_panels.ui[&wid].offset, 0.0);
    scroll_far(&mut w);
    let _ = w.msg(FilePanelMsg::Retry(wid));
    assert_eq!(w.app.file_panels.ui[&wid].offset, 0.0);
    scroll_far(&mut w);
    w.answer_list(&db, vec![file("a.log"), file("b.log")]);
    assert_eq!(w.app.file_panels.ui[&wid].offset, 0.0);
    assert!(drawn_texts(&w, size).iter().any(|t| t == "b.log"));
}

#[test]
fn the_list_starts_at_the_top_when_another_profile_is_shown() {
    let mut w = world();
    let (db, web) = (w.db.clone(), w.web.clone());
    let (_, right) = w.open_split(&db, &web);
    w.toggle();
    w.answer_open(&db, "/home/user");
    w.answer_list(&db, (0..500).map(|i| file(&format!("f{i}"))).collect());
    let wid = w.wid;
    let _ = w.msg(FilePanelMsg::Scrolled(
        wid,
        400.0 * super::view::ROW_HEIGHT,
        500.0,
    ));
    w.focus(right);
    assert_eq!(w.app.file_panels.ui[&wid].offset, 0.0);
}

#[test]
fn navigating_without_a_session_reopens_it() {
    let mut w = World::browsing(vec![folder("logs")]);
    let (wid, db) = (w.wid, w.db.clone());
    let (folder, token) = w.last_list(&db).unwrap();
    w.event(PanelEvent::Listed {
        profile: db.id,
        token,
        folder,
        result: Err(SftpError::Unavailable("The connection was lost.".into())),
    });
    assert_eq!(
        w.shows().as_deref(),
        Some("error: The connection was lost.")
    );
    // Going elsewhere does not leave the old error under the new path: the session is
    // opened again, over this window, for the new folder.
    let _ = w.msg(FilePanelMsg::Navigate(wid, "/var/log".into()));
    assert_eq!(w.shows().as_deref(), Some("loading"));
    assert_eq!(w.opens(&db), 2);
    assert_eq!(w.last_open(&db).unwrap().0, wid);
    w.answer_open(&db, "/home/user");
    assert_eq!(w.last_list(&db).unwrap().0, "/var/log");
}
