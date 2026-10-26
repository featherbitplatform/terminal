# Linux and Windows releases: design

Status: approved by the owner, 2026-10-27. First part of milestone 6 of
`2026-10-23-featherbit-terminal-design.md` (sections 6 and 8), which this document refines.

## 1. Intent

fbterm builds and tests on Linux and Windows already, but only macOS gets a release. The owner
uses Linux machines (Debian- and RPM-based), and teammates use Windows 11. Each tagged release
should give them something they can install the way their system expects, and fbterm should
look and behave on those systems as it does on macOS: the tab bar is the title bar.

Success: on a release tag, the GitHub release has, next to the macOS `.dmg`, a `.deb`, an
`.rpm`, a `.tar.gz` and an `.msi`, each with a checksum; a teammate double-clicks the `.msi` on
Windows 11, starts fbterm from the Start menu and gets a window whose tab bar holds the minimize,
maximize and close buttons; the owner installs the `.deb` with the package manager and finds
fbterm in the app launcher.

## 2. Goals

1. **Linux packages** for x86-64 and ARM64 *(ARM64 my guess, approved)*: `.deb`, `.rpm` and a
   plain `.tar.gz`, each installing a desktop entry and icon (so fbterm appears in the app
   launcher *(my guess, approved)*) and declaring the system libraries fbterm needs (graphics,
   keyboard, secrets service) *(my guess, approved)*.
2. **Windows 11 installer**: an `.msi` for x86-64 with a Start menu entry and an uninstaller.
3. Every file has a SHA-256 checksum on the release, like the `.dmg`.
4. **Window chrome on Linux and Windows** (main design, section 6): no system title bar; the tab
   bar is the title bar. fbterm draws minimize, maximize and close buttons in the tab bar in the
   system's style: on the right on Windows, following the desktop's button layout and order on
   Linux. Dragging the bar moves the window, double-clicking it maximizes, and the window
   resizes from its edges.
5. **Background blur** where the system supports it: Windows 11's translucent window material
   and Linux compositors that offer blur; elsewhere the blur setting is unavailable (opacity
   still works).
6. Install docs for each system, including the SmartScreen warning for the unsigned `.msi`.

## 3. Non-goals (tracked for later, owner's decision 2026-10-27)

- `winget` (needs a public submission per release; later).
- `apt`/`dnf` package repositories (downloads on the release page only for now).
- Automatic updates.
- Flatpak (a sandbox gets in the way of a terminal that must run host shells).
- Signing for Windows and Linux packages (the next milestone 6 piece, together with macOS
  signing).
- Anti-goals: the macOS release and look must not change; a Linux or Windows build must never
  ship with broken window controls (no window that can't be moved, resized or closed).

## 4. Behaviour

- The release workflow builds all artifacts from the same tag, refuses a tag that doesn't
  match the workspace version (as the macOS job does), and publishes them together; a failing
  artifact fails the release.
- Linux: the `.deb` targets Debian 12 / Ubuntu 22.04 and newer, the `.rpm` Fedora 39 / RHEL 9
  and newer *(my call: the oldest releases whose system libraries the build links against)*;
  the `.tar.gz` holds the binary, the desktop entry, the icon and a short `INSTALL` note.
- Windows: the `.msi` installs per machine under `Program Files`, adds a Start menu shortcut,
  and removes cleanly; settings and profiles in the user's config folder are kept on uninstall.
- Window controls: hover and pressed states follow the system's look; on Linux the button order
  and side come from the desktop's setting where it can be read (GNOME/KDE), otherwise
  minimize-maximize-close on the right.
- Each platform's own keychain is used as today (Windows Credential Manager, Linux Secret
  Service); the vault from milestone 5 covers machines without one.

**Settled after the builder check** *(my calls, 2026-10-27)*:

- ARM64 Linux packages build on GitHub's hosted ARM64 runners (`ubuntu-24.04-arm`), free for
  this public repository; no cross-compiling.
- Linux keeps a safety valve for window controls: a `window_decorations` setting with
  `"fbterm"` (default: drawn controls in the tab bar) and `"system"` (the desktop's own title
  bar, with the tab bar below it). If the desktop refuses fbterm's move or resize requests
  (some Wayland compositors), fbterm switches that session to the system title bar on its own
  and says so once. Windows always uses the drawn controls; macOS is unchanged.

## 5. Constraints

- Everything in the main design and `CLAUDE.md` holds; packages are built in CI, never by hand;
  no new `unsafe` beyond what platform window APIs strictly need behind `cfg`, each with a
  `// SAFETY:` comment; no naming of other terminal applications; Conventional Commits with no
  attribution.
- CI gains packaging jobs on Linux (x86-64 and ARM64 runners) and Windows; the existing check
  jobs stay as they are.
- Visual checks for the new chrome use own-window screenshots on CI runners where a GPU or
  software renderer is available; otherwise the chrome logic is covered by unit tests of
  layout and hit-testing.

## 6. Left to the builder

- Packaging tools (e.g. `cargo-deb`, `cargo-generate-rpm`, WiX via `cargo-wix`) and their
  configuration; dependency lists per package format.
- How the window controls are drawn (icon font glyphs or vector shapes) and how edge resizing
  is implemented on each platform.
- Which Linux blur protocols are supported (e.g. KDE's blur, the newer Wayland protocol) and the
  Windows material API used.
- File and package names, following `fbterm-{version}-{os}-{arch}.{ext}`.
