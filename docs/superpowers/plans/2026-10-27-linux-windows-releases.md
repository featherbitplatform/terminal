# Linux and Windows Releases Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every release tag also ships Linux `.deb`/`.rpm`/`.tar.gz` (x86-64 and ARM64) and a Windows `.msi`, and fbterm on Linux and Windows draws its own window controls in the tab bar (with a system-title-bar fallback on Linux) and supports blur where available.

**Architecture:** Window chrome stays in `crates/fbterm/src/chrome.rs`: a new `Chrome::Drawn` variant for Windows and Linux (borderless window, controls drawn in the tab bar, drag/maximize/edge-resize through iced/winit window commands), selected by platform and the Linux `window_decorations` setting with a runtime fallback to `Chrome::Native`. Packaging is configuration plus CI jobs in `release.yml`, with smoke tests that install each package in a clean container or runner and run `fbterm --version`.

**Tech Stack:** Rust 2024, iced 0.14 / winit, `cargo-deb`, `cargo-generate-rpm`, `cargo-wix` (WiX), GitHub Actions (`ubuntu-24.04`, `ubuntu-24.04-arm`, `windows-latest`, `macos-14`).

**Spec:** `docs/superpowers/specs/2026-10-27-linux-windows-releases-design.md` (approved; refines `2026-10-23-featherbit-terminal-design.md` 6 and 8).

## Global Constraints

- The macOS release, chrome and look do not change (`Chrome::Unified` / `Chrome::Native` on macOS untouched).
- Artifact names: `fbterm-{version}-linux-{x86_64|aarch64}.{deb|rpm|tar.gz}`, `fbterm-{version}-windows-x86_64.msi`, plus `.sha256` for each, beside the existing `fbterm-{version}-macos-universal.dmg`.
- Linux targets: `.deb` for Debian 12 / Ubuntu 22.04 and newer, `.rpm` for Fedora 39 / RHEL 9 and newer (build on the oldest glibc of those, e.g. in a Debian 12 container); packages install `/usr/bin/fbterm`, a desktop entry `fbterm.desktop` (Name `featherbit terminal`, Categories `System;TerminalEmulator;`) and icons (hicolor 32/64/128/256/512 PNG from the mark), and declare runtime dependencies for graphics (Vulkan/GL loader), xkbcommon, Wayland and X11 client libraries, and the Secret Service client stack.
- Windows: per-machine `.msi` under `Program Files\featherbit terminal`, Start menu shortcut `featherbit terminal`, clean uninstall that keeps the user's config folder; unsigned (SmartScreen documented).
- Release workflow: one tag builds every artifact; the tag must match the workspace version (as today); any failing artifact fails the release; publish once with all files.
- Drawn chrome (Windows always, Linux by default): no system title bar; controls minimize, maximize/restore, close in the tab bar with system-style hover/pressed states; Windows on the right; Linux order and side from the desktop's layout setting when readable (GNOME `org.gnome.desktop.wm.preferences button-layout`, KDE), else minimize-maximize-close on the right; drag on empty bar moves; double-click toggles maximize; edges resize (8 px hit band, corners included).
- Linux setting `window_decorations = "fbterm" | "system"` (default `"fbterm"`, template comment `# Window title bar on Linux: "fbterm" draws it in the tab bar, "system" uses the desktop's.`); when the compositor refuses move or resize requests, the session falls back to `"system"` chrome and shows once `This desktop doesn't let fbterm move its window, so the system title bar is used. Change it in Settings.`
- Blur: Windows 11 translucent backdrop and Linux compositors with a blur protocol; where unsupported the Settings blur switch is disabled with `Not supported on this system`.
- No new `unsafe` except platform window calls behind `cfg`, each with `// SAFETY:`; no emoji or em-dashes; never name another terminal application; Conventional Commits with no attribution of any kind.
- Gates per task: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; CI on all three systems; `cd website && npm run build` when docs change.

## Review Focus

1. A Linux Wayland session where the compositor ignores move/resize requests: fbterm switches to the system title bar and the window can still be moved, resized and closed. Test in Task 2 (`refused_move_falls_back_to_system_chrome`).
2. Maximized window on Windows: the drawn maximize button shows restore, double-click restores, and edge hit-testing is off while maximized. Test in Task 1 (`maximized_disables_edge_resize`).
3. Installing the `.deb` on a minimal Debian 12 image: dependencies resolve and `fbterm --version` runs. Test in Task 3 (CI smoke job).
4. Uninstalling the `.msi` keeps `%APPDATA%` config; reinstalling finds the profiles. Test in Task 4 (CI smoke job).
5. A tag whose version differs from `Cargo.toml`: every packaging job refuses, nothing is published. Test in Task 5 (`release.yml` dry run on a PR with a mismatched version input).

---

### Task 1: Drawn window controls on Windows and Linux

**Files:**
- Modify: `crates/fbterm/src/chrome.rs` (new `Chrome::Drawn`, selection by platform, `window_settings` with `decorations: false` for Drawn), `window.rs` (tab bar trailing controls, edge resize hit areas), `ui.rs` (control glyphs from the bundled icon font, regenerating the subset if needed), `app.rs` (`BarAction::{Minimize, ToggleMaximize, Close, ResizeFrom(Direction)}` handling via `window::minimize`, `window::toggle_maximize`, close request, `window::drag_resize`)
- Add `fbterm --version` (prints `fbterm {version}`) if it doesn't exist, used by smoke tests.

