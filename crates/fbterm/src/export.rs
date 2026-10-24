//! Export the active tab's text to a file: a native Save dialog, then the copy, formatting,
//! compression and write on tokio's blocking pool, then a short notice in the window.
//!
//! Only the user starts an export (palette or a bound chord); nothing a program prints can.

use std::path::PathBuf;

use fbterm_term::{ExportFormat, SharedTerminal, TermMode};
use iced::widget::{button, container, row, text};
use iced::{Alignment, Element, Length, Padding, Task, window};

use crate::app::{App, Delivered, Message};
use crate::theme::{Tokens, UI_FONT};
use crate::ui::{ICON_X, icon};

#[derive(Debug, Clone)]
pub enum ExportMsg {
    /// The Save dialog closed: where to write, or `None` when it was cancelled.
    Chosen {
        window: window::Id,
        terminal: Delivered<SharedTerminal>,
        path: Option<PathBuf>,
    },
    /// The file was written, or the export failed.
    Done(window::Id, Result<Exported, String>),
    /// "Show in Finder" (or the platform's file manager).
    Reveal,
    Dismiss,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exported {
    pub path: PathBuf,
    /// The alternate screen was up: the file holds the normal screen's history.
    pub alt_screen: bool,
}

/// What a notice reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    /// The file was written (the notice offers to show it).
    Done,
    /// Nothing happened, and why.
    Info,
    Error,
}

/// The notice shown in the window an export ran in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportNotice {
    pub window: window::Id,
    pub kind: NoticeKind,
    pub text: String,
    /// The written file, for the reveal button.
    pub path: Option<PathBuf>,
}

impl ExportNotice {
    fn info(window: window::Id, text: &str) -> Self {
        Self {
            window,
            kind: NoticeKind::Info,
            text: text.into(),
            path: None,
        }
    }
}

/// Most bytes of a tab title kept in a file name (with the date and extension the name stays
/// well under the usual 255-byte limit).
const TITLE_BYTES: usize = 200;

/// What the reveal button says on this platform.
pub const REVEAL_LABEL: &str = if cfg!(target_os = "macos") {
    "Show in Finder"
} else if cfg!(windows) {
    "Show in Explorer"
} else {
    "Show in Files"
};

/// Characters no file name may hold on any platform we build for, nor control characters.
fn forbidden(c: char) -> bool {
    matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()
}

/// A tab title made safe as a file name: forbidden characters become spaces, runs of spaces
/// collapse, leading dots and trailing dots or spaces go, at most 200 bytes; "Terminal"
/// when nothing is left.
pub fn sanitize(title: &str) -> String {
    let replaced: String = title
        .chars()
        .map(|c| if forbidden(c) { ' ' } else { c })
        .collect();
    let collapsed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_start_matches('.');
    // At most TITLE_BYTES bytes, cut on a character boundary.
    let cut = trimmed
        .char_indices()
        .map(|(i, c)| i + c.len_utf8())
        .take_while(|&end| end <= TITLE_BYTES)
        .last()
        .unwrap_or(0);
    let trimmed = &trimmed[..cut];
    let trimmed = trimmed.trim_end_matches(['.', ' ']).trim_start();
    if trimmed.is_empty() {
        "Terminal".into()
    } else {
        trimmed.into()
    }
}

/// `<title> 2026-10-24 15.30.txt`, in local time.
pub fn default_file_name(title: &str, now: chrono::NaiveDateTime) -> String {
    format!(
        "{} {}.{}",
        sanitize(title),
        now.format("%Y-%m-%d %H.%M"),
        ExportFormat::Plain.extension()
    )
}

/// The dialog's title, which says when the normal screen is exported instead of what shows.
pub fn dialog_title(alt_screen: bool) -> &'static str {
    if alt_screen {
        "Export tab (normal screen history; a full-screen program is showing)"
    } else {
        "Export tab"
    }
}

/// The notice after a successful export.
pub fn done_text(e: &Exported) -> String {
    let name = e.path.file_name().map_or_else(
        || e.path.display().to_string(),
        |n| n.to_string_lossy().into(),
    );
    if e.alt_screen {
        format!("Exported {name} (normal screen history; the full-screen program was not included)")
    } else {
        format!("Exported {name}")
    }
}

/// Copy, format and write, off the UI thread.
fn run(terminal: &SharedTerminal, path: PathBuf) -> Result<Exported, String> {
    let format = ExportFormat::from_path(&path);
    let summary = fbterm_term::write_file(&path, format, terminal).map_err(|e| {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into(),
        );
        format!("Could not export {name}: {e}")
    })?;
    Ok(Exported {
        path,
        alt_screen: summary.alt_screen,
    })
}

