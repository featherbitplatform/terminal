//! Do image escape sequences written by a local Windows program reach the terminal unchanged?
//!
//! The whole file is `cfg(windows)`: macOS and Linux compile nothing from it, and its answer comes
//! from the `windows-latest` CI job (`cargo test --workspace`). A passing test's own output stays
//! behind libtest's capture, so read the byte-level evidence with `--nocapture`:
//!
//! ```sh
//! cargo test -p fbterm-session --test conpty_passthrough -- --nocapture
//! ```
//!
//! On Windows portable-pty's native pty system *is* ConPTY, so the child's console is the thing
//! under test. The child is this same test executable, re-spawned with `FBTERM_CONPTY_HELPER=1` and
//! asked to run only [`helper_writes_image_sequences`], which writes three small image sequences to
//! its stdout. Each one sits between unique ASCII markers; the parent reads the master side and
//! compares what arrived between a pair of markers, byte for byte, with what was written.
//!
//! The parent also answers ConPTY's start-up cursor position query, because portable-pty asks for a
//! pseudo-console that inherits the cursor (`win/psuedocon.rs`) and such a ConPTY holds the child's
//! output until its host replies. fbterm answers the same query through feathercritty.

#![cfg(windows)]

use std::io::{ErrorKind, Read, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// Set in the child so [`helper_writes_image_sequences`] writes instead of returning at once.
const HELPER_ENV: &str = "FBTERM_CONPTY_HELPER";

/// The helper's test name, handed to libtest with `--exact`. A rename leaves the child running no
/// test; [`NO_TESTS_RUN`] catches that within a poll of the child starting rather than after the
/// whole deadline.
const HELPER_TEST: &str = "helper_writes_image_sequences";

/// What libtest prints when its filter matched nothing, which is what a renamed helper looks like
/// from here. Caught early so the failure says that instead of blaming ConPTY for silence.
const NO_TESTS_RUN: &[u8] = b"running 0 tests";

/// The query ConPTY writes to the master side at start-up when the pseudo-console was created to
/// inherit the cursor, as portable-pty does (`win/psuedocon.rs`): it wants the cursor position
/// before it releases the child's output. fbterm answers it through feathercritty, so answering it
/// here is what production does, and not answering it would read as ConPTY eating everything.
const CURSOR_QUERY: &[u8] = b"\x1b[6n";

/// The answer to [`CURSOR_QUERY`]: row 1, column 1, where a fresh pseudo-console's cursor sits.
const CURSOR_REPORT: &[u8] = b"\x1b[1;1R";

/// How long the parent waits for all three sequences to arrive and for the child to exit. ConPTY is
/// a real process pair on a shared CI runner, so give it room; the deadline is what keeps a stuck
/// child from hanging the job.
const WAIT: Duration = Duration::from_secs(30);

/// How often the parent looks at the buffer, answers any query in it and checks the child.
const POLL: Duration = Duration::from_millis(50);

/// One sequence to look for: a name for the log, the marker it starts after, the sequence itself
/// and the marker it ends before. The markers are plain ASCII, so nothing else in the stream
/// (libtest's own lines included) can be mistaken for one. `iVBL` in two of them is base64 for the
/// three bytes 0x89 0x50 0x4b — a PNG signature's first three — spelled out so the test needs no
/// base64 crate.
struct Probe {
    name: &'static str,
    start: &'static [u8],
    seq: &'static [u8],
    end: &'static [u8],
}

const CASES: [Probe; 3] = [
    // A kitty graphics transmission and placement in one APC sequence: `a=T` with a 1x1 raw RGB
    // image (`f=24`, so `s=1,v=1` is 3 bytes) under image id 31, ended by ST as the protocol asks.
    Probe {
        name: "kitty APC",
        start: b"<<K>>",
        seq: b"\x1b_Ga=T,f=24,s=1,v=1,i=31;iVBL\x1b\\",
        end: b"<</K>>",
    },
    // A sixel DCS: the `q` introducer, raster attributes, colour register 0 defined as red and
    // selected, graphics start, ended by ST.
    Probe {
        name: "sixel DCS",
        start: b"<<S>>",
        seq: b"\x1bPq\"1;1;1;1#0;2;100;0;0#0~\x1b\\",
        end: b"<</S>>",
    },
    // An OSC 1337 inline image with a 3-byte payload, ended by BEL.
    Probe {
        name: "OSC 1337",
        start: b"<<O>>",
        seq: b"\x1b]1337;File=inline=1;size=3:iVBL\x07",
        end: b"<</O>>",
    },
];

/// What the helper writes: every sequence between its markers, one per line. It is written in one
/// `write` and one `flush`, so it leaves the child in a single console write and nothing can slip
/// inside a sequence between two writes.
fn payload() -> Vec<u8> {
    let mut out = Vec::new();
    for p in &CASES {
        out.extend_from_slice(p.start);
        out.extend_from_slice(p.seq);
        out.extend_from_slice(p.end);
        out.push(b'\n');
    }
    out
}

/// Where `needle` first appears in `hay`, if it fits at all (`windows` refuses to iterate over a
/// slice shorter than the window it is asked for).
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// How many times `needle` appears in `hay`. Each ConPTY cursor query needs its own answer, so the
/// parent counts them and answers that many.
fn count(hay: &[u8], needle: &[u8]) -> usize {
    let (mut hits, mut at) = (0usize, 0usize);
    while let Some(off) = find(&hay[at..], needle) {
        hits += 1;
        at += off + needle.len();
    }
    hits
}

/// The bytes between a probe's markers, or `None` when either marker is missing.
fn arrived<'a>(stream: &'a [u8], p: &Probe) -> Option<&'a [u8]> {
    let head = find(stream, p.start)? + p.start.len();
    let tail = find(&stream[head..], p.end)?;
    Some(&stream[head..head + tail])
}

