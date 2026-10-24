# featherbit terminal: design

Date: 2026-10-23. Status: draft, awaiting approval.

Product name **featherbit terminal** (also written *featherbiterminal*), binary **`fbterm`**, repo
`github.com/featherbitplatform/terminal`, Apache-2.0.

Items marked *(builder's call made at design time)* were decided by the designer, not the owner; they
can be revisited if the plan shows a better option.

## 1. Intent

A native, SSH-first terminal emulator for someone who spends the whole working day inside remote
sessions. A typical heavy day is **about 20 concurrent SSH sessions**: logs streaming, idle shells,
and AI coding agents (full-screen TUIs that redraw constantly). The reason this exists is that
terminals built on a browser runtime get slow and memory-hungry at that scale. featherbit terminal
must stay fast and light with many sessions open, while keeping the conveniences that make SSH work
pleasant: saved profiles, jump hosts, a file-transfer panel, named tunnels, quick launch.

It also works as a plain terminal for local shells (zsh, bash, fish, PowerShell, cmd).

Success: the owner uses it as their only terminal for full working days, starting with milestone 1.

## 2. Goals, non-goals, anti-goals

**Goals**
- Fast and low-memory with ~20 live sessions, including busy TUIs.
- SSH profiles with jump hosts (one or two hops), quick launch by fuzzy search, tabs, then tiling.
- File transfer, port-forward tunnels, export of profiles, color customization (later milestones).
- Cross-platform codebase; macOS releases first, Windows and Linux after.
- A docs website on GitHub Pages, like the gateway's.

**Non-goals (for now)**
- Signed/notarized macOS builds (unsigned `.dmg` until decided otherwise).
- System-wide hotkey; the quick-launch shortcut is application-scoped.
- Importers from other terminal apps. The owner migrates profiles by hand once.
- Cloud sync, accounts, a plugin system, telemetry.

**Anti-goals** (failures even if every feature works)
- Memory or input latency that grows noticeably as sessions are added; background tabs must cost
  little.
- Garbled rendering of full-screen TUIs (agents, vim, htop): wrong colors, broken cursor,
  mis-drawn wide/emoji characters.
- Passwords or key passphrases stored in plain files, or credentials leaking into a "shared" export.
- Positioning the product by comparison to any other terminal app. Nothing committed to the repo
  (code, docs, commits, CLAUDE.md) names a competitor; the product is described on its own terms.

## 3. Decided by the owner

- **Language and GUI:** Rust, GUI with **iced**. If iced's widgets cannot draw the terminal grid fast
  enough, the fallback is **raw wgpu for the grid inside the iced window** (iced stays for everything
  else). Zed's GPUI was mentioned as a possible future alternative, but the decision is to keep iced.
- **SSH auth needed:** **password** and **private key** (with or without passphrase). Other methods
  (keyboard-interactive/2FA, FIDO/hardware keys, OpenSSH certificates, agent forwarding) are
  deferred to a future release.
- **Look:** reuses the gateway's design principles, i.e. design system v2 "Graphite"
  (`../design-system-v2/` in the workspace): dark-first graphite surfaces, violet as the single
  accent, Geist Mono for real values, Lucide icons, no emoji/em-dashes in product copy. Light theme
  as well.
- **SSH profiles:** host, port, user, auth, optional jump chain. Jump chains are usually one bastion,
  sometimes two hops.
- **Saved passwords:** yes, the user should not retype them. Protection via a main password that
  unlocks them at startup is acceptable if needed (see 5.4 for the chosen shape).
- **Quick launch:** **Cmd+Shift+P** (Ctrl+Shift+P on Windows/Linux), application-scoped. Opens a
  search box; typing filters profiles by name as you type (fuzzy); Enter connects in a new tab.
- **Sessions:** multiple connections through tabs and tiling.
- **Local shells:** usable as a plain terminal for the shells available on the machine.
- **File transfer:** a GUI, in the form of a **side panel docked next to the active session**
  (remote file browser of that session's host), with drag and drop to/from the OS file manager and a
  transfer queue. Used mostly for pulling logs down and pushing configs up, but must handle any file.
- **Tunnels:** bound to a specific SSH profile, each with a **user-given name**; per tunnel the user
  chooses **start with the connection** or **on demand** (toggled from the UI). Use cases: a database
  on a private host, a Kubernetes API/admin port, a web server.
- **Export:** a **password-protected archive** of profiles, in two modes: **full** (credentials
  included; for moving to another of the owner's machines) and **shared** (all credentials stripped:
  passwords, passphrases, private key paths; each recipient adds their own key or password).
- **Colors:** user-selectable **terminal color scheme**, and an **accent color per profile**.
- **Milestone 1** (enough to switch for a full day): SSH profiles with jump hosts, tabs, quick
  launch search (on top of local shells and solid emulation).
- **Platforms:** macOS first, others later. **Release artifacts unsigned** for now.
- **Docs website** published on GitHub Pages, like the gateway's.
- **Names:** featherbit terminal / featherbiterminal, binary `fbterm`.

## 4. Constraints

- Workspace conventions shared with the other featherbit repos: **Gitflow** (`main` = tagged
  releases `vX.Y.Z`, `develop` = integration, `feature/*`, `release/X.Y.Z`, `hotfix/*`) and
  **Conventional Commits**. The repo currently has only `main` with an initial commit; `develop` is
  created from it before feature work.
- The code must compile and pass tests on macOS, Linux and Windows from the start, even though only
  macOS ships in milestone 1; no macOS-only APIs in core crates without a cfg-gated alternative.
- Docs site: Docusaurus in `website/`, deployed by a `.github/workflows/docs.yml` to
  `https://featherbitplatform.github.io/terminal/` (same pattern as `gateway/website`,
  `baseUrl: '/terminal/'`).

## 5. How it works

### 5.1 Building blocks *(builder's call made at design time)*

| Concern | Choice | Why |
|---|---|---|
| Terminal emulation (VT parser, grid, scrollback) | `alacritty_terminal` crate | Battle-tested with full-screen TUIs; avoids writing a parser. |
| Local shells | `portable-pty` | Cross-platform PTY incl. ConPTY on Windows. |
| SSH | `russh` (pure Rust, in-process) | One connection carries shell, jump hops, forwards and SFTP; identical on every OS; no dependency on the system `ssh` binary. |
| File transfer | SFTP over the session's existing connection (`russh-sftp`) | Works through jump hosts without a second login. "SCP" in UI wording is fine; the protocol is SFTP. |
| Secrets | OS keychain via the `keyring` crate | macOS Keychain, Windows Credential Manager, Linux Secret Service. |
| Config | TOML files in the platform config dir (`directories` crate) | Human-readable, diffable, hand-editable. |
| Fuzzy search | `nucleo` (or equivalent) | Fast fuzzy matching for the palette. |
| Export archive | `age` with a passphrase recipient (scrypt) wrapping a TOML payload, file extension `.fbprofiles` | Standard, audited format; no home-grown crypto. |

### 5.2 Rendering

iced renders through wgpu. The terminal grid is a custom iced widget fed by `alacritty_terminal`'s
grid snapshot. Drawing each cell as an individual iced text primitive will not meet the performance
goal; the widget should draw from a glyph atlas (custom shader/primitive), redraw only damaged
regions, and do no work at all for tabs that are not visible. **Highest technical risk of the
project: the plan's first task is a spike proving a 200x60 grid scrolling `yes`-style output and a
redrawing TUI at display refresh rate in iced, before UI work is built on it.** If the spike fails
with iced primitives, the plan continues with the owner-approved fallback: a raw wgpu renderer for
the grid embedded in the iced window (iced shader widget or equivalent), iced for all chrome.

Requirements: truecolor, bold/italic/underline variants, wide characters and emoji, ligature-free
monospace by default (Geist Mono bundled, user-selectable font and size), cursor shapes, mouse
reporting, bracketed paste, OSC 8 hyperlinks, selection + copy/paste, scrollback (default 10,000
lines, configurable), the kitty keyboard protocol where `alacritty_terminal` provides it (used by
modern TUIs and AI agents). Inline images (sixel/kitty graphics) are out of scope for now.
The kitty keyboard protocol is optional and off by default (`kitty_keyboard` in `settings.toml`). (Owner decision, 2026-10-23.)
Programs cannot read or write the clipboard (OSC 52 ignored for writes, reads answered empty); only the user's own copy and paste commands touch it. (Owner decision, 2026-10-23.)

### 5.3 Process model

One process, an async runtime (tokio) for I/O. Each session (local PTY or SSH channel) has its own
task reading output into its `alacritty_terminal` instance. Background sessions keep parsing (so
scrollback is complete) but never render. The UI thread only renders the visible panes.

**Connection model** *(builder's call made at design time)*: one SSH connection per profile,
shared by everything using it, in the spirit of OpenSSH's ControlMaster. Each tab or pane on that
profile is its own shell channel on the shared connection; the file panel and tunnels are further
channels on it. Tunnels are owned by the connection, not by a tab, so `on-connect` tunnels start once
when the connection is first established. Starting an on-demand tunnel with no tab open creates a
background connection without a shell. The connection closes when its last channel (tab, file panel
or tunnel) closes; closing a tab never drops a tunnel that is still running.

**Windows and restore** *(builder's call)*: multi-window from the start (iced daemon/multi-window
mode); the palette opens over the focused window. On relaunch the app restores windows, tabs and
pane layout, reconnecting only the visible pane of each window and the rest on first focus.

### 5.4 Profiles and credentials

- **Profile kinds:** `local` (shell path, args, cwd, env) and `ssh` (host, port, user, auth, jump
  chain, tunnels). Both have a name, optional group/tags *(builder's call)*, optional accent color.
- **Auth methods:** private key file (with optional passphrase), password, and ssh-agent
  (`SSH_AUTH_SOCK`) *(builder's call: cheap with `russh`)*. Nothing else in milestone 1 (see 3).
- **Secret storage:** passwords and passphrases never touch the TOML files. *(builder's call)* They
  are stored as **one** OS-keychain item (a serialized map keyed by profile id), not one item per
  profile: unsigned macOS builds change code identity on every release, so macOS re-asks for keychain
  access after each update, and one item keeps that to a single prompt instead of one per profile.
- **Optional main password** *(later milestone)*: for machines where the OS keychain is not trusted,
  secrets go into an `age`-encrypted vault file unlocked once per app start.
- **Jump chain:** an ordered list of references to other saved SSH profiles (so a bastion is defined
  once). The app connects to hop 1, opens a `direct-tcpip` channel to hop 2 through it, runs SSH over
  that channel, and so on. Each hop authenticates with its own profile's credentials. Up to at least
  two hops; no hard limit.
- **Host keys:** read and write `~/.ssh/known_hosts` (shared with OpenSSH). Unknown host: show the
  fingerprint and ask (trust-on-first-use). Changed key: refuse and explain; never auto-accept.
- **Local shell discovery:** on first run, detect available shells (`$SHELL`, `/etc/shells` on
  Unix; PowerShell, pwsh, cmd, Git Bash, WSL on Windows) and create editable local profiles.
- Profiles are edited through a GUI editor; the TOML stays hand-editable and is reloaded on change.

### 5.5 Quick launch / palette

Cmd+Shift+P opens a centered palette over the current window. It lists profiles (SSH and local)
ranked by fuzzy match and recency; Enter opens the selection in a new tab, a modifier opens it in a
split *(once tiling exists)*. *(builder's call)* The same palette also lists app actions (new tab,
split right/down, toggle tunnel `<name>`, open file panel, settings), the way an editor command
palette does; profiles rank above actions.

### 5.6 Tabs and tiling

- Tabs show profile name, connection state (connecting / connected / disconnected) and the profile
  accent color.
- Tiling (milestone 2): split a tab into panes horizontally/vertically, resize, move focus by
  keyboard, close a pane. Any pane can hold any profile.
- Disconnected SSH sessions keep their scrollback and show an inline "disconnected, press Enter to
  reconnect" notice *(builder's call)*.

### 5.7 Tunnels

Defined per SSH profile: name, kind, bind address/port, target host/port, start mode
(`on-connect` | `on-demand`). Kinds: **local forward** (the owner's use cases), plus *(builder's
call)* **remote forward** and **dynamic SOCKS**. All ride the profile's connection, therefore
through its jump chain. Each tunnel shows state (listening, active connections, failed with reason
such as "port in use"); on-demand tunnels toggle from the session UI and the palette.

### 5.8 File panel

Toggled per SSH session; docked beside the terminal (the terminal stays visible). Breadcrumb path,
listing with size/mtime, navigate, download, upload, drag in from and out to Finder/Explorer,
mkdir/rename/delete *(builder's call)*. Transfers run in a queue with progress at the bottom of the
panel and do not block the terminal. Starting directory: the remote home *(builder's call; following
the shell's cwd can come later)*.

### 5.9 Export / import

Export picks profiles (all or a selection) and a mode:
- **full:** profiles + secrets from the keychain, encrypted with a user-chosen archive password.
  *(builder's call)* An opt-in "include private key files" checkbox embeds the referenced key files;
  on import they are written to a chosen directory (default `~/.ssh/`) with `0600` permissions and
  the profiles are repointed to them.
- **shared:** profiles with passwords, passphrases and private-key paths removed (auth method kept
  as a hint, e.g. "key"), encrypted with an archive password.

Import decrypts, previews, and merges by name with "skip / overwrite / keep both" on conflict.
Jump-chain references are exported together with the profiles they point to.

### 5.10 Theming

- App chrome: design system v2 Graphite translated into an iced theme (dark default, light option).
- Terminal color scheme: built-in schemes (including a default derived from the Graphite palette)
  plus user-imported schemes *(builder's call: accept a simple TOML scheme format; supporting popular
  third-party formats is a nice-to-have)*.
- Per-profile accent: shown on tab and pane border; a scheme override per profile is a
  nice-to-have.
- *(Owner's decision, 2026-10-23)* **Unified title bar on macOS.** The tab bar is the title bar:
  the OS title strip is transparent and hidden, the content runs under it, the native window
  buttons stay and sit vertically centered on the bar (32 px high, tabs start 78 px in, at the
  edge in full screen where the buttons are hidden). Empty bar space moves the window, a double
  click zooms it; not while the palette or a dialog is open. The window title is still set (the active
  tab's) for the Dock, Mission Control and the Window menu. The profile editor gets the same
  treatment with its own header. Linux and Windows keep OS decorations until milestone 6; the
  code keeps this behind a per-platform chrome strategy (`crates/fbterm/src/chrome.rs`).
- *(Owner's decision, 2026-10-23)* **Background opacity and optional blur.** `settings.toml`
  `background_opacity` (0.05 to 1.0, default 1.0; not 0, since fully clear pixels can pass
  clicks through and hide the window) applies to the terminal's default background and the tab
  bar only; text, cursor, selection and cells with their own background stay opaque.
  `background_blur` (default off) blurs what shows through. Builder's notes: on macOS terminal
  windows are always created transparent and paint an opaque background at 1.0, so opacity
  changes apply live; on Linux and Windows only windows opened below 1.0 are transparent. Blur
  is a window-creation setting, so it applies to windows opened after a change.
- *(Owner's decision, 2026-10-23)* **Blur implementation.** Blur uses iced's built-in window
  blur (`window::Settings::blur`, through winit); on macOS this relies on an undocumented
  CoreGraphics call; if it stops working, blur is lost but nothing breaks. Replacing it is a
  follow-up item in section 6.
- *(Owner's decision, 2026-10-24)* **Settings window.** A separate window (unified title bar on
  macOS, like the profile editor) with a control for every `settings.toml` option: theme, font
  family (installed monospace families, the bundled Geist Mono first), font size, background
  opacity and blur, scrollback lines, kitty keyboard protocol. Changes apply live, with no Save
  button, and are written to `settings.toml` in place: only the changed key, keeping comments,
  order and other keys (builder's notes: `toml_edit`; slider drags and typing are written after a
  300 ms pause or when the slider is let go; our own writes do not trigger a reload). A file
  that does not load is never written: the window is read-only until it is fixed, and edits on
  disk show in the window. Opened with Cmd+, (Ctrl+, elsewhere) or the palette's **Settings**.

### 5.11 Keyboard defaults *(builder's call)*

Cmd on macOS, Ctrl+Shift elsewhere: new tab (opens the palette), close, next/prev tab, tab 1..9,
palette (Cmd+Shift+P), split right/down, focus pane by direction, toggle file panel, find in
scrollback. All rebindable via config later. *(Owner's decision, 2026-10-24)* Settings is Cmd+,
on macOS and Ctrl+, elsewhere (the one Ctrl chord without Shift); the profile editor is
Cmd+Shift+, (Ctrl+Shift+, elsewhere). Tab-bar buttons show their action and chord in a tooltip,
derived from the same table.

## 6. Milestones

Order after milestone 1 is a *builder's call*; the owner only fixed milestone 1.

1. **Daily driver.** Rendering spike, terminal widget, local shells, tabs, SSH profiles (key, agent,
   keychain password), jump hosts, host-key checks, profile editor, quick-launch palette, settings
   file, Graphite theme, CI (macOS + Linux + Windows build/test), unsigned macOS `.dmg` (Apple
   Silicon and Intel) on GitHub releases, docs site skeleton on GitHub Pages.
2. Tiling, per-profile accent, color schemes, find in scrollback.
3. Tunnels.
4. File panel.
5. Export/import, optional main-password vault.
6. Windows and Linux release artifacts; reading `~/.ssh/config` hosts into the palette; signing
   (when decided); deferred auth methods (2FA, FIDO keys, certificates, agent forwarding).
   Windows and Linux: draw OS-styled window controls in the tab bar (no OS title bar),
   drag/double-click/resize from edges; background blur where the platform supports it.

Look-and-feel follow-up: Evaluate replacing window blur with Apple's public NSVisualEffectView
(follows Reduce transparency, native vibrancy).

Performance follow-up (measured 2026-10-24, `dev/perf/README.md`):

- Done in milestone 1: output-driven redraws are paced to at most 60 per second (a named
  constant, not a setting), with the echo of typed input and all input-driven redraws
  immediate; a window that cannot be seen (minimized, covered, locked screen, sleeping display;
  AppKit's occlusion state on macOS) takes no output redraws until it is visible again. One
  visible 100-lines/s stream went from 84 to 54 frames per second and from 14.1% to 10.2% of a
  core; the same stream on a locked screen from 74-77 frames per second, 11% CPU and 390 MB
  footprint to no frames, 0.1% CPU and 88 MB. This also bounds the runaway seen with the display
  asleep (about 3,000 frames per second and 2-3 GB footprint). The tab-bar and dialog icons are
  glyphs of a bundled Lucide icon font instead of SVGs, so the 2048x2048 image atlas (16 MB of
  GPU memory per window) is never created: idle footprint with one tab went from about 76 MB to
  60 MB.
- Measured facts: Activity Monitor's "Memory" is the phys_footprint (`footprint <pid>`), not
  RSS. It counts GPU memory the process owns, which RSS does not. While any frames are being
  rendered, the Metal driver holds about 300 MB more (a minimal iced app shows the same); it is
  released 10-20 s after rendering stops. So a visible stream shows about 390-430 MB in
  Activity Monitor while RSS is about 120 MB.
- Compact scrollback, owner decision 2026-10-24: a follow-up, not in milestone 1. First step:
  trim history rows to their used length when they scroll off (about -25% at 126 columns, -50%
  or more in wide windows). Full: compress older history (text plus attribute runs), about
  24-33 MB down to 1.5-2.5 MB per full tab. Both change the vendored `alacritty_terminal` grid.
- Still open: per-frame cost (one instanced glyph-atlas primitive and damage tracking, as in
  5.2), batching PTY reads under floods, and a smaller font database at startup.

## 7. Performance targets *(builder's call; to be measured, not guessed)*

- 20 sessions open (19 idle in background tabs, one visible streaming logs): resident memory under
  ~300 MB with default scrollback; idle CPU near zero when nothing is printing. Both the target
  and the scrollback default may be retuned after the first measurement.
- Keypress-to-screen latency comparable to other GPU-accelerated native terminals (target under one
  frame at 60 Hz on Apple Silicon).
- A benchmark/measurement script lives in the repo and is run before each release.

*Measured 2026-10-23 on Apple M5 Max (`dev/perf/README.md`):* base about 103 MB resident with one
tab; about 0.2 MB per idle tab; about 21 MB per tab with the full 10,000-line scrollback; 20 full
tabs about 528 MB, over the ~300 MB target. The owner chose to keep the 10,000-line default and
accept this worst case. One visible log stream costs about 22% of a core; a `yes` flood about one
core. Follow-up work is listed in section 6.

*Measured 2026-10-24 (same machine):* idle footprint with one tab 60 MB after the icon font
(76 MB before); output redraws capped at 60 per second; numbers in section 6.

## 8. Repo and release

- Cargo workspace; crate boundaries left to the builder (expected split: terminal widget/renderer,
  session backends (pty, ssh), profiles/config/secrets, app/UI).
- `cargo fmt`, `cargo clippy -D warnings`, `cargo test`, `cargo deny` in CI; security scanning in
  the spirit of the gateway's (gitleaks etc.) *(builder's call)*.
- Release workflow on `v*` tags: build macOS universal (or per-arch) `.app` bundled into `.dmg`,
  attach to the GitHub release with checksums. Docs explain the unsigned first-launch step
  (right-click > Open, or `xattr -d com.apple.quarantine`).
- A `CLAUDE.md` for the repo is written once this design is approved.

## 9. Left to the builder

Crate layout, internal APIs, config schema details, exact shortcuts, font rendering technique within
the constraints of 5.2, test strategy (expect unit tests for profile/export logic, an integration
test SSH server, e.g. a `russh` test server or a containerized OpenSSH, for jump hosts, tunnels and
SFTP), docs site structure.