**Interfaces:**
- Produces: `pub fn controls_layout(platform: Platform, desktop: Option<&str>) -> ControlsLayout { side: Side, order: Vec<Control> }` (pure, parses GNOME/KDE layout strings), `pub fn edge_hit(size: Size, point: Point, maximized: bool) -> Option<Direction>` (pure).
- [ ] **Step 1: Failing tests**: `windows_controls_on_the_right`, `gnome_layout_parsed` (`"close,minimize,maximize:"` puts controls on the left in that order), `unknown_layout_defaults_right`, `edge_hit_corners_and_edges`, `maximized_disables_edge_resize`, `macos_chrome_unchanged` (existing chrome tests keep passing unchanged), `version_flag_prints_version`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates; CI green on Linux and Windows; own-window screenshots from the Linux and Windows CI runners if a renderer is available (upload as artifacts), else note it — Expected: PASS.
- [ ] **Step 5: Commit** `feat(chrome): drawn window controls on Windows and Linux`.

### Task 2: Linux fallback and blur

**Files:**
- Modify: `crates/fbterm-config/src/{settings.rs, settings_edit.rs}` (`window_decorations`), `crates/fbterm/src/{chrome.rs, app.rs, settings_window/*}`

**Interfaces:**
- Produces: `Settings.window_decorations: WindowDecorations { Fbterm, System }` (Linux only in the Settings window; ignored elsewhere); detection of refused move/resize (the drag or resize request returns an error or produces no window event within 500 ms) switching that session to `Chrome::Native` with the notice; Settings row `Title bar` (`fbterm` / `System`) on Linux; blur on Windows 11 and supporting Linux compositors via the platform window APIs, with the disabled-switch copy where unsupported.
- [ ] **Step 1: Failing tests**: `window_decorations_defaults_and_writes`, `system_setting_uses_native_chrome`, `refused_move_falls_back_to_system_chrome` (fake window backend reporting a refusal), `blur_switch_disabled_when_unsupported`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the gates and CI — Expected: PASS.
- [ ] **Step 5: Commit** `feat(chrome): Linux title bar fallback and blur`.

### Task 3: Linux packages

**Files:**
- Create: `crates/fbterm/linux/{fbterm.desktop, icons/…}` (generated icons committed; a `dev/make-linux-icons.sh` like `dev/make-icns.sh`), `dev/package-linux.sh <version> <arch>`
- Modify: `crates/fbterm/Cargo.toml` (`[package.metadata.deb]`, `[package.metadata.generate-rpm]`), `.github/workflows/release.yml` (jobs `linux-x86_64` on `ubuntu-24.04` building inside a `debian:12` container, `linux-aarch64` on `ubuntu-24.04-arm` likewise; smoke jobs installing the `.deb` in `debian:12` and `ubuntu:22.04` and the `.rpm` in `fedora:39` and `rockylinux:9`, running `fbterm --version`), PR trigger paths include the new files

- [ ] **Step 1: Write** the metadata, desktop entry, icons and script; run `dev/package-linux.sh` locally in Docker for x86-64 (`docker run debian:12 …`) and check `dpkg -I`, `rpm -qip` and the tar contents.
- [ ] **Step 2: Run** the PR dry run of `release.yml` — Expected: all Linux build and smoke jobs green.
- [ ] **Step 3: Commit** `ci(release): Linux deb, rpm and tarball`.

### Task 4: Windows installer

**Files:**
- Create: `crates/fbterm/wix/main.wxs` (per-machine, Start menu shortcut, icon, upgrade code fixed forever), `crates/fbterm/windows/fbterm.ico` (from the mark), `dev/package-windows.ps1 <version>`
- Modify: `crates/fbterm/Cargo.toml` (`[package.metadata.wix]`), `build.rs` or resource embedding for the exe icon if needed, `.github/workflows/release.yml` (job `windows-x86_64` on `windows-latest`; smoke job: `msiexec /i … /qn`, run `fbterm --version`, write a sample config, `msiexec /x … /qn`, check the config is kept, reinstall)

- [ ] **Step 1: Write** the WiX source and script.
- [ ] **Step 2: Run** the PR dry run — Expected: Windows build and smoke jobs green.
- [ ] **Step 3: Commit** `ci(release): Windows msi installer`.

### Task 5: One release for all systems

**Files:**
- Modify: `.github/workflows/release.yml` (version check in every packaging job, `publish` needs all jobs and uploads every artifact and checksum with `fail_on_unmatched_files: true`; a `workflow_dispatch` dry run input that stops before publishing)

- [ ] **Step 1: Write** the workflow changes.
- [ ] **Step 2: Run** a dry run on the PR with a mismatched version (expect every packaging job to refuse) and with the right one (expect all artifacts uploaded as workflow artifacts, no release published).
- [ ] **Step 3: Commit** `ci(release): publish every system's artifacts together`.

### Task 6: Docs

**Files:**
- Create: `website/docs/install-linux.md`, `website/docs/install-windows.md` (SmartScreen: `More info` → `Run anyway`; checksum checks with `sha256sum` and `Get-FileHash`), sidebar entries
- Modify: `README.md` (install section lists all systems), `website/docs/intro.md`, `files.md` (config folder locations per system, `window_decorations`), main spec 6 (milestone 6 releases part done), `CLAUDE.md` (Commands: packaging scripts; Status; a gotcha line on `Chrome::Drawn` and the Linux fallback)

- [ ] **Step 1: Write** the docs.
- [ ] **Step 2: Run** `cd website && npm run build` and the gates — Expected: PASS.
- [ ] **Step 3: Commit** `docs: install on Linux and Windows`.
