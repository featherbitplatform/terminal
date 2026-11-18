//! The ConPTY host fbterm's local shells run under on Windows.
//!
//! portable-pty prefers a `conpty.dll` sitting in its own executable's folder over the system
//! console layer (`win/psuedocon.rs`), and that DLL hosts child sessions in `OpenConsole.exe` from
//! the same folder; the `.msi` installs both beside `fbterm.exe` (`crates/fbterm/wix/main.wxs`,
//! staged by `dev/fetch-conpty.ps1`). Which host a run gets decides whether images from local shells
//! work at all: the system console layer drops the kitty APC and sixel DCS sequences a program
//! writes, while the bundled one passes all three protocols through (measured by
//! `.github/workflows/ci.yml`'s "ConPTY passthrough (bundled)" step). So the log's first lines say
//! which host this is.

/// The file portable-pty sideloads when it sits next to its executable, named by it exactly like
/// this (`win/psuedocon.rs`: `LoadLibraryW` on the bare name).
pub const CONPTY_DLL: &str = "conpty.dll";

/// The process `conpty.dll` starts for each session, from its own folder. Without it the DLL falls
/// back to the inbox console host, so a partial copy behaves like no copy at all — which is why
/// [`status_line`] checks both files.
pub const CONSOLE_HOST: &str = "OpenConsole.exe";

/// The consequence named in every line that is not the bundled one: what a user reporting missing
/// images from a local shell has to see.
const DROPS: &str = "local shells use the system ConPTY, which drops kitty and sixel images";

/// The start-up log line about those two files, for a run whose local shells are started from
/// `exe_dir` (the folder of `std::env::current_exe()`). Written as a pure function of the directory
/// so every branch is unit-testable on any system (`conpty_tests.rs`). "Bundled" is claimed only
/// when both files are there: with only one of them the DLL falls back to the inbox host anyway.
pub fn status_line(exe_dir: &std::path::Path) -> String {
    let dll = exe_dir.join(CONPTY_DLL).is_file();
    let host = exe_dir.join(CONSOLE_HOST).is_file();
    if dll && host {
        "local shells use the bundled ConPTY".to_owned()
    } else if !dll {
        // The pair's anchor is missing (or both are): the line names conpty.dll, because that is
        // the file portable-pty looks for and the one whose absence means the system host outright.
        format!("{CONPTY_DLL} not found beside fbterm.exe: {DROPS}")
    } else {
        format!(
            "{CONSOLE_HOST} not found beside fbterm.exe: conpty.dll falls back to the inbox \
                 console host, so {DROPS}"
        )
    }
}
