# Milestone 1 "Daily Driver" Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship an unsigned macOS `fbterm` that the owner can use as their only terminal for a full day: local shells, tabs, SSH profiles with jump hosts, the Cmd+Shift+P quick-launch palette, plus CI, release workflow and a docs site skeleton.

**Architecture:** A Cargo workspace of four library crates (config, secrets, term, session) and one binary crate (`fbterm`, the iced app). `alacritty_terminal` owns emulation; a custom iced widget renders its grid. Sessions (local PTY or SSH channel) run as tokio tasks that feed a shared `Terminal` and notify the UI. SSH is in-process `russh` with one shared connection per profile and jump hops tunnelled over `direct-tcpip` channels.

**Tech Stack:** Rust stable (edition 2024), iced 0.14 (wgpu), alacritty_terminal 0.26, russh 0.64, portable-pty 0.9, keyring 4.2, nucleo-matcher 0.3, directories 6, serde + toml, tokio, uuid, thiserror. Docs: Docusaurus 3.10.2 (same as `../gateway/website`). Versions are those current on 2026-10-23; check docs.rs for each crate's exact API before coding against it.

**Spec:** `docs/superpowers/specs/2026-10-23-featherbit-terminal-design.md`

## Global Constraints

- Product name "featherbit terminal"; binary `fbterm`; bundle id `io.featherbit.fbterm`; config dir via `directories::ProjectDirs::from("io", "featherbit", "fbterm")`.
- Nothing committed (code, comments, docs, commits) names or compares to another terminal application.
- Code builds and tests on macOS, Linux and Windows; platform APIs only behind `cfg` with an alternative. Only macOS gets release artifacts.
- Secrets never written to TOML; all passwords/passphrases in ONE keychain item (service `fbterm`, account `secrets`).
- SSH auth in scope: password, private key (± passphrase), ssh-agent. Nothing else.
- Host keys: `~/.ssh/known_hosts`; unknown → ask; changed → refuse, never auto-accept.
- Palette shortcut: Cmd+Shift+P on macOS, Ctrl+Shift+P elsewhere; application-scoped only.
- Default scrollback 10,000 lines; default font Geist Mono 13 px, bundled.
- UI follows design system v2 "Graphite" (`../design-system-v2/`): dark-first, violet single accent (`#7c49e3` fill, `#a584ff` focus/selection edge), Geist Mono for real values, no emoji or em-dashes in product copy.
- Gitflow: all work on `feature/milestone-1` off `develop`; Conventional Commits; no `Co-Authored-By` trailer.
- CI gates: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo deny check`.

## Review Focus

1. **SSH connection drops** (laptop sleep, Wi-Fi change): the tab keeps its scrollback, shows "disconnected, press Enter to reconnect", and Enter reconnects through the full jump chain. Tested in Task 9 (kill the server-side connection) and Task 10 (state transition).
2. **Wrong saved password or passphrase** (password rotated on the server): auth fails with a clear message naming the profile, the user is re-prompted, and the bad secret is replaced only after a successful login. Tested in Task 8.
3. **Keychain access denied or unavailable** (user clicks Deny on the post-update prompt; Linux without Secret Service): the app still starts and connects, falling back to prompting for secrets in that session. Tested in Task 7.
4. **Hand-edited `profiles.toml` with a syntax error or a dangling jump reference:** the app starts, shows the error with file and line, and never overwrites the user's file. Tested in Task 6.
5. **Output flood in a background tab** (`cat` of a 500 MB log): the UI stays responsive and other tabs keep typing normally; redraw notifications are coalesced, not one per chunk. Tested in Task 5.

**Execution order:** 1, 2, 6, 7, 3, 4, 5, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17. Task 6 owns shared types (`ProfileId`, `LocalProfile`, `Rgb`) that Tasks 3–5 consume; numbering follows the architecture, not the order.

---

## File Structure

```
Cargo.toml                         workspace, shared deps, lints, profile.release
deny.toml  rustfmt.toml  .gitignore
crates/fbterm-config/src/          lib.rs, paths.rs, profile.rs, store.rs, settings.rs, shells.rs, state.rs
crates/fbterm-secrets/src/         lib.rs (trait + MemoryStore), keychain.rs
crates/fbterm-term/src/            lib.rs, terminal.rs (alacritty wrapper), input.rs (key/mouse encoding),
                                   selection.rs, widget/{mod.rs, atlas.rs, pipeline.rs}, palette.rs (color scheme)
crates/fbterm-term/examples/       spike.rs (rendering go/no-go)
crates/fbterm-session/src/         lib.rs (SessionHandle, events), local.rs, ssh/{mod.rs, auth.rs, known_hosts.rs, pool.rs}
crates/fbterm-session/tests/       local.rs, ssh_live.rs (gated)
crates/fbterm/src/                 main.rs, app.rs, window.rs, tabs.rs, keys.rs, theme.rs, palette.rs,
                                   editor.rs, prompts.rs, restore.rs
crates/fbterm/assets/fonts/        GeistMono-Regular.ttf, GeistMono-Bold.ttf (OFL, with LICENSE)
dev/ssh-test/compose.yaml          bastion + target OpenSSH containers for live tests
dev/package-macos.sh               universal .app + .dmg
dev/perf/sessions.sh               20-session memory measurement
.github/workflows/                 ci.yml, release.yml, docs.yml
website/                           Docusaurus skeleton
```

---

### Task 1: Toolchain, branches, workspace and CI

**Files:**
- Create: `Cargo.toml`, `rustfmt.toml`, `deny.toml`, `.gitignore`, `crates/*/Cargo.toml`, `crates/*/src/lib.rs` (empty modules), `crates/fbterm/src/main.rs`, `.github/workflows/ci.yml`
- Commit also: `CLAUDE.md`, `docs/superpowers/specs/…`, `docs/superpowers/plans/…` (already written)

**Interfaces:**
- Produces: crate names `fbterm-config`, `fbterm-secrets`, `fbterm-term`, `fbterm-session`, binary crate `fbterm`; `[workspace.dependencies]` pinning every crate listed in Tech Stack.

- [ ] **Step 1: Install the toolchain (ask the owner first; it changes their machine)**

Run: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && rustup target add x86_64-apple-darwin aarch64-apple-darwin`
Expected: `cargo --version` prints a stable version.

