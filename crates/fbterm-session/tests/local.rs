use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use alacritty_terminal::index::{Column, Line};
use fbterm_config::LocalProfile;
use fbterm_session::{EventSink, SessionEvent, SessionHandle, SessionId, spawn_local};
use fbterm_term::GridSize;
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
                    h.ack_dirty();
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
