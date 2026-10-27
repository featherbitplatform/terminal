//! The file panel inside the app: toggling, following the focused pane, placeholders, each
//! profile's folder, the remembered width and the docked layout. A fake backend stands in for
//! SFTP, so no network is needed: the tests answer its requests by hand.

use std::path::PathBuf;
use std::sync::Arc;

use fbterm_config::{AuthMethod, Paths, Profile, ProfileId, ProfileKind, SavedState, SshProfile};
use fbterm_session::SessionHandle;
use fbterm_session::ssh::sftp::{Entry, EntryKind, Sftp, SftpError};
use fbterm_session::ssh::transfer::{TransferEvent, TransferId, TransferSink, UploadMode};
use iced::window;
use parking_lot::Mutex;

use super::{Backend, Body, FilePanelMsg, ListRequest, OpenRequest, Opened, PanelEvent, Remote};
use crate::app::{App, DEFAULT_GRID, Delivered, Message};
use crate::keys::Action;
use crate::layout::{PaneId, Side};
use crate::prompts::{Dialog, PromptMsg};
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
    /// Downloads started: the remote path, the destination and the transfer's id.
    downloads: Mutex<Vec<(String, PathBuf, TransferId)>>,
    /// Transfers cancelled.
    cancels: Arc<Mutex<Vec<TransferId>>>,
    /// Save dialogs opened: the window and the name offered.
    saves: Mutex<Vec<(window::Id, String)>>,
    /// Uploads started: the local file, the folder, the name, the mode and the id.
    uploads: Mutex<Vec<Uploaded>>,
    /// `Upload…` pickers opened, by window.
    picks: Mutex<Vec<window::Id>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Uploaded {
    local: PathBuf,
    folder: String,
    name: String,
    mode: UploadMode,
    id: TransferId,
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

    fn download(
        &self,
        request: super::DownloadRequest,
        _sink: TransferSink,
    ) -> Result<super::Started, String> {
        let id = TransferId::next();
        self.downloads.lock().push((request.path, request.dest, id));
        let cancels = self.cancels.clone();
        Ok(super::Started {
            id,
            cancel: Arc::new(move || cancels.lock().push(id)),
        })
    }

    fn upload(
        &self,
        request: super::UploadRequest,
        _sink: TransferSink,
    ) -> Result<super::Started, String> {
        let id = TransferId::next();
        self.uploads.lock().push(Uploaded {
            local: request.local,
            folder: request.folder,
            name: request.name,
            mode: request.mode,
            id,
        });
        let cancels = self.cancels.clone();
        Ok(super::Started {
            id,
            cancel: Arc::new(move || cancels.lock().push(id)),
        })
    }

    fn save_dialog(
        &self,
        window: window::Id,
        name: String,
        _folder: Option<PathBuf>,
    ) -> iced::Task<Option<PathBuf>> {
        self.saves.lock().push((window, name));
        iced::Task::none()
    }

    fn pick_files(&self, window: window::Id) -> iced::Task<Option<Vec<PathBuf>>> {
        self.picks.lock().push(window);
        iced::Task::none()
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
    /// The Downloads stand-in of [`World::downloading`], removed with the world.
    downloads: Option<tempfile::TempDir>,
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
        downloads: None,
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

#[test]
fn navigating_while_the_session_opens_keeps_that_open() {
    let mut w = World::browsing(vec![folder("logs")]);
    let (wid, db) = (w.wid, w.db.clone());
    let (folder, token) = w.last_list(&db).unwrap();
    w.event(PanelEvent::Listed {
        profile: db.id,
        token,
        folder,
        result: Err(SftpError::Unavailable("The connection was lost.".into())),
    });
    let _ = w.msg(FilePanelMsg::Retry(wid));
    assert_eq!(w.opens(&db), 2);
    // The open under way (which may be asking a sign-in question) is not restarted: it
    // lists the new folder when it opens.
    let _ = w.msg(FilePanelMsg::Navigate(wid, "/var/log".into()));
    assert_eq!(w.opens(&db), 2, "no second open");
    assert_eq!(w.folder_of(&db), "/var/log");
    assert_eq!(w.shows().as_deref(), Some("loading"));
    w.answer_open(&db, "/home/user");
    assert_eq!(w.last_list(&db).unwrap().0, "/var/log");
}

// ---- Downloads ----

impl World {
    /// [`World::browsing`] with Downloads at a fresh temporary folder.
    fn downloading(entries: Vec<Entry>) -> (World, PathBuf) {
        let mut w = World::browsing(entries);
        // A fresh folder standing in for Downloads (tests never write to the real one),
        // removed when the world goes.
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().to_path_buf();
        w.downloads = Some(temp);
        w.app.file_panel_work.downloads = Some(dir.clone());
        (w, dir)
    }

    fn select(&mut self, name: &str) {
        let wid = self.wid;
        let _ = self.msg(FilePanelMsg::Select(wid, name.into()));
    }

    fn download(&mut self) {
        let wid = self.wid;
        let _ = self.msg(FilePanelMsg::Download(wid));
    }

    fn download_as(&mut self) {
        let wid = self.wid;
        let _ = self.msg(FilePanelMsg::DownloadAs(wid));
    }

    /// The save dialog of `Download as…` closes with `path`.
    fn choose(&mut self, path: Option<PathBuf>) {
        let wid = self.wid;
        let _ = self.msg(FilePanelMsg::SaveChosen(wid, path));
    }

    fn downloads(&self) -> Vec<(String, PathBuf, TransferId)> {
        self.fake.downloads.lock().clone()
    }

    fn transfer(&mut self, id: TransferId, event: TransferEvent) {
        let _ = self.msg(FilePanelMsg::Transfer(id, event));
    }

    fn rows(&self) -> &[super::state::TransferRow] {
        &self.panel().transfers
    }
}

#[test]
fn download_goes_to_downloads_with_free_name() {
    let (mut w, dir) = World::downloading(vec![file("app.log"), folder("logs")]);
    std::fs::write(dir.join("app.log"), "already here").unwrap();
    w.select("app.log");
    assert!(w.app.can_download(w.wid));
    w.download();
    let started = w.downloads();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].0, "/home/user/app.log");
    assert_eq!(
        started[0].1,
        dir.join("app (1).log"),
        "a free name, never over a file"
    );
    // The row: the name, nothing done yet, running.
    assert_eq!(w.rows().len(), 1);
    assert_eq!(w.rows()[0].name, "app.log");
    assert!(w.rows()[0].is_running());

    // A second Download while the first runs: the first's name is taken though nothing is
    // written there yet, so two transfers never write the same file.
    w.download();
    assert_eq!(w.downloads()[1].1, dir.join("app (2).log"));
    assert!(!dir.join("app (1).log").exists() && !dir.join("app (2).log").exists());

    // Progress updates the row as events come (no timer); finished, it offers the file.
    let id = started[0].2;
    w.transfer(
        id,
        TransferEvent::Progress {
            done: 512,
            total: Some(2048),
        },
    );
    assert_eq!((w.rows()[0].done, w.rows()[0].total), (512, Some(2048)));
    w.transfer(
        id,
        TransferEvent::Finished {
            note: None,
            local: Some(dir.join("app (1).log")),
        },
    );
    assert!(!w.rows()[0].is_running());
    assert_eq!(
        w.rows()[0].revealable(),
        Some(dir.join("app (1).log").as_path())
    );
    assert_eq!(
        super::REVEAL_LABEL,
        if cfg!(target_os = "macos") {
            "Show in Finder"
        } else {
            "Show in folder"
        }
    );
    // Its name is free again for a new Download only once a file is really there.
    std::fs::write(dir.join("app (1).log"), "done").unwrap();
    w.download();
    assert_eq!(w.downloads()[2].1, dir.join("app (3).log"));

    // Cancel stops the transfer; the cancelled row goes away when it reports so.
    let second = w.downloads()[1].2;
    let _ = w.msg(FilePanelMsg::CancelTransfer(second));
    assert_eq!(*w.fake.cancels.lock(), [second]);
    w.transfer(second, TransferEvent::Cancelled);
    assert!(w.rows().iter().all(|r| r.id != second));
    // A failed one shows why.
    let third = w.downloads()[2].2;
    w.transfer(third, TransferEvent::Failed("Permission denied.".into()));
    let row = w.rows().iter().find(|r| r.id == third).unwrap();
    assert_eq!(row.failed(), Some("Permission denied."));
}

