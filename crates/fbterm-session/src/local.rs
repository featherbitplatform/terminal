//! Local PTY sessions.

use std::io::{ErrorKind, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use fbterm_config::LocalProfile;
use fbterm_term::{GridSize, TermEvent, Terminal};
use parking_lot::Mutex;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::{Command, EventSink, SessionError, SessionEvent, SessionHandle, SessionId};

/// How long the waiter lets the reader drain after the child exits before
/// announcing `Exited` anyway (a grandchild may still hold the PTY open).
const DRAIN_GRACE: Duration = Duration::from_millis(500);

fn pty_size(size: GridSize) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: (f32::from(size.cols) * size.cell_width) as u16,
        pixel_height: (f32::from(size.rows) * size.cell_height) as u16,
    }
}

/// Spawn the profile's program on a new PTY and feed its output into a shared terminal.
pub fn spawn_local(
    profile: &LocalProfile,
    size: GridSize,
    scrollback: usize,
    events: EventSink,
) -> Result<SessionHandle, SessionError> {
    let pair = native_pty_system()
        .openpty(pty_size(size))
        .map_err(|e| SessionError::Pty(e.to_string()))?;

    let mut cmd = CommandBuilder::new(&profile.program);
    cmd.args(&profile.args);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    for (k, v) in &profile.env {
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
    let killer = Arc::new(Mutex::new(child.clone_killer()));

    let id = SessionId::next();
    let terminal = Arc::new(Mutex::new(Terminal::new(size, scrollback)));
    let dirty_pending = Arc::new(AtomicBool::new(false));
    let bell_pending = Arc::new(AtomicBool::new(false));
    let (tx, rx) = channel::<Command>();
    let (done_tx, done_rx) = channel::<()>();

    std::thread::Builder::new()
        .name(format!("fbterm-pty-writer-{}", id.0))
        .spawn(move || writer_loop(rx, writer, pair.master))?;

    // Waiter: the only place `Exited` is produced, so it fires exactly once and
    // does not depend on the output pipe reaching EOF (ConPTY, lingering grandchildren).
    let waiter_tx = tx.clone();
    let waiter_events = events.clone();
    let waiter_dirty = dirty_pending.clone();
    std::thread::Builder::new()
        .name(format!("fbterm-pty-waiter-{}", id.0))
        .spawn(move || {
            let code = child.wait().ok().map(|s| s.exit_code() as i32);
            // Dropping the master closes the pseudoconsole, which unblocks the reader on Windows.
            let _ = waiter_tx.send(Command::Close);
            let _ = done_rx.recv_timeout(DRAIN_GRACE);
            if !waiter_dirty.swap(true, Ordering::AcqRel) {
                let _ = waiter_events.send((id, SessionEvent::Dirty));
            }
            let _ = waiter_events.send((id, SessionEvent::Exited(code)));
        })?;

    let reader_tx = tx.clone();
    let reader_terminal = terminal.clone();
    let reader_dirty = dirty_pending.clone();
    let reader_bell = bell_pending.clone();
    std::thread::Builder::new()
        .name(format!("fbterm-pty-reader-{}", id.0))
        .spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            let mut last_title: Option<String> = None;
            loop {
                // EOF, or EIO once the child side closes on Unix: either way we are done.
                let n = match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                let term_events = {
                    let mut term = reader_terminal.lock();
                    term.feed(&buf[..n]);
                    term.take_events()
                };
                for ev in term_events {
                    match ev {
                        TermEvent::PtyWrite(bytes) => {
                            let _ = reader_tx.send(Command::Input(bytes));
                        }
                        TermEvent::Title(t) => {
                            if last_title.as_deref() != Some(t.as_str()) {
                                last_title = Some(t.clone());
                                let _ = events.send((id, SessionEvent::Title(t)));
                            }
                        }
                        TermEvent::Bell => {
                            if !reader_bell.swap(true, Ordering::AcqRel) {
                                let _ = events.send((id, SessionEvent::Bell));
                            }
                        }
                        TermEvent::ClipboardStore(s) => {
                            let _ = events.send((id, SessionEvent::ClipboardStore(s)));
                        }
                        TermEvent::Exit => {}
                    }
                }
                if !reader_dirty.swap(true, Ordering::AcqRel) {
                    let _ = events.send((id, SessionEvent::Dirty));
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
        killer,
    })
}

fn writer_loop(
    rx: Receiver<Command>,
    mut writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
) {
    while let Ok(cmd) = rx.recv() {
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
