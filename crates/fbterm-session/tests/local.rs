use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use alacritty_terminal::index::{Column, Line};
use fbterm_config::LocalProfile;
#[cfg(unix)]
use fbterm_session::spawn_local_into;
use fbterm_session::{EventSink, SessionEvent, SessionHandle, SessionId, spawn_local};
use fbterm_term::GridSize;
#[cfg(unix)]
use fbterm_term::Terminal;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time::timeout;

const LIMIT: Duration = Duration::from_secs(10);

fn size(cols: u16, rows: u16) -> GridSize {
    GridSize {
        cols,
        rows,
        cell_width: 8.0,
        cell_height: 16.0,
    }
}

fn profile(program: &str, args: &[&str]) -> LocalProfile {
    LocalProfile {
        program: PathBuf::from(program),
        args: args.iter().map(|s| s.to_string()).collect(),
        cwd: None,
        env: BTreeMap::new(),
    }
}

fn grid_text(handle: &SessionHandle) -> String {
    let t = handle.terminal.lock();
    let g = t.term().grid();
    let size = t.size();
    let mut out = String::new();
    for row in 0..size.rows as i32 {
        for col in 0..size.cols as usize {
            out.push(g[Line(row)][Column(col)].c);
        }
        out.push('\n');
    }
    out
}

fn start(
    p: &LocalProfile,
    s: GridSize,
) -> (SessionHandle, UnboundedReceiver<(SessionId, SessionEvent)>) {
    let (tx, rx): (EventSink, _) = unbounded_channel();
    let h = spawn_local(p, s, 100, tx).expect("spawn");
    (h, rx)
}

async fn wait_exit(rx: &mut UnboundedReceiver<(SessionId, SessionEvent)>) -> Option<i32> {
    timeout(LIMIT, async {
        loop {
            match rx.recv().await {
                Some((_, SessionEvent::Exited(code))) => return code,
                Some(_) => {}
                None => panic!("sink closed before Exited"),
            }
        }
    })
    .await
    .expect("timed out waiting for Exited")
}