#[test]
fn download_as_uses_chosen_path() {
    let (mut w, dir) = World::downloading(vec![file("app.log")]);
    let wid = w.wid;
    w.select("app.log");
    w.download_as();
    // The save dialog, over this window, offers the remote name.
    assert_eq!(*w.fake.saves.lock(), [(wid, "app.log".to_owned())]);
    assert!(
        w.downloads().is_empty(),
        "nothing starts before a path is chosen"
    );
    // A second click while the dialog is open opens no second one.
    w.download_as();
    assert_eq!(w.fake.saves.lock().len(), 1);
    let chosen = dir.join("saved.log");
    w.choose(Some(chosen.clone()));
    assert_eq!(w.downloads().len(), 1);
    assert_eq!(w.downloads()[0].0, "/home/user/app.log");
    assert_eq!(w.downloads()[0].1, chosen, "exactly where the user chose");

    // Cancelling the dialog starts nothing.
    w.download_as();
    w.choose(None);
    assert_eq!(w.downloads().len(), 1);

    // The same path while that download runs: refused inline, nothing started.
    w.download_as();
    w.choose(Some(chosen.clone()));
    assert_eq!(w.downloads().len(), 1);
    let refused = w.rows().last().unwrap();
    assert_eq!(refused.failed(), Some("Already downloading to saved.log"));
    // `Download` into the same folder skips the name too.
    w.download_as();
    w.choose(Some(dir.join("app.log")));
    w.download();
    assert_eq!(w.downloads()[2].1, dir.join("app (1).log"));

    // Once the first one ended, its path can be chosen again.
    let first = w.downloads()[0].2;
    w.transfer(
        first,
        TransferEvent::Failed("The connection was lost.".into()),
    );
    w.download_as();
    w.choose(Some(chosen.clone()));
    assert_eq!(w.downloads().len(), 4);
    assert_eq!(w.downloads()[3].1, chosen);
}

#[test]
fn download_disabled_for_folders_and_special_files() {
    let other = Entry {
        kind: EntryKind::Other,
        ..file("fifo")
    };
    let (mut w, _dir) = World::downloading(vec![
        folder("logs"),
        file("app.log"),
        link("latest.log", "app.log", Some(EntryKind::File)),
        link("current", "releases/42", Some(EntryKind::Dir)),
        link("dangling", "gone", None),
        other,
    ]);
    let wid = w.wid;
    assert!(!w.app.can_download(wid), "nothing selected");
    for name in ["..", "logs", "current", "dangling", "fifo"] {
        w.select(name);
        assert!(!w.app.can_download(wid), "{name}");
        w.download();
        w.download_as();
        assert!(w.downloads().is_empty(), "{name}");
        assert!(w.fake.saves.lock().is_empty(), "{name}");
    }
    // A file, and a link to a file (its target downloads).
    for name in ["app.log", "latest.log"] {
        w.select(name);
        assert!(w.app.can_download(wid), "{name}");
    }
    w.download();
    assert_eq!(w.downloads()[0].0, "/home/user/latest.log");
    // Nothing the terminal shows can start one: only these panel messages do, and the panel
    // of a local pane offers none.
    let mut w = world();
    let local = w.local.clone();
    w.open_tab(&local);
    w.toggle();
    assert!(!w.app.can_download(w.wid));
}

#[test]
fn download_uses_sanitized_name() {
    let evil = "evil\u{1b}[2J\nname.txt";
    let (mut w, dir) = World::downloading(vec![file(evil)]);
    w.select(evil);
    w.download();
    let (remote, dest, _) = w.downloads()[0].clone();
    assert_eq!(
        remote,
        format!("/home/user/{evil}"),
        "the server's own name"
    );
    assert_eq!(dest, dir.join("evil_[2J_name.txt"));
    assert_eq!(dest.parent(), Some(dir.as_path()));
    // Shown plain in the row.
    assert_eq!(w.rows()[0].name, "evil?[2J?name.txt");
    // `Download as…` offers the same safe name.
    w.download_as();
    assert_eq!(w.fake.saves.lock()[0].1, "evil_[2J_name.txt");
}

