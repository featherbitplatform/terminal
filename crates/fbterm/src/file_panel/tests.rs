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

    fn folder_of(&self, p: &Profile) -> String {
        self.app.file_panels.by_profile[&p.id].folder.clone()
    }
}

#[test]
fn toggle_opens_and_closes_panel() {
    let mut w = world();
    w.open_tab(&w.db.clone());
    assert!(!w.app.file_panels.is_open(w.wid));
    assert_eq!(w.body(), None, "a closed panel shows nothing");
    w.toggle();
    assert!(w.app.file_panels.is_open(w.wid));
    assert!(w.body().is_some());
    w.toggle();
    assert!(!w.app.file_panels.is_open(w.wid));
    assert_eq!(w.body(), None);
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
    assert_eq!(w.body(), Some(Body::Loading));
    w.answer_open(&db, "/home/user");
    assert_eq!(
        w.last_list(&db).map(|(f, _)| f).as_deref(),
        Some("/home/user")
    );
    w.answer_list(&db, vec![file("app.log"), folder("backups")]);
    assert_eq!(
        w.body(),
        Some(Body::Entries(&[folder("backups"), file("app.log")])),
        "folders first"
    );

    // Focus moves to the other profile's pane: the panel switches to its server.
    w.focus(right);
    assert_eq!(w.last_open(&web).map(|(win, _)| win), Some(w.wid));
    assert_eq!(w.body(), Some(Body::Loading));
    w.answer_open(&web, "/srv");
    w.answer_list(&web, vec![file("index.html")]);
    assert_eq!(w.body(), Some(Body::Entries(&[file("index.html")])));

    // And back: db's listing is still there, nothing is opened again.
    w.focus(left);
    assert_eq!(
        w.body(),
        Some(Body::Entries(&[folder("backups"), file("app.log")]))
    );
    assert_eq!(w.opens(&db), 1);

    // A late answer for a superseded read is ignored.
    w.event(PanelEvent::Listed {
        profile: db.id,
        token: 0,
        folder: "/home/user".into(),
        result: Ok(vec![file("stale")]),
    });
    assert_eq!(
        w.body(),
        Some(Body::Entries(&[folder("backups"), file("app.log")]))
    );
}

#[test]
fn local_pane_shows_placeholder() {
    let mut w = world();
    let local = w.local.clone();
    w.open_tab(&local);
    w.toggle();
    assert_eq!(w.body(), Some(Body::Note("Local session".into())));
    assert!(w.fake.opens.lock().is_empty());

    // An SSH pane that is not connected.
    let db = w.db.clone();
    let pane = Pane::for_profile(&db);
    // Pushing a tab makes it the active one.
    w.window().push(Tab::single(pane));
    let _ = w.app.update(Message::DismissNotices);
    assert_eq!(w.body(), Some(Body::Note("Not connected".into())));
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
        w.body(),
        Some(Body::Note(
            "SFTP is not available: the server refused the sftp subsystem".into()
        ))
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
    // The user went to another folder (browsing comes with the listing view).
    w.app.file_panels.by_profile.get_mut(&db.id).unwrap().folder = "/var/log".into();
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
        let mut element = crate::window::dock(content, docked);
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
        let mut element = crate::window::dock(content, Some((panel, 700.0)));
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
        crate::window::dock(fill(), Some((fill(), 300.0)))
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
        let open = crate::window::dock(input(), Some((fill(), 300.0)));
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
    let Some(Body::Note(line)) = w.body() else {
        panic!("a note, not {:?}", w.body());
    };
    assert!(line.starts_with("SFTP is not available: "), "{line}");
}
