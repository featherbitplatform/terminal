# Rendering spike result (go/no-go)

Date: 2026-10-23. Spike code: `crates/fbterm-term/examples/spike.rs` (throwaway).

## Decision

**A: iced-native.** The terminal grid is drawn by a custom `iced::advanced::Widget` using iced's own
primitives: one `fill_quad` per run of cells sharing a background color, one `fill_text` per run of
cells sharing foreground color and font style. Approach B (wgpu shader with our own glyph atlas) was
not built: A holds the display refresh rate on every workload, and its frame-time outliers are the
same as those of a control run that draws no cells at all (see below), so a different renderer
could not remove them.

Open point for the owner: the "worst frame <= 25 ms" criterion was met in 6 of 10 runs of the two
brief workloads. Misses were 25.10 ms and 25.45 ms on a quiet machine (three 120 Hz vsync periods
are 25.0 ms) and 36 ms / 64 ms under heavy unrelated load. The no-drawing control misses it the same
way (4 of 5 runs at 25.04 to 25.11 ms).

## Machine and conditions

- Apple M5 Max, 18 cores, built-in Liquid Retina XDR display (3456x2234, ProMotion), on AC power,
  macOS 27.0.1.
- Observed refresh: **120 Hz** (median frame interval 8.33 ms in every busy run). iced presents with
  vsync (`AutoVsync`), so 120 fps is the ceiling.
- Release build (`lto = "thin"`, `codegen-units = 1`), iced 0.14.0 (wgpu backend),
  alacritty_terminal 0.26.0, portable-pty 0.9.0.
- Window 1460x900 logical px, grid 200x60, system monospace font at 12 px, advanced shaping.
- The machine was not idle: a local LLM server (Metal, about 120% CPU) and a virtual machine (about
  100% CPU) were running. Load average was about 11 for the first batch and 4 to 6 afterwards.

## What is measured

- Child process in a PTY (portable-pty, 200x60), output parsed into an `alacritty_terminal::Term` on
  a reader thread. Every frame, the UI copies the visible grid (char, resolved truecolor fg/bg,
  flags) under the term lock and the widget draws it. Every cell is colored from the Term's cell
  fg/bg (named, indexed and truecolor, inverse honored).
- A frame is one `RedrawRequested` delivered through `iced::window::frames()`, requested only while
  the terminal has new output. **fps** = frames per 1 s stats tick; **worst frame** = largest
  interval between consecutive frames. The first 3 s (window creation) are excluded, then 30 s are
  measured.
- `draw` = CPU time inside the widget's `draw()` only (building primitives; excludes iced's text
  preparation and GPU work, which show up in the frame interval).
- Workloads:
  - `yes`: `yes "$(printf '%.0s█' {1..190})"` (scrolling, 110 to 220 MB/s through the parser).
  - `tui`: a python3 curses loop redrawing the full 200x60 screen with 256-color pairs and bold,
    about 170 screen updates/s (stands in for a redrawing TUI; ships with macOS).
  - `colors` (extra, worst case): every cell written with its own truecolor fg and bg each screen,
    i.e. 12,000 text runs and 12,000 background quads per frame.
  - `idle`: fills the screen once, then prints nothing.
- `--mode null` is a control: same app, PTY, parser and frame loop, but no cells are drawn.

Commands:

```bash
cargo run --release -p fbterm-term --example spike -- --mode native --workload yes --seconds 30 --warmup 3
cargo run --release -p fbterm-term --example spike -- --mode native --workload tui --seconds 30 --warmup 3
cargo run --release -p fbterm-term --example spike -- --mode native --workload colors --seconds 30 --warmup 3
cargo run --release -p fbterm-term --example spike -- --mode null --workload yes --seconds 30 --warmup 3
cargo run --release -p fbterm-term --example spike -- --mode native --idle --seconds 40 --warmup 3
```

## Results

Each row is one 30 s run. Criteria: sustained >= 58 fps, worst frame <= 25 ms, idle CPU < 1%.

