//! featherbit terminal.
//!
//! Debug aids (environment variables):
//! - `FBTERM_CONFIG_DIR=<dir>`: read `profiles.toml` and `settings.toml` (and keep `state.toml`)
//!   in `<dir>` instead of the platform config directory.
//! - `FBTERM_SECRETS=memory`: keep secrets in memory only; the OS keychain is never touched.
//!
//! Measurement mode (works in release builds, see `perf.rs`): `--perf-tabs <N> | --perf-panes <N>
//! --perf-cmd <cmd> [--perf-exit-after <secs>] [--perf-close-after <secs>]`.
//!
//! Debug builds only (ignored in release builds):
//! - `FBTERM_OPEN_TABS=<name>,<name>`: also open these profiles (by name) in the first window.
//!   Runs with this set (or `FBTERM_SPLITS`, `FBTERM_SCREENSHOT`) never write `state.toml`.
//! - `FBTERM_SPLITS=<layout>`: replace the first window's first tab with split panes of these
//!   profiles (by name): `|` side by side, `/` stacked, binding left to right with equal
//!   halves, e.g. `A|B/C` (`A` beside `B`, both above `C`).
//! - `FBTERM_PALETTE_QUERY=<text>`: open the quick-launch palette in the first window, pre-filled.
//! - `FBTERM_SCREENSHOT=<path.png>`: once the first window has rendered for a moment, capture
//!   that window only (never the screen), write it as PNG and exit.
//! - `FBTERM_SCREENSHOT_WINDOW=editor|settings|schemes|tunnels|archive`: capture the profile
//!   editor, the Settings window, the Color schemes window, the Tunnels window or the export
//!   and import window instead.
//! - `FBTERM_OPEN_EDITOR=<profile name>`: open the profile editor at startup, that profile
//!   selected.
//! - `FBTERM_OPEN_SETTINGS=1`: open the Settings window at startup (`secrets`: scrolled to the
//!   Saved secrets section).
//! - `FBTERM_DEMO_VAULT=1`: with `FBTERM_SECRETS=memory`, make the in-memory backend's vault
//!   (in its temporary folder, main password `demo`) so Settings shows `Reset vault`.
//! - `FBTERM_OPEN_SCHEMES=1`: open the Color schemes window at startup (any other value: the
//!   name of the scheme to select).
//! - `FBTERM_OPEN_TUNNELS=1`: open the Tunnels window at startup.
//! - `FBTERM_OPEN_ARCHIVE=export|import`: open the export and import window at startup in that
//!   mode. With `FBTERM_DEMO_ARCHIVE=1` as well, export starts with the jump hosts unchecked
//!   and key files on, and import shows a sample archive built in memory (its key folder in
//!   the temporary folder).
//! - `FBTERM_DEMO_PROMPT=hostkey|password|keyfile|changed|unlock|setpassword`: show a sample
//!   connection or main password dialog (`setpassword` only with `FBTERM_SECRETS=memory`) in the first
//!   window (nothing waits for the answer).
//! - `FBTERM_DEMO_TOOLTIP=new|profiles|tunnels|settings|close`: keep that tab-bar button's
//!   tooltip showing (for screenshots; `close` is the first tab's).
//! - `FBTERM_FIND=<text>`: open the find bar of the first window's active tab with this query
//!   (about a second after startup, once output has arrived).
//! - `FBTERM_FIND_ALL=<text>`: open the find-in-all-tabs box in the first window with this
//!   query (also after about a second).
//! - `FBTERM_DEMO_CHROME=windows|linux[:<button-layout>]`: draw that system's window controls
//!   (in GNOME's `button-layout` form for Linux, e.g. `linux:close,minimize,maximize:`) on
//!   any system, for screenshots.
//!
//! `fbterm --version` prints `fbterm <version>` and exits.

mod actions;
mod app;
mod archive_build;
#[cfg(test)]
mod archive_build_tests;
mod archive_import;
#[cfg(test)]
mod archive_import_tests;
mod archive_window;
mod chrome;
mod config_files;
mod debug;
mod dialogs;
mod display_rate;
mod editor;
mod export;
mod file_panel;
mod find;
mod keys;
mod layout;
mod memory;
mod pacing;
mod palette;
mod pane_actions;
mod perf;
mod prompt_view;
mod prompts;
mod redact;
mod restore;
#[cfg(test)]
mod restore_tests;
mod saved_secrets;
mod schemes;
#[cfg(test)]
mod schemes_tests;
mod schemes_window;
mod sessions;
mod settings_window;
mod shortcuts;
mod tab_look;
mod tabs;
mod theme;
mod tiles;
mod tooltip;
mod tunnels;
#[cfg(test)]
mod tunnels_tests;
mod tunnels_window;
mod ui;
mod visibility;
mod watch;
mod window;
mod windows;

use app::App;

const FONTS: [&[u8]; 5] = [
    include_bytes!("../assets/fonts/GeistMono-Regular.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Bold.ttf"),
    include_bytes!("../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../assets/fonts/lucide-icons.ttf"),
];

/// `fbterm --version` (or `-V`): the line it prints, `None` for any other run.
fn version_line<I: IntoIterator<Item = String>>(args: I) -> Option<String> {
    args.into_iter()
        .any(|a| a == "--version" || a == "-V")
        .then(|| format!("fbterm {}", env!("CARGO_PKG_VERSION")))
}

fn main() -> iced::Result {
    if let Some(line) = version_line(std::env::args().skip(1)) {
        println!("{line}");
        return Ok(());
    }
    perf::init_from_env();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::INFO)
        .init();
    let mut daemon = iced::daemon(App::boot, App::update, window::view)
        .title(App::title)
        .theme(|app: &App, _| app.theme.clone())
        .style(App::style)
        .scale_factor(|app: &App, _| app.ui_scale)
        .subscription(App::subscription)
        .default_font(theme::UI_FONT);
    for font in FONTS {
        daemon = daemon.font(font);
    }
    // Before the first window opens: on Linux this reads the desktop's button layout.
    chrome::init();
    daemon.run()
}
