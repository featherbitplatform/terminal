# Inline images, application side (fbterm) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. The implementer step runs through the `local-implementer` skill.

**Goal:** fbterm decodes, stores, bounds and draws the images feathercritty reports, tells programs its real pixel sizes everywhere they look, offers the two settings, and works for local shells on Windows, so sixel, kitty graphics and OSC 1337 programs show images in fbterm.

**Architecture:** a new module `fbterm-term/src/images/` owns an app-wide `ImageStore` (decode on the blocking pool, decoded buffers, per-pane and app caps with LRU eviction, resampled copies per scale) fed by feathercritty's `ImageReceived`/`ImageReleased` events. The grid widget draws `visible_placements` with iced's image primitive in the same frame as text (below or above glyphs by z-index), placeholders as dim text, and kitty Unicode placeholder cells by resolving them through feathercritty. Pixel sizes reach programs through feathercritty's reports, the local PTY size and SSH `pty-req`/`window-change`.

**Tech Stack:** Rust, iced 0.14 with the `image` feature, `png`, `zune-jpeg`, `gif` (first frame), `fast_image_resize` (pure Rust), feathercritty at the commit produced by its plan.

**Spec:** `docs/superpowers/specs/2026-11-16-inline-images-design.md`. Emulation half: `../feathercritty/docs/plans/2026-11-16-inline-images.md` (must be merged first; its README "Graphics" section documents the API used here).

## Global Constraints

- Everything in `CLAUDE.md` binds (cross-platform, no terminal application names, redraw contract, hairlines, secrets rules); Gitflow on `feature/inline-images` off `develop`; Conventional Commits; no attribution.
- Memory (spec 6.3, exact): 256 MiB of decoded image data per pane, 512 MiB app-wide; past a cap the least recently shown images are evicted, scrollback first (`Term::evict_oldest_scrollback_image`), an evicted on-screen image becomes its placeholder.
- Settings (spec goals 6-7, exact names and defaults): `inline_images = true`, `scrollback_images = false` in `settings.toml`; both live-applied to every pane; rows in the Settings tab's scrollback section next to "Kitty keyboard protocol".
- Hidden tabs and invisible windows: decode yes (replies depend on it), resample, upload and draw never; no new periodic redraw.
- Scaling (spec 5): resample once per (image, physical cell size, scale) and cache; nearest-neighbour when the scale factor is an integer and the source is at most 256×256, a Lanczos3 filter otherwise.
- No new system C libraries; `cargo deny` passes; every new dependency in `[workspace.dependencies]`.
- Windows local shells are in scope (owner): images from local programs must reach fbterm.

## Review Focus

1. Twenty tabs, each having shown large images, then all hidden: memory returns under the caps, nothing redraws, and showing a tab re-uploads only what it displays. Test in Task 2 (`caps_hold_across_panes`) and the controller's perf check in Task 7.
2. Moving a window between monitors of different scale while an image is on screen: the image stays on its cells at the new scale, and programs get the new pixel sizes. Test in Task 3 (`scale_change_resends_pixel_sizes`) and Task 4 (`rescale_keeps_cells`).
3. A program flooding images (a viewer stepping through hundreds of pictures): the app stays responsive, eviction keeps memory bounded, released images free their textures. Test in Task 2 (`flood_is_evicted_lru`).
4. Turning `inline_images` off with images on screen: they disappear at once and memory is released; turning it on again does not resurrect them. Test in Task 1 (`switch_off_clears_images`).
5. A corrupt PNG/JPEG/GIF or a decoder panic: the image fails with the protocol's error reply, other images and the pane keep working. Test in Task 2 (`corrupt_and_panicking_decodes_are_contained`).

---

### Task 1: Pin feathercritty, settings and wiring

**Files:** Modify root `Cargo.toml` (feathercritty `rev`), `crates/fbterm-config/src/settings.rs` (+`settings_edit.rs`), `crates/fbterm/src/settings_window/{view.rs,mod.rs,glue.rs}`, `crates/fbterm/src/sessions.rs`, `crates/fbterm-term/src/terminal.rs`.