| Batch (load avg) | Mode | Workload | avg fps | min fps in any second | worst frame | p99 | frames > 25 ms | draw avg / max |
|---|---|---|---|---|---|---|---|---|
| 1 (about 11) | native | yes | 119.6 | 113 | 64.38 ms | 9.75 ms | 2 | 0.05 / 0.09 ms |
| 1 (about 11) | native | tui | 92.3 | 60 | 36.21 ms | 17.26 ms | 1 | 0.04 / 0.08 ms |
| 1 (about 11) | native | colors | 119.8 | 119 | 16.24 ms | 8.80 ms | 0 | 0.54 / 1.13 ms |
| 2 (about 5) | null | tui | 119.5 | 118 | 25.11 ms | 8.68 ms | 2 | 0 |
| 2 (about 5) | null | yes | 119.7 | 117 | 25.11 ms | 8.51 ms | 2 | 0 |
| 2 (about 5) | native | tui | 119.8 | 117 | 21.40 ms | 9.11 ms | 0 | 0.02 / 0.05 ms |
| 2 (about 4) | native | yes | 119.9 | 118 | 25.10 ms | 9.11 ms | 1 | 0.02 / 0.07 ms |
| 3.1 (about 4) | native | yes | 119.9 | 118 | 24.16 ms | 9.41 ms | 0 | 0.03 / 0.05 ms |
| 3.1 | native | tui | 119.0 | 111 | 20.02 ms | 9.64 ms | 0 | 0.03 / 0.05 ms |
| 3.1 | native | colors | 119.8 | 118 | 25.02 ms | 8.56 ms | 2 | 0.30 / 0.39 ms |
| 3.1 | null | yes | 119.9 | 118 | 22.93 ms | 8.56 ms | 0 | 0 |
| 3.2 (about 5) | native | yes | 120.0 | 118 | 19.49 ms | 9.53 ms | 0 | 0.03 / 0.06 ms |
| 3.2 | native | tui | 119.7 | 118 | 25.45 ms | 8.67 ms | 4 | 0.02 / 0.03 ms |
| 3.2 | native | colors | 119.6 | 117 | 25.09 ms | 8.52 ms | 4 | 0.26 / 0.29 ms |
| 3.2 | null | yes | 119.7 | 117 | 25.04 ms | 8.51 ms | 2 | 0 |
| 3.3 (about 5) | native | yes | 119.9 | 117 | 24.66 ms | 9.14 ms | 0 | 0.02 / 0.05 ms |
| 3.3 | native | tui | 119.8 | 118 | 24.67 ms | 9.16 ms | 0 | 0.02 / 0.06 ms |
| 3.3 | native | colors | 119.8 | 117 | 25.03 ms | 8.57 ms | 1 | 0.40 / 0.52 ms |
| 3.3 | null | yes | 119.8 | 118 | 25.07 ms | 8.56 ms | 2 | 0 |

In batch 1 the `tui` run spent several whole seconds at 60 to 74 fps with frame intervals of about
17 ms (two vsync periods); this did not recur once the unrelated load dropped.

Idle (native, screen filled once, then no output; 40 s run):

- `ps -o %cpu` sampled once per second for 30 s from t=6 s:
  `0.0 0.0 0.0 0.0 0.0 0.0 0.0 0.0 0.0 0.0 0.0 0.0 0.0 0.4 0.4 0.4 1.7 0.2 0.2 0.0 0.1 0.0 0.0 0.0 0.1 2.3 0.0 0.0 0.0 0.0`
  (mean 0.19%).
- Cumulative CPU time over the same window: 0.16 s to 0.26 s, i.e. 0.10 s in about 30 s (about 0.3%).
- `top -pid`: 0.0%, 0.7%, 0.2%.
- No frames are requested without output; the only redraw is one per second caused by the spike's
  own stats timer.

## Verdict per criterion (approach A)

| Criterion | Result |
|---|---|
| Sustained >= 58 fps | **Pass** in all 14 native runs (lowest single second: 60 fps, under heavy load; otherwise >= 111) |
| Worst frame <= 25 ms over 30 s | **Mixed**: 6 of 10 `yes`/`tui` runs pass; quiet-machine misses 25.10 and 25.45 ms; loaded-machine misses 36.21 and 64.38 ms. The no-drawing control misses in 4 of 5 runs (25.04 to 25.11 ms) |
| Idle CPU < 1% | **Pass** (about 0.2 to 0.3%) |

## Notes for Task 4

- Per-run `fill_text` with iced's text cache is cheap here: even 12,000 single-cell runs per frame
  cost 0.3 to 0.5 ms of widget CPU and kept 120 fps. Grouping cells into runs matters; keep it.
- Request frames only while the terminal has new output (wakeup from the reader thread, coalesced
  with an atomic flag); this is what keeps idle CPU near zero.
- Copy the visible grid under the term lock once per frame (alacritty_terminal's `FairMutex`) so
  the parser thread is never blocked by drawing.
- Not covered by the spike: wide characters/emoji alignment, cursor, selection, scrollback viewport,
  damage tracking, multiple panes. The design's glyph-atlas/damage plan remains available if later
  features push frame cost up.
