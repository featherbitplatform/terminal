# Inline images: design

Status: written 2026-11-16 from the design conversation with the owner. Supersedes the "Inline images (sixel/kitty graphics) are out of scope for now" line of the main design, section 5.2, and the proposal memo `docs/superpowers/memos/2026-11-14-inline-images.md`.

Decisions are tagged **[owner]** when the owner made them, and **[builder's call]** when they were made while writing this document. Builder's calls may be revisited by whoever plans the work; owner decisions may not, without the owner.

## 1. Intent

fbterm shows images that programs send, so terminal programs that draw images work in fbterm as they are designed to: the featherbittorrent header and about screen with the featherbit mark, a Claude Code mod's `Image` element, file managers with previews, plotting tools, image viewers, locally or over SSH. **[owner]**

The owner wants the standards and the protocols TUI software actually relies on, not one product's private extension, and asked for all of them. **[owner]**

## 2. Goals

1. **Three image protocols, complete enough for real TUI libraries: [owner]**
   - **Sixel** (DCS `ESC P <params> q <data> ESC \`), the formal DEC standard and the common denominator of image-capable TUI libraries: raster attributes, the full colour register model (HLS and RGB definitions, at least 1024 registers, private registers per image), repeat, carriage return and newline within the image, transparent background (`P2=1`).
   - **The kitty graphics protocol** (APC `ESC _ G <keys> ; <payload> ESC \`): transmission of PNG, RGB and RGBA data, direct and chunked (`t=d`, `m=1`/`m=0`), optionally zlib-compressed (`o=z`); image ids and numbers; display by placement (`a=T`, `a=t` then `a=p`) with cell size (`c`, `r`), source rectangle, cell offsets and z-index above or below text; deletion (`a=d` with its selectors); the support query (`a=q`) and its replies; **Unicode placeholders** (`U=1`, virtual placements shown through `U+10EEEE` cells with row and column diacritics and the image id in the foreground colour), which Claude Code's mod images and multiplexers rely on.
   - **OSC 1337 inline images** (`ESC ] 1337 ; File=<args> : <base64> BEL`): `inline=1`, `width`/`height` in cells, pixels, percent or `auto`, `preserveAspectRatio`, PNG, JPEG and GIF (first frame) data.
2. **Detection works without configuration [builder's call, required by goal 1]:** DA1 advertises sixel (`4`); `CSI ? 1 ; 1 S`-style XTSMGRAPHICS queries for the colour register count and the maximum geometry answer; the kitty `a=q` query answers OK; the text-area and cell pixel size reports (`CSI 14 t`, `CSI 16 t`, `CSI 18 t`) report physical pixels at the current display scale, because image programs size images from them.
3. **Exact placement and size [owner: "all of them" when asked what would make him wince]:** an image covers exactly the cells the program asked for, at every font size, line height and display scale, including fractional scales; it moves with the text when the screen scrolls. A placement is anchored in cells: its cell span is fixed when it is placed (from the program's `c`/`r`, `width`/`height`, or the image's pixel size divided by the cell size at that moment). When the font, line height or display scale changes, the image keeps that cell span and is scaled to fit inside it with its aspect ratio kept (anchored top-left, never stretched or distorted); the text it covers stays the same. **[builder's call]**
4. **Fast [owner]:** typing and scrolling stay as fast with images on screen as without; decoding and resampling run off the UI thread; an image is uploaded to the GPU once and reused.
5. **Bounded memory [owner]:** caps per image, per pane and for the whole app (section 6.3); hidden tabs and invisible windows do no drawing work at all, like text today: no resampling, no GPU upload. They still parse and decode (bounded by the caps), because protocol replies such as kitty's `OK`/`EBADPNG` depend on the decode result and must not wait for the tab to be shown. **[builder's call]**
6. **Scrollback images are a setting, off by default [owner]:**
   - Off (default): an image that scrolls off the screen leaves a one-line placeholder in its first row, `[image 640×480]` (pixel size, `×` as in the example), and its memory is freed. **[owner]**
   - On: images stay in scrollback with their lines and scroll back into view; they count against the memory caps, and when a cap is hit the oldest scrollback images are replaced by their placeholder first.
7. **A master switch [builder's call]:** `inline_images` (default on) turns image support off completely: no DA1 sixel bit, `a=q` answered with an error, sequences consumed and discarded. Both settings are rows in the Settings tab and keys in `settings.toml`, applied live to every pane.

## 3. Non-goals and anti-goals

Non-goals:
- **Animation** (kitty animation frames, animated GIF over OSC 1337): the first frame only for now; animation is a planned future feature. **[owner, 2026-11-16]** When it is built: only the visible tab of a visible window animates and its timers stop when hidden (the one sanctioned exception to "no redraw without new output or input"), the frame rate is capped by `max_fps`, every decoded frame counts against the memory caps (an animation that does not fit falls back to its first frame), and a setting turns it off.
- **ReGIS, other vector graphics, and the kitty file-transfer and shared-memory modes** (`t=f`, `t=t`, `t=s`): refused (see the anti-goals). **[builder's call, from fbterm's rules]**
- **Copying or saving an image** from the terminal: no context-menu action yet. **[builder's call]**

Anti-goals (a failure even if images appear):
- **A remote program makes fbterm touch the local machine.** Images are display-only: a program on the far side of SSH never makes fbterm read local files, shared memory or temporary files, write anything to disk, touch the clipboard, or start anything. The file-based kitty transmission modes are refused with the protocol's error reply. **[from fbterm's "no remote side effects" rule]**
- **Images slow down text** or cost anything in background tabs. **[owner]**
- **Unbounded memory**: a hostile stream (an endless chunked transmission, a huge sixel, an endless OSC) cannot grow memory or stall rendering. **[owner]**
- **Wrong size or place**, blurry scaling of pixel art at integer scales, or images that drift from their text. **[owner]**
- **Naming other terminal applications** in anything committed here (protocol names are allowed). **[repo rule]**

## 4. Constraints from the project

- **Emulation belongs to feathercritty**, our fork of the emulation crate: protocol parsing, image placements in the grid, and their interaction with scrolling, resizing, clearing, alternate screen and scrollback. Every changed file carries the fork's notice line and documents itself against upstream (merge-ready rule).
- **The escape-sequence parser must change.** fbterm uses `vte 0.15` (through feathercritty's `vte::ansi::Handler`): it discards APC strings without a callback, gives DCS only debug-logged `hook`/`put`/`unhook`, and buffers OSC payloads without any size limit in `std` builds (already a memory risk today). **[builder's call]** A copy of vte is brought into feathercritty as a module (or a sibling crate in the same repo), with its licence and a notice of our changes, adding: APC dispatch, a DCS sixel hook with streaming `put`, OSC 1337 streaming, and a byte cap on every string sequence (OSC, DCS, APC). Keeping a copy inside feathercritty needs no new repository and no second fork to keep pinned.
- **Rendering stays within the approved architecture:** iced draws the chrome; the grid widget draws images in the same frame as text, using iced's image support or a shader primitive (the approved fallback for the grid). An image sits above cell backgrounds and below text by default, honouring the kitty z-index (negative below text, positive above).
- **Who owns what [builder's call]:** feathercritty owns placements (anchor, cell span, z-index, screen) and the protocol state machines; it hands complete image payloads to an image store that fbterm provides through a small trait (store, look up by id, evict), and gets back a handle. fbterm owns the store: decoding on the blocking pool, decoded pixel buffers, the per-pane and app-wide memory accounting and eviction, resampled copies per scale and GPU textures. Replies that depend on decoding are sent when the decode finishes.
- **Pixel sizes for programs [builder's call]:** besides the escape-sequence reports, the window-size pixel fields programs read (`ws_xpixel`/`ws_ypixel`) are filled in physical pixels for local shells (through the PTY size) and in the SSH `pty-req` and `window-change` messages, and resent when the display scale changes (a window moved to another monitor).
- **Scrollback storage:** the compact history codec stays as it is for text. Image placements live in a side table keyed by stable line (`Grid::scrolled_lines()` plus the grid line), so history rows do not grow; kitty Unicode placeholders are ordinary characters plus colours and diacritics, which the codec already stores.
- **Redraw contract unchanged:** image work happens only when a pane is drawn; decoding finishes on the blocking pool and marks the pane dirty.

- **Windows local shells are in scope [owner, 2026-11-16]:** images from local Windows programs must reach fbterm, not only over SSH. The system console layer (ConPTY) used through portable-pty may strip APC and DCS sequences; the plan first verifies this on a real Windows machine, and if it strips them, fbterm ships Microsoft's open-source ConPTY (`conpty.dll` and `OpenConsole.exe`, MIT) with its Windows package and loads it in place of the system one, with a fallback to the system ConPTY when the bundled one cannot load. **[owner: in scope; bundling is the builder's call for how]**

## 5. Behaviour details

- **Display scale:** decoded images are kept at their source resolution and resampled once per physical cell size (cached per scale); integer scale factors of small images use nearest-neighbour so pixel art stays crisp, everything else a high-quality filter. **[builder's call]**
- **Cell geometry reported to programs** is in physical pixels, so a program that sizes an image to N cells gets exactly N cells.
- **Alternate screen:** images belong to the screen they were drawn on; leaving the alternate screen drops its images.
- **Erase, clear, insert and delete lines**, reverse index and scroll regions move or remove placements like the text they cover; `ED 3`/`RIS` drop scrollback images.
- **Resize:** placements keep their cell anchor; reflow of wrapped lines moves them with their first cell; an image anchored in a reflowed region that cannot follow is dropped to its placeholder.
- **Placeholder drawing [builder's call]:** the off-mode placeholder is drawn over the image's first row from the placement side table, without writing into the grid or the history codec; it is not selectable or searchable. Images shown through kitty Unicode placeholders (`U+10EEEE` cells) get no extra placeholder: their cells are already text.
- **Selection and search** ignore images. **[builder's call]**

## 6. Limits **[builder's call: owner asked for bounded memory, numbers are the builder's]**

6.1 Per image: at most 10 000 × 10 000 pixels and 64 MiB decoded (RGBA); larger images are refused with the protocol's error (kitty `ENOMEM`/`EINVAL`, sixel and OSC 1337 silently).

6.2 Per transmission: an incomplete chunked kitty transmission or an unterminated string sequence is abandoned after 16 MiB of payload or 10 s without progress.

6.3 Per pane 256 MiB and for the whole app 512 MiB of decoded image data, plus GPU textures only for images currently visible; past a cap the least recently shown images are evicted (scrollback first), and an evicted on-screen image becomes its placeholder.

All limits are named constants with tests that feed hostile sequences, and each protocol parser gets a fuzz target.

## 7. Verification

- Unit and property tests per protocol (feathercritty), fuzz targets for the three parsers, and golden tests of the CPU side (decoding, resampling and the pixel buffer an image becomes at scales 1, 1.25, 1.5 and 2), which run in CI. Comparing the final GPU output is a local check through own-window screenshots, since CI has no GPU. **[builder's call]**
- End-to-end checks by the controller in fbterm itself (own-window screenshots, `FBTERM_SCREENSHOT`): featherbittorrent's header and about screen show the featherbit mark; a Claude Code mod with an `Image` element draws its picture (fbterm may need `CLAUDE_CODE_FORCE_TERMINAL_IMAGES=1` until Claude Code recognises it); a sixel tool (`img2sixel` or `chafa -f sixel`) draws its test image; the same three over an SSH session.
- Performance: the existing `dev/perf` scenarios with an image on screen stay within 10 % of today's numbers; 20 tabs each having shown images stay within the memory caps.

## 8. Left to the builder

- The exact split of the work into milestones and the order of the protocols.
- The image decoding crates (PNG, JPEG, GIF, zlib) within the dependency rules (`cargo deny`, no system C libraries).
- iced image primitive or shader primitive for drawing, decided by measurement.
- How the copied parser is organised inside the feathercritty repository.
