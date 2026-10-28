//! Something to collect when fbterm crashes or cannot start (Windows release builds and apps
//! started from the macOS Finder have no visible stderr).
//!
//! - The run log `fbterm.log` in the config dir gets everything stderr shows, without colors;
//!   the log of the run before is kept as `fbterm.previous.log`.
//! - A panic, or `daemon.run()` failing, appends a report to `crash.log` (moved aside to
//!   `crash.previous.log` once it is over [`MAX_CRASH_LOG`]) and creates the empty marker
//!   `crash.pending`, which the next launch consumes to say so once in the window banner.
//!
//! Nothing here ever fails start or panics: every I/O error is ignored.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use fbterm_config::Paths;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;

/// Size above which `crash.log` is moved to `crash.previous.log` before the next report.
pub const MAX_CRASH_LOG: u64 = 256 * 1024;

/// Log at INFO to stderr (as always) and to the run log, when it can be opened.
pub fn init_logging(paths: &Paths) {
    let file =
        open_run_log(paths).map(|f| fmt::layer().with_ansi(false).with_writer(Mutex::new(f)));
    tracing_subscriber::registry()
        .with(LevelFilter::INFO)
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(file)
        .init();
}

/// Start a new run log, keeping the previous one as `fbterm.previous.log` (replacing an
/// older one). `None` when the file cannot be created.
pub fn open_run_log(paths: &Paths) -> Option<File> {
    let _ = fs::create_dir_all(&paths.config_dir);
    let log = paths.run_log();
    if log.exists() {
        let _ = fs::rename(&log, paths.previous_run_log());
    }
    File::create(&log).ok()
}

/// On every panic, append a report to `crash.log`, then let the previous (default) hook print
/// to stderr as before.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("(no message)");
        let location = info.location().map(|l| l.to_string());
        let thread = std::thread::current();
        let report = Report {
            time: now_rfc3339(),
            thread: thread.name().unwrap_or("(unnamed)").to_owned(),
            what: format!("panic: {message}"),
            location,
            backtrace: std::backtrace::Backtrace::force_capture().to_string(),
        };
        write_report(&crate::app::paths(), &report.text());
        previous(info);
    }));
}

/// `daemon.run()` failed: log it, write a report and, on Windows (no console), say where it
/// is in a message box.
pub fn start_failed(error: &str) {
    tracing::error!("fbterm could not start: {error}");
    let paths = crate::app::paths();
    let report = Report {
        time: now_rfc3339(),
        thread: std::thread::current()
            .name()
            .unwrap_or("(unnamed)")
            .to_owned(),
        what: format!("fbterm could not start: {error}"),
        location: None,
        backtrace: std::backtrace::Backtrace::force_capture().to_string(),
    };
    let path = write_report(&paths, &report.text());
    #[cfg(windows)]
    {
        let text = format!(
            "fbterm could not start: {error}. A report was saved to {}.",
            path.display()
        );
        let _ = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("fbterm")
            .set_description(text)
            .set_buttons(rfd::MessageButtons::Ok)
            .show();
    }
    #[cfg(not(windows))]
    let _ = path;
}

/// The current UTC time in RFC 3339, to the second.
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// One entry of `crash.log`.
#[derive(Debug, Clone)]
pub struct Report {
    /// UTC time in RFC 3339.
    pub time: String,
    /// Name of the thread it happened on.
    pub thread: String,
    /// `panic: <message>` or `fbterm could not start: <error>`.
    pub what: String,
    /// Source location of a panic.
    pub location: Option<String>,
    /// The captured backtrace.
    pub backtrace: String,
}

