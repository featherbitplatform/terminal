//! Window chrome: how a window's top edge is drawn on each platform.
//!
//! macOS uses a unified title bar: the OS title strip is transparent and hidden, the content
//! runs under it, and the tab bar (or the editor's header) takes its place with the native
//! window buttons drawn over its leading end. Empty space in that bar moves the window and a
//! double click zooms it.
//!
//! Windows and Linux draw their own: the window has no OS decorations, the tab bar (or the
//! editor's header) is the title bar and holds drawn minimize, maximize and close controls
//! ([`Chrome::Drawn`]). Windows puts them on the right; Linux follows the desktop's button
//! layout ([`controls_layout`]). Empty bar space moves the window, a double click maximizes
//! it, and an [`EDGE_BAND`] wide band along the window's edges resizes it ([`edges`]).
//!
//! [`Chrome::Native`] (the OS title bar) is Linux's other choice: the `window_decorations`
//! setting picks it, and so does a desktop that refuses fbterm's move and resize requests.
//! The chrome can change while fbterm runs ([`ChromeCell`]); open windows then switch their
//! OS decorations.

use fbterm_config::WindowDecorations;
use iced::advanced::layout::{self, Layout};
use iced::advanced::mouse::{self, click};
use iced::advanced::widget::{Operation, Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell, overlay, renderer};
use iced::window::Direction;
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector, window};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

/// Whether the macOS app menu's Quit keeps `Cmd+Q` with `running` transfers: only while none
/// runs. While some do, the key is taken from the menu so it reaches the app's own Quit, which
/// asks first; the menu would end the app at once (and works with every window minimised).
pub(crate) fn quit_key_wanted(running: usize) -> bool {
    running == 0
}

/// Whether a menu item with `action` is the app menu's Quit.
#[cfg(any(test, target_os = "macos"))]
pub(crate) fn is_quit_item(action: Option<&str>) -> bool {
    action == Some("terminate:")
}

/// Give the macOS app menu's Quit its `Cmd+Q` (`menu_has_it`) or take it away, so the key
/// goes through the app's own Quit. The item stays; choosing it with the mouse still ends the
/// app at once. Returns whether the item was found and set (the menu exists once the app has
/// launched). Elsewhere, and in tests, there is no menu: it only reports success.
#[cfg(all(target_os = "macos", not(test)))]
pub(crate) fn set_quit_key(menu_has_it: bool) -> bool {
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
    let key = NSString::from_str(if menu_has_it { "q" } else { "" });
    let mut set = false;
    for top in menu.itemArray().iter() {
        let Some(submenu) = top.submenu() else {
            continue;
        };
        for item in submenu.itemArray().iter() {
            let action = item
                .action()
                .map(|sel| sel.name().to_string_lossy().into_owned());
            if is_quit_item(action.as_deref()) {
                // The modifier mask (Cmd) stays as the menu built it.
                item.setKeyEquivalent(&key);
                set = true;
            }
        }
    }
    set
}

#[cfg(any(not(target_os = "macos"), test))]
pub(crate) fn set_quit_key(_menu_has_it: bool) -> bool {
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

/// Height of the drawn title bar (Windows, Linux).
const DRAWN_BAR_HEIGHT: f32 = 36.0;

/// How wide the band along a window's edges is that resizes it where the chrome is drawn.
pub const EDGE_BAND: f32 = 8.0;

/// How tall the band along the top edge is: the gap above the tabs, so a press on a tab never
/// resizes the window.
pub const NORTH_BAND: f32 = 4.0;

/// Within the drawn controls only this outermost corner square still resizes the window.
const CONTROLS_CORNER: f32 = 2.0;

/// Width of a drawn control in the Windows style (the system's caption button width).
pub const WINDOWS_CONTROL_WIDTH: f32 = 46.0;
/// Diameter of a drawn control in the Linux style (round, as the desktops draw them).
pub const LINUX_CONTROL_SIZE: f32 = 24.0;
/// Space between Linux-style controls, and around the group.
pub const LINUX_CONTROL_GAP: f32 = 8.0;

/// How far a corner's resize area reaches along each edge from the corner, so a corner is
/// easier to hit than the band's own 8 by 8 px square.
const CORNER_REACH: f32 = 16.0;

/// The system whose window conventions apply: where the controls go and how they look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    /// Linux and the other Unix desktops.
    Linux,
}

impl Platform {
    /// The system this build runs on.
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// How a platform draws the top edge of a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chrome {
    /// The tab bar is the title bar; the OS draws only its window buttons over it (macOS).
    Unified,
    /// The OS draws a title bar with its decorations above the content.
    Native,
    /// No OS decorations: the tab bar is the title bar and fbterm draws the window controls
    /// in it, and resizes the window from its edges (Windows, Linux).
    Drawn,
}

/// A drawn window control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Minimize,
    /// Maximize, or restore a maximized window.
    Maximize,
    Close,
}

/// Which end of the bar the drawn controls sit at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// Where the drawn controls go and in which order (left to right).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlsLayout {
    pub side: Side,
    pub order: Vec<Control>,
}

impl ControlsLayout {
    /// Minimize, maximize, close on the right: Windows, and Linux when the desktop's layout
    /// cannot be read.
    fn standard() -> Self {
        Self {
            side: Side::Right,
            order: vec![Control::Minimize, Control::Maximize, Control::Close],
        }
    }
}

/// The controls of `platform`. On Linux `desktop` is the desktop's button layout in GNOME's
/// `button-layout` form (`"close,minimize,maximize:"`: names before the colon go left, after
/// it right; KDE's setting is turned into it by `kde_layout`); anything unreadable gives the
/// standard layout. Elsewhere `desktop` is ignored.
pub fn controls_layout(platform: Platform, desktop: Option<&str>) -> ControlsLayout {
    match platform {
        Platform::Linux => desktop
            .and_then(parse_button_layout)
            .unwrap_or_else(ControlsLayout::standard),
        Platform::Windows | Platform::MacOs => ControlsLayout::standard(),
    }
}