#[test]
fn running_downloads_keep_the_session_when_the_panel_hides() {
    let (mut w, _dir) = World::downloading(vec![file("app.log")]);
    w.select("app.log");
    w.download();
    w.toggle();
    assert!(
        w.panel().sftp.is_some(),
        "a running transfer keeps the session"
    );
    let id = w.downloads()[0].2;
    w.transfer(
        id,
        TransferEvent::Finished {
            note: None,
            local: None,
        },
    );
    // Any message runs the sync: nothing runs any more, so the session goes.
    let _ = w.msg(FilePanelMsg::ResizeEnd);
    assert!(w.panel().sftp.is_none());
}

#[test]
fn transfer_rows_show_progress_and_their_actions() {
    let size = iced::Size::new(300.0, 700.0);
    let (mut w, dir) = World::downloading(vec![file("app.log"), file("db.dump")]);
    let texts = drawn_texts(&w, size);
    assert!(texts.iter().any(|t| t == "Download"));
    assert!(texts.iter().any(|t| t == "Download as\u{2026}"));
    w.select("app.log");
    w.download();
    w.select("db.dump");
    w.download();
    w.download();
    let ids: Vec<TransferId> = w.downloads().iter().map(|d| d.2).collect();
    w.transfer(
        ids[0],
        TransferEvent::Progress {
            done: 512,
            total: Some(2048),
        },
    );
    w.transfer(
        ids[1],
        TransferEvent::Finished {
            note: None,
            local: Some(dir.join("db.dump")),
        },
    );
    w.transfer(ids[2], TransferEvent::Failed("Disk full.".into()));
    let texts = drawn_texts(&w, size);
    for want in [
        "app.log",
        "512 B of 2.0 KB",
        "Cancel",
        super::REVEAL_LABEL,
        "Disk full.",
    ] {
        assert!(texts.iter().any(|t| t == want), "{want} in {texts:?}");
    }
    assert_eq!(
        texts.iter().filter(|t| *t == "Cancel").count(),
        1,
        "only the running row can be cancelled"
    );
}

#[test]
fn names_differing_only_in_case_never_share_a_destination() {
    // A case-insensitive disk (the default on macOS and Windows) holds one file for both.
    let (mut w, dir) = World::downloading(vec![file("App.log"), file("app.log")]);
    w.select("App.log");
    w.download();
    w.select("app.log");
    w.download();
    assert_eq!(w.downloads()[0].1, dir.join("App.log"));
    assert_eq!(w.downloads()[1].1, dir.join("app (1).log"));
    // `Download as…` onto a running destination in another case: refused.
    w.download_as();
    w.choose(Some(dir.join("APP.LOG")));
    assert_eq!(w.downloads().len(), 2);
    assert_eq!(
        w.rows().last().unwrap().failed(),
        Some("Already downloading to APP.LOG")
    );
}

#[test]
fn download_needs_the_selection_on_screen() {
    let (mut w, _dir) = World::downloading(vec![file(".env"), file("app.log")]);
    let wid = w.wid;
    w.select(".env");
    assert!(w.app.can_download(wid));
    // The filter hides it: nothing the user cannot see is downloaded.
    let _ = w.msg(FilePanelMsg::Filter(wid, "app".into()));
    assert!(!w.app.can_download(wid));
    w.download();
    w.download_as();
    assert!(w.downloads().is_empty());
    assert!(w.fake.saves.lock().is_empty());
    // So does turning hidden files off.
    let _ = w.msg(FilePanelMsg::Filter(wid, String::new()));
    assert!(w.app.can_download(wid));
    let _ = w.msg(FilePanelMsg::ShowHidden(false));
    assert!(!w.app.can_download(wid));
}

#[test]
fn closing_the_window_forgets_its_save_dialog() {
    let (mut w, _dir) = World::downloading(vec![file("app.log")]);
    let wid = w.wid;
    w.select("app.log");
    w.download_as();
    assert!(w.app.file_panels.saving.contains_key(&wid));
    let _ = w.app.update(Message::WindowClosed(wid));
    assert!(w.app.file_panels.saving.is_empty());
}

// ---- Uploads, the queue and closing with transfers running ----

/// A session whose connection has dropped.
struct DeadRemote;

#[async_trait::async_trait]
impl Remote for DeadRemote {
    async fn list(&self, _path: &str) -> Result<Vec<Entry>, SftpError> {
        Err(SftpError::Unavailable("The connection was lost.".into()))
    }

    fn sftp(&self) -> Option<Arc<Sftp>> {
        None
    }

    fn is_alive(&self) -> bool {
        false
    }
}

impl World {
    /// [`World::browsing`] with a fresh local folder holding `files` (a name ending in `/`
    /// is a folder) to upload from.
    fn uploading(entries: Vec<Entry>, files: &[&str]) -> (World, PathBuf) {
        let mut w = World::browsing(entries);
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().to_path_buf();
        for f in files {
            match f.strip_suffix('/') {
                Some(folder) => std::fs::create_dir(dir.join(folder)).unwrap(),
                None => std::fs::write(dir.join(f), "content").unwrap(),
            }
        }
        w.downloads = Some(temp);
        (w, dir)
    }

    fn pick(&mut self, paths: Vec<PathBuf>) {
        let wid = self.wid;
        let _ = self.msg(FilePanelMsg::Upload(wid));
        let _ = self.msg(FilePanelMsg::Picked(wid, Some(paths)));
    }

    fn uploads(&self) -> Vec<Uploaded> {
        self.fake.uploads.lock().clone()
    }