**Interfaces:** `Settings { inline_images: bool /* default true */, scrollback_images: bool /* default false */ }`; `SettingChange::{InlineImages(bool), ScrollbackImages(bool)}`; `Terminal::set_images(&self, enabled: bool, keep_scrollback: bool)` calling feathercritty's `set_images_enabled` and `set_scrollback_images`; applied at session start (`sessions.rs`) and live (`glue.rs`, the `kitty_keyboard` pattern). Settings rows: `Inline images` ("Show pictures that programs send") and `Images in scrollback` ("Keep pictures when they scroll off the screen"), the second disabled while the first is off.
- [ ] Tests: `settings_round_trip_images`, `switch_off_clears_images` (a terminal with a placement, switched off: no visible placements, `ImageReleased` for each), and the Settings view test pattern the repo uses for rows.
- [ ] Commit `feat: image settings`.

### Task 2: The image store

**Files:** Create `crates/fbterm-term/src/images/{mod.rs,store.rs,decode.rs,resample.rs}`; Modify `crates/fbterm-term/src/terminal.rs` (route `ImageReceived`/`ImageReleased` from the event proxy to the store with the pane id).

**Interfaces:**
- `pub struct ImageStore` (one per app, `Arc`, internally `Mutex` with short critical sections): `submit(&self, pane: PaneId, key: ImageKey, data: ImageData)` spawns the decode on the blocking pool and, when done, calls back through a channel the session task drains: `DecodeDone { pane, key, result }` → `term.image_decoded(key, result)` then marks the pane dirty; `release(&self, pane, key)`; `drop_pane(&self, pane)`; `get(&self, pane, key, cell_px: (u32, u32), span: (u32, u32), scale_bits: u32) -> Option<Arc<ResampledImage>>` returning a cached resample (computed on demand on the blocking pool; returns `None` until ready and marks dirty when ready); `touch(&self, pane, key)` for LRU on every draw.
- Accounting: decoded bytes per pane ≤ 256 MiB and total ≤ 512 MiB; when a submit would exceed a cap, evict least recently shown images of that pane (asking the terminal for `evict_oldest_scrollback_image` first), then app-wide LRU; resampled copies count too and are dropped first.
- `decode.rs`: `fn decode(data: ImageData) -> Result<Rgba8Image, DecodeError>` (PNG via `png` with limits, JPEG via `zune-jpeg`, GIF first frame via `gif`, RGB→RGBA), refusing beyond 10 000 px a side / 64 MiB; run inside `std::panic::catch_unwind` so a decoder panic is a `DecodeError`.
- `resample.rs`: `fn resample(src: &Rgba8Image, target_px: (u32, u32), integer_scale: bool) -> Rgba8Image` per the Global Constraint filter rule; fit-inside-keeping-aspect for the rescale rule (spec goal 3).
- [ ] Tests: `decodes_png_jpeg_gif` (fixtures), `golden_resample_at_scales` (reference buffers for scales 1, 1.25, 1.5, 2 under `tests/fixtures/images/`, compared byte-exact for nearest and within a tolerance of 1 per channel for Lanczos), `caps_hold_across_panes`, `flood_is_evicted_lru`, `corrupt_and_panicking_decodes_are_contained`, `release_frees_bytes`.
- [ ] Commit `feat(images): decode, store and bound images`.

### Task 3: Pixel sizes for programs

