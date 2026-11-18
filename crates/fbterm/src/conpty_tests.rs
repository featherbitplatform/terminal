//! The start-up log line about which ConPTY host local shells get (`conpty.rs`). A pure function of
//! a directory, so every branch is checked here on every system: `tempfile` makes the folder with
//! both files beside `fbterm.exe`, with neither, and with only one of them.

use crate::conpty;

/// A temporary folder holding exactly these two names as files.
fn dir_with(names: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for name in names {
        std::fs::write(dir.path().join(name), "not really the real file").unwrap();
    }
    dir
}

#[test]
fn status_line_names_the_bundled_host_only_when_both_files_are_there() {
    let dir = dir_with(&[conpty::CONPTY_DLL, conpty::CONSOLE_HOST]);
    assert_eq!(
        conpty::status_line(dir.path()),
        "local shells use the bundled ConPTY"
    );
}

#[test]
fn status_line_names_the_system_host_and_what_it_drops_without_either_file() {
    let dir = dir_with(&[]);
    assert_eq!(
        conpty::status_line(dir.path()),
        "conpty.dll not found beside fbterm.exe: local shells use the system ConPTY, which drops \
         kitty and sixel images"
    );
}

#[test]
fn status_line_names_the_missing_host_when_only_the_dll_is_there() {
    // The case a user with missing images actually hits after a partial copy or portable unzip:
    // the DLL is there, but it falls back to the inbox host without OpenConsole.exe beside it.
    let dir = dir_with(&[conpty::CONPTY_DLL]);
    assert_eq!(
        conpty::status_line(dir.path()),
        "OpenConsole.exe not found beside fbterm.exe: conpty.dll falls back to the inbox console \
         host, so local shells use the system ConPTY, which drops kitty and sixel images"
    );
}

#[test]
fn status_line_names_the_dll_when_only_the_host_is_there() {
    // Without the DLL there is no sideload at all: the line names conpty.dll, the file
    // portable-pty looks for.
    let dir = dir_with(&[conpty::CONSOLE_HOST]);
    assert_eq!(
        conpty::status_line(dir.path()),
        "conpty.dll not found beside fbterm.exe: local shells use the system ConPTY, which drops \
         kitty and sixel images"
    );
}