    /// Every running upload finishes; the folder is read again and lists `entries`.
    fn finish_uploads(&mut self, entries: Vec<Entry>) {
        let running: Vec<TransferId> = self
            .rows()
            .iter()
            .filter(|r| r.is_running() && r.direction == super::state::Direction::Upload)
            .map(|r| r.id)
            .collect();
        for id in running {
            self.transfer(
                id,
                TransferEvent::Finished {
                    note: None,
                    local: None,
                },
            );
        }
        let db = self.db.clone();
        self.answer_list(&db, entries);
    }

    fn prompt(&mut self, m: PromptMsg) {
        let _ = self.app.update(Message::Prompt(m));
    }

    fn dialog(&self) -> Option<&Dialog> {
        self.app.prompts.front(self.wid)
    }

    /// The clash dialog shown: its name, whether it offers `Apply to all`, and the rename
    /// field's text when it shows one.
    fn clash(&self) -> Option<(String, bool, Option<String>)> {
        match self.dialog()? {
            Dialog::Clash(c) => Some((
                c.name.clone(),
                c.offer_all,
                c.rename.as_ref().map(|r| r.value.clone()),
            )),
            _ => None,
        }
    }

    fn rename_problem(&self) -> Option<String> {
        match self.dialog()? {
            Dialog::Clash(c) => c.rename.as_ref()?.problem.clone(),
            _ => None,
        }
    }

    /// The close confirmation shown: how many transfers it counts.
    fn running_asked(&self) -> Option<usize> {
        match self.dialog()? {
            Dialog::Transfers { count, .. } => Some(*count),
            _ => None,
        }
    }
}

fn uploaded(
    local: &std::path::Path,
    name: &str,
    mode: UploadMode,
) -> (PathBuf, String, String, UploadMode) {
    (local.to_path_buf(), "/home/user".into(), name.into(), mode)
}

fn summary(u: &[Uploaded]) -> Vec<(PathBuf, String, String, UploadMode)> {
    u.iter()
        .map(|u| (u.local.clone(), u.folder.clone(), u.name.clone(), u.mode))
        .collect()
}

#[test]
fn upload_new_files_into_current_folder() {
    let (mut w, dir) = World::uploading(vec![file("app.log")], &["a.conf", "b.conf"]);
    let wid = w.wid;
    assert!(w.app.can_upload(wid));
    let _ = w.msg(FilePanelMsg::Upload(wid));
    assert_eq!(
        *w.fake.picks.lock(),
        [wid],
        "the picker opens over this window"
    );
    // A second click while the picker is open opens no second one.
    let _ = w.msg(FilePanelMsg::Upload(wid));
    assert_eq!(w.fake.picks.lock().len(), 1);
    // Cancelling the picker uploads nothing.
    let _ = w.msg(FilePanelMsg::Picked(wid, None));
    assert!(w.uploads().is_empty());

    w.pick(vec![dir.join("a.conf"), dir.join("b.conf")]);
    assert_eq!(
        summary(&w.uploads()),
        [
            uploaded(&dir.join("a.conf"), "a.conf", UploadMode::New),
            uploaded(&dir.join("b.conf"), "b.conf", UploadMode::New),
        ]
    );
    assert!(w.dialog().is_none(), "no name clashes, nothing asked");
    let rows: Vec<_> = w
        .rows()
        .iter()
        .map(|r| (r.name.clone(), r.direction))
        .collect();
    assert_eq!(
        rows,
        [
            ("a.conf".to_owned(), super::state::Direction::Upload),
            ("b.conf".to_owned(), super::state::Direction::Upload),
        ]
    );
    assert!(w.rows().iter().all(super::state::TransferRow::is_running));

    // Finishing: the note shows; once no upload into the folder runs, it is read again.
    let ids: Vec<TransferId> = w.uploads().iter().map(|u| u.id).collect();
    let reads = w.fake.lists.lock().len();
    w.transfer(
        ids[0],
        TransferEvent::Finished {
            note: None,
            local: None,
        },
    );
    assert_eq!(w.fake.lists.lock().len(), reads, "b.conf still runs");
    w.transfer(
        ids[1],
        TransferEvent::Finished {
            note: Some("Replaced in place".into()),
            local: None,
        },
    );
    assert_eq!(w.rows()[1].note(), Some("Replaced in place"));
    assert_eq!(
        w.fake.lists.lock().len(),
        reads + 1,
        "the folder is read again"
    );
    assert_eq!(w.last_list(&w.db.clone()).unwrap().0, "/home/user");
    // Uploads never claim a local destination.
    assert!(w.rows().iter().all(|r| r.writing().is_none()));
    let texts = drawn_texts(&w, iced::Size::new(300.0, 700.0));
    for want in ["Upload\u{2026}", "Replaced in place"] {
        assert!(texts.iter().any(|t| t == want), "{want} in {texts:?}");
    }
}

