//! The ConPTY host fbterm's local shells run under on Windows.
//!
//! portable-pty prefers a `conpty.dll` sitting in its own executable's folder over the system
//! console layer (`win/psuedocon.rs`), and `.msi` installs that file beside `fbterm.exe`
//! (`crates/fbterm/wix/main.wxs`, staged by `dev/fetch-conpty.ps1`). Which one a run gets decides
//! whether images from local shells work at all: the system console layer drops the kitty APC and
//! sixel DCS sequences a program writes, while the bundled one passes all three protocols through
//! (measured by `.github/workflows/ci.yml`'s "ConPTY passthrough (bundled)" step). So the log's
//! first lines say which host this is.

/// The file portable-pty sideloads when it sits next to its executable, named by it exactly like
/// this (`win/psuedocon.rs`: `LoadLibraryW` on the bare name).
pub const DLL: &str = "conpty.dll";

/// The start-up log line about that file, for a run whose local shells are started from `exe_dir`
/// (the folder of `std::env::current_exe()`). Written as a pure function of the directory so both
/// lines are unit-testable on any system (`conpty_tests.rs`).
pub fn status_line(exe_dir: &std::path::Path) -> String {
    if exe_dir.join(DLL).is_file() {
        "local shells use the bundled ConPTY".to_owned()
    } else {
        // The installer puts conpty.dll here; its absence means a portable run, or an install of a
        // build that predates it. Naming what is lost is what makes the line worth reading in a
        // report about images not showing up.
        format!(
            "{DLL} not found beside fbterm.exe: local shells use the system ConPTY, which drops \
             kitty and sixel images"
        )
    }
}
