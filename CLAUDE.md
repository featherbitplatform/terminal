# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

**featherbit terminal** (also *featherbiterminal*), binary `fbterm`: a native, SSH-first terminal emulator in Rust with an **iced** GUI. Built to stay fast and light with ~20 concurrent sessions (streaming logs, idle shells, AI-agent TUIs), with saved SSH profiles, jump hosts, a quick-launch palette, tabs and tiling, named tunnels, an SFTP file panel and encrypted profile export. Also a plain terminal for local shells.

Binding design: `docs/superpowers/specs/2026-10-23-featherbit-terminal-design.md`. Read it before any feature work; it records which decisions are the owner's and which are open builder's calls. Implementation plans go next to it under `docs/superpowers/plans/`.

## Rules specific to this repo

- **Describe the product only on its own terms.** Never name or compare to another terminal application anywhere committed to this repo: code, comments, docs, website, commit messages, PR text, this file.
- **Cross-platform code, macOS releases first.** Code must build and test on macOS, Linux and Windows (CI covers all three); platform-specific APIs only behind `cfg` with an alternative. Release artifacts are unsigned macOS `.dmg` (Apple Silicon + Intel) until signing is decided.
- **Secrets never go in config files.** Passwords and key passphrases live in a single OS-keychain item (a map keyed by profile id) via the `keyring` crate; config is TOML in the platform config dir.
- **No home-grown crypto.** Export archives (`.fbprofiles`) use `age` with a passphrase recipient. "Shared" exports strip passwords, passphrases and key paths.
- **Host keys:** `~/.ssh/known_hosts` shared with OpenSSH; trust-on-first-use prompt for unknown hosts; changed keys are refused, never auto-accepted.
- **Look and feel** follows the workspace design system v2 "Graphite" (`../design-system-v2/`, start at its `SKILL.md`): dark-first graphite, violet as the single accent, Geist Mono for real values, Lucide icons, no emoji or em-dashes in product copy.

## Architecture (from the design)