- [ ] **Step 2: Create `develop` from `main` and `feature/milestone-1` from `develop`**

Run: `git switch -c develop && git push -u origin develop && git switch -c feature/milestone-1`

- [ ] **Step 3: Write the workspace**

Root `Cargo.toml`: `resolver = "3"`, `members = ["crates/*"]`, `edition = "2024"`, `license = "Apache-2.0"`, version `0.1.0`, `[workspace.lints.clippy] all = "warn"`, `[profile.release] lto = "thin"`, `codegen-units = 1`. `.gitignore`: `/target`, `/website/node_modules`, `/website/build`, `.superpowers/`, `*.dmg`. `deny.toml`: allow Apache-2.0, MIT, BSD-2/3-Clause, ISC, Unicode-3.0, Zlib, MPL-2.0, OFL-1.1 (adjust only with a comment explaining which crate needed it). `main.rs` prints `fbterm <version>` for now.

- [ ] **Step 4: Verify locally**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all succeed (zero tests).

- [ ] **Step 5: CI workflow**

`ci.yml` on push to `develop`/`main` and on pull requests: matrix `macos-14`, `ubuntu-latest`, `windows-latest` running fmt (ubuntu only), clippy and tests; Linux job installs `libxkbcommon-dev libwayland-dev libdbus-1-dev pkg-config`; a `deny` job runs `EmbarkStudios/cargo-deny-action@v2`; an `ssh-live` job (ubuntu, added in Task 8) is a placeholder comment for now. Use `dtolnay/rust-toolchain@stable` and `Swatinem/rust-cache@v2` like the gateway.

- [ ] **Step 6: Commit and push**

```bash
git add -A && git commit -m "chore: scaffold cargo workspace, CI and project docs"
git push -u origin feature/milestone-1
```
Expected: CI green on all three OSes.

---

### Task 2: Rendering spike (go/no-go)

Highest risk in the project. Throwaway-quality code in `examples/`, but the measurement and the decision are kept.

**Files:**
- Create: `crates/fbterm-term/examples/spike.rs`, `docs/superpowers/specs/2026-10-23-rendering-spike-result.md`

**Interfaces:**
- Produces: a written decision, either **A: iced-native** (draw via iced text/paragraph primitives inside a custom `iced::advanced::Widget`) or **B: wgpu shader** (`iced::widget::shader` with our own glyph atlas and instanced quads). Task 4 implements whichever passes.

- [ ] **Step 1: Build approach A** — an iced app showing a 200x60 grid fed by an `alacritty_terminal::Term` that is driven by a child process (`yes "$(printf '%.0s█' {1..190})"` and a redrawing TUI such as `htop` or a Python curses loop) through `portable-pty`. Color every cell from the Term's cell fg/bg (truecolor). Print frames-per-second and the worst frame time to stdout every second.

- [ ] **Step 2: Measure A** on Apple Silicon at the display's refresh rate, release build.

Run: `cargo run --release -p fbterm-term --example spike -- --mode native`
Pass criteria: sustained ≥ 58 fps at 60 Hz, worst frame ≤ 25 ms over 30 s, idle CPU (no output) < 1%.

- [ ] **Step 3: If A fails, build B** (`--mode shader`): glyphs rasterized once into a texture atlas (use `cosmic-text`/`swash` as iced already does), one instanced quad per non-empty cell, background quads per cell run. Same measurement and criteria.

- [ ] **Step 4: Record the result** in `rendering-spike-result.md`: numbers for each mode tried, machine, chosen approach. If neither passes, stop and report to the owner (the only remaining option is outside the approved design).

- [ ] **Step 5: Commit**

```bash
git add crates/fbterm-term/examples/spike.rs docs/superpowers/specs/2026-10-23-rendering-spike-result.md
git commit -m "test(term): rendering spike and go/no-go result"
```

---

### Task 3: Terminal core (emulation wrapper and input encoding)

**Files:**
- Create: `crates/fbterm-term/src/terminal.rs`, `crates/fbterm-term/src/input.rs`, `crates/fbterm-term/src/selection.rs`
- Test: inline `#[cfg(test)]` modules

