//! featherbit terminal.
//!
//! Debug aids (environment variables):
//! - `FBTERM_CONFIG_DIR=<dir>`: read `profiles.toml` and `settings.toml` from `<dir>` instead of
//!   the platform config directory.
//! - `FBTERM_SECRETS=memory`: keep secrets in memory only; the OS keychain is never touched.
//!
//! Debug builds only (ignored in release builds):
//! - `FBTERM_OPEN_TABS=<name>,<name>`: also open these profiles (by name) in the first window.
//! - `FBTERM_SCREENSHOT=<path.png>`: once the first window has rendered for a moment, capture
//!   that window only (never the screen), write it as PNG and exit.

mod app;
mod keys;
mod sessions;
mod tabs;
mod theme;
mod window;

use app::App;

const FONTS: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/GeistMono-Regular.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Bold.ttf"),
    include_bytes!("../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../assets/fonts/Geist-Medium.ttf"),
];

fn main() -> iced::Result {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::INFO)
        .init();
    let mut daemon = iced::daemon(App::boot, App::update, window::view)
        .title(App::title)
        .theme(|app: &App, _| app.theme.clone())
        .subscription(App::subscription)
        .default_font(theme::UI_FONT);
    for font in FONTS {
        daemon = daemon.font(font);
    }
    daemon.run()
}