#[test]
fn clash_asks_replace_rename_cancel() {
    let (mut w, dir) = World::uploading(
        vec![file("app.conf"), file("other")],
        &["app.conf", "other"],
    );
    let wid = w.wid;
    let local = dir.join("app.conf");

    // Replace.
    w.pick(vec![local.clone()]);
    assert!(
        w.uploads().is_empty(),
        "nothing replaced before the user says so"
    );
    assert_eq!(w.clash(), Some(("app.conf".into(), false, None)));
    assert_eq!(
        super::clash_message("app.conf", "/home/user"),
        "\"app.conf\" already exists in /home/user."
    );
    w.prompt(PromptMsg::ClashReplace(wid));
    assert!(w.dialog().is_none());
    assert_eq!(
        summary(&w.uploads()),
        [uploaded(&local, "app.conf", UploadMode::Replace)]
    );
    w.finish_uploads(vec![file("app.conf"), file("other")]);

    // Cancel (Escape answers the same): nothing uploaded, no row.
    let rows = w.rows().len();
    w.pick(vec![local.clone()]);
    w.prompt(PromptMsg::Cancel(wid));
    assert!(w.dialog().is_none());
    assert_eq!(w.uploads().len(), 1);
    assert_eq!(w.rows().len(), rows);

    // Rename: a name that is checked before it is used.
    w.pick(vec![local.clone()]);
    w.prompt(PromptMsg::ClashRename(wid));
    for (typed, problem) in [
        ("", "Type a name."),
        ("  ", "Type a name."),
        ("a/b.conf", "A name cannot contain /."),
        (
            "bell\u{7}.conf",
            "A name cannot contain control characters.",
        ),
        ("..", "Choose another name."),
    ] {
        w.prompt(PromptMsg::ClashName(wid, typed.into()));
        w.prompt(PromptMsg::Submit(wid));
        assert_eq!(w.rename_problem().as_deref(), Some(problem), "{typed:?}");
        assert_eq!(w.uploads().len(), 1, "{typed:?}");
    }
    // Escape (`Back`) in the name field returns to the choice; nothing is decided.
    w.prompt(PromptMsg::Cancel(wid));
    assert_eq!(w.clash(), Some(("app.conf".into(), false, None)));
    assert_eq!(w.uploads().len(), 1);
    w.prompt(PromptMsg::ClashRename(wid));
    w.prompt(PromptMsg::ClashName(wid, "app-new.conf".into()));
    w.prompt(PromptMsg::Submit(wid));
    assert!(w.dialog().is_none());
    assert_eq!(
        summary(&w.uploads())[1],
        uploaded(&local, "app-new.conf", UploadMode::New)
    );

    // A new name that clashes again is asked about again.
    w.pick(vec![local.clone()]);
    w.prompt(PromptMsg::ClashRename(wid));
    w.prompt(PromptMsg::ClashName(wid, "other".into()));
    w.prompt(PromptMsg::Submit(wid));
    assert_eq!(w.clash(), Some(("other".into(), false, None)));
    w.prompt(PromptMsg::ClashReplace(wid));
    assert_eq!(
        summary(&w.uploads())[2],
        uploaded(&local, "other", UploadMode::Replace)
    );
}

#[test]
fn apply_to_all_covers_remaining() {
    let (mut w, dir) = World::uploading(
        vec![file("a.conf"), file("b.conf"), file("c.conf")],
        &["a.conf", "b.conf", "c.conf", "d.conf", "e.conf"],
    );
    let wid = w.wid;
    let all: Vec<PathBuf> = ["a.conf", "b.conf", "c.conf", "d.conf", "e.conf"]
        .iter()
        .map(|n| dir.join(n))
        .collect();
    w.pick(all[..4].to_vec());
    assert_eq!(w.clash(), Some(("a.conf".into(), true, None)));
    // Without `Apply to all` each clash is asked about.
    w.prompt(PromptMsg::ClashReplace(wid));
    assert_eq!(w.clash(), Some(("b.conf".into(), true, None)));
    w.prompt(PromptMsg::ClashApplyAll(wid, true));
    // One new name is for one file: `Rename` does nothing with `Apply to all`.
    w.prompt(PromptMsg::ClashRename(wid));
    assert_eq!(w.clash(), Some(("b.conf".into(), true, None)));
    w.prompt(PromptMsg::ClashReplace(wid));
    assert!(w.dialog().is_none());
    assert_eq!(
        summary(&w.uploads()),
        [
            uploaded(&all[0], "a.conf", UploadMode::Replace),
            uploaded(&all[1], "b.conf", UploadMode::Replace),
            uploaded(&all[2], "c.conf", UploadMode::Replace),
            uploaded(&all[3], "d.conf", UploadMode::New),
        ]
    );

    w.finish_uploads(["a.conf", "b.conf", "c.conf", "d.conf"].map(file).to_vec());

    // Cancel for all: only the file without a clash goes.
    let before = w.uploads().len();
    w.pick(vec![all[0].clone(), all[1].clone(), all[4].clone()]);
    w.prompt(PromptMsg::ClashApplyAll(wid, true));
    w.prompt(PromptMsg::Cancel(wid));
    assert!(w.dialog().is_none());
    let names: Vec<String> = w.uploads()[before..]
        .iter()
        .map(|u| u.name.clone())
        .collect();
    assert_eq!(names, ["e.conf"]);

    // The last clash of a batch offers no `Apply to all`: nothing is left to apply it to.
    w.pick(vec![dir.join("d.conf"), all[0].clone()]);
    assert_eq!(w.clash(), Some(("d.conf".into(), true, None)));
    w.prompt(PromptMsg::Cancel(wid));
    assert_eq!(w.clash(), Some(("a.conf".into(), false, None)));
    w.prompt(PromptMsg::Cancel(wid));
    assert!(w.dialog().is_none());
}

#[test]
fn rename_prefills_free_name() {
    let (mut w, dir) = World::uploading(
        vec![file("app.conf"), file("app (1).conf"), file("notes")],
        &["app.conf", "notes"],
    );
    let wid = w.wid;
    w.pick(vec![dir.join("app.conf")]);
    w.prompt(PromptMsg::ClashRename(wid));
    assert_eq!(
        w.clash(),
        Some(("app.conf".into(), false, Some("app (2).conf".into())))
    );
    w.prompt(PromptMsg::Submit(wid));
    assert_eq!(w.uploads()[0].name, "app (2).conf");
    w.pick(vec![dir.join("notes")]);
    w.prompt(PromptMsg::ClashRename(wid));
    assert_eq!(
        w.clash(),
        Some(("notes".into(), false, Some("notes (1)".into())))
    );
    // `app (2).conf` is uploading now: the next free name skips it.
    w.prompt(PromptMsg::Cancel(wid));
    w.prompt(PromptMsg::Cancel(wid));
    assert!(w.dialog().is_none());
    w.pick(vec![dir.join("app.conf")]);
    w.prompt(PromptMsg::ClashRename(wid));
    assert_eq!(w.clash().and_then(|c| c.2).as_deref(), Some("app (3).conf"));
    assert_eq!(
        super::remote_free_name(".bashrc", |n| n == ".bashrc"),
        ".bashrc (1)"
    );
    assert_eq!(
        super::remote_free_name("logs.tar.gz", |n| n == "logs.tar.gz"),
        "logs.tar (1).gz"
    );
}