**Interfaces:**
- Produces:
  - `pub struct GridSize { pub cols: u16, pub rows: u16, pub cell_width: f32, pub cell_height: f32 }`
  - `pub struct Terminal` with `new(size: GridSize, scrollback: usize) -> Self`, `feed(&mut self, bytes: &[u8])`, `resize(&mut self, size: GridSize)`, `scroll(&mut self, delta_lines: i32)`, `mode(&self) -> TermMode` (re-export of alacritty's), `take_events(&mut self) -> Vec<TermEvent>`, `term(&self) -> &alacritty_terminal::Term<EventProxy>` (for the widget).
  - `pub enum TermEvent { Title(String), Bell, PtyWrite(Vec<u8>), ClipboardStore(String), Exit }` (PtyWrite carries terminal replies such as DA/DSR that must go back to the session).
  - `pub type SharedTerminal = Arc<parking_lot::Mutex<Terminal>>`.
  - `pub fn encode_key(key: &iced::keyboard::Key, physical: iced::keyboard::key::Physical, text: Option<&str>, mods: iced::keyboard::Modifiers, mode: TermMode) -> Option<Vec<u8>>`
  - `pub fn encode_paste(text: &str, mode: TermMode) -> Vec<u8>`
  - `pub fn encode_mouse(event: MouseReport, mode: TermMode) -> Option<Vec<u8>>` with `pub struct MouseReport { pub button: MouseButton, pub kind: MouseKind, pub col: u16, pub row: u16, pub mods: iced::keyboard::Modifiers }`
  - `pub struct Selection` with `start(point)`, `update(point)`, `text(&self, term: &Terminal) -> String`, `contains(point) -> bool`; `point` = `alacritty_terminal::index::Point`.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn feed_plain_text_lands_in_grid()        // feed b"hi" → cells (0,0)='h', (0,1)='i'
#[test] fn sgr_truecolor_sets_cell_fg()           // feed b"\x1b[38;2;10;20;30mX" → fg == Rgb{10,20,30}
#[test] fn wide_char_occupies_two_cells()         // feed "界" → cell 0 has WIDE_CHAR flag, cell 1 spacer
#[test] fn resize_reflows_and_keeps_scrollback()  // 80x24, feed 100 lines, resize 40x24 → history len > 0
#[test] fn scrollback_is_capped()                 // scrollback 100, feed 1000 lines → history ≤ 100
#[test] fn device_attributes_query_produces_ptywrite() // feed b"\x1b[c" → take_events contains PtyWrite
#[test] fn enter_is_cr()                          // encode_key(Enter) == b"\r"
#[test] fn arrow_respects_app_cursor_mode()       // Up: b"\x1b[A" normal, b"\x1bOA" with APP_CURSOR
#[test] fn ctrl_c_is_etx()                        // Ctrl+C == [0x03]
#[test] fn alt_letter_is_esc_prefixed()           // Alt+x == b"\x1bx"
#[test] fn paste_is_bracketed_when_mode_set()     // "a" → b"\x1b[200~a\x1b[201~" with BRACKETED_PASTE
#[test] fn sgr_mouse_press_encoding()             // left press at col 4,row 2 with SGR_MOUSE → b"\x1b[<0;5;3M"
#[test] fn selection_text_spans_lines_and_trims_trailing_spaces()
```

- [ ] **Step 2: Run** `cargo test -p fbterm-term` — Expected: FAIL (unresolved items).

- [ ] **Step 3: Implement.** `Terminal` holds `Term<EventProxy>` and `vte::ansi::Processor`; `EventProxy` implements `alacritty_terminal::event::EventListener` by pushing into an internal `Vec<TermEvent>`. Use alacritty's `Config { scrolling_history: scrollback, .. }`. Keyboard: on macOS, Cmd-chords are never encoded (they belong to the app); Option acts as Alt.

- [ ] **Step 4: Run** `cargo test -p fbterm-term` — Expected: PASS.

- [ ] **Step 5: Commit** `git commit -m "feat(term): terminal core, input encoding and selection"`

---

### Task 4: Terminal widget

**Files:**
- Create: `crates/fbterm-term/src/widget/mod.rs`, `widget/atlas.rs` and `widget/pipeline.rs` (approach B only), `crates/fbterm-term/src/palette.rs`

**Interfaces:**
- Consumes: `SharedTerminal`, `GridSize`, encoders, `Selection` (Task 3); approach from Task 2; `Rgb` (Task 6).
- Produces:
  - `pub struct ColorScheme { pub foreground: Rgb, pub background: Rgb, pub cursor: Rgb, pub selection: Rgb, pub ansi: [Rgb; 16] }` with `ColorScheme::graphite()` (background `#0b0b0e`, foreground `#f5f5f7`, cursor `#a584ff`, selection `rgba(165,132,255,0.30)` flattened over background; ANSI red/green/yellow from the design tokens `#ed4a49`/`#2bbb71`/`#f2a635`, bright variants `#fd736d`/`#52cd86`/`#fac053`, blue `#70b3f7`, magenta `#a584ff`; remaining entries are the builder's choice, contrast ≥ 4.5:1 against background).
  - `pub fn terminal_view<'a, Message>(term: &'a SharedTerminal, style: &'a TerminalStyle, on_event: impl Fn(TerminalOutput) -> Message + 'a) -> iced::Element<'a, Message>`
  - `pub struct TerminalStyle { pub scheme: ColorScheme, pub font: iced::Font, pub font_size: f32, pub accent: Option<Rgb>, pub focused: bool }`
  - `pub enum TerminalOutput { Input(Vec<u8>), Resized(GridSize), Copy(String), OpenUrl(String) }`

- [ ] **Step 1: Write failing tests** for the pure parts: `grid_size_from_bounds` (800x600 px with cell 8x16 → 100x37), `pixel_to_point` (incl. clamping outside bounds), `cell_colors` (inverse flag swaps fg/bg; dim; selected cell uses selection color), `graphite_scheme_contrast_at_least_4_5` for ANSI 1–15 against background.

- [ ] **Step 2: Run** `cargo test -p fbterm-term widget` — Expected: FAIL.

- [ ] **Step 3: Implement** the widget: measures the cell from the font, emits `Resized` when the grid size changes, draws backgrounds, glyphs (bold/italic/underline/strikethrough, wide chars, emoji fallback), cursor shapes (block/beam/underline, hollow when unfocused), selection, OSC 8 links underlined on hover with Cmd/Ctrl+click → `OpenUrl`; mouse drag selects (double-click word, triple-click line), Cmd+C/Ctrl+Shift+C copies, Cmd+V/Ctrl+Shift+V pastes via `encode_paste`; wheel scrolls scrollback, or is reported via `encode_mouse` when mouse mode is on; when `accent` is `Some`, a 1 px accent border. Lock the terminal only for the duration of building the draw list.

- [ ] **Step 4: Run** `cargo test -p fbterm-term` — Expected: PASS. Then `cargo run --release -p fbterm-term --example spike` switched to the production widget still meets the Task 2 criteria.

- [ ] **Step 5: Commit** `git commit -m "feat(term): terminal widget with selection, mouse and links"`

---

### Task 5: Local PTY sessions

**Files:**
- Create: `crates/fbterm-session/src/lib.rs`, `crates/fbterm-session/src/local.rs`
- Test: `crates/fbterm-session/tests/local.rs`

**Interfaces:**
- Consumes: `SharedTerminal`, `GridSize`, `TermEvent` (Task 3); `LocalProfile` (Task 6).
- Produces:
  - `pub struct SessionId(pub u64)`
  - `pub enum SessionEvent { Dirty, Title(String), Bell, Exited(Option<i32>), Disconnected(String) }`
  - `pub type EventSink = tokio::sync::mpsc::UnboundedSender<(SessionId, SessionEvent)>`
  - `pub struct SessionHandle { pub id: SessionId, pub terminal: SharedTerminal, /* private: input tx, keepalive */ }` with `send(&self, bytes: Vec<u8>)`, `resize(&self, size: GridSize)`, `close(&self)`.
  - `pub fn spawn_local(profile: &LocalProfile, size: GridSize, scrollback: usize, events: EventSink) -> Result<SessionHandle, SessionError>`

- [ ] **Step 1: Write failing tests**

```rust
#[tokio::test] async fn echo_reaches_terminal()       // run `sh -c 'echo fbterm-ok'` (cmd /C on Windows) → grid contains "fbterm-ok", then Exited(Some(0))
#[tokio::test] async fn input_reaches_process()       // run `cat`, send b"abc\n" → grid shows "abc" twice
#[tokio::test] async fn resize_reaches_process()      // run `sh -c 'sleep 0.2; stty size'` after resize to 100x30 → "30 100"
#[tokio::test] async fn flood_coalesces_dirty_events() // run a command printing 50 MB; count Dirty events < 2_000 and the receiver never holds > 1 pending Dirty
```

- [ ] **Step 2: Run** `cargo test -p fbterm-session --test local` — Expected: FAIL.

- [ ] **Step 3: Implement.** A blocking reader thread reads PTY output in 64 KiB chunks, locks the terminal, feeds, forwards `TermEvent::PtyWrite` back to the PTY writer, and sends `Dirty` only if an `AtomicBool dirty_pending` was false (the UI clears it when it redraws; expose `SessionHandle::ack_dirty()`). Set `TERM=xterm-256color`, `COLORTERM=truecolor`.

- [ ] **Step 4: Run** the tests — Expected: PASS on all three CI OSes.

- [ ] **Step 5: Commit** `git commit -m "feat(session): local pty sessions with coalesced redraw events"`

---

### Task 6: Config, profiles and shell discovery

**Files:**
- Create: `crates/fbterm-config/src/{paths.rs, profile.rs, store.rs, settings.rs, shells.rs}`

**Interfaces:**
- Produces:
  - `pub struct Paths { pub config_dir: PathBuf }` with `from_env() -> Self`, `profiles() -> PathBuf` (`profiles.toml`), `settings()` (`settings.toml`), `state()` (`state.toml`).
  - `pub struct ProfileId(pub uuid::Uuid)` (serde as string)
  - `pub struct Rgb { pub r: u8, pub g: u8, pub b: u8 }` (serde as `"#rrggbb"`; also used by `fbterm-term::ColorScheme`)
  - `pub struct Profile { pub id: ProfileId, pub name: String, pub group: Option<String>, pub accent: Option<Rgb>, pub kind: ProfileKind }`
  - `pub enum ProfileKind { Local(LocalProfile), Ssh(SshProfile) }` (serde `tag = "kind"`, values `local`/`ssh`)
  - `pub struct LocalProfile { pub program: PathBuf, pub args: Vec<String>, pub cwd: Option<PathBuf>, pub env: BTreeMap<String, String> }`
  - `pub struct SshProfile { pub host: String, pub port: u16 /* default 22 */, pub user: String, pub auth: AuthMethod, pub jump: Vec<ProfileId> }`
  - `pub enum AuthMethod { Password, Key { path: PathBuf, passphrase: bool }, Agent }`
  - `pub struct ProfileStore { pub profiles: Vec<Profile> }` with `load(path) -> Result<Self, ConfigError>`, `save(&self, path) -> Result<(), ConfigError>` (write temp file + rename), `validate(&self) -> Vec<ValidationError>`, `get(&self, id) -> Option<&Profile>`, `jump_chain(&self, id) -> Result<Vec<&Profile>, ValidationError>` (hops in connection order, target excluded).
  - `pub enum ValidationError { DuplicateName(String), MissingJump { profile: String, missing: ProfileId }, JumpNotSsh { profile: String }, JumpCycle { profile: String }, EmptyHost(String) }`
  - `pub struct ConfigError { pub path: PathBuf, pub line: Option<usize>, pub message: String }`
  - `pub struct Settings { pub font_family: String /* "Geist Mono" */, pub font_size: f32 /* 13.0 */, pub scrollback_lines: usize /* 10_000 */, pub theme: ThemeMode /* Dark */ }`, `ThemeMode { Dark, Light }`; `Settings::load_or_default(path)`.
  - `pub fn discover_shells() -> Vec<Profile>`

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn round_trip_profiles_toml()               // save then load equals original; file has no "password" key
#[test] fn ssh_port_defaults_to_22()
#[test] fn syntax_error_reports_line_and_keeps_file() // load "[[profiles]\n" → Err with line == Some(1); file bytes unchanged
#[test] fn dangling_jump_is_a_validation_error()    // → MissingJump
#[test] fn jump_cycle_detected()                    // a→b→a → JumpCycle
#[test] fn jump_chain_order_two_hops()              // target jumps [b1, b2] → [b1, b2]
#[test] fn jump_to_local_profile_rejected()         // → JumpNotSsh
#[test] fn missing_settings_file_gives_defaults()   // font 13.0, scrollback 10_000
#[cfg(unix)] #[test] fn discover_reads_etc_shells_format() // parse fixture "/bin/zsh\n# c\n/bin/bash\n" → names "zsh", "bash"
```

- [ ] **Step 2: Run** `cargo test -p fbterm-config` — Expected: FAIL.

- [ ] **Step 3: Implement.** Unix discovery: `$SHELL` first, then `/etc/shells` entries that exist; Windows: `pwsh.exe`, `powershell.exe`, `cmd.exe`, Git Bash (`%ProgramFiles%\Git\bin\bash.exe`), `wsl.exe` when present. Profile name = file stem. The store never writes a file it failed to parse.

- [ ] **Step 4: Run** — Expected: PASS.

- [ ] **Step 5: Commit** `git commit -m "feat(config): profiles, settings and shell discovery"`

---

### Task 7: Secret store

**Files:**
- Create: `crates/fbterm-secrets/src/lib.rs`, `crates/fbterm-secrets/src/keychain.rs`

**Interfaces:**
- Consumes: `ProfileId` (Task 6).
- Produces:
  - `pub enum SecretKind { Password, KeyPassphrase }`
  - `pub trait SecretStore: Send + Sync { fn get(&self, id: &ProfileId, kind: SecretKind) -> Result<Option<String>, SecretError>; fn set(&self, id: &ProfileId, kind: SecretKind, value: &str) -> Result<(), SecretError>; fn remove(&self, id: &ProfileId) -> Result<(), SecretError>; }`
  - `pub struct MemoryStore` (tests, and the fallback)
  - `pub struct KeychainStore` — one keychain entry `("fbterm", "secrets")` holding a JSON map `{ "<uuid>": { "password": "...", "key_passphrase": "..." } }`; the map is read once and cached; writes rewrite the entry.
  - `pub struct FallbackStore { primary: KeychainStore, session: MemoryStore }` — on any keychain error, logs once, sets `degraded = true`, and serves from `session`. `pub fn degraded(&self) -> bool` (the app shows a notice).

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn memory_store_set_get_remove()
#[test] fn map_serialization_has_one_entry_for_many_profiles() // 20 profiles → one serialized blob
#[test] fn fallback_serves_session_secrets_when_primary_fails() // primary = failing mock → get after set works, degraded() == true
#[test] #[ignore = "touches the real OS keychain"] fn keychain_round_trip()
```

- [ ] **Step 2: Run** `cargo test -p fbterm-secrets` — Expected: FAIL.
- [ ] **Step 3: Implement** with `keyring` 4.2 (enable its platform-native features for macOS, Windows and Linux Secret Service). Make the primary an internal trait so the failing mock can be injected.
- [ ] **Step 4: Run** — Expected: PASS. Also once by hand: `cargo test -p fbterm-secrets -- --ignored` on macOS.
- [ ] **Step 5: Commit** `git commit -m "feat(secrets): single-item keychain store with session fallback"`

---

### Task 8: SSH connect, auth and host keys

**Files:**
- Create: `crates/fbterm-session/src/ssh/{mod.rs, auth.rs, known_hosts.rs}`, `dev/ssh-test/compose.yaml`
- Test: `crates/fbterm-session/tests/ssh_live.rs`; modify `.github/workflows/ci.yml` (enable `ssh-live` job)

**Interfaces:**
- Consumes: `SshProfile`, `AuthMethod`, `ProfileId` (Task 6), `SecretStore`, `SecretKind` (Task 7).
- Produces:
  - `#[async_trait] pub trait Prompter: Send + Sync { async fn confirm_host_key(&self, host: &str, port: u16, fingerprint_sha256: &str) -> bool; async fn ask_secret(&self, profile_name: &str, kind: SecretKind, retry: bool) -> Option<SecretAnswer>; }`
  - `pub struct SecretAnswer { pub value: String, pub remember: bool }`
  - `pub enum HostKeyStatus { Known, Unknown, Changed }`; `pub fn check_host_key(known_hosts: &Path, host: &str, port: u16, key: &PublicKey) -> Result<HostKeyStatus, SshError>`; `pub fn learn_host_key(known_hosts: &Path, host: &str, port: u16, key: &PublicKey) -> Result<(), SshError>` (OpenSSH format; `[host]:port` when port ≠ 22).
  - `pub enum SshError { HostKeyChanged { host: String, port: u16 }, HostKeyRejected, AuthFailed { profile: String }, Cancelled, Connect(std::io::Error), Jump(ValidationError), Protocol(String) }`
  - `pub(crate) async fn connect_hop<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(stream: S, profile: &Profile, ctx: &SshContext) -> Result<russh::client::Handle<ClientHandler>, SshError>`
  - `pub struct SshContext { pub known_hosts: PathBuf, pub secrets: Arc<dyn SecretStore>, pub prompter: Arc<dyn Prompter> }`

- [ ] **Step 1: Test servers.** `dev/ssh-test/compose.yaml`: two `linuxserver/openssh-server` containers on a private network, `bastion` (published on host port 2222) and `target` (not published, reachable only from bastion), each with user `fb`, password `fb-pass` and an authorized ed25519 test key; `bastion` has `AllowTcpForwarding yes`. A second `target2` behind `target` for two-hop tests. Test keys live in `dev/ssh-test/keys/` (test-only, with a README saying so; add the path to `.gitleaks.toml` allowlist if gitleaks is enabled).

- [ ] **Step 2: Write failing tests** (gated: skip unless `FBTERM_TEST_SSH=1`, like the gateway's `FEATHERBIT_TEST_*` pattern; each test uses a temp `known_hosts`)

```rust
#[tokio::test] async fn password_auth_then_shell_echo()         // bastion, Password from MemoryStore → "echo ok" prints ok
#[tokio::test] async fn key_auth_with_passphrase()              // passphrase-protected test key, passphrase in store
#[tokio::test] async fn unknown_host_prompts_and_learns()        // prompter says yes → known_hosts gains a line; second connect does not prompt
#[tokio::test] async fn unknown_host_rejected()                 // prompter says no → Err(HostKeyRejected)
#[tokio::test] async fn changed_host_key_refused_without_prompt() // known_hosts holds a different key → Err(HostKeyChanged), prompter never called
#[tokio::test] async fn wrong_saved_password_reprompts_and_replaces_only_on_success() // store has "bad"; prompter returns "fb-pass" remember=true → connected; store now "fb-pass"
#[tokio::test] async fn cancelled_prompt_gives_cancelled()      // prompter returns None → Err(Cancelled)
```
Plus unit tests (not gated) in `known_hosts.rs`: parse plain and `[host]:port` entries, hashed (`|1|`) entries match, unknown vs changed.

- [ ] **Step 3: Run** `docker compose -f dev/ssh-test/compose.yaml up -d && FBTERM_TEST_SSH=1 cargo test -p fbterm-session --test ssh_live` — Expected: FAIL.

- [ ] **Step 4: Implement.** Auth order follows `AuthMethod`: `Agent` → `russh::keys::agent::client::AgentClient::connect_env()`; `Key` → `russh::keys::load_secret_key(path, passphrase)` asking `KeyPassphrase` when the store has none or it fails; `Password` → store, then prompter with `retry = true` up to 3 tries. Save to the store only after the server accepts and only if `remember`. Host-key check happens in `ClientHandler::check_server_key` via the context. Enable keepalive (`keepalive_interval` 15 s, `keepalive_max` 2) so dead connections are detected.

- [ ] **Step 5: Run** — Expected: PASS. Enable the `ssh-live` CI job (ubuntu: compose up, wait for port 2222, run with `FBTERM_TEST_SSH=1`).

- [ ] **Step 6: Commit** `git commit -m "feat(ssh): connect, password/key/agent auth and known_hosts checks"`

---

### Task 9: Jump chains and the shared connection pool

**Files:**
- Create: `crates/fbterm-session/src/ssh/pool.rs`
- Test: extend `crates/fbterm-session/tests/ssh_live.rs`

**Interfaces:**
- Consumes: `connect_hop`, `SshContext` (Task 8); `ProfileStore::jump_chain` (Task 6); `SessionHandle`, `EventSink` (Task 5).
- Produces:
  - `pub struct SshPool` with `new(ctx: SshContext) -> Self`, `async fn open_shell(&self, store: &ProfileStore, profile: &ProfileId, size: GridSize, scrollback: usize, events: EventSink) -> Result<SessionHandle, SshError>`, `fn connected_profiles(&self) -> Vec<ProfileId>`.
  - `SessionHandle` gains a private `Option<Arc<Connection>>` keepalive; `Connection` holds the target `Handle` plus every hop `Handle` (dropping it closes the chain).

- [ ] **Step 1: Write failing tests** (gated)

```rust
#[tokio::test] async fn one_hop_reaches_unpublished_target()   // target via bastion → `hostname` prints target's name
#[tokio::test] async fn two_hops_reach_target2()
#[tokio::test] async fn two_tabs_share_one_connection()        // open_shell twice on the same profile → server sees one TCP connection (check via `ss -tn` on bastion or count sshd children)
#[tokio::test] async fn connection_closes_after_last_tab()     // close both handles → connected_profiles() empty within 2 s
#[tokio::test] async fn server_drop_emits_disconnected()       // `docker compose restart target` (or kill sshd child) → SessionEvent::Disconnected; terminal content preserved
```

- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.** For each hop after the first: `handle.channel_open_direct_tcpip(next_host, next_port, "127.0.0.1", 0)` → `channel.into_stream()` → `connect_hop(stream, next_profile, ctx)`. Pool map is `Mutex<HashMap<ProfileId, Weak<Connection>>>`; a concurrent second `open_shell` for a profile that is mid-connect awaits the same in-flight connect (store a shared future) instead of dialing twice. Shell channel: `request_pty("xterm-256color", cols, rows, 0, 0, &[])`, `request_shell`, window-change on resize; same coalesced `Dirty` logic as local sessions.
- [ ] **Step 4: Run** — Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "feat(ssh): jump chains and per-profile shared connections"`

---

### Task 10: App shell — windows, tabs, keyboard

**Files:**
- Create: `crates/fbterm/src/{main.rs, app.rs, window.rs, tabs.rs, keys.rs, theme.rs}`, `crates/fbterm/assets/fonts/*`

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `pub enum TabState { NotConnected, Connecting, Connected, Disconnected(String), Exited(Option<i32>) }`
  - `pub struct Tab { pub profile: ProfileId, pub title: String, pub state: TabState, pub session: Option<SessionHandle> }`
  - `pub struct WindowState { pub tabs: Vec<Tab>, pub active: usize }`
  - `pub enum Message` covering: `Session(SessionId, SessionEvent)`, `Terminal(window::Id, TerminalOutput)`, `NewTab`, `CloseTab(usize)`, `SelectTab(usize)`, `NextTab`, `PrevTab`, `OpenProfile { profile: ProfileId, window: window::Id }`, `Connected(..)`, `ConnectFailed(..)`, `NewWindow`, `WindowClosed(window::Id)`, `Palette(PaletteMsg)`, `Editor(EditorMsg)`, `Prompt(PromptMsg)`.
  - `pub fn graphite_theme(mode: ThemeMode) -> iced::Theme` (custom palette: background `#101013`, surfaces `#151518`/`#1f1f22`, borders `#313135`, text `#f5f5f7`/`#b6b6bf`/`#919199`, primary `#7c49e3`, success `#2bbb71`, danger `#ed4a49`; light values from `../design-system-v2/tokens/colors.css` light block).
  - `pub fn shortcut(key, mods) -> Option<Action>` in `keys.rs`, with `pub enum Action { NewTab, CloseTab, NextTab, PrevTab, SelectTab(u8), Palette, NewWindow, Settings, EditProfiles }`.

- [ ] **Step 1: Write failing tests** for the pure state logic: `keys::shortcut` table (Cmd+T NewTab, Cmd+W CloseTab, Cmd+Shift+] / Cmd+Shift+[ next/prev, Cmd+1..9 SelectTab, Cmd+Shift+P Palette, Cmd+N NewWindow, Cmd+, Settings on macOS; Ctrl+Shift+ equivalents elsewhere), and `TabState` transitions in `tabs.rs`: `Disconnected` + Enter → reconnect requested; `Exited` + Enter → tab respawns; closing the active last tab selects the previous; closing the window's last tab closes the window.

- [ ] **Step 2: Run** `cargo test -p fbterm` — Expected: FAIL.

- [ ] **Step 3: Implement** with `iced::daemon` (multi-window). Tab bar per window: profile name, state dot (connecting amber, connected green, disconnected red), accent stripe if set, close button; new tab (Cmd+T) opens the palette (Task 11) with no profile preselected. Disconnected and exited tabs print a dim inline notice into the terminal ("disconnected, press Enter to reconnect" / "process exited, press Enter to restart") and keep scrollback. Only the active tab of each window builds a `terminal_view`; background tabs render nothing. Startup with no saved state opens one window with the first local profile (the user's `$SHELL`). Load Geist Mono from bundled bytes.

- [ ] **Step 4: Run** tests — Expected: PASS. Manual check: `cargo run -p fbterm`, open 3 local tabs, run `vim`, `htop`, switch tabs with Cmd+1..3.

- [ ] **Step 5: Commit** `git commit -m "feat(app): multi-window tabs, shortcuts and graphite theme"`

---

### Task 11: Quick-launch palette

**Files:**
- Create: `crates/fbterm/src/palette.rs`

**Interfaces:**
- Consumes: `ProfileStore` (Task 6), `Action` (Task 10).
- Produces:
  - `pub enum PaletteItem { Profile { id: ProfileId, name: String, detail: String }, Action { action: Action, label: String } }` (`detail` = `user@host` via jump names, or the shell path)
  - `pub struct Recency` (profile id → last-opened unix seconds; persisted in `state.toml` by Task 13)
  - `pub fn rank(query: &str, items: &[PaletteItem], recency: &Recency) -> Vec<usize>`
  - `pub enum PaletteMsg { Open, Query(String), Up, Down, Submit, Close }`

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn empty_query_lists_recent_profiles_first_then_alphabetical_then_actions()
#[test] fn fuzzy_matches_noncontiguous()          // "prdb" matches "prod-db-01"
#[test] fn profiles_rank_above_actions()           // "split" matches profile "split-brain" and action "Split right" → profile first
#[test] fn tie_broken_by_recency()                 // equal scores → more recent first
#[test] fn no_match_returns_empty()
#[test] fn matches_detail_too()                    // query "10.0.4" finds a profile by host
```

- [ ] **Step 2: Run** `cargo test -p fbterm palette` — Expected: FAIL.
- [ ] **Step 3: Implement** with `nucleo_matcher::{Matcher, pattern::Pattern}` (smart case, normalize). UI: centered overlay over the focused window (width 560 px, radius 12, surface `#1f1f22`, input focused immediately, up to 10 rows, selected row with the violet edge), Esc closes, Enter submits; a profile opens in a new tab of that window and its recency is bumped. M1 actions listed: New tab, New window, Close tab, Edit profiles, Settings.
- [ ] **Step 4: Run** — Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "feat(app): quick-launch palette with fuzzy ranking"`

---

### Task 12: Profile editor and prompts

**Files:**
- Create: `crates/fbterm/src/editor.rs`, `crates/fbterm/src/prompts.rs`

**Interfaces:**
- Consumes: `ProfileStore`, `ValidationError`, `ConfigError` (Task 6); `SecretStore` (Task 7); `Prompter`, `SecretAnswer` (Task 8).
- Produces:
  - `pub struct EditorState` + `pub enum EditorMsg`; `fn apply(&mut self, store: &mut ProfileStore) -> Result<(), Vec<ValidationError>>`
  - `pub struct UiPrompter` implementing `Prompter` by sending a request plus a `oneshot::Sender` to the UI and awaiting the answer; `pub enum PromptMsg`.

- [ ] **Step 1: Write failing tests** for `EditorState`: new SSH profile with empty host refused (`EmptyHost`); jump list only offers SSH profiles other than itself; renaming keeps the id (so jump references survive); deleting a profile used as a jump is refused with the dependants' names; choosing auth `Password` with a typed password stores it in `SecretStore`, never in the profile; deleting a profile calls `SecretStore::remove`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement** the editor as a window (opened from the palette or Cmd+Shift+,): list grouped by `group`, form fields for name, group, accent (8 preset swatches from the design system plus none), kind-specific fields, jump chain as an ordered list with add/remove/reorder, auth method with password / key path picker (`rfd` file dialog) / agent. Save validates and writes `profiles.toml` atomically. Prompts: host-key dialog shows `host:port` and the SHA256 fingerprint in Geist Mono with "Trust and connect" / "Cancel"; changed key shows an error dialog with the `ssh-keygen -R` command and no accept button; secret prompt has a masked field and "Remember in keychain" (checked by default). If the profiles file failed to load, the editor shows the `ConfigError` (path, line, message) and is read-only until the file is fixed and reloaded.
- [ ] **Step 4: Run** — Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "feat(app): profile editor and connection prompts"`

---

### Task 13: Settings, restore and config reload

**Files:**
- Create: `crates/fbterm-config/src/state.rs`, `crates/fbterm/src/restore.rs`

**Interfaces:**
- Consumes: `Paths`, `Settings` (Task 6), `WindowState` (Task 10), `Recency` (Task 11).
- Produces:
  - `pub struct SavedState { pub windows: Vec<SavedWindow>, pub recency: BTreeMap<ProfileId, i64> }`, `pub struct SavedWindow { pub tabs: Vec<ProfileId>, pub active: usize, pub size: (f32, f32) }`; `SavedState::load_or_default(path)`, `save(path)` (atomic).

- [ ] **Step 1: Write failing tests**: round trip; tabs whose profile no longer exists are dropped on restore; `active` clamped when tabs are dropped; a window with no remaining tabs is dropped; missing or corrupt `state.toml` → default (one window), never an error.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.** Save on window close, tab open/close and app quit. On launch restore windows and tabs in `TabState::NotConnected`; connect the active tab of each window immediately and others on first selection. Watch `profiles.toml` and `settings.toml` (`notify` crate) and reload on change; a reload that fails to parse keeps the previous in-memory config and shows the error.
- [ ] **Step 4: Run** — Expected: PASS. Manual: open 4 tabs across 2 windows, quit, relaunch → same layout, only 2 connect.
- [ ] **Step 5: Commit** `git commit -m "feat(app): session restore and live config reload"`

---

### Task 14: Performance measurement

**Files:**
- Create: `dev/perf/sessions.sh`, `dev/perf/README.md`; modify `crates/fbterm/src/main.rs` (hidden flag)

**Interfaces:**
- Produces: hidden CLI flag `fbterm --perf-tabs <N> --perf-cmd "<shell command>"` opening N local tabs running the command (no restore, no state save).

- [ ] **Step 1: Script.** `sessions.sh` builds release, launches `fbterm --perf-tabs 20 --perf-cmd 'tail -f /var/log/system.log'` (Linux: `journalctl -f`), waits 30 s, then prints RSS (`ps -o rss= -p`) and average CPU over 10 s, and a second scenario with one tab running `yes`.
- [ ] **Step 2: Run** `bash dev/perf/sessions.sh` — Expected: RSS < 300 MB and idle CPU near 0% for the tail scenario. Record numbers in `dev/perf/README.md` with machine and date. If over target, report to the owner with the numbers before tuning further (the target is a builder's call and may be retuned).
- [ ] **Step 3: Commit** `git commit -m "test(perf): 20-session memory and cpu measurement"`

---

### Task 15: macOS packaging and release workflow

**Files:**
- Create: `dev/package-macos.sh`, `crates/fbterm/macos/Info.plist`, `crates/fbterm/macos/fbterm.icns` (generated from `../design-system-v2/assets/featherbit-mark.png` with `sips` + `iconutil`; commit the `.icns`), `.github/workflows/release.yml`

- [ ] **Step 1: Script.** `package-macos.sh <version>`: `cargo build --release --target aarch64-apple-darwin` and `x86_64-apple-darwin`, `lipo -create` into a universal binary, assemble `featherbit terminal.app` (`CFBundleExecutable` `fbterm`, `CFBundleIdentifier` `io.featherbit.fbterm`, `CFBundleShortVersionString` = version, `LSMinimumSystemVersion` 12.0, `NSHighResolutionCapable` true), ad-hoc sign it (`codesign --force --deep -s -`, required for Apple Silicon to launch), `hdiutil create -volname "featherbit terminal" -srcfolder … -format UDZO fbterm-<version>-macos-universal.dmg`, write `.sha256`.
- [ ] **Step 2: Verify locally** `bash dev/package-macos.sh 0.1.0-dev` → mount the dmg, copy the app to `/Applications`, right-click > Open launches it; `lipo -archs` shows `x86_64 arm64`.
- [ ] **Step 3: Workflow.** `release.yml` on push of tags `v*`: `macos-14` job runs CI checks, the script with the tag's version (fails if it differs from `Cargo.toml`), then `softprops/action-gh-release@v2` uploads `.dmg` and `.sha256` with generated notes.
- [ ] **Step 4: Commit** `git commit -m "ci(release): unsigned universal macOS dmg on version tags"`

---

### Task 16: Docs site skeleton

**Files:**
- Create: `website/` (Docusaurus 3.10.2, TypeScript, same structure as `../gateway/website`), `.github/workflows/docs.yml`

- [ ] **Step 1: Scaffold** with `url: 'https://featherbitplatform.github.io'`, `baseUrl: '/terminal/'`, title "featherbit terminal", Graphite colors and Geist/Geist Mono fonts (self-hosted via `@fontsource`), the feather mark as logo. Pages: Introduction (what it is, on its own terms), Install on macOS (download the dmg, drag to Applications, first launch with right-click > Open or `xattr -dr com.apple.quarantine "/Applications/featherbit terminal.app"`), Profiles and jump hosts (TOML reference matching Task 6 exactly), Keyboard shortcuts (table from Task 10/11), Where files live (config dir per OS, keychain item).
- [ ] **Step 2: Verify** `cd website && npm ci && npm run build` — Expected: build succeeds with no broken links.
- [ ] **Step 3: Workflow** `docs.yml`: on push to `main` and `workflow_dispatch`, build and deploy with `actions/upload-pages-artifact` + `actions/deploy-pages` (permissions `pages: write`, `id-token: write`, concurrency group `pages`), like the gateway's. Note for the owner: enable GitHub Pages with source "GitHub Actions" in the repo settings.
- [ ] **Step 4: Commit** `git commit -m "docs: docusaurus site skeleton and pages deployment"`

---

### Task 17: Wrap-up

- [ ] **Step 1:** Update `CLAUDE.md` "Status" and add the real commands section (build, run, single test, live SSH tests, perf script, package script, docs dev server).
- [ ] **Step 2:** Run the full gate: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && FBTERM_TEST_SSH=1 cargo test -p fbterm-session --test ssh_live && cargo deny check` — Expected: all pass.
- [ ] **Step 3:** Read through every committed doc page, README and commit message on the branch and confirm the product is described only on its own terms (Global Constraints, line 2).
- [ ] **Step 4:** Commit `docs: commands and status for milestone 1`, push, open a PR `feature/milestone-1` → `develop`.
- [ ] **Step 5:** Release 0.1.0 when the owner says so: `release/0.1.0` off `develop`, merge to `main`, annotated tag `v0.1.0`, merge back to `develop`.
