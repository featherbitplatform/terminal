//! featherbit terminal.
//!
//! Debug aids (environment variables):
//! - `FBTERM_CONFIG_DIR=<dir>`: read `profiles.toml` and `settings.toml` (and keep `state.toml`)
//!   in `<dir>` instead of the platform config directory.
//! - `FBTERM_SECRETS=memory`: keep secrets in memory only; the OS keychain is never touched.
//!
//! Measurement mode (works in release builds, see `perf.rs`): `--perf-tabs <N> --perf-cmd <cmd>
//! [--perf-exit-after <secs>]`.
//!
//! Debug builds only (ignored in release builds):
//! - `FBTERM_OPEN_TABS=<name>,<name>`: also open these profiles (by name) in the first window.
//!   Runs with this set (or `FBTERM_SCREENSHOT`) never write `state.toml`.
//! - `FBTERM_PALETTE_QUERY=<text>`: open the quick-launch palette in the first window, pre-filled.
//! - `FBTERM_SCREENSHOT=<path.png>`: once the first window has rendered for a moment, capture
//!   that window only (never the screen), write it as PNG and exit.
//! - `FBTERM_SCREENSHOT_WINDOW=editor`: capture the profile editor window instead.
//! - `FBTERM_OPEN_EDITOR=<profile name>`: open the profile editor at startup, that profile
//!   selected.
//! - `FBTERM_DEMO_PROMPT=hostkey|password|changed`: show a sample connection dialog in the first
//!   window (nothing waits for the answer).
//! - `FBTERM_DEMO_TOOLTIP=new|profiles|close`: keep that tab-bar button's tooltip showing (for
//!   screenshots; `close` is the first tab's).

mod app;
mod chrome;
mod config_files;
mod debug;
mod dialogs;
mod editor;
mod keys;
mod palette;
mod perf;
mod prompt_view;
mod prompts;
mod redact;
mod restore;
#[cfg(test)]
mod restore_tests;
mod sessions;
mod tabs;
mod theme;
mod tooltip;
mod ui;
mod watch;
mod window;

use app::App;

const FONTS: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/GeistMono-Regular.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Bold.ttf"),
    include_bytes!("../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../assets/fonts/Geist-Medium.ttf"),
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
        .subscription(App::subscription)
        .default_font(theme::UI_FONT);
    for font in FONTS {
        daemon = daemon.font(font);
    }
    daemon.run()
}
