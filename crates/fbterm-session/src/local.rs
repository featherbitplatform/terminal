//! Local PTY sessions.

use std::io::{ErrorKind, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::time::Duration;

use fbterm_config::LocalProfile;
use fbterm_term::{GridSize, SharedTerminal, Terminal};
use parking_lot::Mutex;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::{
    Closer, Command, EventSink, Feeder, SessionError, SessionEvent, SessionHandle, SessionId,
};

/// How long the waiter lets the reader drain after the child exits before
/// announcing `Exited` anyway (a grandchild may still hold the PTY open).
const DRAIN_GRACE: Duration = Duration::from_millis(500);

/// Variables any one of which tells programs which locale (and so which encoding) to use.
const LOCALE_VARS: [&str; 3] = ["LANG", "LC_ALL", "LC_CTYPE"];

/// The UTF-8 locale a session gets when nothing chose one. An app started from the macOS
/// desktop inherits no locale at all, and shells then fall back to ASCII; `en_US.UTF-8` exists
/// on every macOS install. On Linux `C.UTF-8` is the one UTF-8 locale that is always present.
#[cfg(target_os = "macos")]
const DEFAULT_LANG: &str = "en_US.UTF-8";
#[cfg(not(target_os = "macos"))]
const DEFAULT_LANG: &str = "C.UTF-8";

/// Variables set for a local session on top of the inherited environment, in order (later
/// entries win): the terminal type, a UTF-8 `LANG` when neither the inherited environment
/// (`inherited_locale`) nor the profile sets any of `LANG`, `LC_ALL` or `LC_CTYPE`, then the
/// profile's own variables.
fn session_env(
    profile_env: &std::collections::BTreeMap<String, String>,
    inherited_locale: bool,
) -> Vec<(String, String)> {
    let mut env = vec![
        ("TERM".to_owned(), "xterm-256color".to_owned()),
        ("COLORTERM".to_owned(), "truecolor".to_owned()),
    ];
    let profile_locale = LOCALE_VARS
        .iter()
        .any(|k| profile_env.get(*k).is_some_and(|v| !v.is_empty()));
    if !inherited_locale && !profile_locale {
        env.push(("LANG".to_owned(), DEFAULT_LANG.to_owned()));
    }
    env.extend(profile_env.iter().map(|(k, v)| (k.clone(), v.clone())));
    env
}

fn pty_size(size: GridSize) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: (f32::from(size.cols) * size.cell_width) as u16,
        pixel_height: (f32::from(size.rows) * size.cell_height) as u16,
    }
}

/// Spawn the profile's program on a new PTY and feed its output into a new shared terminal.
pub fn spawn_local(
    profile: &LocalProfile,
    size: GridSize,
    scrollback: usize,
    events: EventSink,
) -> Result<SessionHandle, SessionError> {
    let terminal = Arc::new(Mutex::new(Terminal::new(size, scrollback)));
    spawn_local_into(profile, terminal, events)
}

/// Like [`spawn_local`], but the program's output continues in an existing terminal (at its
/// current size), below whatever it already shows; its scrollback is kept.
pub fn spawn_local_into(
    profile: &LocalProfile,
    terminal: SharedTerminal,
    events: EventSink,
) -> Result<SessionHandle, SessionError> {
    let size = terminal.lock().size();
    let pair = native_pty_system()
        .openpty(pty_size(size))
        .map_err(|e| SessionError::Pty(e.to_string()))?;

    let mut cmd = CommandBuilder::new(&profile.program);
    cmd.args(&profile.args);
    let inherited_locale = LOCALE_VARS
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()));
    for (k, v) in session_env(&profile.env, inherited_locale) {
        cmd.env(k, v);
    }
    let cwd = profile
        .cwd
        .clone()
        .or_else(|| directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()));
    if let Some(cwd) = cwd {
        cmd.cwd(cwd);
    }

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| SessionError::Spawn {
            program: profile.program.display().to_string(),
            reason: e.to_string(),
        })?;
    // The child holds its own copy; keeping ours would stop the reader seeing EOF on Unix.
    drop(pair.slave);

    let io = pair
        .master
        .try_clone_reader()
        .and_then(|r| pair.master.take_writer().map(|w| (r, w)));
    let (mut reader, writer) = match io {
        Ok(io) => io,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SessionError::Pty(e.to_string()));
        }
    };
    let killer = Mutex::new(child.clone_killer());
    // Set by the waiter as soon as the child is reaped: from then on its pid may belong to
    // another process, so the closer must not signal it.
    let reaped = Arc::new(AtomicBool::new(false));

    let id = SessionId::next();
    let dirty_pending = Arc::new(AtomicBool::new(false));
    let bell_pending = Arc::new(AtomicBool::new(false));
    let detached = Arc::new(AtomicBool::new(false));
    let ended = crate::Ended::default();
    let (tx, rx) = unbounded_channel::<Command>();
    let (done_tx, done_rx) = channel::<()>();

    std::thread::Builder::new()
        .name(format!("fbterm-pty-writer-{}", id.0))
        .spawn(move || writer_loop(rx, writer, pair.master))?;

    // Waiter: the only place `Exited` is produced, so it fires exactly once and
    // does not depend on the output pipe reaching EOF (ConPTY, lingering grandchildren).
    let waiter_tx = tx.clone();
    let waiter = Feeder::new(
        id,
        terminal.clone(),
        events.clone(),
        dirty_pending.clone(),
        bell_pending.clone(),
        detached.clone(),
        ended.clone(),
    );
    std::thread::Builder::new()
        .name(format!("fbterm-pty-waiter-{}", id.0))
        .spawn({
            let reaped = reaped.clone();
            move || {
                let code = child.wait().ok().map(|s| s.exit_code() as i32);
                reaped.store(true, Ordering::Release);
                // Dropping the master closes the pseudoconsole, which unblocks the reader on Windows.
                let _ = waiter_tx.send(Command::Close);
                let _ = done_rx.recv_timeout(DRAIN_GRACE);
                waiter.mark_dirty();
                waiter.send(SessionEvent::Exited(code));
            }
        })?;

    let reader_tx = tx.clone();
    let mut feeder = Feeder::new(
        id,
        terminal.clone(),
        events,
        dirty_pending.clone(),
        bell_pending.clone(),
        detached.clone(),
        ended.clone(),
    );
    std::thread::Builder::new()
        .name(format!("fbterm-pty-reader-{}", id.0))
        .spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                // EOF, or EIO once the child side closes on Unix: either way we are done.
                let n = match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                for reply in feeder.feed(&buf[..n]) {
                    let _ = reader_tx.send(Command::Input(reply));
                }
                // Closed: whoever still holds the PTY no longer reaches the terminal.
                if feeder.is_detached() {
                    break;
                }
            }
            let _ = done_tx.send(());
        })?;

    Ok(SessionHandle {
        id,
        terminal,
        commands: tx,
        dirty_pending,
        bell_pending,
        detached,
        ended,
        auth_warnings: Vec::new(),
        closer: kill_unless_reaped(reaped, move || {
            let _ = killer.lock().kill();
        }),
        connection: Mutex::new(None),
    })
}

