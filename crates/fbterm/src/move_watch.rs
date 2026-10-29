//! Whether the desktop carries out fbterm's window move and resize requests (Linux, drawn
//! chrome). Some Wayland compositors ignore them, which would leave a window without a title
//! bar that can be moved; fbterm then falls back to the system title bar for the session.
//!
//! A request is refused when, within [`ANSWER_TIME`] of it, the window neither moved nor
//! resized while the pointer travelled more than [`TRAVEL`] over the window with the button
//! still held, and the pointer never left the window before that travel (on Wayland: never
//! left it while the button was held).
//!
//! A desktop that carries a request out takes the pointer for it: the pointer leaves the
//! window when the move starts and enters it again when it ends, and the button release goes
//! to the desktop, so fbterm never sees it. A leave while the button is held and before any
//! travel is therefore an answer, and after any leave or enter travel no longer counts (the
//! button may have been let go unseen). A desktop that refuses never takes the pointer, so its
//! travel is seen before the pointer can leave. A press let go in place, or one that stays
//! still, is no evidence either way.
//!
//! On Wayland the window keeps the pointer while the button is held, so a leave before the
//! release is always the compositor's grab and answers the request, travel or not: a fast
//! flick can travel before the request reaches the compositor. On X11 a leave after travel
//! may be the pointer simply moving off a window the desktop would not move.
//!
//! The decision is a pure state machine over the events the app feeds it and the time it is
//! given; the app checks it once, from a one-shot timer started with the request.

use std::time::{Duration, Instant};

use fbterm_config::WindowDecorations;
use iced::advanced::mouse;
use iced::{Event, Point, Task, window};

use crate::app::{App, Message};
use crate::chrome::{self, BarAction, Chrome};

/// How long the desktop has to start moving or resizing the window.
pub const ANSWER_TIME: Duration = Duration::from_millis(500);

/// How far the pointer must travel over the window, button held, to count as an attempt.
pub const TRAVEL: f32 = 6.0;

/// What the app saw on a window while a request was watched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Seen {
    /// The pointer is at this point of the window.
    Cursor(Point),
    /// The left button was let go.
    Released,
    /// The pointer left the window (the desktop took it, or it moved off).
    Left,
    /// The pointer entered the window (the desktop gave it back).
    Entered,
    WindowMoved,
    WindowResized,
}

/// The outcome of a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The desktop ignored the request.
    Refused,
    /// The window moved or resized, or there is no evidence of a refusal.
    Answered,
    /// The answer time has not passed yet; still watching.
    TooEarly,
    /// Not the request being watched (a newer one replaced it, or it was checked already).
    Stale,
}

/// The move or resize request being watched, if any.
#[derive(Debug, Default)]
pub struct MoveWatch {
    pending: Option<Pending>,
    /// The token of the last request.
    last: u64,
    /// The session is Wayland's (else X11's): see [`seen`](Self::seen).
    wayland: bool,
}

#[derive(Debug)]
struct Pending {
    token: u64,
    window: window::Id,
    started: Instant,
    /// Where the pointer first was after the request.
    origin: Option<Point>,
    held: bool,
    /// The pointer left or entered the window since the request: travel no longer counts.
    crossed: bool,
    travelled: bool,
    answered: bool,
}

impl MoveWatch {
    /// A watch for a Wayland session (`wayland`) or an X11 one.
    pub fn for_session(wayland: bool) -> Self {
        Self {
            wayland,
            ..Self::default()
        }
    }

    /// A move or resize of `window` was asked for at `now`, with the button held. Returns the
    /// token to [`check`](Self::check) with once [`ANSWER_TIME`] has passed. A request still
    /// watched is replaced.
    pub fn start(&mut self, window: window::Id, now: Instant) -> u64 {
        self.last += 1;
        self.pending = Some(Pending {
            token: self.last,
            window,
            started: now,
            origin: None,
            held: true,
            crossed: false,
            travelled: false,
            answered: false,
        });
        self.last
    }

    /// Whether a request is being watched (the app listens to pointer and window events only
    /// then).
    pub fn watching(&self) -> bool {
        self.pending.is_some()
    }