/// A GNOME `button-layout` value as [`ControlsLayout`]. The controls sit together at one end:
/// the side that names close (else the side naming more of them, the right on a tie), in the
/// order the value names them. Close is always there (at the outer end if the value leaves it
/// out), so a window can always be closed. `None` when the value names no control.
fn parse_button_layout(value: &str) -> Option<ControlsLayout> {
    let value = value.trim().trim_matches(|c| c == '\'' || c == '"').trim();
    let (left, right) = value.split_once(':')?;
    let names = |part: &str| -> Vec<Control> {
        part.split(',')
            .filter_map(|name| match name.trim() {
                "minimize" => Some(Control::Minimize),
                "maximize" => Some(Control::Maximize),
                "close" => Some(Control::Close),
                _ => None,
            })
            .collect()
    };
    let (left, right) = (names(left), names(right));
    if left.is_empty() && right.is_empty() {
        return None;
    }
    let side = if left.contains(&Control::Close) {
        Side::Left
    } else if right.contains(&Control::Close) || right.len() >= left.len() {
        Side::Right
    } else {
        Side::Left
    };
    let mut order: Vec<Control> = Vec::new();
    for c in left.into_iter().chain(right) {
        if !order.contains(&c) {
            order.push(c);
        }
    }
    if !order.contains(&Control::Close) {
        match side {
            Side::Left => order.insert(0, Control::Close),
            Side::Right => order.push(Control::Close),
        }
    }
    Some(ControlsLayout { side, order })
}

/// KDE's button setting (`ButtonsOnLeft` / `ButtonsOnRight` in the `[org.kde.kdecoration2]`
/// group of `kwinrc`, one letter per button: `I` minimize, `A` maximize, `X` close) in GNOME's
/// `button-layout` form. A key left out has KDE's default. `None` when the group sets neither.
#[cfg(any(test, target_os = "linux"))]
fn kde_layout(kwinrc: &str) -> Option<String> {
    let mut in_group = false;
    let (mut left, mut right) = (None, None);
    for line in kwinrc.lines().map(str::trim) {
        if line.starts_with('[') {
            in_group = line == "[org.kde.kdecoration2]";
        } else if in_group && let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "ButtonsOnLeft" => left = Some(value.trim().to_owned()),
                "ButtonsOnRight" => right = Some(value.trim().to_owned()),
                _ => {}
            }
        }
    }
    if left.is_none() && right.is_none() {
        return None;
    }
    let names = |letters: &str| -> String {
        letters
            .chars()
            .filter_map(|c| match c {
                'I' => Some("minimize"),
                'A' => Some("maximize"),
                'X' => Some("close"),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(",")
    };
    Some(format!(
        "{}:{}",
        names(left.as_deref().unwrap_or("MS")),
        names(right.as_deref().unwrap_or("HIAX"))
    ))
}

/// The desktop's button layout, read once before the first window opens: KDE's `kwinrc`
/// under KDE, else GNOME's `button-layout` through `gsettings` (a subprocess given at most
/// `PROBE_TIMEOUT`, killed after that). `None` when neither can be read.
#[cfg(target_os = "linux")]
fn desktop_layout() -> Option<String> {
    if is_kde() {
        let dir = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config"))
            })?;
        let rc = std::fs::read_to_string(dir.join("kwinrc")).ok()?;
        return kde_layout(&rc);
    }
    gsettings_button_layout()
}

#[cfg(target_os = "linux")]
fn gsettings_button_layout() -> Option<String> {
    let mut command = std::process::Command::new("gsettings");
    command.args(["get", "org.gnome.desktop.wm.preferences", "button-layout"]);
    probe_output(command, PROBE_TIMEOUT, "gsettings")
}

/// What this run draws, chosen once by [`init`] before the first window opens.
#[derive(Debug)]
pub struct Setup {
    /// The chrome the run starts with; [`Chrome::current`] is what it uses now.
    pub chrome: Chrome,
    /// Whose conventions the drawn controls follow (look and default placement).
    pub look: Platform,
    pub controls: ControlsLayout,
}

static SETUP: OnceLock<Setup> = OnceLock::new();

/// This run's chrome. Without [`init`] (tests) it is the platform's, with the standard
/// controls.
pub fn setup() -> &'static Setup {
    SETUP.get_or_init(|| {
        let platform = Platform::current();
        Setup {
            chrome: Chrome::for_platform(platform),
            look: platform,
            controls: controls_layout(platform, None),
        }
    })
}

/// Choose this run's chrome, before the first window opens (on Linux this reads the desktop's
/// button layout: see `desktop_layout`). The Linux `window_decorations` setting and the
/// fallback to [`Chrome::Native`] where the desktop refuses drawn chrome switch it later,
/// through [`CHROME`].
///
/// Debug aid: `FBTERM_DEMO_CHROME=windows|linux[:<button-layout>]` draws that system's
/// controls on any system (e.g. `linux:close,minimize,maximize:`), for screenshots.
pub fn init() {
    let platform = Platform::current();
    // Blur is the real system's, also with the debug aid.
    probe_blur(platform);
    let demo =
        crate::app::debug_env("FBTERM_DEMO_CHROME").and_then(|v| v.to_str().and_then(demo_chrome));
    let setup = match demo {
        Some((look, desktop)) => Setup {
            chrome: Chrome::Drawn,
            look,
            controls: controls_layout(look, desktop.as_deref()),
        },
        None => {
            let chrome = Chrome::for_platform(platform);
            #[cfg(target_os = "linux")]
            let desktop = (chrome == Chrome::Drawn).then(desktop_layout).flatten();
            #[cfg(not(target_os = "linux"))]
            let desktop: Option<String> = None;
            Setup {
                chrome,
                look: platform,
                controls: controls_layout(platform, desktop.as_deref()),
            }
        }
    };
    tracing::info!(chrome = ?setup.chrome, controls = ?setup.controls, "window chrome");
    let _ = SETUP.set(setup);
}