**Files:** Modify `crates/fbterm-term/src/terminal.rs` (call `term.set_cell_pixel_size` with physical pixels on every resize and scale change; answer `TextAreaSizeRequest` in physical pixels), `crates/fbterm-session/src/local.rs` (`pty_size`: `pixel_width`/`pixel_height` in physical pixels), `crates/fbterm-session/src/ssh/pool.rs` (`request_pty` and `window_change` pass pixel width/height instead of 0), `crates/fbterm/src/app.rs` (a scale change resizes every pane's session so the pixel fields are resent).

**Interfaces:** `GridSize` gains `scale: f32` (or a separate physical cell size) so sessions compute physical pixels: `fn physical_cell(size: &GridSize) -> (u32, u32)` rounding to whole pixels exactly as the renderer does.
- [ ] Tests: `local_pty_gets_physical_pixels` (unit on `pty_size`), `ssh_window_change_carries_pixels` (live test pattern behind `FBTERM_TEST_SSH`, plus a unit test on the message values), `scale_change_resends_pixel_sizes`.
- [ ] Commit `feat: report physical pixel sizes to programs`.

### Task 4: Drawing images

**Files:** Modify `crates/fbterm-term/Cargo.toml` (iced `image` feature), `crates/fbterm-term/src/widget/{mod.rs,draw.rs}`; Create `crates/fbterm-term/src/widget/images.rs`.

**Interfaces:** the widget's `draw` gets the store and the scale: for each `visible_placement` (z < 0 drawn after cell backgrounds and before text; z ≥ 0 after text via `renderer.with_layer`), it asks `ImageStore::get` for the resample at the current physical cell size and draws an iced image handle created once per resample (`image::Handle::from_rgba`, kept in the resample so the GPU upload happens once); partly scrolled-off images are clipped to the pane; `src`, `x_off`, `y_off` honoured; placeholders drawn in the scheme's muted text colour; `U+10EEEE` cells resolved through `kitty_virtual_image`/`decode_placeholder_cell` and drawn as the matching tile of the image. Hidden tabs never reach this code (existing visibility rules).
- [ ] Tests: `rescale_keeps_cells` (layout function: placement cells to pixel rects at two scales), `placement_rects_clip_to_pane`, `virtual_placeholder_tiles` (pure tile math), and own-window screenshots in Task 7.
- [ ] Commit `feat: draw inline images`.

### Task 5: Windows local shells

**Files:** Create `crates/fbterm-session/tests/conpty_passthrough.rs` (cfg(windows)); depending on its result, `crates/fbterm-session/src/conpty.rs`, packaging changes in `dev/package-windows.ps1` and `crates/fbterm/wix/main.wxs`.

- [ ] **Step 1 (decides the rest):** a test, run on the `windows-latest` CI runner, that starts a child through portable-pty which writes a kitty APC, a sixel DCS and an OSC 1337 sequence (a small helper binary in the test crate), and asserts the bytes arrive unchanged at the master side. Record the result in the task report and the ledger.
- [ ] **Step 2 (only if Step 1 shows stripping):** ship Microsoft's open-source ConPTY (`conpty.dll` + `OpenConsole.exe` from the `Microsoft.Windows.Console.ConPTY` package, MIT, pinned version and SHA-256 checked in `dev/package-windows.ps1`), installed next to `fbterm.exe` by the `.msi`; `conpty.rs` loads `conpty.dll` from the executable's directory (`LoadLibraryExW` with `LOAD_LIBRARY_SEARCH_APPLICATION_DIR` only, never the search path) and uses its `CreatePseudoConsole`/`ResizePseudoConsole`/`ClosePseudoConsole`, falling back to the system ones when it cannot load (one log line). portable-pty does not expose this choice: patch it through `[patch.crates-io]` with a small fork, or wrap our own ConPTY spawn for local shells (the builder decides by reading portable-pty's `win/conpty.rs`; record the ruling). The Step 1 test then runs against the bundled DLL and passes.
- [ ] Commit `feat(windows): images from local shells`.

### Task 6: Docs

**Files:** Modify `CLAUDE.md` (images architecture paragraph, settings, the parser copy in feathercritty), `website/docs/` (a new `images.md` page: supported protocols, settings, limits, the Claude Code environment variable `CLAUDE_CODE_FORCE_TERMINAL_IMAGES=1`), `docs/superpowers/specs/2026-10-23-featherbit-terminal-design.md` is already updated; `CHANGELOG.md` Unreleased → Added.
- [ ] Commit `docs: inline images`.

### Task 7: Verification (controller)

- [ ] Own-window screenshots (`FBTERM_SCREENSHOT`, scratch config, `FBTERM_SECRETS=memory`) at scale 1 and 2: `chafa -f sixel` and `chafa -f kitty` of a reference image, an OSC 1337 image from a small script, the same over the live SSH test container, a kitty Unicode-placeholder image, and featherbittorrent's logo once its milestone 3 draws it.
- [ ] `bash dev/perf/sessions.sh` with an image shown in each tab: within 10 % of the current numbers and under the caps.
- [ ] A Claude Code mod with an `Image` element (owner's check, with `CLAUDE_CODE_FORCE_TERMINAL_IMAGES=1`).