/// The one file-type filter the dialog offers. The extension of the name decides the format:
/// the dialogs cannot report a chosen filter, and some do not rename the file when another is
/// picked, so a list of formats would promise what it cannot keep.
pub const FILTER_LABEL: &str = "Text (.txt, .txt.gz, .ansi, .ansi.gz)";
pub const FILTER_EXTENSIONS: [&str; 3] = ["txt", "gz", "ansi"];

fn dialog(title: &'static str, name: String) -> rfd::AsyncFileDialog {
    rfd::AsyncFileDialog::new()
        .set_title(title)
        .set_file_name(name)
        .add_filter(FILTER_LABEL, &FILTER_EXTENSIONS)
}

impl App {
    /// Start exporting `wid`'s active tab: open the Save dialog (one at a time).
    pub(crate) fn export_tab(&mut self, wid: Option<window::Id>) -> Task<Message> {
        let Some(wid) = wid else {
            return Task::none();
        };
        let Some(tab) = self.windows.get(&wid).and_then(|w| w.active_tab()) else {
            return Task::none();
        };
        if self.exporting.is_some() {
            self.export_notice = Some(ExportNotice::info(
                wid,
                "An export is already running; try again when it is done.",
            ));
            return Task::none();
        }
        let Some(terminal) = tab.terminal().cloned() else {
            self.export_notice = Some(ExportNotice::info(
                wid,
                "This tab has nothing to export yet.",
            ));
            return Task::none();
        };
        let alt = terminal.lock().mode().contains(TermMode::ALT_SCREEN);
        let name = default_file_name(&tab.title, chrono::Local::now().naive_local());
        self.exporting = Some(wid);
        let terminal = Delivered::new(terminal);
        let title = dialog_title(alt);
        // The dialog belongs to the window (a sheet on macOS).
        window::run(wid, move |w| dialog(title, name).set_parent(w)).then(move |d| {
            let terminal = terminal.clone();
            Task::perform(
                async move { d.save_file().await.map(|f| f.path().to_path_buf()) },
                move |path| {
                    Message::Export(ExportMsg::Chosen {
                        window: wid,
                        terminal,
                        path,
                    })
                },
            )
        })
    }

    pub(crate) fn export_msg(&mut self, msg: ExportMsg) -> Task<Message> {
        match msg {
            ExportMsg::Chosen {
                window,
                terminal,
                path,
            } => {
                let (Some(path), Some(terminal)) = (path, terminal.take()) else {
                    self.exporting = None;
                    return Task::none();
                };
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || run(&terminal, path))
                            .await
                            .unwrap_or_else(|e| Err(format!("Could not export: {e}")))
                    },
                    move |r| Message::Export(ExportMsg::Done(window, r)),
                )
            }
            ExportMsg::Done(window, result) => {
                self.exporting = None;
                self.export_notice = Some(match result {
                    Ok(e) => {
                        tracing::info!(path = %e.path.display(), "exported a tab");
                        ExportNotice {
                            window,
                            kind: NoticeKind::Done,
                            text: done_text(&e),
                            path: Some(e.path),
                        }
                    }
                    Err(text) => {
                        tracing::warn!(error = %text, "tab export failed");
                        ExportNotice {
                            window,
                            kind: NoticeKind::Error,
                            text,
                            path: None,
                        }
                    }
                });
                Task::none()
            }
            ExportMsg::Reveal => {
                if let Some(path) = self.export_notice.as_ref().and_then(|n| n.path.as_deref()) {
                    crate::config_files::reveal_file(path);
                }
                Task::none()
            }
            ExportMsg::Dismiss => {
                self.export_notice = None;
                Task::none()
            }
        }
    }
}