/// The chrome for the `window_decorations` setting where the run started with `initial`
/// (see [`init`]) on a system with `look`'s conventions. Only Linux has the choice: the
/// system's title bar for `System`, or for the rest of the session once the desktop refused
/// drawn chrome (`fell_back`). Elsewhere the setting is ignored.
pub fn chrome_for_setting(
    look: Platform,
    initial: Chrome,
    setting: WindowDecorations,
    fell_back: bool,
) -> Chrome {
    if look != Platform::Linux || initial != Chrome::Drawn {
        return initial;
    }
    if setting == WindowDecorations::System || fell_back {
        Chrome::Native
    } else {
        Chrome::Drawn
    }
}

/// What decides whether this system can blur behind a window (read once by [`init`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlurProbe {
    /// Windows' build number, if known.
    pub windows_build: Option<u32>,
    /// The session is a Wayland one (Linux).
    pub wayland: bool,
    /// The desktop is KDE (Linux).
    pub kde: bool,
}

/// The first Windows build with system backdrops (`DWMWA_SYSTEMBACKDROP_TYPE`): Windows 11
/// 22H2.
pub const WINDOWS_BACKDROP_BUILD: u32 = 22621;

/// Whether `platform` can blur what shows through a window, given `probe`. macOS always can;
/// Windows 11 from [`WINDOWS_BACKDROP_BUILD`] through its acrylic backdrop; Linux on Wayland
/// under KDE, whose compositor offers the KDE blur protocol (the one the windowing layer
/// speaks; X11 has none it uses).
pub fn blur_supported(platform: Platform, probe: &BlurProbe) -> bool {
    match platform {
        Platform::MacOs => true,
        Platform::Windows => probe
            .windows_build
            .is_some_and(|b| b >= WINDOWS_BACKDROP_BUILD),
        Platform::Linux => probe.wayland && probe.kde,
    }
}

/// Whether this system can blur, once known (see [`blur_here`]).
static BLUR: OnceLock<bool> = OnceLock::new();

/// Whether this system can blur what shows through a window ([`blur_supported`]). Until the
/// probe [`init`] starts has answered (Windows reads its build in the background) and in
/// tests, it is what the platform alone says: macOS yes, the others no.
pub fn blur_here() -> bool {
    BLUR.get()
        .copied()
        .unwrap_or_else(|| blur_supported(Platform::current(), &BlurProbe::default()))
}

/// Find out whether this system can blur: at once where that only takes reading the
/// environment, on a background thread on Windows (a registry query), so startup never waits.
fn probe_blur(platform: Platform) {
    if platform == Platform::Windows {
        let spawned = std::thread::Builder::new()
            .name("blur-probe".into())
            .spawn(move || {
                let _ = BLUR.set(blur_supported(platform, &BlurProbe::read()));
            });
        if spawned.is_err() {
            let _ = BLUR.set(false);
        }
    } else {
        let _ = BLUR.set(blur_supported(platform, &BlurProbe::read()));
    }
}

/// The build number in the output of `reg query "HKLM\SOFTWARE\Microsoft\Windows
/// NT\CurrentVersion" /v CurrentBuildNumber`: the line `CurrentBuildNumber    REG_SZ
/// 22631`, the same in every system language. Anything else (a missing value, another type,
/// a value that is not a whole number) gives `None`.
#[cfg(any(test, target_os = "windows"))]
pub fn windows_build(reg: &str) -> Option<u32> {
    reg.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let (name, kind, value) = (fields.next()?, fields.next()?, fields.next()?);
        let digits = !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
        (name == "CurrentBuildNumber" && kind == "REG_SZ" && digits && fields.next().is_none())
            .then(|| value.parse().ok())
            .flatten()
    })
}

impl BlurProbe {
    /// This system's (on Windows a registry query, given at most `WINDOWS_PROBE_TIMEOUT`).
    fn read() -> Self {
        #[cfg(target_os = "linux")]
        {
            let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
            Self {
                windows_build: None,
                wayland: set("WAYLAND_DISPLAY") || set("WAYLAND_SOCKET"),
                kde: is_kde(),
            }
        }
        #[cfg(target_os = "windows")]
        {
            Self {
                windows_build: read_windows_build(),
                ..Self::default()
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            Self::default()
        }
    }
}

/// Whether the desktop is KDE.
#[cfg(target_os = "linux")]
fn is_kde() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|d| d.split(':').any(|d| d.eq_ignore_ascii_case("kde")))
}

/// How long the desktop query at startup may take (it runs before the first window opens).
#[cfg(target_os = "linux")]
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(300);

/// How long the Windows build query may take (it runs in the background).
#[cfg(target_os = "windows")]
const WINDOWS_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// The output of `command`, or `None` if it fails or takes longer than `timeout` (then it is
/// killed).
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn probe_output(
    mut command: std::process::Command,
    timeout: std::time::Duration,
    what: &str,
) -> Option<String> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::info!("{what} did not answer");
                return None;
            }
        }
    };
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    status.success().then_some(out)
}

/// Windows' build number, from the registry through `reg query` (no console window).
#[cfg(target_os = "windows")]
fn read_windows_build() -> Option<u32> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = std::process::Command::new("reg");
    command
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion",
            "/v",
            "CurrentBuildNumber",
        ])
        .creation_flags(CREATE_NO_WINDOW);
    probe_output(command, WINDOWS_PROBE_TIMEOUT, "reg query")
        .as_deref()
        .and_then(windows_build)
}

