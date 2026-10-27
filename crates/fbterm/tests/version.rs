//! `fbterm --version`, as the package smoke tests run it.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn version_flag_prints_version() {
    // Should the flag ever be ignored, the app must not touch the real config or keychain,
    // and the test must fail rather than wait for a window to close.
    let config = tempfile::tempdir().expect("temporary folder");
    let mut child = Command::new(env!("CARGO_BIN_EXE_fbterm"))
        .arg("--version")
        .env("FBTERM_CONFIG_DIR", config.path())
        .env("FBTERM_SECRETS", "memory")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("fbterm runs");
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().expect("fbterm is waited for") {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("fbterm --version did not exit");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut out = String::new();
    child
        .stdout
        .take()
        .expect("piped")
        .read_to_string(&mut out)
        .expect("utf-8 output");
    assert!(status.success(), "{status:?}");
    assert_eq!(out, format!("fbterm {}\n", env!("CARGO_PKG_VERSION")));
}