#[test]
fn drop_uploads_when_panel_open_and_is_ignored_when_closed() {
    let (mut w, dir) = World::uploading(
        vec![file("x.conf"), file("y.conf")],
        &["a.conf", "b.conf", "x.conf", "y.conf"],
    );
    let wid = w.wid;
    let drop = |w: &mut World, paths: &[&str]| {
        for p in paths {
            let _ = w.msg(FilePanelMsg::Dropped(wid, dir.join(p)));
        }
        let _ = w.msg(FilePanelMsg::DropsSettled(wid));
    };
    // Several files dropped together: one batch into the current folder.
    drop(&mut w, &["a.conf", "b.conf"]);
    assert_eq!(
        summary(&w.uploads()),
        [
            uploaded(&dir.join("a.conf"), "a.conf", UploadMode::New),
            uploaded(&dir.join("b.conf"), "b.conf", UploadMode::New),
        ]
    );
    // One batch: `Apply to all` covers them.
    drop(&mut w, &["x.conf", "y.conf"]);
    assert_eq!(w.clash(), Some(("x.conf".into(), true, None)));
    w.prompt(PromptMsg::ClashApplyAll(wid, true));
    w.prompt(PromptMsg::ClashReplace(wid));
    assert!(w.dialog().is_none());
    assert_eq!(w.uploads().len(), 4);

    // The panel closed: drops are ignored.
    w.toggle();
    drop(&mut w, &["a.conf"]);
    assert_eq!(w.uploads().len(), 4);
    // The panel of a local pane: nowhere to upload to.
    let local = w.local.clone();
    w.open_tab(&local);
    w.toggle();
    drop(&mut w, &["a.conf"]);
    assert_eq!(w.uploads().len(), 4);
    assert!(w.dialog().is_none());
}

#[test]
fn folders_are_skipped_with_note() {
    let (mut w, dir) = World::uploading(vec![], &["conf.d/", "app.conf"]);
    let wid = w.wid;
    w.pick(vec![dir.join("conf.d"), dir.join("app.conf")]);
    assert_eq!(
        summary(&w.uploads()),
        [uploaded(&dir.join("app.conf"), "app.conf", UploadMode::New)]
    );
    let skipped = &w.rows()[0];
    assert_eq!(skipped.note(), Some("Folders are not uploaded: conf.d"));
    assert!(!skipped.is_running());
    // Dropped folders too.
    let _ = w.msg(FilePanelMsg::Dropped(wid, dir.join("conf.d")));
    let _ = w.msg(FilePanelMsg::DropsSettled(wid));
    assert_eq!(w.uploads().len(), 1);
    assert_eq!(
        w.rows().last().unwrap().note(),
        Some("Folders are not uploaded: conf.d")
    );
    let texts = drawn_texts(&w, iced::Size::new(300.0, 700.0));
    assert!(
        texts
            .iter()
            .any(|t| t == "Folders are not uploaded: conf.d")
    );
    // A skipped row is dismissed.
    let id = w.rows()[0].id;
    let _ = w.msg(FilePanelMsg::Dismiss(id));
    assert!(w.rows().iter().all(|r| r.id != id));
}

#[test]
fn quit_with_running_transfer_asks() {
    let (mut w, _dir) = World::downloading(vec![file("app.log"), file("db.dump")]);
    let wid = w.wid;
    w.select("app.log");
    w.download();
    let _ = w.app.update(Message::Shortcut(wid, Action::Quit));
    assert_eq!(w.running_asked(), Some(1));
    assert_eq!(super::running_message(1), "1 transfer is still running.");
    assert_eq!(super::running_message(2), "2 transfers are still running.");
    assert!(!w.app.quitting);
    // `Keep open` keeps everything.
    w.prompt(PromptMsg::Cancel(wid));
    assert!(w.dialog().is_none());
    assert!(!w.app.quitting);
    assert!(w.fake.cancels.lock().is_empty());
    assert!(w.rows()[0].is_running());
    // The window's close button on the last window is a quit: asked too.
    let _ = w.app.update(Message::CloseRequested(wid));
    assert_eq!(w.running_asked(), Some(1));
    w.prompt(PromptMsg::Cancel(wid));

    w.select("db.dump");
    w.download();
    let _ = w.app.update(Message::Shortcut(wid, Action::Quit));
    // Asked once however often Quit is pressed.
    let _ = w.app.update(Message::Shortcut(wid, Action::Quit));
    assert_eq!(w.running_asked(), Some(2));
    w.prompt(PromptMsg::StopTransfers(wid));
    assert!(w.dialog().is_none());
    let ids: Vec<TransferId> = w.downloads().iter().map(|d| d.2).collect();
    assert_eq!(*w.fake.cancels.lock(), ids);
    // The app quits once the transfers have cleaned up after themselves.
    assert!(!w.app.quitting);
    w.transfer(ids[0], TransferEvent::Cancelled);
    assert!(!w.app.quitting);
    w.transfer(ids[1], TransferEvent::Cancelled);
    assert!(w.app.quitting);
}