/// The closer of a local session: runs `kill` (which signals the child's pid) only while the
/// child has not been reaped. Once the waiter has collected the exit status the pid is free
/// for reuse, and signalling it could hit an unrelated process.
fn kill_unless_reaped(reaped: Arc<AtomicBool>, kill: impl Fn() + Send + Sync + 'static) -> Closer {
    Box::new(move || {
        if !reaped.load(Ordering::Acquire) {
            kill();
        }
    })
}

fn writer_loop(
    mut rx: UnboundedReceiver<Command>,
    mut writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
) {
    while let Some(cmd) = rx.blocking_recv() {
        match cmd {
            Command::Input(bytes) => {
                if writer
                    .write_all(&bytes)
                    .and_then(|()| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
            Command::Resize(size) => {
                let _ = master.resize(pty_size(size));
            }
            Command::Close => break,
        }
    }
    drop(writer);
    drop(master);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::{DEFAULT_LANG, kill_unless_reaped, session_env};

    fn get<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
        env.iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn lang_is_set_only_when_no_locale_is_chosen() {
        let none = BTreeMap::new();
        let env = session_env(&none, false);
        assert_eq!(get(&env, "LANG"), Some(DEFAULT_LANG));
        assert_eq!(get(&env, "TERM"), Some("xterm-256color"));
        assert_eq!(get(&env, "COLORTERM"), Some("truecolor"));

        // Inherited LANG, LC_ALL or LC_CTYPE (the caller checks those): left alone.
        assert_eq!(get(&session_env(&none, true), "LANG"), None);

        for key in ["LANG", "LC_ALL", "LC_CTYPE"] {
            let profile = BTreeMap::from([(key.to_owned(), "de_DE.UTF-8".to_owned())]);
            let env = session_env(&profile, false);
            assert_eq!(get(&env, key), Some("de_DE.UTF-8"), "{key}");
            if key != "LANG" {
                assert_eq!(get(&env, "LANG"), None, "{key} chosen by the profile");
            }
        }
        // An empty value chooses nothing.
        let empty = BTreeMap::from([("LC_ALL".to_owned(), String::new())]);
        assert_eq!(get(&session_env(&empty, false), "LANG"), Some(DEFAULT_LANG));
    }

    #[test]
    fn profile_env_overrides_terminal_type() {
        let profile = BTreeMap::from([("TERM".to_owned(), "vt100".to_owned())]);
        assert_eq!(get(&session_env(&profile, true), "TERM"), Some("vt100"));
    }

    #[test]
    fn closer_kills_only_before_the_child_is_reaped() {
        let reaped = Arc::new(AtomicBool::new(false));
        let kills = Arc::new(AtomicUsize::new(0));
        let closer = kill_unless_reaped(reaped.clone(), {
            let kills = kills.clone();
            move || {
                kills.fetch_add(1, Ordering::SeqCst);
            }
        });
        closer();
        assert_eq!(kills.load(Ordering::SeqCst), 1, "running child is killed");
        reaped.store(true, Ordering::Release);
        closer();
        closer();
        assert_eq!(
            kills.load(Ordering::SeqCst),
            1,
            "a reaped pid is never signalled"
        );
    }
}
