//! Do image escape sequences written by a local Windows program reach the terminal unchanged?
//!
//! The whole file is `cfg(windows)`: macOS and Linux compile nothing from it. The check runs only
//! with `FBTERM_TEST_CONPTY=1` set (the `FBTERM_TEST_SSH` pattern); otherwise the test prints one
//! line and returns, so an ordinary `cargo test --workspace` never opens a pseudo-console. Set the
//! variable when `conpty.dll` and `OpenConsole.exe` sit next to the test executable: portable-pty
//! prefers that sideloaded pair over the system console layer (`win/psuedocon.rs`), and this test
//! measures whichever one it gets, so it refuses to run at all without them.
//! `.github/workflows/ci.yml`'s "ConPTY passthrough (bundled)" step builds the test, copies the
//! executable into a fresh folder together with the files `dev/fetch-conpty.ps1` stages, and runs the
//! copy with `--nocapture`, which is how its byte-level evidence reaches the CI log: a passing run's
//! own output stays behind libtest's capture. The gated run first applies the same DLL-search
//! hardening fbterm's `main` applies, and after opening the pseudo-console it reports which
//! `conpty.dll` the process really loaded, so the step measures the bundled host under production's
//! lookup rather than trusting that a staged file was used.
//!
//! The child is this same test executable, re-spawned through portable-pty with
//! `FBTERM_CONPTY_HELPER=1` and asked to run only [`helper_writes_image_sequences`], which writes
//! three small image sequences to its stdout. Each one sits between unique ASCII markers; the parent
//! reads the master side and looks for the sequence's exact bytes inside its own pair of markers.
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

/// Set to `1` to run the check at all. Without it the test prints one line and returns.
const RUN_ENV: &str = "FBTERM_TEST_CONPTY";

/// Set in the child so [`helper_writes_image_sequences`] writes instead of returning at once.
const HELPER_ENV: &str = "FBTERM_CONPTY_HELPER";

/// The helper's test name, handed to libtest with `--exact`. A rename leaves the child running no
/// test; [`NO_TESTS_RUN`] catches that within a poll of the child starting rather than after the
/// whole deadline.
const HELPER_TEST: &str = "helper_writes_image_sequences";

/// What libtest prints when its filter matched nothing, which is what a renamed helper looks like
/// from here. Caught early so the failure says that instead of blaming ConPTY for silence.
const NO_TESTS_RUN: &[u8] = b"running 0 tests";

/// The two files portable-pty sideloads when they sit next to its executable, named by it exactly
/// like this. Their presence is what makes a run a measurement of the bundled ConPTY rather than the
/// system one, so [`image_sequences_pass_through_conpty`] insists on them first.
const CONPTY_DLL: &str = "conpty.dll";
const OPEN_CONSOLE: &str = "OpenConsole.exe";

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

/// Restricts this process's DLL search to the application folder and System32, exactly as fbterm's
/// `main` does (`crates/fbterm/src/main.rs`, `harden_dll_search`). Called first in a gated run, so
/// what this measures is the lookup the shipped program performs — the staged `conpty.dll` found as
/// the executable's own, not one the default search order might have found in PATH. The failure
/// return is ignored for production's reason: it cannot fail on the Windows versions fbterm runs on.
#[cfg(windows)]
fn harden_dll_search() {
    use windows_sys::Win32::System::LibraryLoader::{
        LOAD_LIBRARY_SEARCH_APPLICATION_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories,
    };
    // BOOL: zero is the only failure.
    unsafe {
        SetDefaultDllDirectories(
            LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        );
    }
}

/// The full path of the `conpty.dll` this process has loaded, or `None` when nothing with that name
/// is loaded or its path cannot be read. `GetModuleHandleW` matches an already-loaded module by base
/// name and loads nothing; `GetModuleFileNameW` then says where the loader actually found it — a copy
/// in some other searched folder would report that path here rather than pass unnoticed. With long
/// paths enabled a real path can exceed MAX_PATH, so a buffer that comes back full is doubled and
/// retried up to the longest path Windows allows; only a failure or a path beyond even that reads as
/// nothing loaded.
#[cfg(windows)]
fn loaded_conpty_dll() -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};

    let wide: Vec<u16> = std::ffi::OsStr::new(CONPTY_DLL)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let module = unsafe { GetModuleHandleW(wide.as_ptr()) };
    if module.is_null() {
        return None;
    }
    // Start at MAX_PATH code units plus the terminator, what the API documents as enough. A result
    // of exactly the buffer length means truncation: double it and ask again, up to 32768 units,
    // the longest path Windows allows, so a long path enabled on the volume still reports its DLL
    // rather than reading as "nothing loaded". 0 means failure.
    let mut buf: Vec<u16> = vec![0; 261];
    let len = loop {
        let len = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) };
        if len == 0 || len as usize != buf.len() {
            break len;
        }
        if buf.len() >= 32768 {
            return None;
        }
        buf = vec![0; (buf.len() * 2).min(32768)];
    };
    // 0 means failure.
    if len == 0 {
        return None;
    }
    let end = buf[..len as usize]
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(len as usize);
    Some(std::ffi::OsString::from_wide(&buf[..end]).into())
}

