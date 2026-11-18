//! The start-up log line about which ConPTY host local shells get (`conpty.rs`). A pure function of
//! a directory, so both lines are checked here on every system: `tempfile` makes one with the DLL
//! beside `fbterm.exe`, and one without it.

use crate::conpty;

#[test]
fn status_line_names_the_bundled_host_when_the_dll_is_there() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(conpty::DLL), "not really a dll").unwrap();
    assert_eq!(
        conpty::status_line(dir.path()),
        "local shells use the bundled ConPTY"
    );
}

#[test]
fn status_line_names_the_system_host_and_what_it_drops_without_the_dll() {
    let dir = tempfile::tempdir().unwrap();
    // A folder of its own, holding nothing: what a portable run or an old install looks like.
    assert_eq!(
        conpty::status_line(dir.path()),
        "conpty.dll not found beside fbterm.exe: local shells use the system ConPTY, which drops \
         kitty and sixel images"
    );
}
