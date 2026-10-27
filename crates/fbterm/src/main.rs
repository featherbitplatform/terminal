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
//! - `FBTERM_SCREENSHOT_WINDOW=editor|settings|schemes|tunnels`: capture the profile editor,
//!   the Settings window, the Color schemes window or the Tunnels window instead.
//! - `FBTERM_OPEN_EDITOR=<profile name>`: open the profile editor at startup, that profile
//!   selected.
//! - `FBTERM_OPEN_SETTINGS=1`: open the Settings window at startup.
//! - `FBTERM_OPEN_SCHEMES=1`: open the Color schemes window at startup (any other value: the
//!   name of the scheme to select).
//! - `FBTERM_OPEN_TUNNELS=1`: open the Tunnels window at startup.
//! - `FBTERM_DEMO_PROMPT=hostkey|password|changed`: show a sample connection dialog in the first
//!   window (nothing waits for the answer).
//! - `FBTERM_DEMO_TOOLTIP=new|profiles|tunnels|settings|close`: keep that tab-bar button's
//!   tooltip showing (for screenshots; `close` is the first tab's).
//! - `FBTERM_FIND=<text>`: open the find bar of the first window's active tab with this query
//!   (about a second after startup, once output has arrived).
//! - `FBTERM_FIND_ALL=<text>`: open the find-in-all-tabs box in the first window with this
//!   query (also after about a second).

mod actions;
mod app;
mod archive_build;
#[cfg(test)]
mod archive_build_tests;
mod archive_import;
#[cfg(test)]
mod archive_import_tests;
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

fn main() -> iced::Result {
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
    daemon.run()
}