#[test]
fn close_tab_with_running_transfer_asks() {
    let (mut w, _dir) = World::downloading(vec![file("app.log")]);
    let wid = w.wid;
    w.select("app.log");
    w.download();
    let local = w.local.clone();
    w.open_tab(&local);
    let tabs = |w: &World| w.app.windows[&w.wid].tabs.len();
    assert_eq!(tabs(&w), 2);

    // The tab's close button.
    let _ = w
        .app
        .update(Message::TabBar(wid, crate::app::TabBarMsg::Close(0)));
    assert_eq!(w.running_asked(), Some(1));
    assert_eq!(tabs(&w), 2);
    w.prompt(PromptMsg::Cancel(wid));
    assert_eq!(tabs(&w), 2, "Keep open");
    assert!(w.rows()[0].is_running());
    // Closing the local tab asks nothing: no transfer depends on it.
    let _ = w
        .app
        .update(Message::TabBar(wid, crate::app::TabBarMsg::Close(1)));
    assert!(w.dialog().is_none());
    assert_eq!(tabs(&w), 1);

    // Another tab of the same profile keeps the connection: closing one asks nothing.
    let db = w.db.clone();
    w.open_tab(&db);
    let _ = w
        .app
        .update(Message::TabBar(wid, crate::app::TabBarMsg::Close(1)));
    assert!(w.dialog().is_none());
    assert_eq!(tabs(&w), 1);

    // The close shortcut on the tab's last pane asks too; confirming cancels and closes.
    w.open_tab(&local);
    let _ = w
        .app
        .update(Message::TabBar(wid, crate::app::TabBarMsg::Select(0)));
    let _ = w.app.update(Message::Shortcut(wid, Action::CloseTab));
    assert_eq!(w.running_asked(), Some(1));
    w.prompt(PromptMsg::StopTransfers(wid));
    let id = w.downloads()[0].2;
    assert_eq!(*w.fake.cancels.lock(), [id]);
    assert_eq!(tabs(&w), 1);
    assert!(!w.app.quitting);

    // A window's close button with other windows open: asked for that window.
    let (mut w, _dir) = World::downloading(vec![file("app.log")]);
    let wid = w.wid;
    w.select("app.log");
    w.download();
    let other = window::Id::unique();
    w.app.windows.insert(other, WindowState::new());
    let _ = w.app.update(Message::CloseRequested(wid));
    assert_eq!(w.running_asked(), Some(1));
    assert!(!w.app.closing.contains(&wid));
    w.prompt(PromptMsg::StopTransfers(wid));
    assert!(w.app.closing.contains(&wid), "the window closes");
    assert!(!w.app.quitting);
}

#[test]
fn retry_after_connection_drop() {
    let (mut w, dir) = World::downloading(vec![file("app.log")]);
    let wid = w.wid;
    let db = w.db.clone();
    w.select("app.log");
    w.download();
    let first = w.downloads()[0].2;
    w.transfer(
        first,
        TransferEvent::Failed("The connection was lost.".into()),
    );
    assert_eq!(w.rows()[0].failed(), Some("The connection was lost."));
    let texts = drawn_texts(&w, iced::Size::new(300.0, 700.0));
    assert!(texts.iter().any(|t| t == "Retry"), "{texts:?}");
    // The connection is gone: Retry opens the session again, over this window, first.
    w.app.file_panels.by_profile.get_mut(&db.id).unwrap().sftp = Some(Arc::new(DeadRemote));
    let opens = w.opens(&db);
    let _ = w.msg(FilePanelMsg::RetryTransfer(wid, first));
    assert_eq!(w.opens(&db), opens + 1);
    assert_eq!(w.last_open(&db).unwrap().0, wid);
    assert_eq!(w.downloads().len(), 1, "not before the session is open");
    assert!(w.rows()[0].is_running(), "the row waits");
    assert_eq!(w.rows().len(), 1);
    w.answer_open(&db, "/home/user");
    assert_eq!(w.downloads().len(), 2);
    assert_eq!(w.downloads()[1].0, "/home/user/app.log");
    assert_eq!(w.downloads()[1].1, dir.join("app.log"));
    assert_eq!(w.rows().len(), 1, "the same row again");
    assert_eq!(w.rows()[0].id, w.downloads()[1].2);
    assert!(w.rows()[0].is_running());

    // An in-place replace that failed: Retry replaces again at once on the open session.
    let local = dir.join("app.conf");
    std::fs::write(&local, "x").unwrap();
    w.answer_list(&db, vec![file("app.log"), file("app.conf")]);
    w.pick(vec![local.clone()]);
    w.prompt(PromptMsg::ClashReplace(wid));
    let up = w.uploads()[0].id;
    w.transfer(up, TransferEvent::Failed("File may be incomplete.".into()));
    let opens = w.opens(&db);
    let _ = w.msg(FilePanelMsg::RetryTransfer(wid, up));
    assert_eq!(w.opens(&db), opens);
    assert_eq!(
        summary(&w.uploads())[1],
        uploaded(&local, "app.conf", UploadMode::Replace)
    );
    // A failed row is dismissed; a running one cannot be.
    let running = w.rows()[0].id;
    let _ = w.msg(FilePanelMsg::Dismiss(running));
    assert!(w.rows().iter().any(|r| r.id == running));
    let again = w.uploads()[1].id;
    w.transfer(again, TransferEvent::Failed("Disk full.".into()));
    let _ = w.msg(FilePanelMsg::Dismiss(again));
    assert!(w.rows().iter().all(|r| r.id != again));
}

#[test]
fn transfers_do_not_block_terminal_input() {
    let mut w = world();
    let db = w.db.clone();
    let pane = Pane::for_profile(&db);
    let id = pane.id;
    w.window().push(Tab::single(pane));
    let (handle, mut input) = SessionHandle::recording(w.app.new_terminal(DEFAULT_GRID));
    let _ = w
        .app
        .update(Message::Connected(id, Delivered::new(handle), DEFAULT_GRID));
    w.toggle();
    w.answer_open(&db, "/home/user");
    w.answer_list(&db, vec![file("app.conf")]);
    let temp = tempfile::tempdir().unwrap();
    let local = temp.path().join("big.tar");
    std::fs::write(&local, "x").unwrap();
    w.pick(vec![local]);
    let up = w.uploads()[0].id;
    assert!(w.rows()[0].is_running());
    // Typing while the upload runs reaches the shell, between its progress reports.
    let wid = w.wid;
    let type_in = |w: &mut World, s: &str| {
        let _ = w.app.update(Message::Terminal(
            wid,
            id,
            fbterm_term::TerminalOutput::Input(s.as_bytes().to_vec()),
        ));
    };
    type_in(&mut w, "ls\r");
    w.transfer(
        up,
        TransferEvent::Progress {
            done: 1024,
            total: Some(1 << 30),
        },
    );
    type_in(&mut w, "pwd\r");
    assert_eq!(input.inputs(), [b"ls\r".to_vec(), b"pwd\r".to_vec()]);
    assert!(w.rows()[0].is_running());
}