    /// Something happened on `window`.
    pub fn seen(&mut self, window: window::Id, what: Seen) {
        let Some(p) = self.pending.as_mut().filter(|p| p.window == window) else {
            return;
        };
        match what {
            Seen::Cursor(at) if p.held && !p.crossed => match p.origin {
                None => p.origin = Some(at),
                Some(o) => {
                    let (dx, dy) = (at.x - o.x, at.y - o.y);
                    if dx.hypot(dy) > TRAVEL {
                        p.travelled = true;
                    }
                }
            },
            Seen::Cursor(_) => {}
            Seen::Released => p.held = false,
            Seen::Left => {
                // The desktop took the pointer: it is carrying the request out. On Wayland
                // the window keeps the pointer while the button is held, so any leave then is
                // the compositor's grab, even after travel seen before the request reached it.
                if p.held && (self.wayland || !p.travelled) {
                    p.answered = true;
                }
                p.crossed = true;
            }
            Seen::Entered => p.crossed = true,
            Seen::WindowMoved | Seen::WindowResized => p.answered = true,
        }
    }

    /// Decide the request `token` at `now`. Stops watching it unless it is too early.
    pub fn check(&mut self, token: u64, now: Instant) -> Verdict {
        let Some(p) = self.pending.as_ref().filter(|p| p.token == token) else {
            return Verdict::Stale;
        };
        if now.duration_since(p.started) < ANSWER_TIME {
            return Verdict::TooEarly;
        }
        let refused = p.travelled && !p.answered;
        self.pending = None;
        if refused {
            Verdict::Refused
        } else {
            Verdict::Answered
        }
    }
}

/// The events a watched request needs, for `iced::event::listen_with` (subscribed only while
/// one is watched).
pub(crate) fn watch_event(
    event: Event,
    _status: iced::event::Status,
    id: window::Id,
) -> Option<Message> {
    let seen = match event {
        Event::Mouse(mouse::Event::CursorMoved { position }) => Seen::Cursor(position),
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => Seen::Released,
        Event::Mouse(mouse::Event::CursorLeft) => Seen::Left,
        Event::Mouse(mouse::Event::CursorEntered) => Seen::Entered,
        Event::Window(window::Event::Moved(_)) => Seen::WindowMoved,
        Event::Window(window::Event::Resized(_)) => Seen::WindowResized,
        _ => return None,
    };
    Some(Message::MoveSeen(id, seen))
}

/// The notice shown once when the desktop refuses drawn chrome.
pub const FALLBACK_NOTICE: &str = "This desktop doesn't let fbterm move its window, so the \
                                   system title bar is used. Change it in Settings.";

impl App {
    /// After asking the desktop to move or resize `window` for a bar `action`: where drawn
    /// chrome may be refused (Linux), watch the request and check it once after
    /// [`ANSWER_TIME`].
    pub(crate) fn watch_move(&mut self, window: window::Id, action: BarAction) -> Task<Message> {
        let request = matches!(action, BarAction::Drag | BarAction::ResizeFrom(_));
        if !request || !self.watch_moves || self.chrome.get() != Chrome::Drawn {
            return Task::none();
        }
        let token = self.move_watch.start(window, Instant::now());
        Task::perform(async { tokio::time::sleep(ANSWER_TIME).await }, move |()| {
            Message::MoveCheck(token)
        })
    }

    /// The answer time of request `token` passed: fall back to the system title bar if the
    /// desktop refused it.
    pub(crate) fn check_move(&mut self, token: u64) -> Task<Message> {
        match self.move_watch.check(token, Instant::now()) {
            Verdict::Refused => self.fall_back_to_system_chrome(),
            // The timer never fires early, but a check must not be lost if it did.
            Verdict::TooEarly => Task::perform(
                async { tokio::time::sleep(ANSWER_TIME / 10).await },
                move |()| Message::MoveCheck(token),
            ),
            Verdict::Answered | Verdict::Stale => Task::none(),
        }
    }

    /// The desktop refused a move or resize: the system title bar for the rest of the session,
    /// on every window, saying so once. The setting stays as it is.
    pub(crate) fn fall_back_to_system_chrome(&mut self) -> Task<Message> {
        tracing::info!("the desktop refused a window move or resize; using its title bar");
        self.chrome_fell_back = true;
        if !self.chrome_notice_shown {
            self.chrome_notice_shown = true;
            self.notices.push(FALLBACK_NOTICE.to_owned());
            self.notices_dismissed = false;
        }
        match chrome::fall_back_to_system(self.chrome, self.open_windows.iter().copied()) {
            Some(toggle) => {
                // Only drawn chrome tracks maximized windows.
                self.maximized.clear();
                toggle
            }
            None => Task::none(),
        }
    }