/// `FBTERM_DEMO_CHROME`: the system to imitate and its button layout.
fn demo_chrome(value: &str) -> Option<(Platform, Option<String>)> {
    let (system, desktop) = match value.split_once(':') {
        Some((system, rest)) => (system, Some(rest.to_owned())),
        None => (value, None),
    };
    match system {
        "windows" => Some((Platform::Windows, None)),
        "linux" => Some((Platform::Linux, desktop)),
        _ => None,
    }
}

/// Where a point `point` (window coordinates) in a window of `size` resizes it from: the
/// [`EDGE_BAND`] wide band along each edge ([`NORTH_BAND`] along the top, above the tabs),
/// corners reaching `CORNER_REACH` along both edges. A maximized (or full-screen) window does
/// not resize from its edges. `controls`: where the drawn controls are (see
/// [`controls_rect`]); a press there is theirs, except in the outermost corner.
pub fn edge_hit(
    size: Size,
    point: Point,
    maximized: bool,
    controls: Option<Rectangle>,
) -> Option<Direction> {
    let (x, y, w, h) = (point.x, point.y, size.width, size.height);
    if maximized || x < 0.0 || y < 0.0 || x > w || y > h {
        return None;
    }
    if controls.is_some_and(|r| r.contains(point)) {
        let corner = y < CONTROLS_CORNER && (x < CONTROLS_CORNER || x >= w - CONTROLS_CORNER);
        if !corner {
            return None;
        }
    }
    let (north, south) = (y < NORTH_BAND, y >= h - EDGE_BAND);
    let (west, east) = (x < EDGE_BAND, x >= w - EDGE_BAND);
    if !(north || south || west || east) {
        return None;
    }
    let (top, bottom) = (y < CORNER_REACH, y >= h - CORNER_REACH);
    let (left, right) = (x < CORNER_REACH, x >= w - CORNER_REACH);
    Some(if top && left {
        Direction::NorthWest
    } else if top && right {
        Direction::NorthEast
    } else if bottom && left {
        Direction::SouthWest
    } else if bottom && right {
        Direction::SouthEast
    } else if north {
        Direction::North
    } else if south {
        Direction::South
    } else if west {
        Direction::West
    } else {
        Direction::East
    })
}

/// Where the drawn controls of `layout` in `look`'s style sit in a window of `size`: at the
/// top, at the layout's end, as tall as the bar.
pub fn controls_rect(size: Size, layout: &ControlsLayout, look: Platform) -> Rectangle {
    let width = controls_width(look, layout.order.len());
    let x = match layout.side {
        Side::Left => 0.0,
        Side::Right => size.width - width,
    };
    Rectangle::new(Point::new(x, 0.0), Size::new(width, DRAWN_BAR_HEIGHT))
}

/// Width of `count` drawn controls in `look`'s style, with their spacing.
pub fn controls_width(look: Platform, count: usize) -> f32 {
    let n = count as f32;
    match look {
        Platform::Windows => n * WINDOWS_CONTROL_WIDTH,
        Platform::Linux | Platform::MacOs => n * LINUX_CONTROL_SIZE + (n + 1.0) * LINUX_CONTROL_GAP,
    }
}

/// The pointer over an edge that resizes toward `direction`.
fn resize_cursor(direction: Direction) -> mouse::Interaction {
    match direction {
        Direction::North | Direction::South => mouse::Interaction::ResizingVertically,
        Direction::East | Direction::West => mouse::Interaction::ResizingHorizontally,
        Direction::NorthEast | Direction::SouthWest => mouse::Interaction::ResizingDiagonallyUp,
        Direction::NorthWest | Direction::SouthEast => mouse::Interaction::ResizingDiagonallyDown,
    }
}

/// What the title bar asks of its window: a press in its empty part, a drawn control, or a
/// press on a drawn window's edge.
#[derive(Debug, Clone, Copy)]
pub enum BarAction {
    /// Move the window with the pointer.
    Drag,
    /// Zoom (maximize) the window, or restore it.
    ToggleMaximize,
    Minimize,
    /// Close the window as its OS close button would (the app may ask first).
    Close,
    /// Resize the window from that edge or corner with the pointer.
    ResizeFrom(Direction),
}

impl PartialEq for BarAction {
    fn eq(&self, other: &Self) -> bool {
        use std::mem::discriminant;
        match (self, other) {
            // `Direction` has no `PartialEq`; it has no fields either.
            (Self::ResizeFrom(a), Self::ResizeFrom(b)) => discriminant(a) == discriminant(b),
            _ => discriminant(self) == discriminant(other),
        }
    }
}

impl Eq for BarAction {}

impl Chrome {
    /// The strategy in use now (see [`init`] and [`CHROME`]).
    pub fn current() -> Self {
        CHROME.get()
    }

    /// The default strategy of `platform`.
    pub const fn for_platform(platform: Platform) -> Self {
        match platform {
            Platform::MacOs => Self::Unified,
            Platform::Windows | Platform::Linux => Self::Drawn,
        }
    }

    /// Height of the bar at the top of a window (the tab bar, the editor's header).
    pub fn bar_height(self) -> f32 {
        match self {
            Self::Unified => MACOS_TITLEBAR_HEIGHT,
            Self::Native => NATIVE_BAR_HEIGHT,
            Self::Drawn => DRAWN_BAR_HEIGHT,
        }
    }

    /// Space before the first tab: clear of the window buttons where they sit in the bar. In
    /// full screen the OS hides them, so the tabs start at the edge. Drawn controls are laid
    /// out beside the tabs, not over them.
    pub fn leading_inset(self, fullscreen: bool) -> f32 {
        match self {
            Self::Unified if !fullscreen => MACOS_TRAFFIC_LIGHTS_INSET,
            Self::Unified | Self::Native | Self::Drawn => NATIVE_LEADING_INSET,
        }
    }

    /// Whether this window has a bar of its own above the content (the editor's header): only
    /// where the OS title bar is gone.
    pub fn draws_title_bar(self) -> bool {
        matches!(self, Self::Unified | Self::Drawn)
    }

