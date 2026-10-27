//! Windows builds: embeds the icon (`windows/fbterm.ico`, made by `dev/make-windows-icon.sh`)
//! and the version information in `fbterm.exe`, so Explorer, the taskbar and the installer's
//! shortcut show them. Every other target: nothing.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=windows/fbterm.ico");
    // The resource compiler runs on the build machine, which is always Windows for a Windows
    // build here (no cross-compiling), so the dependency exists only on Windows hosts.
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        windows_resources();
    }
}

#[cfg(windows)]
fn windows_resources() {
    let version = std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
    let mut res = winresource::WindowsResource::new();
    res.set_language(0x0409) // English (United States)
        .set_icon("windows/fbterm.ico")
        .set("ProductName", "featherbit terminal")
        .set("FileDescription", "featherbit terminal")
        .set("CompanyName", "featherbit")
        .set("LegalCopyright", "featherbit, licensed under Apache-2.0")
        .set("InternalName", "fbterm")
        .set("OriginalFilename", "fbterm.exe")
        .set("FileVersion", &version)
        .set("ProductVersion", &version);
    // Only the app binary carries the resources, not the test executables.
    res.compile_for("fbterm")
        .expect("embedding the icon and version information in fbterm.exe");
}