impl Report {
    /// The report as written: a separator line, then one fact per line, the backtrace last.
    pub fn text(&self) -> String {
        let mut out = String::from("==== fbterm crash report ====\n");
        out.push_str(&format!("time: {}\n", self.time));
        out.push_str(&format!("fbterm {}\n", env!("CARGO_PKG_VERSION")));
        out.push_str(&format!(
            "os: {} {}\n",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
        out.push_str(&format!("thread: {}\n", self.thread));
        out.push_str(&self.what);
        out.push('\n');
        if let Some(at) = &self.location {
            out.push_str(&format!("at: {at}\n"));
        }
        out.push_str("backtrace:\n");
        out.push_str(&self.backtrace);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        out
    }
}

/// Append `text` to `crash.log` (creating the config dir, moving a log over
/// [`MAX_CRASH_LOG`] aside first) and mark it pending for the next launch. Returns the path of
/// `crash.log`.
pub fn write_report(paths: &Paths, text: &str) -> PathBuf {
    let _ = fs::create_dir_all(&paths.config_dir);
    let log = paths.crash_log();
    if fs::metadata(&log).is_ok_and(|m| m.len() > MAX_CRASH_LOG) {
        let _ = fs::rename(&log, paths.previous_crash_log());
    }
    let written = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .and_then(|mut f| f.write_all(text.as_bytes()));
    if written.is_ok() {
        let _ = File::create(paths.crash_pending());
    }
    log
}

/// The banner line for a report the last run left, once: removes the `crash.pending` marker.
pub fn take_pending(paths: &Paths) -> Option<String> {
    fs::remove_file(paths.crash_pending()).ok()?;
    Some(format!(
        "fbterm closed unexpectedly last time. The report is in {}.",
        paths.crash_log().display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_dir: dir.path().join("config"),
        };
        (dir, paths)
    }

    fn sample(what: &str, location: Option<&str>) -> Report {
        Report {
            time: "2026-10-28T09:15:00Z".into(),
            thread: "main".into(),
            what: what.into(),
            location: location.map(str::to_owned),
            backtrace: "   0: fbterm::main\n".into(),
        }
    }

    #[test]
    fn report_lists_every_fact() {
        let text = sample("panic: boom", Some("src/app.rs:10:5")).text();
        let expected = format!(
            "==== fbterm crash report ====\ntime: 2026-10-28T09:15:00Z\nfbterm {}\nos: {} {}\n\
             thread: main\npanic: boom\nat: src/app.rs:10:5\nbacktrace:\n   0: fbterm::main\n\n",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        assert_eq!(text, expected);
    }

    #[test]
    fn start_failure_report_has_no_location() {
        let text = sample("fbterm could not start: no adapter", None).text();
        assert!(text.contains("\nfbterm could not start: no adapter\nbacktrace:\n"));
        assert!(!text.contains("at: "));
    }

    #[test]
    fn now_is_rfc3339_utc() {
        let now = now_rfc3339();
        assert!(chrono::DateTime::parse_from_rfc3339(&now).is_ok(), "{now}");
        assert!(now.ends_with('Z'), "{now}");
    }

    #[test]
    fn report_creates_dir_appends_and_marks_pending() {
        let (_dir, paths) = temp_paths();
        let log = write_report(&paths, "one\n");
        assert_eq!(log, paths.crash_log());
        write_report(&paths, "two\n");
        assert_eq!(fs::read_to_string(&log).unwrap(), "one\ntwo\n");
        assert!(paths.crash_pending().exists());
        assert!(!paths.previous_crash_log().exists());
    }

    #[test]
    fn large_crash_log_moves_aside_first() {
        let (_dir, paths) = temp_paths();
        fs::create_dir_all(&paths.config_dir).unwrap();
        let big = "x".repeat(MAX_CRASH_LOG as usize + 1);
        fs::write(paths.crash_log(), &big).unwrap();
        fs::write(paths.previous_crash_log(), "oldest").unwrap();
        write_report(&paths, "new\n");
        assert_eq!(fs::read_to_string(paths.crash_log()).unwrap(), "new\n");
        assert_eq!(fs::read_to_string(paths.previous_crash_log()).unwrap(), big);
    }

    #[test]
    fn crash_log_at_the_limit_stays() {
        let (_dir, paths) = temp_paths();
        fs::create_dir_all(&paths.config_dir).unwrap();
        let full = "x".repeat(MAX_CRASH_LOG as usize);
        fs::write(paths.crash_log(), &full).unwrap();
        write_report(&paths, "new\n");
        assert_eq!(
            fs::read_to_string(paths.crash_log()).unwrap(),
            format!("{full}new\n")
        );
        assert!(!paths.previous_crash_log().exists());
    }

    #[test]
    fn run_log_keeps_the_previous_run() {
        let (_dir, paths) = temp_paths();
        let mut first = open_run_log(&paths).expect("first log");
        first.write_all(b"run one\n").unwrap();
        drop(first);
        fs::write(paths.previous_run_log(), "run zero\n").unwrap();
        let second = open_run_log(&paths).expect("second log");
        drop(second);
        assert_eq!(fs::read_to_string(paths.run_log()).unwrap(), "");
        assert_eq!(
            fs::read_to_string(paths.previous_run_log()).unwrap(),
            "run one\n"
        );
    }

    #[test]
    fn run_log_that_cannot_open_is_none() {
        let (dir, _) = temp_paths();
        // The config dir is a file, so nothing can be created in it.
        let blocker = dir.path().join("file");
        fs::write(&blocker, "").unwrap();
        let paths = Paths {
            config_dir: blocker,
        };
        assert!(open_run_log(&paths).is_none());
    }

    #[test]
    fn pending_marker_is_consumed_once() {
        let (_dir, paths) = temp_paths();
        assert_eq!(take_pending(&paths), None);
        write_report(&paths, "report\n");
        assert_eq!(
            take_pending(&paths),
            Some(format!(
                "fbterm closed unexpectedly last time. The report is in {}.",
                paths.crash_log().display()
            ))
        );
        assert!(!paths.crash_pending().exists());
        assert_eq!(take_pending(&paths), None);
        assert!(paths.crash_log().exists());
    }

    #[test]
    fn launch_shows_the_pending_report_once() {
        let paths = crate::restore_tests::paths_with(&[]);
        write_report(&paths, "report\n");
        let line = take_pending(&paths).unwrap();
        write_report(&paths, "report\n");

        let (app, _) = crate::app::App::build(paths.clone(), true);
        assert!(app.visible_notices().contains(&line), "{:?}", app.notices);
        assert!(!paths.crash_pending().exists());
        let (again, _) = crate::app::App::build(paths, true);
        assert!(!again.notices.contains(&line), "{:?}", again.notices);
    }
}