#[test]
fn same_name_uploading_is_skipped_with_note() {
    let (mut w, dir) = World::uploading(vec![file("b.conf")], &["a.conf", "b.conf", "other/"]);
    let wid = w.wid;
    std::fs::write(dir.join("other").join("A.CONF"), "x").unwrap();
    w.pick(vec![dir.join("a.conf")]);
    assert_eq!(w.uploads().len(), 1);
    // The same name while that upload runs (in any case): never a second upload, never
    // Replace offered, a note in the queue.
    w.pick(vec![dir.join("a.conf"), dir.join("other").join("A.CONF")]);
    assert!(w.dialog().is_none());
    assert_eq!(w.uploads().len(), 1);
    let notes: Vec<_> = w.rows().iter().filter_map(|r| r.note()).collect();
    assert_eq!(
        notes,
        ["Already uploading a.conf", "Already uploading A.CONF"]
    );
    // Not even with `Apply to all` Replace.
    w.pick(vec![dir.join("b.conf"), dir.join("a.conf")]);
    w.prompt(PromptMsg::ClashApplyAll(wid, true));
    w.prompt(PromptMsg::ClashReplace(wid));
    assert!(w.dialog().is_none());
    assert_eq!(
        summary(&w.uploads()),
        [
            uploaded(&dir.join("a.conf"), "a.conf", UploadMode::New),
            uploaded(&dir.join("b.conf"), "b.conf", UploadMode::Replace),
        ]
    );
    assert_eq!(
        w.rows().last().unwrap().note(),
        Some("Already uploading a.conf")
    );
    // Nor by renaming to it.
    w.finish_uploads(vec![file("b.conf")]);
    w.pick(vec![dir.join("a.conf")]);
    w.pick(vec![dir.join("b.conf")]);
    w.prompt(PromptMsg::ClashRename(wid));
    w.prompt(PromptMsg::ClashName(wid, "A.conf".into()));
    w.prompt(PromptMsg::Submit(wid));
    assert_eq!(
        w.rename_problem().as_deref(),
        Some("Already uploading A.conf")
    );
    assert_eq!(w.uploads().len(), 3);
}

#[test]
fn drops_wait_for_the_listing() {
    let mut w = world();
    let db = w.db.clone();
    w.open_tab(&db);
    w.toggle();
    w.answer_open(&db, "/home/user");
    // The folder is still being read.
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("app.conf"), "x").unwrap();
    let wid = w.wid;
    let _ = w.msg(FilePanelMsg::Dropped(wid, temp.path().join("app.conf")));
    let _ = w.msg(FilePanelMsg::DropsSettled(wid));
    assert!(w.uploads().is_empty(), "not before the listing is there");
    assert!(w.dialog().is_none());
    // It lists the name: asked, not uploaded blindly.
    w.answer_list(&db, vec![file("app.conf")]);
    assert_eq!(w.clash(), Some(("app.conf".into(), false, None)));
    w.prompt(PromptMsg::Cancel(wid));
    assert!(w.uploads().is_empty());
}

/// A session that answers until told it is gone.
struct FlakyRemote(std::sync::atomic::AtomicBool);

#[async_trait::async_trait]
impl Remote for FlakyRemote {
    async fn list(&self, _path: &str) -> Result<Vec<Entry>, SftpError> {
        Ok(Vec::new())
    }

    fn sftp(&self) -> Option<Arc<Sftp>> {
        None
    }

    fn is_alive(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[test]
fn a_transfer_finding_the_session_gone_lets_it_go() {
    let (mut w, _dir) = World::downloading(vec![file("app.log"), file("db.dump")]);
    let wid = w.wid;
    let db = w.db.clone();
    let flaky = Arc::new(FlakyRemote(true.into()));
    w.app.file_panels.by_profile.get_mut(&db.id).unwrap().sftp = Some(flaky.clone());
    w.select("app.log");
    w.download();
    let id = w.downloads()[0].2;
    // A file failing on a session that still answers keeps the session.
    w.select("db.dump");
    w.download();
    let other = w.downloads()[1].2;
    w.transfer(other, TransferEvent::Failed("Permission denied.".into()));
    assert!(w.panel().sftp.is_some());
    // The session itself went away (its channel closed, the connection may be fine).
    flaky.0.store(false, std::sync::atomic::Ordering::Relaxed);
    w.transfer(
        id,
        TransferEvent::Failed("The SFTP session has ended.".into()),
    );
    assert!(w.panel().sftp.is_none());
    assert_eq!(
        w.shows().as_deref(),
        Some("error: The SFTP session has ended.")
    );
    // Retry opens a new one.
    let opens = w.opens(&db);
    let _ = w.msg(FilePanelMsg::RetryTransfer(wid, id));
    assert_eq!(w.opens(&db), opens + 1);
}

#[test]
fn close_question_stays_current_and_quit_falls_back() {
    let (mut w, _dir) = World::downloading(vec![file("app.log"), file("db.dump")]);
    let wid = w.wid;
    assert!(!w.app.quit_key_free, "no transfer: the menu keeps Cmd+Q");
    w.select("app.log");
    w.download();
    w.select("db.dump");
    w.download();
    let ids: Vec<TransferId> = w.downloads().iter().map(|d| d.2).collect();
    // While transfers run, Cmd+Q is taken from the app menu (so it asks first).
    assert!(w.app.quit_key_free);
    let _ = w.app.update(Message::Shortcut(wid, Action::Quit));
    assert_eq!(w.running_asked(), Some(2));
    let done = || TransferEvent::Finished {
        note: None,
        local: None,
    };
    w.transfer(ids[0], done());
    assert_eq!(w.running_asked(), Some(1), "the count follows");
    w.transfer(ids[1], done());
    assert!(w.dialog().is_none(), "nothing runs: nothing to ask about");
    assert!(!w.app.quitting, "nothing closes by itself");
    // Nothing runs: the menu has Cmd+Q again.
    assert!(!w.app.quit_key_free);

    // A cancelled transfer that never reports: the quit goes ahead after the wait.
    w.select("app.log");
    w.download();
    let _ = w.app.update(Message::Shortcut(wid, Action::Quit));
    w.prompt(PromptMsg::StopTransfers(wid));
    assert!(!w.app.quitting);
    let _ = w.msg(FilePanelMsg::QuitWaited);
    assert!(w.app.quitting);
}