#[cfg(unix)]
async fn wait_for_text(handle: &SessionHandle, needle: &str, count: usize) {
    timeout(LIMIT, async {
        loop {
            if grid_text(handle).matches(needle).count() >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {needle:?}: {}", grid_text(handle)));
}

#[tokio::test]
async fn echo_reaches_terminal() {
    #[cfg(unix)]
    let p = profile("sh", &["-c", "echo fbterm-ok"]);
    #[cfg(windows)]
    let p = profile("cmd", &["/C", "echo fbterm-ok"]);
    let (h, mut rx) = start(&p, size(80, 24));
    assert_eq!(wait_exit(&mut rx).await, Some(0));
    assert!(grid_text(&h).contains("fbterm-ok"), "{}", grid_text(&h));
}

#[cfg(unix)]
#[tokio::test]
async fn input_reaches_process() {
    let (h, _rx) = start(&profile("cat", &[]), size(80, 24));
    h.send(b"abc\n".to_vec());
    wait_for_text(&h, "abc", 2).await;
    h.close();
}

#[cfg(unix)]
#[tokio::test]
async fn resize_reaches_process() {
    let (h, mut rx) = start(
        &profile("sh", &["-c", "sleep 0.5; stty size"]),
        size(80, 24),
    );
    h.resize(size(100, 30));
    assert_eq!(wait_exit(&mut rx).await, Some(0));
    assert!(grid_text(&h).contains("30 100"), "{}", grid_text(&h));
}

#[cfg(unix)]
#[tokio::test]
async fn flood_coalesces_dirty_events() {
    let (h, mut rx) = start(
        &profile("sh", &["-c", "yes fbterm-flood | head -c 52428800"]),
        size(80, 24),
    );
    let mut dirty = 0usize;
    let code = timeout(Duration::from_secs(60), async {
        loop {
            match rx.recv().await.expect("sink closed").1 {
                SessionEvent::Dirty => {
                    dirty += 1;
                    // No second Dirty may be queued while one is unacknowledged.
                    while let Ok((_, ev)) = rx.try_recv() {
                        assert!(!matches!(ev, SessionEvent::Dirty), "two pending Dirty");
                        if let SessionEvent::Exited(c) = ev {
                            return c;
                        }
                    }
                    // Model a UI redrawing at ~60 Hz.
                    tokio::time::sleep(Duration::from_millis(16)).await;
                    h.take_dirty();
                }
                SessionEvent::Exited(c) => return c,
                _ => {}
            }
        }
    })
    .await
    .expect("flood timed out");
    assert_eq!(code, Some(0));
    assert!(dirty > 0 && dirty < 2_000, "dirty events: {dirty}");
}

#[cfg(unix)]
#[tokio::test]
async fn close_ends_session_and_drop_is_safe() {
    let (h, mut rx) = start(&profile("sleep", &["30"]), size(80, 24));
    h.close();
    wait_exit(&mut rx).await;
    drop(h);
    let (h2, _rx2) = start(&profile("sleep", &["30"]), size(80, 24));
    drop(h2);
}

#[cfg(unix)]
#[tokio::test]
async fn output_after_ack_rearms_dirty() {
    let (h, mut rx) = start(&profile("cat", &[]), size(80, 24));
    h.send(b"one\n".to_vec());
    next_dirty(&mut rx).await;
    assert!(h.take_dirty());
    // Drain Dirty events caused by echo/tty output of the first write.
    wait_for_text(&h, "one", 2).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    while rx.try_recv().is_ok() {}
    h.take_dirty();
    h.send(b"two\n".to_vec());
    next_dirty(&mut rx).await;
    h.close();
}

#[cfg(unix)]
async fn next_dirty(rx: &mut UnboundedReceiver<(SessionId, SessionEvent)>) {
    timeout(LIMIT, async {
        loop {
            if let Some((_, SessionEvent::Dirty)) = rx.recv().await {
                return;
            }
        }
    })
    .await
    .expect("no Dirty");
}

#[cfg(unix)]
#[tokio::test]
async fn exit_fires_even_if_grandchild_holds_pty() {
    let (h, mut rx) = start(
        &profile("sh", &["-c", "sleep 30 & echo done"]),
        size(80, 24),
    );
    assert_eq!(wait_exit(&mut rx).await, Some(0));
    assert!(grid_text(&h).contains("done"));
}

#[cfg(unix)]
#[tokio::test]
async fn close_kills_child_with_blocked_writer() {
    let (h, mut rx) = start(&profile("sleep", &["30"]), size(80, 24));
    h.send(vec![b'x'; 4 * 1024 * 1024]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    h.close();
    wait_exit(&mut rx).await;
}

#[cfg(unix)]
#[tokio::test]
async fn respawn_into_existing_terminal_keeps_old_content() {
    let mut term = Terminal::new(size(80, 24), 100);
    term.feed(b"old-session-line\r\n");
    let term = std::sync::Arc::new(parking_lot::Mutex::new(term));
    let (tx, mut rx): (EventSink, _) = unbounded_channel();
    let h = spawn_local_into(
        &profile("sh", &["-c", "echo new-session-line"]),
        term.clone(),
        tx,
    )
    .expect("spawn");
    assert!(std::sync::Arc::ptr_eq(&h.terminal, &term));
    assert_eq!(wait_exit(&mut rx).await, Some(0));
    let text = grid_text(&h);
    let old = text.find("old-session-line").expect("old content kept");
    let new = text.find("new-session-line").expect("new output shown");
    assert!(old < new, "new output must follow the old content:\n{text}");
}

#[cfg(unix)]
#[tokio::test]
async fn respawn_ignores_output_from_old_sessions_orphans() {
    let term = std::sync::Arc::new(parking_lot::Mutex::new(Terminal::new(size(80, 24), 100)));
    let (tx, mut rx): (EventSink, _) = unbounded_channel();
    // The shell exits at once; its background child ignores the hangup, keeps the PTY and
    // writes later.
    let old = spawn_local_into(
        &profile(
            "sh",
            &["-c", "(trap '' HUP; sleep 1.5; echo ORPHAN) & exit 0"],
        ),
        term.clone(),
        tx,
    )
    .expect("spawn");
    assert_eq!(wait_exit(&mut rx).await, Some(0));
    // What the app does on Enter: close the old session, then continue in the same terminal.
    // The orphan writes at ~1.5 s, well after the 500 ms drain window that runs before Exited,
    // so its output lands after the old session is closed and after the respawn.
    drop(old);
    let (tx2, _rx2): (EventSink, _) = unbounded_channel();
    let new =
        spawn_local_into(&profile("sh", &["-c", "echo NEW; sleep 5"]), term, tx2).expect("respawn");
    wait_for_text(&new, "NEW", 1).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        !grid_text(&new).contains("ORPHAN"),
        "orphan output reached the reused terminal:\n{}",
        grid_text(&new)
    );
    new.close();
}

/// The same guarantee where the platform lets the old program keep writing: a child that
/// ignores the hangup `close` sends is still writing when the terminal is reused.
#[cfg(unix)]
#[tokio::test]
async fn closed_session_output_never_reaches_reused_terminal() {
    let term = std::sync::Arc::new(parking_lot::Mutex::new(Terminal::new(size(80, 24), 100)));
    let (tx, _rx): (EventSink, _) = unbounded_channel();
    let old = spawn_local_into(
        &profile(
            "sh",
            &["-c", "trap '' HUP; sleep 0.3; echo LINGER; sleep 3"],
        ),
        term.clone(),
        tx,
    )
    .expect("spawn");
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(old);
    let (tx2, _rx2): (EventSink, _) = unbounded_channel();
    let new =
        spawn_local_into(&profile("sh", &["-c", "echo NEW; sleep 2"]), term, tx2).expect("respawn");
    wait_for_text(&new, "NEW", 1).await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(
        !grid_text(&new).contains("LINGER"),
        "closed session's output reached the reused terminal:\n{}",
        grid_text(&new)
    );
    new.close();
}
