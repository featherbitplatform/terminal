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

- **Emulation:** `alacritty_terminal` owns parsing, grid and scrollback per session. Local shells via `portable-pty`.
- **Rendering:** custom iced widget drawing from a glyph atlas with damage tracking; invisible tabs do no render work. If iced primitives are too slow, the approved fallback is a raw wgpu renderer for the grid inside the iced window — iced stays for all chrome. Switching GUI toolkit is not an option without the owner.
- **SSH:** in-process `russh`, never shelling out to system `ssh`. Auth in scope: password, private key (± passphrase), ssh-agent; 2FA, FIDO keys, certificates and agent forwarding are deferred.
- **Connection model:** one SSH connection per profile, shared like ControlMaster. Every tab/pane is a shell channel on it; the file panel (SFTP via `russh-sftp`) and tunnels are further channels. Tunnels belong to the connection, not to a tab: `on-connect` tunnels start once, on-demand tunnels may open a shell-less background connection, and the connection closes only when its last channel does.
- **Jump hosts:** a profile's jump chain references other saved profiles; each hop is SSH over a `direct-tcpip` channel from the previous hop, authenticated with that hop's own profile.
- **Runtime:** one process, tokio for I/O, one task per session feeding its terminal; background sessions keep parsing but never render. Multi-window (iced daemon mode); windows, tabs and pane layout restore on relaunch, reconnecting lazily on first focus.
- **Palette:** Cmd+Shift+P (Ctrl+Shift+P elsewhere), application-scoped, fuzzy (`nucleo`) over profiles first, then app actions.

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
docker compose -f dev/ssh-test/compose.yaml down

bash dev/perf/sessions.sh                # 20-tab memory/CPU measurement, about 25 min; see dev/perf/README.md
bash dev/package-macos.sh <version>      # universal .app + .dmg into target/dist/ (macOS only)
bash dev/make-icns.sh [mark.png]         # regenerate crates/fbterm/macos/fbterm.icns (needs Pillow)
cd website && npm start                  # docs dev server; `npm run build` builds (fails on broken links)
```

### Debug aids (environment variables)

- Always available: `FBTERM_CONFIG_DIR=<dir>` (use this directory instead of the real config), `FBTERM_SECRETS=memory` (never touch the OS keychain), and the release-build measurement flags `--perf-tabs <N> --perf-cmd <cmd> [--perf-exit-after <secs>]`.
- Debug builds only (ignored in release): `FBTERM_OPEN_TABS=<profile>,<profile>`, `FBTERM_PALETTE_QUERY=<text>`, `FBTERM_SCREENSHOT=<path.png>` (capture the first window and exit), `FBTERM_SCREENSHOT_WINDOW=editor`, `FBTERM_OPEN_EDITOR=<profile>`, `FBTERM_DEMO_PROMPT=hostkey|password|changed`. Runs with `FBTERM_OPEN_TABS` or `FBTERM_SCREENSHOT` never write `state.toml`.
- Rule for agents: look at the app only through its own-window screenshots (`FBTERM_SCREENSHOT`), never full-screen capture. Combine with `FBTERM_CONFIG_DIR` and `FBTERM_SECRETS=memory` so a run cannot touch real profiles or secrets.

## Gotchas learned

- Windows CI clippy fails on imports and helpers used only by `#[cfg(unix)]` tests; gate them with the same `cfg`.
- `hdiutil create` fails locally on macOS 27 ("Resource busy"); `dev/package-macos.sh` falls back to `diskutil image create` locally, CI uses `hdiutil` only.
- Redraw contract: call `take_dirty` before locking the terminal to draw; hidden tabs leave `Dirty` pending until they are shown.
- A session can continue into an existing `SharedTerminal` (`spawn_local_into`, `open_shell_into`), which is how reconnect keeps scrollback.

## Status

Milestone 1 (daily driver) is implemented; not yet merged to `develop`: GPU terminal widget on `alacritty_terminal`, local shells, tabs and multiple windows with layout restore, SSH profiles (password, key, agent) with jump hosts, shared connection per profile and host-key checks, profile editor, quick-launch palette, TOML settings, Graphite theme, CI on macOS/Linux/Windows with live SSH tests, unsigned macOS `.dmg` packaging and release workflow, and the Docusaurus docs site. Memory/CPU measurements and the owner's scrollback decision are in `dev/perf/README.md` and spec section 7. Next: milestone 2 (see the spec, section 6); the 0.1.0 release is the owner's call.