fn enabled() -> bool {
    std::env::var(RUN_ENV).as_deref() == Ok("1")
}

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

/// The bytes a probe's markers enclose, or `None` when its end marker never appears after its start.
fn enclosed<'a>(stream: &'a [u8], p: &Probe) -> Option<&'a [u8]> {
    let head = find(stream, p.start)? + p.start.len();
    let tail = find(&stream[head..], p.end)?;
    Some(&stream[head..head + tail])
}

/// Whether every sequence has arrived intact somewhere inside its own markers. ConPTY may add bytes
/// around it — cursor visibility codes, for instance — and that is not a failure, so this is the
/// condition the wait loop stops on: it never judges a capture that is still mid-sequence.
fn complete(stream: &[u8]) -> bool {
    CASES
        .iter()
        .all(|p| enclosed(stream, p).is_some_and(|region| find(region, p.seq).is_some()))
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
    if !enabled() {
        println!(
            "skipped: set {RUN_ENV}=1 with {CONPTY_DLL} and {OPEN_CONSOLE} beside this executable \
             (see dev/fetch-conpty.ps1 and the ConPTY step in .github/workflows/ci.yml)"
        );
        return;
    }

    // Before anything can load a DLL, as production does: the check below is about which conpty.dll
    // this locked-down search finds. (The whole file is `cfg(windows)`, so no cfg attribute here.)
    harden_dll_search();

    let exe = std::env::current_exe().expect("this test's own executable");
    let dir = exe
        .parent()
        .expect("the test executable is in a directory")
        .to_path_buf();
    // Without them portable-pty uses the system console layer and this measures that instead,
    // silently: refuse rather than report the system ConPTY as though it were the bundled one.
    for name in [CONPTY_DLL, OPEN_CONSOLE] {
        assert!(
            dir.join(name).is_file(),
            "{name} is not next to {} ({RUN_ENV}=1 asks for the bundled ConPTY, and portable-pty \
             falls back to the system one without it). Stage it with: pwsh dev/fetch-conpty.ps1 \
             -OutDir {}",
            dir.display(),
            dir.display()
        );
    }
    // LoadLibraryW on the bare name searches the executable's own directory first, so these are the
    // files portable-pty gets.
    println!(
        "bundled ConPTY: {CONPTY_DLL} and {OPEN_CONSOLE} next to {}",
        dir.display()
    );

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open a pseudo-console");

    // Opening a pseudo-console needs the conpty functions, and portable-pty loads them on first use
    // (`win/psuedocon.rs`), so by here this process has loaded some `conpty.dll`. Say which: a null
    // handle means it never loaded the staged file at all (portable-pty's silent fallback to
    // kernel32 — nothing of the bundled host was measured), and a path from another searched folder
    // would mean a copy outside the executable's own was used. Windows compares paths
    // case-insensitively, so this does too; the loaded path goes into the log either way.
    let loaded = loaded_conpty_dll().unwrap_or_else(|| {
        panic!(
            "no conpty.dll is loaded after opening a pseudo-console, so portable-pty fell back \
                to the system console layer and nothing of the bundled one was measured"
        )
    });
    let loaded_dir = loaded
        .parent()
        .expect("a loaded module has a path with a directory");
    assert_eq!(
        loaded_dir.to_string_lossy().to_lowercase(),
        dir.to_string_lossy().to_lowercase(),
        "{CONPTY_DLL} was loaded from {loaded_dir:?}, not from the test executable's own folder \
         ({dir:?})"
    );
    println!("loaded {CONPTY_DLL} from {loaded:?}");

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
            (
                complete(&bytes) || find(&bytes, NO_TESTS_RUN).is_some(),
                count(&bytes, CURSOR_QUERY),
            )
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
        // What matters is the sequence's own bytes arriving intact somewhere inside its markers:
        // conhost forwards an unknown sequence as soon as it parses one and renders the text around
        // it afterwards, so it can add its own codes on either side of it. Those extras are worth
        // reading in the log but they do not fail the run.
        let span = enclosed(&bytes, p);
        if let Some((span, at)) = span.and_then(|span| find(span, p.seq).map(|at| (span, at))) {
            let (before, after) = (&span[..at], &span[at + p.seq.len()..]);
            if before.is_empty() && after.is_empty() {
                println!(
                    "{name}: unchanged, all {} bytes arrived between its markers",
                    p.seq.len()
                );
            } else {
                println!(
                    "{name}: unchanged, all {} bytes arrived between its markers; ConPTY added {} \
                     byte(s) around it: before {before:?}, after {after:?}",
                    p.seq.len(),
                    before.len() + after.len()
                );
            }
            continue;
        }
        problems.push(name);
        let note = match span {
            Some(span) => format!("its markers held {span:?} ({} bytes)", span.len()),
            None => "its end marker never appeared after its start".to_owned(),
        };
        // A sequence that arrived whole but outside its markers moved relative to the text around
        // it, which is not the same as being stripped; only report it as missing when its exact
        // bytes are nowhere in the capture at all.
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