/// The notice under the tab bar: the file name with a reveal button, or the error.
pub fn notice_view<'a>(
    t: &'static Tokens,
    base: iced::Color,
    notice: &'a ExportNotice,
) -> Element<'a, Message> {
    let done = notice.kind == NoticeKind::Done && notice.path.is_some();
    let mut line = row![
        text(notice.text.as_str())
            .font(UI_FONT)
            .size(12.5)
            .color(t.text)
            .width(Length::Fill)
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if done {
        line = line.push(
            button(text(REVEAL_LABEL).font(UI_FONT).size(12))
                .on_press(Message::Export(ExportMsg::Reveal))
                .padding(Padding::from([3.0, 10.0]))
                .style(move |_, status| crate::ui::secondary(t, status)),
        );
    }
    line = line.push(
        button(icon(ICON_X, 12.0, t.text_muted))
            .on_press(Message::Export(ExportMsg::Dismiss))
            .padding(4)
            .style(move |_, status| crate::ui::ghost(t, status)),
    );
    let tint = match notice.kind {
        NoticeKind::Done => t.accent_soft,
        NoticeKind::Info => t.hover,
        NoticeKind::Error => t.danger_soft,
    };
    let banner = container(line)
        .padding(Padding::from([5.0, 12.0]))
        .width(Length::Fill)
        .style(move |_| container::Style {
            background: Some(tint.into()),
            ..container::Style::default()
        });
    // The tint sits on the chrome's background, so it reads the same at any opacity.
    container(banner)
        .width(Length::Fill)
        .style(move |_| container::Style {
            background: Some(base.into()),
            ..container::Style::default()
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(h: u32, m: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 10, 24)
            .unwrap()
            .and_hms_opt(h, m, 7)
            .unwrap()
    }

    #[test]
    fn the_default_name_is_the_title_and_the_local_time() {
        assert_eq!(
            default_file_name("zsh", at(15, 30)),
            "zsh 2026-10-24 15.30.txt"
        );
        assert_eq!(
            default_file_name("user@host: ~/src", at(9, 5)),
            "user@host ~ src 2026-10-24 09.05.txt"
        );
    }

    #[test]
    fn titles_are_made_safe_for_file_names() {
        assert_eq!(sanitize("a/b\\c:d*e?f\"g<h>i|j"), "a b c d e f g h i j");
        assert_eq!(sanitize("  vim   notes.md  "), "vim notes.md");
        assert_eq!(sanitize("..hidden"), "hidden");
        assert_eq!(sanitize("trailing. . "), "trailing");
        assert_eq!(sanitize("tab\tand\u{7}bell"), "tab and bell");
        assert_eq!(sanitize("///"), "Terminal");
        assert_eq!(sanitize(""), "Terminal");
        assert_eq!(sanitize("日本語 ok"), "日本語 ok");
        assert_eq!(sanitize(&"x".repeat(300)).len(), 200);
    }

    #[test]
    fn long_titles_fit_a_file_name_in_bytes_and_are_cut_between_characters() {
        let now = at(15, 30);
        // Three bytes each (CJK) and four bytes each (emoji): cut on a character boundary.
        for title in [
            "漢".repeat(150),
            "🦀".repeat(120),
            format!("a{}", "é".repeat(150)),
        ] {
            let cut = sanitize(&title);
            assert!(cut.len() <= TITLE_BYTES, "{} bytes", cut.len());
            assert!(cut.len() > TITLE_BYTES - 4);
            assert!(title.starts_with(&cut));
            let name = default_file_name(&title, now);
            assert!(name.len() <= 255, "{} bytes", name.len());
            assert!(name.ends_with(" 2026-10-24 15.30.txt"));
        }
    }

    #[test]
    fn the_dialog_offers_one_filter_where_the_extension_decides() {
        assert!(FILTER_LABEL.contains(".txt.gz") && FILTER_LABEL.contains(".ansi.gz"));
        // Every format's name ends in an extension the filter accepts.
        for f in ExportFormat::ALL {
            let last = f.extension().rsplit('.').next().unwrap();
            assert!(FILTER_EXTENSIONS.contains(&last), "{f:?}");
        }
    }

    #[test]
    fn the_notice_names_the_file_and_the_alt_screen() {
        let e = Exported {
            path: PathBuf::from("/tmp/dir/zsh 2026-10-24 15.30.txt"),
            alt_screen: false,
        };
        assert_eq!(done_text(&e), "Exported zsh 2026-10-24 15.30.txt");
        let e = Exported {
            alt_screen: true,
            ..e
        };
        assert!(done_text(&e).contains("normal screen history"));
        assert!(dialog_title(true).contains("normal screen"));
        assert_eq!(dialog_title(false), "Export tab");
    }

    #[test]
    fn an_export_writes_the_chosen_format_and_reports_failures() {
        let dir = std::env::temp_dir().join(format!("fbterm-export-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut term = fbterm_term::Terminal::new(crate::app::DEFAULT_GRID, 100);
        term.feed(b"\x1b[31mred\x1b[0m line\r\n");
        let shared: SharedTerminal = std::sync::Arc::new(parking_lot::Mutex::new(term));

        let path = dir.join("out.ansi");
        let done = run(&shared, path.clone()).unwrap();
        assert_eq!(done.path, path);
        assert!(!done.alt_screen);
        assert!(path.is_file());
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, "\x1b[0;31mred\x1b[0m line\n");

        let missing = dir.join("no such dir").join("out.txt");
        let err = run(&shared, missing).unwrap_err();
        assert!(err.starts_with("Could not export out.txt:"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