/// Writes the three sequences, or does nothing. The parent test re-spawns this executable with
/// [`HELPER_ENV`] set and runs only this test; in a normal run it returns at once.
#[test]
fn helper_writes_image_sequences() {
    if std::env::var_os(HELPER_ENV).is_none() {
        return;
    }
    let mut stdout = std::io::stdout();
    // The payload is ASCII, so whatever encoding the child's console uses cannot change a byte of it.
    stdout.write_all(&payload()).expect("write the sequences");
    stdout.flush().expect("flush the sequences");
}

/// What a local Windows program writes as an image sequence has to reach the terminal unchanged:
/// ConPTY passes all three protocols through, kitty APC, sixel DCS and OSC 1337 alike.
#[test]
fn image_sequences_pass_through_conpty() {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open a pseudo-console");

    let exe = std::env::current_exe().expect("this test's own executable");
    let mut cmd = CommandBuilder::new(&exe);
    cmd.args(["--exact", HELPER_TEST, "--nocapture", "--test-threads=1"]);
    cmd.env(HELPER_ENV, "1");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .unwrap_or_else(|e| panic!("spawn {} through ConPTY: {e}", exe.display()));
    // The child holds its own copy of the slave side; keeping ours would hold the pty open.
    drop(pair.slave);

    // Taken before reading, and kept open until every query in the output has been answered below.
    let mut writer = pair.master.take_writer().expect("take the writer");
    let mut reader = pair.master.try_clone_reader().expect("clone the reader");

    // A master read blocks until bytes or EOF arrive, so it runs beside this thread and shares
    // everything it gets. The reader ends when the pseudo-console closes and its read returns EOF.
    let received = Arc::new(Mutex::new(Vec::new()));
    {
        let received = received.clone();
        std::thread::Builder::new()
            .name("fbterm-conpty-probe".to_owned())
            .spawn(move || {
                let mut chunk = [0u8; 4096];
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => received.lock().extend_from_slice(&chunk[..n]),
                        Err(e) if e.kind() == ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
            })
            .expect("spawn the reader thread");
    }

    let deadline = Instant::now() + WAIT;
    let mut answered = 0usize;
    loop {
        // One snapshot: whether the capture is complete (or shows the child ran no test), and how
        // many cursor queries it holds so far.
        let (finished, asked) = {
            let bytes = received.lock();
            let done = find(&bytes, NO_TESTS_RUN).is_some()
                || CASES
                    .iter()
                    .all(|p| find(&bytes, p.start).is_some() && find(&bytes, p.end).is_some());
            (done, count(&bytes, CURSOR_QUERY))
        };
        // Answer every query before giving up on the output: ConPTY withholds that output until it
        // has the report, so this is what makes the markers arrive at all.
        while answered < asked {
            writer
                .write_all(CURSOR_REPORT)
                .expect("answer ConPTY's cursor position query");
            writer.flush().expect("flush the cursor position answer");
            answered += 1;
        }
        if finished || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(POLL);
    }

    // The helper exits as soon as it has written. Poll instead of `wait`, which has no timeout, so
    // a child that never exits costs this test its deadline rather than the whole job.
    let mut status = child.try_wait().ok().flatten();
    while status.is_none() && Instant::now() < deadline {
        std::thread::sleep(POLL);
        status = child.try_wait().ok().flatten();
    }
    if status.is_none() {
        // Only reached when the child hung: the helper writes and returns. Stop it so the job can
        // finish, and let the evidence printed below show what did arrive.
        let _ = child.kill();
    }

    // ConPTY delivers EOF only once nothing is left writing to the pseudo-console, so drop our
    // writer and master (the child's copy went with it) rather than leave the reader blocked.
    drop(writer);
    drop(pair.master);

    let bytes = std::mem::take(&mut *received.lock());
    assert!(
        find(&bytes, NO_TESTS_RUN).is_none(),
        "the child ran no test (libtest said {:?}), so {HELPER_TEST:?} is not the helper's test \
         name any more; nothing of ConPTY was measured",
        String::from_utf8_lossy(NO_TESTS_RUN),
    );

    // Say once what became of the child, because `ExitStatus` is not `Copy` and the loop below
    // reports per sequence. `None` here means this test killed it after its deadline.
    let outcome = match status {
        Some(s) => format!("exited {}", s.exit_code()),
        None => "had not exited when its deadline passed".to_owned(),
    };
    println!("child: {outcome}");

    // One line on the query, because output held back by an unanswered query says nothing about
    // image sequences: `asked > answered` means this run was inconclusive, not that ConPTY ate them.
    let asked = count(&bytes, CURSOR_QUERY);
    println!(
        "cursor position query: ConPTY sent {asked}, this test answered {answered}{}",
        if asked == 0 {
            " (this conhost did not ask)"
        } else if answered < asked {
            ": its output may have been held back, so this run proves nothing"
        } else {
            ""
        }
    );

    let mut problems = Vec::new();
    if asked > answered {
        problems.push("cursor position query left unanswered");
    }
    for p in &CASES {
        let name = p.name;
        // Three verdicts, because conhost forwards an unknown sequence as soon as it parses one and
        // renders the text around it afterwards: identical between the markers, present elsewhere in
        // the capture (moved, which is not stripped), or gone.
        let span = arrived(&bytes, p);
        if span.is_some_and(|got| got == p.seq) {
            println!(
                "{name}: unchanged, all {} bytes arrived between its markers",
                p.seq.len()
            );
            continue;
        }
        problems.push(name);
        let inside = span.unwrap_or_default();
        let note = match span {
            Some(_) => format!(
                "its markers held {inside:?} ({} of {} bytes)",
                inside.len(),
                p.seq.len()
            ),
            None => "its markers never enclosed anything".to_owned(),
        };
        if find(&bytes, p.seq).is_some() {
            println!(
                "{name}: moved, not stripped: its sequence is elsewhere in the capture; {note}"
            );
        } else {
            println!("{name}: missing: its sequence appears nowhere in the capture; {note}");
        }
    }
    assert!(
        problems.is_empty(),
        "ConPTY did not deliver everything unchanged: {problems:?}\nwhat the master read: {:?}",
        String::from_utf8_lossy(&bytes)
    );
}