    /// Whether fbterm draws the window controls (and resizes from the edges) itself.
    pub fn draws_controls(self) -> bool {
        self == Self::Drawn
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
        // Linux desktops match a window to its desktop entry (`fbterm.desktop`, installed by
        // the packages) by this id: the Wayland app id and the X11 window class.
        #[cfg(target_os = "linux")]
        let base = window::Settings {
            platform_specific: window::settings::PlatformSpecific {
                application_id: LINUX_APP_ID.to_owned(),
                ..base.platform_specific
            },
            ..base
        };
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
        if self == Self::Drawn {
            #[cfg_attr(not(target_os = "windows"), allow(unused_mut))]
            let mut settings = window::Settings {
                decorations: false,
                ..base
            };
            // Windows 11 keeps the shadow and rounded corners of an undecorated window only
            // when asked.
            #[cfg(target_os = "windows")]
            {
                settings.platform_specific.undecorated_shadow = true;
                settings.platform_specific.corner_preference =
                    window::settings::platform::CornerPreference::Round;
            }
            return settings;
        }
        base
    }
}

/// The Linux application id: the basename of the packages' desktop entry `fbterm.desktop`.
#[cfg(target_os = "linux")]
const LINUX_APP_ID: &str = "fbterm";

/// The chrome in use, which can switch between [`Chrome::Drawn`] and [`Chrome::Native`]
/// while fbterm runs (Linux: the `window_decorations` setting, or a desktop that refuses
/// drawn chrome). Until first switched it is the [`Setup`]'s.
#[derive(Debug)]
pub struct ChromeCell(AtomicU8);

/// The chrome every window uses now.
pub static CHROME: ChromeCell = ChromeCell::new();

impl ChromeCell {
    const UNSET: u8 = 0;

    /// A cell that follows [`setup`] until switched.
    pub const fn new() -> Self {
        Self(AtomicU8::new(Self::UNSET))
    }

    /// A cell that starts at `chrome` (tests, and the app's own cell in tests).
    #[cfg(test)]
    pub fn starting(chrome: Chrome) -> Self {
        Self(AtomicU8::new(Self::code(chrome)))
    }

    fn code(chrome: Chrome) -> u8 {
        match chrome {
            Chrome::Unified => 1,
            Chrome::Native => 2,
            Chrome::Drawn => 3,
        }
    }

    pub fn get(&self) -> Chrome {
        match self.0.load(Ordering::Relaxed) {
            1 => Chrome::Unified,
            2 => Chrome::Native,
            3 => Chrome::Drawn,
            _ => setup().chrome,
        }
    }

    /// Use `to` from now on, if this is a switch between drawn and system chrome (the
    /// unified bar never switches). Returns whether it changed.
    fn switch(&self, to: Chrome) -> bool {
        let from = self.get();
        let switchable = |c: Chrome| matches!(c, Chrome::Drawn | Chrome::Native);
        if from == to || !switchable(from) || !switchable(to) {
            return false;
        }
        self.0.store(Self::code(to), Ordering::Relaxed);
        true
    }
}

impl Default for ChromeCell {
    fn default() -> Self {
        Self::new()
    }
}

/// Switch `cell` to `to` (drawn or system chrome). The `open` windows were made for the old
/// one, so each has its OS decorations toggled; new windows get `to`'s through
/// [`Chrome::window_settings`]. `None` when nothing changes. The caller forgets what it
/// tracks only for drawn chrome (which windows are maximized).
pub fn use_chrome<T: Send + 'static>(
    cell: &ChromeCell,
    to: Chrome,
    open: impl IntoIterator<Item = window::Id>,
) -> Option<iced::Task<T>> {
    cell.switch(to).then(|| {
        tracing::info!(chrome = ?to, "window chrome switched");
        iced::Task::batch(open.into_iter().map(window::toggle_decorations))
    })
}

/// Give up drawn chrome for this run: the desktop's title bar on every open window and every
/// new one (see [`use_chrome`]).
pub fn fall_back_to_system<T: Send + 'static>(
    cell: &ChromeCell,
    open: impl IntoIterator<Item = window::Id>,
) -> Option<iced::Task<T>> {
    use_chrome(cell, Chrome::Native, open)
}

/// The chrome each window was opened with, until the app sees it open (see
/// [`take_created`]).
static CREATED: std::sync::Mutex<Vec<(window::Id, Chrome)>> = std::sync::Mutex::new(Vec::new());

/// Note that window `id` was opened with `chrome`'s window settings.
pub fn record_created(id: window::Id, chrome: Chrome) {
    if let Ok(mut created) = CREATED.lock() {
        created.push((id, chrome));
    }
}

/// The chrome window `id` was opened with, once (`None` if not recorded or already taken).
pub fn take_created(id: window::Id) -> Option<Chrome> {
    let mut created = CREATED.lock().ok()?;
    let i = created.iter().position(|(w, _)| *w == id)?;
    Some(created.swap_remove(i).1)
}

/// Open a window from `base` with the chrome in use now, noting which (a switch can come
/// before the window has opened; the app then fixes its decorations).
pub fn open_window(base: window::Settings) -> (window::Id, iced::Task<window::Id>) {
    open_window_as(Chrome::current(), base)
}

/// [`open_window`] with `chrome`, the chrome in use the caller already read.
pub fn open_window_as(
    chrome: Chrome,
    base: window::Settings,
) -> (window::Id, iced::Task<window::Id>) {
    let (id, open) = window::open(chrome.window_settings(base));
    record_created(id, chrome);
    (id, open)
}