    /// Put the chrome the `window_decorations` setting and the session's fallback ask for in
    /// use, switching every open window to it (see [`chrome::use_chrome`]).
    pub(crate) fn reapply_chrome(&mut self) -> Task<Message> {
        let (look, initial) = self.chrome_base;
        let to = chrome::chrome_for_setting(
            look,
            initial,
            self.settings.window_decorations,
            self.chrome_fell_back,
        );
        let Some(toggle) = chrome::use_chrome(self.chrome, to, self.open_windows.iter().copied())
        else {
            return Task::none();
        };
        // Only drawn chrome tracks maximized windows; ask again when it comes back.
        self.maximized.clear();
        if to != Chrome::Drawn {
            return toggle;
        }
        let ask = self
            .open_windows
            .iter()
            .map(|&id| window::is_maximized(id).map(move |m| Message::WindowMaximized(id, m)));
        Task::batch(std::iter::once(toggle).chain(ask))
    }

    /// `window_decorations` changed to `setting`. Choosing `"fbterm"` tries drawn chrome
    /// again even after the desktop refused it.
    pub(crate) fn window_decorations_changed(
        &mut self,
        setting: WindowDecorations,
    ) -> Task<Message> {
        self.settings.window_decorations = setting;
        if setting == WindowDecorations::Fbterm {
            self.chrome_fell_back = false;
        }
        self.reapply_chrome()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::{Chrome, ChromeCell, fall_back_to_system};

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn refused_move_falls_back_to_system_chrome() {
        let t0 = Instant::now();
        let w = window::Id::unique();
        let cell = ChromeCell::starting(Chrome::Drawn);
        let mut watch = MoveWatch::default();
        // A fake desktop that refuses the move: the button is held, the pointer travels over
        // the window, and the window never moves.
        let token = watch.start(w, t0);
        assert!(watch.watching());
        watch.seen(w, Seen::Cursor(Point::new(100.0, 10.0)));
        watch.seen(w, Seen::Cursor(Point::new(112.0, 12.0)));
        watch.seen(w, Seen::Cursor(Point::new(130.0, 14.0)));
        // The timer is one-shot; a check before the answer time decides nothing.
        assert_eq!(watch.check(token, t0 + ms(200)), Verdict::TooEarly);
        assert!(watch.watching());
        assert_eq!(watch.check(token, t0 + ANSWER_TIME), Verdict::Refused);
        assert!(!watch.watching());
        // The app then switches this session to the system title bar on every open window.
        let other = window::Id::unique();
        let task = fall_back_to_system::<()>(&cell, [w, other]).expect("was drawn");
        assert_eq!(task.units(), 2);
        assert_eq!(cell.get(), Chrome::Native);
    }

    #[test]
    fn a_window_that_moves_or_resizes_answered() {
        let t0 = Instant::now();
        let w = window::Id::unique();
        let mut watch = MoveWatch::default();
        for answer in [Seen::WindowMoved, Seen::WindowResized] {
            let token = watch.start(w, t0);
            watch.seen(w, Seen::Cursor(Point::new(100.0, 10.0)));
            watch.seen(w, Seen::Cursor(Point::new(140.0, 10.0)));
            watch.seen(w, answer);
            assert_eq!(watch.check(token, t0 + ANSWER_TIME), Verdict::Answered);
        }
    }

    #[test]
    fn a_press_without_travel_is_no_refusal() {
        let t0 = Instant::now();
        let w = window::Id::unique();
        let mut watch = MoveWatch::default();
        // Pressed and let go in place, or a jitter of a few pixels: no evidence either way.
        let token = watch.start(w, t0);
        watch.seen(w, Seen::Cursor(Point::new(100.0, 10.0)));
        watch.seen(w, Seen::Cursor(Point::new(103.0, 12.0)));
        watch.seen(w, Seen::Released);
        assert_eq!(watch.check(token, t0 + ANSWER_TIME), Verdict::Answered);
        // No pointer event at all (the desktop holds the pointer while it moves the window).
        let token = watch.start(w, t0);
        assert_eq!(watch.check(token, t0 + ANSWER_TIME), Verdict::Answered);
    }

    #[test]
    fn travel_after_release_or_over_another_window_does_not_count() {
        let t0 = Instant::now();
        let (w, other) = (window::Id::unique(), window::Id::unique());
        let mut watch = MoveWatch::default();
        let token = watch.start(w, t0);
        watch.seen(w, Seen::Cursor(Point::new(100.0, 10.0)));
        watch.seen(w, Seen::Released);
        watch.seen(w, Seen::Cursor(Point::new(300.0, 10.0)));
        watch.seen(other, Seen::Cursor(Point::new(0.0, 0.0)));
        watch.seen(other, Seen::Cursor(Point::new(300.0, 300.0)));
        assert_eq!(watch.check(token, t0 + ANSWER_TIME), Verdict::Answered);
    }

    #[test]
    fn a_newer_request_makes_the_older_timer_stale() {
        let t0 = Instant::now();
        let w = window::Id::unique();
        let mut watch = MoveWatch::default();
        let first = watch.start(w, t0);
        let second = watch.start(w, t0 + ms(100));
        assert_eq!(watch.check(first, t0 + ANSWER_TIME), Verdict::Stale);
        watch.seen(w, Seen::Cursor(Point::new(10.0, 10.0)));
        watch.seen(w, Seen::Cursor(Point::new(60.0, 10.0)));
        assert_eq!(
            watch.check(second, t0 + ms(100) + ANSWER_TIME),
            Verdict::Refused
        );
        // Checked once only.
        assert_eq!(
            watch.check(second, t0 + ms(100) + ANSWER_TIME),
            Verdict::Stale
        );
    }

    /// An app on a temporary config folder (removed when the returned guard drops).
    fn app() -> (App, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let paths = fbterm_config::Paths {
            config_dir: dir.path().to_path_buf(),
        };
        let (mut app, _) = App::build(paths, true);
        // A Linux run with drawn chrome, whatever system the test runs on.
        app.chrome = Box::leak(Box::new(ChromeCell::starting(Chrome::Drawn)));
        app.chrome_base = (chrome::Platform::Linux, Chrome::Drawn);
        app.watch_moves = true;
        (app, dir)
    }

    #[test]
    fn a_granted_move_never_counts_as_refused() {
        let t0 = Instant::now();
        let w = window::Id::unique();
        let mut watch = MoveWatch::default();
        // The desktop takes the pointer for the move (it leaves the window) and gives it back
        // when the move ends; the release went to the desktop. Then the pointer travels.
        let token = watch.start(w, t0);
        watch.seen(w, Seen::Cursor(Point::new(100.0, 10.0)));
        watch.seen(w, Seen::Left);
        watch.seen(w, Seen::Entered);
        watch.seen(w, Seen::Cursor(Point::new(100.0, 10.0)));
        watch.seen(w, Seen::Cursor(Point::new(140.0, 10.0)));
        assert_eq!(watch.check(token, t0 + ANSWER_TIME), Verdict::Answered);

        // A click on the bar: the release is unseen, the pointer comes back and moves on
        // into the terminal.
        let token = watch.start(w, t0);
        watch.seen(w, Seen::Entered);
        watch.seen(w, Seen::Cursor(Point::new(100.0, 10.0)));
        watch.seen(w, Seen::Cursor(Point::new(100.0, 200.0)));
        assert_eq!(watch.check(token, t0 + ANSWER_TIME), Verdict::Answered);
    }

    #[test]
    fn x11_click_without_move_and_edge_click_are_answered() {
        let t0 = Instant::now();
        let w = window::Id::unique();
        let mut watch = MoveWatch::default();
        // X11: the window manager grabs the pointer on the request (leave), the click ends
        // without moving the window (no Moved), the pointer comes back and travels.
        let token = watch.start(w, t0);
        watch.seen(w, Seen::Left);
        watch.seen(w, Seen::Entered);
        watch.seen(w, Seen::Cursor(Point::new(300.0, 20.0)));
        watch.seen(w, Seen::Cursor(Point::new(300.0, 80.0)));
        assert_eq!(watch.check(token, t0 + ANSWER_TIME), Verdict::Answered);
        // A click on a resize edge: the same, with no Resized.
        let token = watch.start(w, t0);
        watch.seen(w, Seen::Cursor(Point::new(799.0, 300.0)));
        watch.seen(w, Seen::Left);
        watch.seen(w, Seen::Entered);
        watch.seen(w, Seen::Cursor(Point::new(760.0, 300.0)));
        watch.seen(w, Seen::Cursor(Point::new(700.0, 320.0)));
        assert_eq!(watch.check(token, t0 + ANSWER_TIME), Verdict::Answered);
        // A refusing desktop never takes the pointer: travel first, then the pointer may
        // leave the window it could not move. Still refused.
        let token = watch.start(w, t0);
        watch.seen(w, Seen::Cursor(Point::new(100.0, 10.0)));
        watch.seen(w, Seen::Cursor(Point::new(140.0, 10.0)));
        watch.seen(w, Seen::Left);
        assert_eq!(watch.check(token, t0 + ANSWER_TIME), Verdict::Refused);
    }

    #[test]
    fn a_wayland_leave_while_held_is_the_compositors_grab() {
        let t0 = Instant::now();
        let w = window::Id::unique();
        // Wayland: while the button is held the pointer stays on the window (implicit grab),
        // so a leave mid-press can only be the compositor taking it for the move, even after
        // a fast flick travelled before the request reached the compositor.
        let mut watch = MoveWatch::for_session(true);
        let token = watch.start(w, t0);
        watch.seen(w, Seen::Cursor(Point::new(100.0, 10.0)));
        watch.seen(w, Seen::Cursor(Point::new(140.0, 10.0)));
        watch.seen(w, Seen::Left);
        assert_eq!(watch.check(token, t0 + ANSWER_TIME), Verdict::Answered);
        // A leave after the release proves nothing: travel while held stays a refusal.
        let token = watch.start(w, t0);
        watch.seen(w, Seen::Cursor(Point::new(100.0, 10.0)));
        watch.seen(w, Seen::Cursor(Point::new(140.0, 10.0)));
        watch.seen(w, Seen::Released);
        watch.seen(w, Seen::Left);
        assert_eq!(watch.check(token, t0 + ANSWER_TIME), Verdict::Refused);
        // Travel with no leave at all: refused, as on X11.
        let token = watch.start(w, t0);
        watch.seen(w, Seen::Cursor(Point::new(100.0, 10.0)));
        watch.seen(w, Seen::Cursor(Point::new(140.0, 10.0)));
        assert_eq!(watch.check(token, t0 + ANSWER_TIME), Verdict::Refused);
        // X11 keeps its rule: a leave after travel follows a refusal.
        let mut watch = MoveWatch::for_session(false);
        let token = watch.start(w, t0);
        watch.seen(w, Seen::Cursor(Point::new(100.0, 10.0)));
        watch.seen(w, Seen::Cursor(Point::new(140.0, 10.0)));
        watch.seen(w, Seen::Left);
        assert_eq!(watch.check(token, t0 + ANSWER_TIME), Verdict::Refused);
    }

    #[test]
    fn the_app_falls_back_once_and_the_setting_switches_back() {
        use crate::settings_window::SettingsMsg;
        let (mut app, _dir) = app();
        let (w, other) = (window::Id::unique(), window::Id::unique());
        let _ = app.update(Message::AnyWindowOpened(w));
        let _ = app.update(Message::AnyWindowOpened(other));
        app.maximized.insert(w);
        // Windows does not watch: its desktop always moves the window.
        app.watch_moves = false;
        let _ = app.update(Message::TitleBar(w, BarAction::Drag));
        assert!(!app.move_watch.watching());
        app.watch_moves = true;
        // A press on a control is not a move request.
        let _ = app.update(Message::TitleBar(w, BarAction::Minimize));
        assert!(!app.move_watch.watching());
        let _ = app.update(Message::TitleBar(w, BarAction::Drag));
        assert!(app.move_watch.watching());
        let _ = app.update(Message::MoveSeen(w, Seen::Cursor(Point::new(50.0, 10.0))));
        let _ = app.update(Message::MoveSeen(w, Seen::Cursor(Point::new(90.0, 12.0))));
        std::thread::sleep(ANSWER_TIME + Duration::from_millis(20));
        let _ = app.update(Message::MoveCheck(1));
        assert_eq!(app.chrome.get(), Chrome::Native);
        assert!(app.maximized.is_empty());
        assert_eq!(
            app.notices.iter().filter(|n| *n == FALLBACK_NOTICE).count(),
            1
        );
        assert!(app.visible_notices().iter().any(|n| n == FALLBACK_NOTICE));
        // The setting stays "fbterm"; the fallback is for the session.
        assert_eq!(app.settings.window_decorations, WindowDecorations::Fbterm);
        // With the system title bar nothing is watched any more.
        let _ = app.update(Message::TitleBar(w, BarAction::Drag));
        assert!(!app.move_watch.watching());

        // From the Settings window: "System", then "fbterm" tries drawn chrome again.
        let _ = app.update(Message::Settings(SettingsMsg::Open));
        let _ = app.update(Message::Settings(SettingsMsg::TitleBar(
            WindowDecorations::System,
        )));
        assert_eq!(app.settings.window_decorations, WindowDecorations::System);
        assert_eq!(app.chrome.get(), Chrome::Native);
        let _ = app.update(Message::Settings(SettingsMsg::TitleBar(
            WindowDecorations::Fbterm,
        )));
        assert_eq!(app.chrome.get(), Chrome::Drawn);
        assert!(!app.chrome_fell_back);
        let _ = app.update(Message::Settings(SettingsMsg::TitleBar(
            WindowDecorations::System,
        )));
        assert_eq!(app.chrome.get(), Chrome::Native);
        // A second refusal never repeats the notice.
        let _ = app.update(Message::Settings(SettingsMsg::TitleBar(
            WindowDecorations::Fbterm,
        )));
        let _ = app.fall_back_to_system_chrome();
        assert_eq!(
            app.notices.iter().filter(|n| *n == FALLBACK_NOTICE).count(),
            1
        );
        assert_eq!(app.chrome.get(), Chrome::Native);
        // The Title bar row shows what is on screen, so one click on fbterm tries again.
        use crate::settings_window::view::title_bar_shown;
        assert_eq!(
            title_bar_shown(app.settings.window_decorations, app.chrome_fell_back),
            WindowDecorations::System
        );
        let _ = app.update(Message::Settings(SettingsMsg::TitleBar(
            WindowDecorations::Fbterm,
        )));
        assert_eq!(app.chrome.get(), Chrome::Drawn);
        assert!(!app.chrome_fell_back);
        assert_eq!(
            title_bar_shown(app.settings.window_decorations, app.chrome_fell_back),
            WindowDecorations::Fbterm
        );
    }

    #[test]
    fn a_unified_window_is_held_still_only_while_the_pointer_is_over_a_tab() {
        let (mut app, _dir) = app();
        app.chrome = Box::leak(Box::new(ChromeCell::starting(Chrome::Unified)));
        let w = window::Id::unique();
        // Opening a window leaves it movable (the one task reads the window's scale).
        assert_eq!(app.update(Message::AnyWindowOpened(w)).units(), 1);
        let over = |app: &mut App, over| {
            app.update(Message::TitleBar(w, BarAction::PointerOverWidget(over)))
                .units()
        };
        // On macOS each change is one call to AppKit; nothing when nothing changes.
        let call = usize::from(cfg!(target_os = "macos"));
        assert_eq!(over(&mut app, false), 0);
        assert_eq!(over(&mut app, true), call);
        assert!(app.held_still.contains(&w));
        assert_eq!(over(&mut app, true), 0);
        // Off the tab: movable again.
        assert_eq!(over(&mut app, false), call);
        assert!(app.held_still.is_empty());
        // Losing focus over a tab: movable again.
        assert_eq!(over(&mut app, true), call);
        assert_eq!(app.update(Message::WindowUnfocused(w)).units(), call);
        assert!(app.held_still.is_empty());
        assert_eq!(app.update(Message::WindowUnfocused(w)).units(), 0);
    }

    #[test]
    fn other_chrome_never_holds_a_window_still() {
        let (mut app, _dir) = app();
        let w = window::Id::unique();
        for chrome in [Chrome::Drawn, Chrome::Native] {
            app.chrome = Box::leak(Box::new(ChromeCell::starting(chrome)));
            for over in [true, false, true] {
                let task = app.update(Message::TitleBar(w, BarAction::PointerOverWidget(over)));
                assert_eq!(task.units(), 0);
                assert!(app.held_still.is_empty());
            }
            assert_eq!(app.update(Message::WindowUnfocused(w)).units(), 0);
        }
    }

    #[test]
    fn a_window_opened_across_a_switch_gets_the_current_decorations() {
        let (mut app, _dir) = app();
        let (late, fine) = (window::Id::unique(), window::Id::unique());
        // Opened while drawn; the switch to the system title bar came before it was shown.
        chrome::record_created(late, Chrome::Drawn);
        chrome::record_created(fine, Chrome::Native);
        let _ = app.fall_back_to_system_chrome();
        assert_eq!(app.chrome.get(), Chrome::Native);
        // Every opened window reads its scale (one task); `late` also gets its decorations.
        assert_eq!(app.update(Message::AnyWindowOpened(late)).units(), 2);
        assert_eq!(app.update(Message::AnyWindowOpened(fine)).units(), 1);
        assert!(app.open_windows.contains(&late) && app.open_windows.contains(&fine));
        // Recorded once: a second report changes nothing but the scale read.
        assert_eq!(app.update(Message::AnyWindowOpened(late)).units(), 1);
    }
}