- **Emulation:** `feathercritty` (our Apache-2.0 fork of the upstream emulation crate, [github.com/featherbitplatform/feathercritty](https://github.com/featherbitplatform/feathercritty)) owns parsing, grid and scrollback per session. Local shells via `portable-pty`.
- **Rendering:** custom iced widget drawing from a glyph atlas with damage tracking; invisible tabs do no render work. If iced primitives are too slow, the approved fallback is a raw wgpu renderer for the grid inside the iced window — iced stays for all chrome. Switching GUI toolkit is not an option without the owner.
- **SSH:** in-process `russh`, never shelling out to system `ssh`. Auth in scope: password, private key (± passphrase), ssh-agent; 2FA, FIDO keys, certificates and agent forwarding are deferred.
- **Connection model:** one SSH connection per profile, shared like ControlMaster. Every tab/pane is a shell channel on it; the file panel (SFTP via `russh-sftp`) and tunnels are further channels. Tunnels belong to the connection, not to a tab: `with-shell` tunnels start when the profile's first shell connects and stop with its last, on-demand tunnels may open a shell-less background connection, and the connection closes only when its last channel does.
- **Jump hosts:** a profile's jump chain references other saved profiles; each hop is SSH over a `direct-tcpip` channel from the previous hop, authenticated with that hop's own profile.
- **Runtime:** one process, tokio for I/O, one task per session feeding its terminal; background sessions keep parsing but never render. Multi-window (iced daemon mode); windows, tabs and pane layout restore on relaunch, reconnecting as built: every pane of each window's visible tab at once, background tabs on first show.
- **Palette:** Cmd+Shift+P (Ctrl+Shift+P elsewhere), application-scoped, fuzzy (`nucleo`) over profiles first, then tunnels (choosing one switches it on or off), then app actions.
- **Shortcuts:** actions, chords, per-platform defaults and the binding table are data in `fbterm-config/src/keys.rs`; the user's overrides are the `[keys]` table of `settings.toml`. The app keeps one `Bindings` (`App::bindings`, rebuilt when settings apply) and matches presses against it in `keys.rs`/`shortcuts.rs`; tooltips, the palette and the Settings window's Keyboard section read the same table, so never hardcode a chord label.

## Conventions

- **Gitflow:** `main` holds tagged releases (`vX.Y.Z`) only; `develop` is the integration branch; work on `feature/<name>` off `develop`; `release/X.Y.Z` and `hotfix/<name>` as in the gateway repo.
- **Conventional Commits** (`feat:`, `fix:`, `docs:`, `ci:`, `chore:`, `refactor:`, `test:`, optional scope). Do not add a `Co-Authored-By` trailer.
- **Semantic versioning**, released like the gateway: `release/X.Y.Z` off `develop` (version bump in `Cargo.toml` there), merged into `main` with an annotated `vX.Y.Z` tag, then merged back into `develop`. The `v*` tag triggers the release workflow.
- Docs website: Docusaurus in `website/`, deployed to `https://featherbitplatform.github.io/terminal/` by `.github/workflows/docs.yml` — same pattern as `../gateway/website`.
- CI gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo deny check`.

## Commands

Run `source "$HOME/.cargo/env"` first if cargo is not on PATH.

```
cargo run -p fbterm                                   # build and run the app
cargo test -p fbterm-config jump_cycle_detected       # one test by name (any crate: -p <crate> <filter>)

# Full gate, as CI runs it (fmt is Linux-only in CI; deny is cargo-deny-action)
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny --all-features check

# Live SSH tests (CI job "ssh live tests"; needs Docker)
docker compose -f dev/ssh-test/compose.yaml up -d && dev/ssh-test/wait-ready.sh
FBTERM_TEST_SSH=1 cargo test -p fbterm-session --test ssh_live
FBTERM_TEST_SSH=1 cargo test -p fbterm-session --test ssh_tunnels
FBTERM_TEST_SSH=1 cargo test -p fbterm-session --test ssh_sftp
docker compose -f dev/ssh-test/compose.yaml down

bash dev/perf/sessions.sh                # 20-tab memory/CPU measurement, about 25 min; see dev/perf/README.md
bash dev/package-macos.sh <version>      # universal .app + .dmg into target/dist/ (macOS only)
bash dev/make-icns.sh [mark.png]         # regenerate crates/fbterm/macos/fbterm.icns (needs Pillow)
cd website && npm start                  # docs dev server; `npm run build` builds (fails on broken links)
```

### Debug aids (environment variables)

- Always available: `FBTERM_CONFIG_DIR=<dir>` (use this directory instead of the real config), `FBTERM_SECRETS=memory` (never touch the OS keychain), and the release-build measurement flags `--perf-tabs <N>` (or `--perf-panes <N>`: one tab with N panes) `--perf-cmd <cmd> [--perf-exit-after <secs>] [--perf-close-after <secs>]` (the last closes every tab but the active one).
- Debug builds only (ignored in release): `FBTERM_OPEN_TABS=<profile>,<profile>`, `FBTERM_SPLITS=<layout>` (the first tab as split panes of these profiles: `|` side by side, `/` stacked, binding left to right, e.g. `A|B/C`), `FBTERM_PALETTE_QUERY=<text>`, `FBTERM_SCREENSHOT=<path.png>` (capture the first window and exit), `FBTERM_SCREENSHOT_WINDOW=editor|settings|schemes|tunnels`, `FBTERM_OPEN_EDITOR=<profile>`, `FBTERM_OPEN_SETTINGS=1`, `FBTERM_OPEN_SCHEMES=1` (the Color schemes window; any other value selects the scheme of that name), `FBTERM_OPEN_TUNNELS=1` (the Tunnels window), `FBTERM_DEMO_FILES=<folder>:<name>,<name>/,...` (the first window's file panel open on a sample listing served for every SSH pane, connected or not, never touching the network; names ending in `/` are folders), `FBTERM_DEMO_PROMPT=hostkey|password|changed`, `FBTERM_DEMO_TOOLTIP=new|profiles|tunnels|settings|close` (keep that tab-bar tooltip showing), `FBTERM_FIND=<text>` / `FBTERM_FIND_ALL=<text>` (open the active tab's find bar or the find-in-all-tabs box with that query, about a second after startup), `FBTERM_SCALE=<factor>` (extra UI scale on top of the OS's, e.g. `0.625` on a 2x screen for a fractional 1.25). Runs with `FBTERM_OPEN_TABS`, `FBTERM_SPLITS` or `FBTERM_SCREENSHOT` never write `state.toml`.
- Rule for agents: look at the app only through its own-window screenshots (`FBTERM_SCREENSHOT`), never full-screen capture. Combine with `FBTERM_CONFIG_DIR` and `FBTERM_SECRETS=memory` so a run cannot touch real profiles or secrets.

## Gotchas learned

- Windows CI clippy fails on imports and helpers used only by `#[cfg(unix)]` tests; gate them with the same `cfg`.
- `hdiutil create` fails locally on macOS 27 ("Resource busy"); `dev/package-macos.sh` falls back to `diskutil image create` locally, CI uses `hdiutil` only.
- Redraw contract: call `take_dirty` before locking the terminal to draw; hidden tabs, and every tab of a window that cannot be seen (`visibility.rs`), leave `Dirty` pending until they are shown. Every message redraws every window, so output `Dirty` is paced before it reaches the update loop (`pacing.rs`, at most `max_fps` per second: the setting, default `OUTPUT_FPS` = 60, or the fastest display with `"display"` via `display_rate.rs`; read live from the shared `OutputRate`); never add a periodic subscription or redraw loop that keeps running without new output or input (debug aids aside).
- `feathercritty` is a git dependency pinned by `rev` in the root `Cargo.toml` (its changes from upstream are listed in its README; `deny.toml` allows git sources from `github.com/featherbitplatform` only). To change emulation code: commit in the feathercritty repo (`../feathercritty`, Conventional Commits, notice line at the top of each file it modifies), push, wait for its CI, then bump the `rev` here and run the checks. For local development, point Cargo at the working copy with a `[patch."https://github.com/featherbitplatform/feathercritty"]` entry `feathercritty = { path = "../feathercritty" }` in the root `Cargo.toml`; never commit it.
- feathercritty stores scrollback in a compact byte form, so `grid[...]` indexing is screen-only: use `grid.screen()[...]` for the visible rows and `grid.screen_mut()` to write them. History rows are read through `RowReader`, `Grid::cell`, `Grid::wraps` and `Grid::is_clear` (see the feathercritty README, "Compact history"); never index a history line directly.
- Search positions are `StablePoint`s (`fbterm-term/src/search.rs`): stable line = feathercritty's `Grid::scrolled_lines()` + grid line, unchanged while output scrolls; a resize (reflow) or a screen switch invalidates them, so the find bar searches again then.
- The tab bar paints its background around the active tab only (`BarBackground` in `window.rs`: rectangles plus concave top-corner pieces and bottom flares drawn as clipped disc and ring quads, all in whole physical pixels from `App::physical_scale`; it also paints the inactive tabs' hover and tint), so a translucent bar never lies under the active tab; anything drawn behind it would double the alpha. Tab colors (stripe or whole-tab tint, contrast-picked text) come from `tab_look.rs`.
- Tab export (`fbterm-term/src/export.rs`, `fbterm/src/export.rs`) streams the history on the blocking pool in 1000-row chunks, locking the terminal per chunk and addressing rows by stable line (like search), into a temporary file renamed at the end; with the alternate screen up it reads the normal grid through feathercritty's `Term::inactive_grid()`.
- A session can continue into an existing `SharedTerminal` (`spawn_local_into`, `open_shell_into`), which is how reconnect keeps scrollback.
- Freed scrollback goes back to the OS through `memory::release_freed()` (macOS `malloc_zone_pressure_relief`, glibc `malloc_trim`, nothing elsewhere; at most once per 5 s, on the blocking pool). Only user actions call it (closing a tab or window, requested when the closed session task reports `SessionEvent::Released` after dropping its terminal, which only a user close triggers; lowering `scrollback_lines`); a program's own history clear (`CSI 3 J`, `RIS`) must never, because remote programs cause no host work. Lowering `scrollback_lines` applies to every open tab through `Terminal::set_scrollback`; `apply_settings` takes a `SettingsSource` (`File` sets the settings notice, `Window` does not).
- Tunnels share the pooled SSH connection of their profile (`SshPool::acquire`); the tunnel runtime claims its forward exclusively (`claim_forward`), and a restart must `stop_and_wait` first so the port is released. The client handler (`ssh/mod.rs`) refuses every server-opened channel unless it is a `forwarded-tcpip` for a remote forward registered on that connection, and always on hop connections: a remote program must never open, change or close a tunnel. A pooled connection is handed out only on the profile's current route (the SSH settings of the profile and its jump chain), so an edit makes the next caller dial afresh while running tunnels keep the old one. The raw remote-forward calls on `Connection` exist only with the crate's `test-support` feature, for the live tests.
- Color scheme resolution: profile override, then global `color_scheme`, then Graphite, always through `App::scheme_for` (never read a pane's colors elsewhere). After scheme files change call `App::reload_schemes`; after a setting or profile change `schemes_moved` recolors the affected panes (`reapply_schemes`). Scheme notices (unreadable files, missing ids) go to the window banner via `refresh_scheme_notices`, once per distinct line. Pane accent borders draw only in tabs with more than one pane.

## Status

Milestone 1 (daily driver) and compact scrollback history were released as 0.1.0 (tag `v0.1.0` on `main`). Milestone 1: GPU terminal widget on `feathercritty`, local shells, tabs and multiple windows with layout restore, SSH profiles (password, key, agent) with jump hosts, shared connection per profile and host-key checks, profile editor, quick-launch palette, TOML settings, Graphite theme, CI on macOS/Linux/Windows with live SSH tests, unsigned macOS `.dmg` packaging and release workflow, and the Docusaurus docs site. Compact scrollback history (feathercritty `0.26.0+fbterm.5`, live `scrollback_lines` with a Settings confirmation, memory release): 20 tabs with full scrollback about 123 MB resident instead of 528 MB, against a standing target of at most 200 MB (spec section 7). Gate results and the accepted scenario G miss are in `dev/perf/README.md`. Split panes (tiling, the first part of milestone 2: splits, per-pane zoom, maximize, rearranging tiles and merging tabs, search across panes, restore) are on `develop`; measurements are in `dev/perf/README.md` and spec section 7. Color schemes (four built-ins, user scheme files, the Color schemes window, global and per-profile choice, scheme fonts) and pane accent borders are on `develop`, finishing milestone 2. Tunnels (milestone 3: local, remote and SOCKS5 forwards per SSH profile, on-demand or with-the-shell start, backoff reconnecting with the `tunnel_max_retries` setting, the Tunnels window, tab-bar indicator and palette rows; spec `docs/superpowers/specs/2026-10-27-tunnels-design.md`) are on `develop`. What comes next (the file panel, see the spec, section 6) and the next release are the owner's call.