/// Whether a window opened with `made` needs its decorations toggled to suit `now`.
pub fn needs_toggle(made: Chrome, now: Chrome) -> bool {
    let switchable = |c: Chrome| matches!(c, Chrome::Drawn | Chrome::Native);
    made != now && switchable(made) && switchable(now)
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

/// What blurs what shows through terminal window `id`, beyond what its window settings ask
/// for (`blur` there covers macOS and the KDE blur protocol on Linux): on Windows, the acrylic
/// system backdrop. Nothing elsewhere.
pub fn system_backdrop<T: Send + 'static>(id: window::Id) -> iced::Task<T> {
    #[cfg(target_os = "windows")]
    {
        window::run(id, set_acrylic_backdrop).discard()
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = id;
        iced::Task::none()
    }
}

/// Ask DWM for the acrylic backdrop behind `window` (Windows 11 22H2 and later; older builds
/// refuse the attribute, which only logs).
#[cfg(target_os = "windows")]
fn set_acrylic_backdrop(window: &dyn window::Window) {
    use raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Dwm::{
        DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DwmSetWindowAttribute,
    };
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return;
    };
    let hwnd = win32.hwnd.get() as HWND;
    let backdrop = DWMSBT_TRANSIENTWINDOW;
    // SAFETY: `hwnd` is the handle of a live window: iced runs this callback on the thread
    // that owns the window, while it exists. The value pointer is to a local i32 that outlives
    // the call, and its size is passed with it; DWM only reads it.
    let result = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE as u32,
            (&raw const backdrop).cast(),
            std::mem::size_of_val(&backdrop) as u32,
        )
    };
    if result != 0 {
        tracing::info!(result, "the system refused the blurred backdrop");
    }
}

/// The task for a bar action on window `id`. [`BarAction::Close`] is the app's to handle (it
/// may ask before closing), so it does nothing here.
pub fn perform<T>(id: window::Id, action: BarAction) -> iced::Task<T> {
    match action {
        BarAction::Drag => window::drag(id),
        BarAction::ToggleMaximize => window::toggle_maximize(id),
        BarAction::Minimize => window::minimize(id, true),
        BarAction::ResizeFrom(direction) => window::drag_resize(id, direction),
        BarAction::Close => iced::Task::none(),
    }
}

/// Wraps a title bar drawn with `chrome`: left presses its children leave alone become
/// [`BarAction`]s. `modal`: the palette or a dialog covers the window, so presses leave the
/// window alone.
pub fn title_bar<'a, Message: 'a>(
    chrome: Chrome,
    content: impl Into<Element<'a, Message>>,
    modal: bool,
    on_action: impl Fn(BarAction) -> Message + 'a,
) -> Element<'a, Message> {
    Element::new(TitleBar {
        content: content.into(),
        on_action: Box::new(on_action),
        chrome,
        modal,
    })
}

/// Wraps a whole drawn-chrome window: a left press in the [`EDGE_BAND`] along its edges
/// becomes [`BarAction::ResizeFrom`] (and never reaches the content), and the pointer there
/// shows the resize cursor. `maximized` (or full screen): the edges do nothing.
pub fn edges<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    maximized: bool,
    on_action: impl Fn(BarAction) -> Message + 'a,
) -> Element<'a, Message> {
    Element::new(Edges {
        content: content.into(),
        on_action: Box::new(on_action),
        maximized,
    })
}

struct Edges<'a, Message> {
    content: Element<'a, Message>,
    on_action: Box<dyn Fn(BarAction) -> Message + 'a>,
    maximized: bool,
}

