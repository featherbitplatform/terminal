//! Local PTY sessions.

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};

use fbterm_config::LocalProfile;
use fbterm_term::{GridSize, TermEvent, Terminal};
use parking_lot::Mutex;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::{Command, EventSink, SessionError, SessionEvent, SessionHandle, SessionId};

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

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| SessionError::Pty(e.to_string()))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| SessionError::Pty(e.to_string()))?;
    let mut killer = child.clone_killer();

    let id = SessionId::next();
    let terminal = Arc::new(Mutex::new(Terminal::new(size, scrollback)));
    let dirty_pending = Arc::new(AtomicBool::new(false));
    let (tx, rx) = channel::<Command>();

    std::thread::Builder::new()
        .name(format!("fbterm-pty-writer-{}", id.0))
        .spawn(move || writer_loop(rx, writer, pair.master, &mut *killer))?;

    let reader_tx = tx.clone();
    let reader_terminal = terminal.clone();
    let reader_dirty = dirty_pending.clone();
    std::thread::Builder::new()
        .name(format!("fbterm-pty-reader-{}", id.0))
        .spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                // EOF, or EIO once the child side closes on Unix: either way we are done.
                let n = match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
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
                            let _ = events.send((id, SessionEvent::Title(t)));
                        }
                        TermEvent::Bell => {
                            let _ = events.send((id, SessionEvent::Bell));
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
            let code = child.wait().ok().map(|s| s.exit_code() as i32);
            if !reader_dirty.swap(true, Ordering::AcqRel) {
                let _ = events.send((id, SessionEvent::Dirty));
            }
            let _ = events.send((id, SessionEvent::Exited(code)));
            // Release the master so the writer thread ends too.
            let _ = reader_tx.send(Command::Close);
        })?;

    Ok(SessionHandle {
        id,
        terminal,
        commands: tx,
        dirty_pending,
    })
}

fn writer_loop(
    rx: Receiver<Command>,
    mut writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    killer: &mut dyn portable_pty::ChildKiller,
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
    // Idempotent; a no-op if the child already exited.
    let _ = killer.kill();
    drop(writer);
    drop(master);
}