impl<Message> Edges<'_, Message> {
    fn hit(&self, layout: Layout<'_>, cursor: mouse::Cursor) -> Option<Direction> {
        let bounds = layout.bounds();
        let p = cursor.position_over(bounds)?;
        let setup = setup();
        edge_hit(
            bounds.size(),
            Point::new(p.x - bounds.x, p.y - bounds.y),
            self.maximized,
            Some(controls_rect(bounds.size(), &setup.controls, setup.look)),
        )
    }
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for Edges<'_, Message> {
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
        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event
            && let Some(direction) = self.hit(layout, cursor)
        {
            shell.publish((self.on_action)(BarAction::ResizeFrom(direction)));
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
        if let Some(direction) = self.hit(layout, cursor) {
            return resize_cursor(direction);
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
            Chrome::Drawn
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
    fn windows_controls_on_the_right() {
        let expected = ControlsLayout {
            side: Side::Right,
            order: vec![Control::Minimize, Control::Maximize, Control::Close],
        };
        // Windows ignores any desktop layout string.
        assert_eq!(controls_layout(Platform::Windows, None), expected);
        assert_eq!(
            controls_layout(Platform::Windows, Some("close,minimize,maximize:")),
            expected
        );
    }

    #[test]
    fn gnome_layout_parsed() {
        assert_eq!(
            controls_layout(Platform::Linux, Some("close,minimize,maximize:")),
            ControlsLayout {
                side: Side::Left,
                order: vec![Control::Close, Control::Minimize, Control::Maximize],
            }
        );
        // As `gsettings get` prints it: quoted, with a newline; unknown names are skipped.
        assert_eq!(
            controls_layout(Platform::Linux, Some("'appmenu:minimize,close'\n")),
            ControlsLayout {
                side: Side::Right,
                order: vec![Control::Minimize, Control::Close],
            }
        );
        // A layout without close still gets one, at the outer end.
        assert_eq!(
            controls_layout(Platform::Linux, Some("maximize:")),
            ControlsLayout {
                side: Side::Left,
                order: vec![Control::Close, Control::Maximize],
            }
        );
        assert_eq!(
            controls_layout(Platform::Linux, Some(":minimize")),
            ControlsLayout {
                side: Side::Right,
                order: vec![Control::Minimize, Control::Close],
            }
        );
    }

    #[test]
    fn kde_layout_becomes_a_desktop_layout() {
        let rc =
            "[Windows]\nFoo=1\n\n[org.kde.kdecoration2]\nButtonsOnLeft=XIA\nButtonsOnRight=H\n";
        let layout = kde_layout(rc).expect("keys present");
        assert_eq!(
            controls_layout(Platform::Linux, Some(&layout)),
            ControlsLayout {
                side: Side::Left,
                order: vec![Control::Close, Control::Minimize, Control::Maximize],
            }
        );
        assert_eq!(kde_layout("[Windows]\nButtonsOnLeft=X\n"), None);
    }

    #[test]
    fn unknown_layout_defaults_right() {
        let default = ControlsLayout {
            side: Side::Right,
            order: vec![Control::Minimize, Control::Maximize, Control::Close],
        };
        for desktop in [
            None,
            Some(""),
            Some("garbage"),
            Some("appmenu:"),
            Some("'menu:spacer'"),
        ] {
            assert_eq!(
                controls_layout(Platform::Linux, desktop),
                default,
                "{desktop:?}"
            );
        }
    }

    #[test]
    fn edge_hit_corners_and_edges() {
        let size = Size::new(800.0, 600.0);
        let hit = |x: f32, y: f32| edge_hit(size, Point::new(x, y), false, None);
        let is = |d: Option<Direction>, want: Direction| {
            d.is_some_and(|d| std::mem::discriminant(&d) == std::mem::discriminant(&want))
        };
        // Inside the window, away from the edges: nothing.
        assert!(hit(400.0, 300.0).is_none());
        assert!(hit(EDGE_BAND, 300.0).is_none());
        // The 8 px band on each edge.
        assert!(is(hit(0.0, 300.0), Direction::West));
        assert!(is(hit(EDGE_BAND - 0.5, 300.0), Direction::West));
        assert!(is(hit(799.0, 300.0), Direction::East));
        assert!(is(hit(400.0, 0.0), Direction::North));
        assert!(is(hit(400.0, NORTH_BAND - 0.5), Direction::North));
        assert!(is(hit(400.0, 595.0), Direction::South));
        // Corners, reaching along both edges.
        assert!(is(hit(1.0, 1.0), Direction::NorthWest));
        assert!(is(hit(12.0, 2.0), Direction::NorthWest));
        assert!(is(hit(798.0, 2.0), Direction::NorthEast));
        assert!(is(hit(2.0, 590.0), Direction::SouthWest));
        assert!(is(hit(795.0, 598.0), Direction::SouthEast));
        // Outside the window: nothing.
        assert!(hit(-1.0, 300.0).is_none());
        assert!(hit(400.0, 600.5).is_none());
    }

    fn is_dir(d: Option<Direction>, want: Direction) -> bool {
        d.is_some_and(|d| std::mem::discriminant(&d) == std::mem::discriminant(&want))
    }

    #[test]
    fn the_top_band_stays_above_the_tabs() {
        let size = Size::new(800.0, 600.0);
        let hit = |x: f32, y: f32| edge_hit(size, Point::new(x, y), false, None);
        // The tabs start 4 px below the top: a press on them never resizes.
        assert!(hit(400.0, 5.0).is_none());
        assert!(hit(400.0, 4.0).is_none());
        assert!(is_dir(hit(400.0, 3.0), Direction::North));
        // The other edges and the corners keep their reach.
        assert!(is_dir(hit(2.0, 12.0), Direction::NorthWest));
        assert!(is_dir(hit(798.0, 12.0), Direction::NorthEast));
        assert!(is_dir(hit(7.0, 300.0), Direction::West));
        assert!(is_dir(hit(400.0, 593.0), Direction::South));
    }

    #[test]
    fn the_drawn_controls_are_not_an_edge() {
        let size = Size::new(800.0, 600.0);
        let windows = controls_rect(size, &ControlsLayout::standard(), Platform::Windows);
        assert_eq!(windows.width, 3.0 * WINDOWS_CONTROL_WIDTH);
        assert_eq!(windows.height, DRAWN_BAR_HEIGHT);
        assert_eq!(windows.x, 800.0 - windows.width);
        let hit = |x: f32, y: f32| edge_hit(size, Point::new(x, y), false, Some(windows));
        // The top of the caption buttons and the right of close are the buttons.
        assert!(hit(700.0, 2.0).is_none());
        assert!(hit(799.0, 20.0).is_none());
        assert!(hit(795.0, 10.0).is_none());
        // Only the outermost corner still resizes.
        assert!(is_dir(hit(799.5, 0.5), Direction::NorthEast));
        // Away from the controls the edges are as before.
        assert!(is_dir(hit(600.0, 2.0), Direction::North));
        assert!(is_dir(hit(799.0, 300.0), Direction::East));
        assert!(is_dir(hit(1.0, 1.0), Direction::NorthWest));

        // Linux controls on the left: the west edge beside them is theirs.
        let layout = controls_layout(Platform::Linux, Some("close,minimize,maximize:"));
        let linux = controls_rect(size, &layout, Platform::Linux);
        assert_eq!(linux.x, 0.0);
        assert_eq!(
            linux.width,
            3.0 * LINUX_CONTROL_SIZE + 4.0 * LINUX_CONTROL_GAP
        );
        let hit = |x: f32, y: f32| edge_hit(size, Point::new(x, y), false, Some(linux));
        assert!(hit(2.0, 20.0).is_none());
        assert!(hit(40.0, 1.0).is_none());
        assert!(is_dir(hit(0.5, 0.5), Direction::NorthWest));
        assert!(is_dir(hit(400.0, 1.0), Direction::North));
        assert!(is_dir(hit(799.0, 10.0), Direction::NorthEast));
    }

    #[test]
    fn gnome_default_and_canonical_layouts() {
        // GNOME's default: only a close button, on the right.
        assert_eq!(
            controls_layout(Platform::Linux, Some("appmenu:close")),
            ControlsLayout {
                side: Side::Right,
                order: vec![Control::Close],
            }
        );
        assert_eq!(
            controls_layout(Platform::Linux, Some(":minimize,maximize,close")),
            ControlsLayout::standard()
        );
    }

    #[test]
    fn kde_missing_key_takes_kdes_default() {
        // Only the left key: the right one is KDE's default (help, minimize, maximize, close).
        let layout = kde_layout("[org.kde.kdecoration2]\nButtonsOnLeft=X\n").expect("a key set");
        assert_eq!(layout, "close:minimize,maximize,close");
        assert_eq!(
            controls_layout(Platform::Linux, Some(&layout)),
            ControlsLayout {
                side: Side::Left,
                order: vec![Control::Close, Control::Minimize, Control::Maximize],
            }
        );
    }

    #[test]
    fn the_chrome_switches_between_drawn_and_system_while_running() {
        let a = window::Id::unique();
        let b = window::Id::unique();
        let cell = ChromeCell::starting(Chrome::Drawn);
        assert_eq!(cell.get(), Chrome::Drawn);
        // Falling back toggles the decorations of every open window once.
        let task: iced::Task<()> = fall_back_to_system(&cell, [a, b]).expect("was drawn");
        assert_eq!(task.units(), 2);
        assert_eq!(cell.get(), Chrome::Native);
        // Already the system's: nothing more to do.
        assert!(fall_back_to_system::<()>(&cell, [a, b]).is_none());
        // And back to drawn chrome.
        let task: iced::Task<()> = use_chrome(&cell, Chrome::Drawn, [a]).expect("was native");
        assert_eq!(task.units(), 1);
        assert_eq!(cell.get(), Chrome::Drawn);
        // macOS's unified bar never switches.
        let mac = ChromeCell::starting(Chrome::Unified);
        assert!(fall_back_to_system::<()>(&mac, [a]).is_none());
        assert!(use_chrome::<()>(&mac, Chrome::Drawn, [a]).is_none());
        assert_eq!(mac.get(), Chrome::Unified);
        // Nothing is ever switched to the unified bar.
        assert!(use_chrome::<()>(&cell, Chrome::Unified, [a]).is_none());
        assert_eq!(cell.get(), Chrome::Drawn);
    }

    #[test]
    fn system_setting_uses_native_chrome() {
        use fbterm_config::WindowDecorations::{Fbterm, System};
        let linux = |setting, fell_back| {
            chrome_for_setting(Platform::Linux, Chrome::Drawn, setting, fell_back)
        };
        assert_eq!(linux(System, false), Chrome::Native);
        assert_eq!(linux(Fbterm, false), Chrome::Drawn);
        // A desktop that refused drawn chrome keeps the system's for the session.
        assert_eq!(linux(Fbterm, true), Chrome::Native);
        assert!(
            linux(System, false)
                .window_settings(window::Settings::default())
                .decorations
        );
        // Other systems ignore the setting.
        assert_eq!(
            chrome_for_setting(Platform::Windows, Chrome::Drawn, System, true),
            Chrome::Drawn
        );
        assert_eq!(
            chrome_for_setting(Platform::MacOs, Chrome::Unified, System, true),
            Chrome::Unified
        );
    }

    #[test]
    fn maximized_disables_edge_resize() {
        let size = Size::new(800.0, 600.0);
        for p in [(0.0, 0.0), (0.0, 300.0), (799.0, 599.0), (400.0, 2.0)] {
            assert!(edge_hit(size, Point::new(p.0, p.1), true, None).is_none());
        }
    }

    #[test]
    fn drawn_chrome_has_its_own_bar_without_os_decorations() {
        let c = Chrome::Drawn;
        assert!(c.draws_title_bar());
        assert!(c.draws_controls());
        assert!(!Chrome::Unified.draws_controls());
        assert!(!Chrome::Native.draws_controls());
        assert_eq!(
            c.bar_press(false, false, click::Kind::Single),
            Some(BarAction::Drag)
        );
        assert_eq!(
            c.bar_press(false, false, click::Kind::Double),
            Some(BarAction::ToggleMaximize)
        );
        assert_eq!(c.bar_press(true, false, click::Kind::Single), None);
        let s = c.window_settings(window::Settings::default());
        assert!(!s.decorations);
        assert!(
            Chrome::Native
                .window_settings(window::Settings::default())
                .decorations
        );
        assert_eq!(c.leading_inset(false), NATIVE_LEADING_INSET);
        assert_eq!(c.backdrop(1.0, true), Chrome::Native.backdrop(1.0, true));
    }

    #[test]
    fn macos_chrome_unchanged() {
        let c = Chrome::Unified;
        assert_eq!(c.bar_height(), MACOS_TITLEBAR_HEIGHT);
        assert_eq!(c.leading_inset(false), MACOS_TRAFFIC_LIGHTS_INSET);
        assert!(c.draws_title_bar());
        assert!(!c.draws_controls());
        let s = c.window_settings(window::Settings::default());
        assert!(s.decorations, "the OS keeps drawing the window buttons");
        assert_eq!(Chrome::for_platform(Platform::MacOs), Chrome::Unified);
        assert_eq!(Chrome::for_platform(Platform::Windows), Chrome::Drawn);
        assert_eq!(Chrome::for_platform(Platform::Linux), Chrome::Drawn);
        if cfg!(target_os = "macos") {
            assert_eq!(Chrome::current(), Chrome::Unified);
        }
    }

    #[test]
    fn the_menus_quit_keeps_cmd_q_unless_transfers_run() {
        assert!(is_quit_item(Some("terminate:")));
        assert!(!is_quit_item(Some("hide:")));
        assert!(!is_quit_item(Some("performClose:")));
        assert!(!is_quit_item(None));
        // The menu keeps Cmd+Q unless a transfer runs.
        assert!(quit_key_wanted(0));
        assert!(!quit_key_wanted(1));
        assert!(!quit_key_wanted(3));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_windows_carry_the_desktop_entry_id() {
        for c in [Chrome::Drawn, Chrome::Native] {
            let s = c.window_settings(window::Settings::default());
            assert_eq!(s.platform_specific.application_id, "fbterm");
        }
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
