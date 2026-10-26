# Performance measurement

`bash dev/perf/sessions.sh` builds the release binary and measures fbterm with many tabs open.
Target (design spec, section 7): 20 sessions (19 idle background tabs, one visible tab
streaming logs) under about 300 MB resident with the default scrollback (10,000 lines), and
idle CPU near zero. The numbers may be retuned after measuring.

## How it works

- `fbterm --perf-tabs <N> --perf-cmd "<cmd>" [--perf-exit-after <secs>]` opens N local tabs in
  one window, each running the command through the user's shell (`$SHELL -c`), the last tab
  active (each shell gets `FBTERM_PERF_TAB` and `FBTERM_PERF_TABS`). The flag implies no session restore, no state saving and in-memory secrets (no
  keychain). It works in release builds. `--perf-exit-after` quits the app cleanly; `--perf-close-after <secs>` closes every tab but the
  active one after that long (scenario G).
- The script uses a temporary `FBTERM_CONFIG_DIR` (and `--perf-tabs` alone defaults to a fresh temp dir), so nothing touches a real config. No screen
  capture is involved.
- Per run: wait 30 s (warm-up), read RSS of the fbterm process (`ps -o rss=`), then mean CPU
  over 10 s from the process CPU-time delta (`ps -o cputime=`). The RSS and CPU are of the
  fbterm process only; child shells are separate processes and are summed separately.
- Scenarios: A is the spec scenario (19 idle `sleep` tabs, the visible last tab runs
  `/usr/bin/log stream --style compact`, the macOS unified log, roughly 100 lines per second
  and no privileges needed; Linux would use `journalctl -f`). B is 20 idle tabs. C is 19 idle
  tabs and a visible tab running `yes` (unthrottled flood). D is a single idle tab (baseline,
  to derive the per-tab cost). F is E with 16-character lines. G is E, then 19 tabs closed
  and RSS read 10 s later. Every run also records `footprint <pid>` (macOS).
- Each scenario ran 3 times. Whether the window stays responsive under the flood is not
  measurable headlessly; what the run shows is that the process stays alive and its output
  handling is bounded (see C).

## Results

Machine: Apple M5 Max, 128 GB, macOS (Darwin 27.0.0), the owner's working Mac with other apps
running. Date: 2026-10-23. Commit: see `git log` for `test(perf)`. Load averages (1/5/15 min)
are sampled twice per run (`load_start` when the process launches, `load_at_sample` at the
start of the CPU window) and are in the raw lines. The fbterm window must be visible and
unoccluded during runs (a hidden window may not render, which would understate CPU).

| Scenario | fbterm RSS (MB), median [range] | fbterm CPU over 10 s, median [range] | child shells RSS (MB) |
|---|---|---|---|
| A: 20 tabs, 19 idle + 1 visible streaming the log | 144.5 [144.0 - 148.2] | 21.9% [5.5 - 22.1] | ~29 (20 processes) |
| B: 20 idle tabs | 107.1 [107.0 - 107.5] | 0.00% [0.00 - 0.00] | ~24 (20 processes) |
| C: 19 idle + 1 visible `yes` flood | 168.7 [168.6 - 278.0] | 104.4% [104.3 - 104.5] | ~24 (20 processes) |
| D: 1 idle tab (baseline) | 103.0 [102.6 - 104.3] | 0.00% [0.00 - 0.00] | ~1.2 |
| E: 20 tabs, every scrollback full (12,000 lines each), last visible, 120 s soak, then idle | 528.3 [527.5 - 528.8] | 0.00% [0.00 - 0.00] | ~24 (20 processes) |

Verdict against the ~300 MB target:

- Meets it: A (the spec scenario as literally written: idle tabs have little output, ~145 MB),
  B, C (the one 278 MB run is just under) and D.
- DOES NOT meet it: E. With every one of the 20 tabs holding its full default scrollback
  (10,000 lines), fbterm uses about 528 MB resident, roughly 1.75x the target. This is the
  realistic worst case for a long-lived 20-session workday (every session has produced a lot
  of output). Decision (owner, 2026-10-23): keep the 10,000-line default and accept this
  worst case. Compact scrollback storage, CPU use while streaming and the ~100 MB base are
  tracked as a performance follow-up in the design spec (section 6). Nothing has been tuned.
- Per-full-tab cost: (E - B) / 20 = (528.3 - 107.1) / 20, about 21 MB per tab with a full
  10,000-line scrollback of ~100-column lines; an idle tab with an empty scrollback costs
  about 0.2 MB ((B - D) / 19). The process base is about 103 MB.
- Idle CPU is near zero (0.00%) in B and E. A with a log streaming into the visible tab costs
  about 22% of a core (one run 5.5%), at roughly 100 lines per second; C pins one core
  (about 104%) as it drains `yes` unthrottled.

Notes:

- Earlier A and C numbers (first commit) were wrong: the streaming command was handed to
  whichever shell started in the last slot, not reliably the visible tab. Now each tab gets
  `FBTERM_PERF_TAB` / `FBTERM_PERF_TABS` in its environment and the command branches on it,
  so the visible tab is deterministic. A and C were re-run; B and D do not depend on that
  (all tabs idle the same way) and keep their first-round numbers, which the script change
  does not affect.
- The spread in A CPU (5.5% vs about 22%) and in C RSS (278 MB once vs 169 MB) was not
  investigated; any explanation (log bursts, system load, scrollback fill timing) would be
  speculation.
- Child shell RSS includes the `sleep`/`yes`/`awk` processes of each tab.

## Raw runs

All lines: `scenario rss_mb rss_end_mb children children_rss_mb cpu_pct load_start load_at_sample stragglers`.

```
A 148.2 148.2 20 29.1   5.50 3.35 3.05 4.27 3.13 3.04 4.23 0
A 144.0 144.1 20 29.2  22.10 3.26 3.07 4.23 3.60 3.19 4.23 0
A 144.5 144.8 20 29.5  21.90 3.50 3.18 4.21 3.80 3.29 4.22 0
C 168.6 168.6 20 24.2 104.40 4.58 3.48 4.27 4.85 3.66 4.31 0
C 278.0 278.0 20 24.1 104.50 5.02 3.76 4.34 4.26 3.69 4.29 0
C 168.7 168.7 20 24.1 104.30 4.21 3.70 4.28 4.74 3.87 4.33 0
E 528.8 528.8 20 24.2   0.00 4.71 3.89 4.33 2.15 3.18 3.98 0
E 528.3 528.3 20 24.3   0.00 2.13 3.14 3.96 2.59 3.05 3.82 0
E 527.5 527.5 20 24.2   0.00 2.40 2.98 3.78 3.45 2.98 3.66 0
```

B and D (first round, load average at sampling 1.8-3.7, children 20 / 1, stragglers 0):

```
B 107.1 107.1 20 24.1 0.00   B 107.5 107.5 20 24.2 0.00   B 107.0 107.0 20 24.2 0.00
D 104.3 104.3 1 1.2 0.00     D 102.6 102.6 1 1.2 0.00     D 103.0 103.0 1 1.2 0.00
```

The script snapshots each run's process tree, stops fbterm (bounded wait, then SIGKILL), kills
the descendants, removes the temp dir (also on INT/TERM/EXIT via a trap) and exits non-zero if
any process of the run survives (`stragglers` is that count; always 0).

## 2026-10-24: footprint, redraw pacing, icon font

Same machine, macOS 27.0.1. Each run used a temporary `FBTERM_CONFIG_DIR`, `FBTERM_SECRETS=memory`
and `--perf-tabs 1 --perf-cmd <cmd>`, and killed fbterm and its children afterwards. Frames per
second came from a temporary, uncommitted draw counter (the median of the per-second counts in
the CPU window); CPU is the process CPU-time delta over 10 s after a 10 s warm-up, as above.

### RSS vs footprint

Activity Monitor's "Memory" column is the phys_footprint (`footprint <pid>`), not RSS
(`ps -o rss=`). The two count different things:

- RSS also counts clean file-backed pages (the binary's code, mapped system files), which
  footprint leaves out.
- RSS does **not** count GPU memory the process owns; footprint does. While any frames are being
  rendered, the Metal driver holds about 300 MB of it (a minimal iced app with no fbterm code
  shows the same), released 10-20 s after rendering stops. So a visible stream shows about
  390-430 MB in Activity Monitor while RSS is about 120 MB.

The table above is RSS. For memory as users see it, read `footprint <pid>` too, and measure idle
footprint at least 20 s after the last frame.

### Output redraws paced to 60 per second

Every message redraws every window, so before this change a visible stream drew one frame per
output chunk. Output `Dirty` is now paced (`crates/fbterm/src/pacing.rs`, `OUTPUT_FPS = 60`), the
echo of typed input skips the wait, and a window that cannot be seen (AppKit occlusion state:
minimized, covered, locked screen, sleeping display) takes no output redraws.

1 visible tab, `perl dev/perf/loggen.pl 1 100` (one ~110-character log line, 100 times per second; about
84 chunks per second arrive), window visible:

| Build | Frames/s | CPU, 3 runs | Load (1 min) |
|---|---|---|---|
| before | 84 / 84 / 84 | 14.1 / 13.9 / 14.5% (median 14.1) | 3.4-4.6 |
| after | 54 / 54 / 54 | 10.3 / 10.2 / 10.1% (median 10.2) | 3.1-4.7 |

The same stream with the screen locked (display awake, window on screen behind the lock):

| Build | Frames/s | CPU, 3 runs | Footprint |
|---|---|---|---|
| before | 77 / 74 / 74 | 11.5 / 11.3 / 10.7% | 390 MB |
| after | 0 | 0.2 / 0.1 / 0.1% | 88 MB |

A minimized window behaves the same: 0 frames while minimized, back to 54 frames per second on
restore. The runaway seen earlier with the display asleep (about 3,000 frames per second, 2-3 GB
footprint) is bounded by construction (not re-measured) by the same two rules.

### Icons from a font

The eight Lucide icons were SVGs, which made iced create a 2048x2048 RGBA image atlas (16 MB of
GPU memory) per window. They are now glyphs of a bundled, subset Lucide icon font, and iced's
`svg` feature is off.

1 idle tab (`sleep`), footprint 25 s after launch, 3 runs each (screen locked, load 2.1-5.4):

| Build | Footprint |
|---|---|
| before (SVG icons) | 77 / 76 / 76 MB |
| after (icon font) | 60 / 60 / 60 MB |

### Configurable redraw rate

The cap is now the `max_fps` setting (default 60; a whole number from 30 to 240, or `"display"`
to follow the screen's refresh rate, `NSScreen.maximumFramesPerSecond` on macOS). Measured as
above (temporary draw counter, 10 s warm-up, CPU over 10 s), 1 visible tab running
`perl dev/perf/loggen.pl 1 100`, on a 120 Hz display (detected as 120), window visible:

| `max_fps` | Frames/s | CPU, 3 runs | Load (1 min) |
|---|---|---|---|
| `60` | 54 / 54 / 54 | 9.3 / 8.3 / 9.1% (median 9.1) | 4.9-8.1 |
| `"display"` (120) | 84 / 83 / 83 | 12.9 / 10.2 / 12.7% (median 12.7) | 4.4-6.7 |

At 120 the cap is above the arrival rate (about 84 chunks per second), so every chunk is drawn,
as before pacing; the extra frames cost about 3.5 points of a core. A faster stream would draw up
to 120 frames per second. Footprint was the same (371-375 MB) in both.

## Before compact history (2026-10-25)

Re-measured baseline for the compact scrollback history work (design spec section 6.6); the
pass conditions apply to these numbers, not to the 2026-10-23 table. Release build of commit
`f5582a5` plus the perf flag below, Apple M5 Max, macOS 27.0.1, 3 runs per scenario, the
fbterm window on screen, load average 1.3-1.6 when the run finished (otherwise quiet machine; per-run loads are in
the script's raw output). Footprint is `footprint <pid>`
(`phys_footprint`), read at the RSS sample and again after the CPU window.

New in the script: scenarios F (20 tabs, 12,000 lines of 16 characters, then idle, 120 s soak)
and G (E, then the app closes 19 of the 20 tabs at 120 s via `--perf-close-after 120`, RSS and
footprint read 10 s later); footprint beside RSS in every scenario (`na` where the tool is
missing, RSS only). `--perf-close-after <secs>` is a release-build perf flag in the style of
`--perf-exit-after`: after that long it closes every tab of the perf window but the active one
through the normal tab-close path.

| Scenario | RSS (MB), median [range] | Footprint (MB) at sample, median [range] | CPU over 10 s, median [range] | child shells RSS (MB) |
|---|---|---|---|---|
| A: 19 idle + 1 visible streaming the log | 136.2 [114.0 - 138.9] | 104.9 [83.5 - 418.8] | 2.1% [0.1 - 2.3] | ~29 |
| B: 20 idle tabs | 100.5 [100.5 - 101.6] | 66.8 [66.8 - 67.7] | 0.10% [0.10 - 0.10] | ~24 |
| C: 19 idle + 1 visible `yes` flood | 161.2 [161.0 - 163.8] | 111.7 [111.7 - 114.0] | 104.5% [104.5 - 104.6] | ~24 |
| D: 1 idle tab | 95.1 [94.8 - 97.9] | 63.6 [61.2 - 71.3] | 0.10% [0.10 - 0.20] | ~1.2 |
| E: 20 tabs, ~100-character lines, full history | 524.3 [524.0 - 525.8] | 473.2 [473.0 - 473.8] | 0.00% | ~24 |
| F: 20 tabs, 16-character lines, full history | 543.9 [543.7 - 544.3] | 478.9 [477.8 - 492.0] | 0.00% | ~24 |
| G: E, then 19 tabs closed, 10 s later | 524.2 [523.6 - 525.0] | 238.7 [236.2 - 238.8] | 0.00% | ~1.2 |

Derived, for the spec's pass conditions (RSS):

- History cost over the idle base: E - B = 423.8 MB, F - B = 443.4 MB (the 6.6 gate for F is
  F - B <= 50 MB; for E, E <= 200 MB). Per full tab: about 21 MB (E), about 22 MB (F). Short
  lines cost as much as long ones (rows are stored as full cell arrays).
- G: RSS stays at 524 MB after closing 19 tabs (the allocator keeps the freed pages mapped);
  the footprint falls to 239 MB (from 473 MB in E), so the footprint, not RSS, shows the
  release. The gate "G <= D + one full tab + 15 MB" read on RSS is D + 21 + 15 = about 131 MB;
  today 524 MB. The phase that adds the pressure-relief call should be judged on both numbers.
- Footprint at E/F/G: 473 MB with no Metal GPU memory in it (nothing rendering), so the
  footprint is within 10% of RSS there.

Caveats:

- A and C footprint swing between about 85-110 MB and 400-420 MB depending on whether the Metal
  driver's roughly 300 MB was resident when read (it is while frames are being drawn and for
  10-20 s after). Compare A/C on RSS and CPU, not footprint at the sample (the end-of-window
  footprint, 402-412 MB in 3 of 4 runs that rendered, is the "while streaming" figure).
- A's CPU is 2% here against 21.9% on 2026-10-23, because redraws are paced to 60 per second
  since then (see the 2026-10-24 section). Run 1 of A shows 0.10% and no GPU memory: its window
  was probably not drawing (occluded or display idle) during that run; it is kept in the
  table. The A pass condition (within 1 point of a core) applies to this 2.1% median, with
  the same caveat on window visibility.
- RSS in B and D is about 6 MB lower than on 2026-10-23 (100.5 vs 107.1, 95.1 vs 103.0); C is
  7 MB lower and A 8 MB lower. Same machine, newer builds (icon font, pacing); this is the
  baseline, not a regression.
- Idle CPU reads 0.10% rather than 0.00% in B and D (`ps` cputime has 10 ms resolution; not
  investigated further); the 6.6 condition is "0.00%" for B and D, so the
  gate should be read as "no higher than this baseline".

Raw lines, `scenario rss_mb rss_end_mb footprint_mb footprint_end_mb children children_rss_mb cpu_pct stragglers`:

```
A 138.9 139.0 104.9 104.9 20 30.2 0.10 0
A 136.2 136.2 418.8 402.3 20 28.9 2.30 0
A 114.0 127.5 83.5 412.2 20 28.6 2.10 0
B 101.6 101.6 67.7 70.7 20 24.1 0.10 0
B 100.5 100.5 66.8 69.8 20 24.1 0.10 0
B 100.5 100.4 66.8 69.8 20 24.1 0.10 0
C 161.2 161.6 111.7 408.7 20 24.1 104.50 0
C 163.8 164.3 114.0 411.0 20 24.1 104.50 0
C 161.0 161.6 111.7 408.8 20 24.1 104.60 0
D 95.1 95.1 71.3 64.4 1 1.2 0.10 0
D 97.9 97.9 63.6 66.7 1 1.2 0.10 0
D 94.8 94.8 61.2 64.2 1 1.2 0.20 0
E 525.8 525.8 473.8 473.8 20 24.2 0.00 0
E 524.3 524.3 473.2 473.2 20 24.3 0.00 0
E 524.0 524.0 473.0 473.0 20 24.2 0.00 0
F 543.9 543.9 492.0 492.0 20 24.2 0.00 0
F 543.7 543.7 477.8 477.8 20 24.3 0.00 0
F 544.3 544.3 478.9 478.9 20 24.2 0.00 0
G 524.2 524.2 236.2 233.2 1 1.2 0.00 0
G 525.0 525.0 238.8 235.8 1 1.2 0.00 0
G 523.6 523.6 238.7 235.7 1 1.2 0.00 0
```

### Library-level read timings (`search_bench`, spec 6.4 fbterm rows)

`cargo run --release -p fbterm-term --example search_bench`, same machine and commit, best of
5 per row (3 for all-tabs):

| Read | Today |
|---|---|
| search, one full tab (10,050 rows x 160 columns): literal, no match | 5.38 ms |
| same, regex, rare / 1 in 4 lines | 5.76 / 5.67 ms |
| search, 20 tabs, one thread per tab: rare literal / common literal / regex | 20.0 / 1.7 / 3.1 ms (sequential rare literal 105.7 ms) |
| export, 10,000 rows x 200 columns, plain, to memory | 2.78 ms |
| export, same rows, 5 colors per row, with colors (SGR) | 7.90 ms |
| draw-list build, 160x50 view: bottom / middle (offset 5,000) / top (offset 10,000) | 0.057 / 0.057 / 0.058 ms |

The draw-list rows are best of 200 on plain log lines; the build is dominated by the 50 rows in
view, so the "1.15x of the bottom" gate is a ratio of numbers near 0.06 ms and is sensitive to
timer noise; judge it with the same best-of-200 harness. The colored-export figure depends on
how many color runs a row has (about 8 ms with 5 runs of 40 cells, about 15 ms with 20 runs of
10 cells); the 6.5 ms in the spec was measured with different content, so the gate compares
this harness before and after.

### After the screen and history split (feathercritty `0.26.0+fbterm.4`, 2026-10-25)

History is read through feathercritty's read API (`RowReader`, `Grid::wraps`, `Grid::cell`);
rows are still stored dense. Same harness, release builds of `9747adb` (before) and the
migration commit (after), run alternately 5 times each on the same machine; load average
3.5-4.6 during the runs, so absolute numbers are higher than in the table above. Median
[range] of the per-run best:

| Read | Before | After | After / before |
|---|---|---|---|
| search, one tab: literal, no match | 6.45 [5.29 - 6.85] ms | 5.69 [5.51 - 6.49] ms | 0.88 |
| search, one tab: literal, 1 in 4 lines | 2.35 [2.29 - 2.72] ms | 2.47 [2.43 - 2.74] ms | 1.05 |
| search, one tab: regex, rare / 1 in 4 lines | 5.68 / 5.73 ms | 5.98 / 6.03 ms | 1.05 / 1.05 |
| search, 20 tabs, rare literal: sequential / one thread per tab | 109.7 / 21.5 ms | 116.5 / 21.4 ms | 1.06 / 1.00 |
| export, plain / with colors | 2.81 / 7.99 ms | 2.55 / 7.21 ms | 0.91 / 0.90 |
| draw-list build: bottom / middle / top | 0.058 / 0.058 / 0.058 ms | 0.056 / 0.057 / 0.056 ms | 0.97 / 0.98 / 0.97 |

Search is about 5% slower (the literal no-match row is noisy in both builds), export about 10%
faster, the draw-list build unchanged. The per-chunk lock time stays about 0.5 ms.

## After compact history (2026-10-26)

Measurements for the compact scrollback history work (design spec 6.4, 6.6, 6.8). Release build
of the fbterm branch `feature/compact-history` at `6052e07` with feathercritty `0.26.0+fbterm.5`
(rev `3c290b8c1f0c89b6e726c27e1f5eb15fb08742be`), Apple M5 Max, macOS 27.0.1, `bash
dev/perf/sessions.sh` (A to G, 3 runs each, window visible, every scenario with zero stragglers).
Load average (1/5/15 min) was 1.3-3.5 at run start, falling from 3.3 (A) to 1.3 (G) over the
roughly 30 minutes of the run; per-run loads are in the raw lines.

| Scenario | RSS (MB), median [range] | Footprint (MB) at sample, median [range] | CPU over 10 s, median [range] | child shells RSS (MB) |
|---|---|---|---|---|
| A: 19 idle + 1 visible streaming the log | 104.5 [103.2 - 107.9] | 373.1 [69.5 - 375.7] | 11.7% [2.6 - 12.1] | ~29 |
| B: 20 idle tabs | 101.3 [101.0 - 101.6] | 55.4 [52.6 - 55.6] | 0.00% | ~24 |
| C: 19 idle + 1 visible `yes` flood | 102.3 [102.2 - 102.6] | 357.9 [357.9 - 358.3] | 102.1% [102.1 - 102.1] | ~24 |
| D: 1 idle tab | 97.8 [97.2 - 98.5] | 51.5 [49.5 - 51.9] | 0.00% [0.00 - 0.10] | ~1.2 |
| E: 20 tabs, ~100-character lines, full history | 123.0 [122.3 - 123.0] | 77.0 [73.9 - 77.1] | 0.00% | ~24 |
| F: 20 tabs, 16-character lines, full history | 116.5 [116.4 - 117.5] | 70.5 [70.4 - 74.4] | 0.00% | ~24 |
| G: E, then 19 tabs closed, 10 s later | 122.2 [122.0 - 122.4] | 80.8 [78.0 - 80.9] | 0.00% | ~1.2 |

Against the "Before compact history" table above (RSS 524.3 MB for E, 543.9 for F, 524.2 for G):
E is 123.0 MB, F 116.5 MB, G 122.2 MB.

Derived: E - B = 21.7 MB of history over the idle base (was 423.8); F - B = 15.2 MB (was 443.4);
per full tab about 1.1 MB (was about 21). Footprint at E, F and G sits 3-14 MB below RSS and has no
Metal GPU memory in it.

### Gates (spec 6.6)

The A to D rows are judged twice: against the recorded baseline above, and against a paired run
of the pre-change build (commit `f5582a5`, the same A, B and D scenarios run right after the new
numbers on the same machine, 3 runs each: A RSS 141.0 [140.4 - 144.9] and CPU 11.8% [11.8 - 11.8],
B RSS 104.2 [103.2 - 104.5], D RSS 99.8 [99.5 - 101.2], CPU 0.00% in B and D; load 1.4-8.0, the
first A run began at load 8.0 after the build).

| Gate | Pass condition | Measured | Verdict |
|---|---|---|---|
| E | RSS <= 200 MB | 123.0 MB | pass |
| F | F - B <= 50 MB | 15.2 MB | pass |
| G | RSS <= D + one full tab + 15 MB = 97.8 + 1.1 + 15 = 113.9 MB | 122.2 MB (footprint 80.8 MB against 51.5 + 1.1 + 15 = 67.6) | **fail** on RSS (8.3 MB over), accepted: allocator cache, no leak |
| A RSS | within 5 MB of baseline | 104.5 MB against 136.2 recorded (141.0 paired) | pass (lower) |
| A CPU | within 1 point of a core | 11.7% against 2.1% recorded; 11.8% paired | **fail against the recorded 2.1%, pass against the paired 11.8%** |
| B RSS / CPU | within 2 MB / 0.00% | 101.3 MB against 100.5 recorded (+0.8; 104.2 paired); 0.00% | pass |
| D RSS / CPU | within 2 MB / 0.00% | 97.8 MB against 95.1 recorded (+2.7); 99.8 paired (-2.0); 0.00% | **marginal**: 0.7 MB over the recorded baseline, within it against the paired run |
| C CPU / RSS | within 5% relative / no higher | 102.1% against 104.5% recorded; 102.3 MB against 161.2 | pass |

Readings:

- G is the one miss on RSS, accepted by the owner on 2026-10-26 on the condition that it is not a
  leak, which the investigation establishes. Closing 19 tabs leaves RSS at 122.2 MB, almost where
  E was (123.0 MB), and footprint does not fall either (77.0 in E, 80.8 in G); the gate's
  allowance with a full tab at about 1.1 MB is 113.9 MB. Findings:
  - Every closed terminal is dropped, and the release call runs right after.
  - Live malloc after closing the 19 tabs is 13.2 MB, against 13.1 MB for an app that only ever
    had one full tab.
  - The remaining ~25 MB is the macOS 27 allocator keeping freed pages dirty:
    `malloc_zone_pressure_relief` returns nothing, and RSS also counts pages malloc has marked
    reusable.
  - A repetition test (`crates/fbterm-term/tests/memory_release.rs`) shows +0 live bytes per
    create-and-drop cycle of 20 full terminals.
  - The `leaks` residue is an OS framework object (an XPC connection) that is also present with
    one tab, independent of tabs.

  Verdict: fail on RSS, accepted: allocator cache, no leak.
- The recorded A CPU of 2.1% came from runs whose window was probably not drawing (two of three
  runs read 0.1-2.3%; the baseline caveats above say so). The paired pre-change build, measured
  with the window visible, is 11.8% in all three runs, the same as the new build's 11.7% and
  12.1% (the new build's first run, 2.6%, again looks like a window not drawing). Read against
  that, A CPU is unchanged. The literal gate against 2.1% is recorded as failed.
- D reads +2.7 MB over its recorded baseline and -2.0 MB against the paired build; B and D are
  within noise of the paired builds. Treat D as marginal.
- A and C footprint swing with the Metal driver's roughly 300 MB (see the caveats above), so A and
  C are judged on RSS and CPU. A's first run shows no GPU memory at the sample.

### Library reads (`search_bench`, spec 6.4 fbterm rows)

`cargo run --release -p fbterm-term --example search_bench`, release builds of the branch with
feathercritty `3c290b8c` (new) and with the previous pin `7cc656ba084e4c3a4c77a74575583721a478af02`
(previous; a temporary, uncommitted `rev` change in the root `Cargo.toml`, same fbterm code, which
compiles against both). Seven runs each, alternating previous, new, previous, new, ...; median of
the per-run figure the harness prints. Load average 4.0-4.5 for all fourteen runs: the machine was
**not quiet**, since an unrelated virtual machine process held a core at about 100% and an endpoint
scanner was active (nothing of this task's ran besides the benchmark). Run-to-run spread on the
plain export was 0.04 ms in both builds, so the ratios below are stable even though absolute values
are higher than the recorded baseline.

| Read | Recorded baseline | Previous pin (median) | New (median) | New / baseline | New / previous | Gate (1.5x) |
|---|---|---|---|---|---|---|
| export, 10,000 rows x 200 columns, plain | 2.78 ms | 2.55 ms | 3.87 ms | 1.39 | 1.52 | pass against the baseline (limit 4.17 ms), **marginal: 1.52x the previous pin** |
| export, same rows, with colors | 7.90 ms | 7.16 ms | 8.52 ms | 1.08 | 1.19 | pass |
| search, one tab, literal, no match | 5.38 ms | 6.26 ms | 6.80 ms | 1.26 | 1.09 | pass |
| search, one tab, regex, rare | 5.76 ms | 6.26 ms | 7.14 ms | 1.24 | 1.14 | pass |
| search, 20 tabs, rare literal, one thread per tab | 20.0 ms | 22.45 ms | 24.44 ms | 1.22 | 1.09 | pass |
| search, 20 tabs, rare literal, sequential | 105.7 ms | 118.9 ms | 136.0 ms | 1.29 | 1.14 | - |
| draw-list build, bottom / middle / top | 0.057 / 0.057 / 0.058 ms | 0.057 / 0.057 / 0.058 ms | 0.057 / 0.063 / 0.062 ms | - | - | middle 1.11x and top 1.09x of the bottom: pass (1.15x) |

The plain export is the only row near its gate. It is 1.52x the previous pin in paired runs and
1.39x the recorded baseline (the 3.86 ms of the earlier loaded run was this same figure, so the
earlier number was not a load artefact). The recorded baseline was taken at lower load, so the
two ratios differ mainly because the previous pin itself read 2.55 ms here against 2.78 ms
recorded. Where the extra cost sits was not investigated. Judged by the spec (1.5x the recorded
baseline) the row passes; judged against the previous pin it is 0.02 over.

### Large histories (spec 6.8, measured not gated)

From feathercritty's `benches/RESULTS.md` ("Large histories", `cargo test --release --test
large_history -- --ignored --nocapture`, 126-column terminal, 100,000 and 1,000,000 history
lines; dense 0.26.0 rows would take 302 MB and 3,024 MB):

| Content | Lines | Live MB | RSS MB after fill | Rewrap 126->100 / 100->126 columns (ms) | Trim to 10,000 (ms) |
|---|---:|---:|---:|---|---:|
| short lines | 100,000 | 7.9 | 11.1 | 4.3 / 4.6 | 0.3 |
| log lines (95 chars, 4 colors) | 100,000 | 20.1 | 22.0 | 3.0 / 3.5 | 0.3 |
| full-width lines | 100,000 | 19.5 | 21.1 | 28.3 / 32.8 | 0.3 |
| short lines | 1,000,000 | 57.8 | 60.2 | 43.1 / 46.7 | 2.9 |
| log lines | 1,000,000 | 179.5 | 182.0 | 31.1 / 35.1 | 3.0 |
| full-width lines | 1,000,000 | 173.8 | 175.9 | 280.8 / 328.5 | 3.0 |

Basis for the large-history threshold: `SCROLLBACK_WARN_ABOVE` stays at 100,000 lines (owner's
choice, 2026-10-26). At 100,000 lines a rewrap takes a median of 3-33 ms (the slowest case is
full-width lines, every row split at 100 columns), with a 56.9 ms worst first full-width pass; at
1,000,000 lines it is about 0.3 s in that worst case (31-47 ms when lines are narrower than both
widths). Lines-only resizes and trims are negligible at both sizes. The Settings note shown above
100,000 lines is therefore a warning about memory (up to about 180 MB for 1,000,000 log lines,
against 3 GB dense) and a resize pause of a few hundredths of a second to about a third of a
second, not about correctness.

Raw lines of the three `sessions.sh` scenario runs, `scenario ... rss_mb rss_end_mb footprint_mb
footprint_end_mb children children_rss_mb cpu_pct stragglers`:

```
A 103.2 104.8 69.5 389.9 20 28.7 2.60 0   (load_start 3.26)
A 104.5 104.6 373.1 373.2 20 29.6 12.10 0 (2.48)
A 107.9 107.9 375.7 375.3 20 29.0 11.70 0 (2.79)
B 101.3 101.3 55.4 55.4 20 24.1 0.00 0    (2.30)
B 101.6 101.6 55.6 55.6 20 24.1 0.00 0    (2.26)
B 101.0 101.0 52.6 52.6 20 24.2 0.00 0    (1.70)
C 102.6 102.6 358.3 358.1 20 24.2 102.10 0 (1.57)
C 102.3 102.3 357.9 357.9 20 24.2 102.10 0 (2.30)
C 102.2 102.2 357.9 357.8 20 24.2 102.10 0 (3.45)
D 97.2 97.2 49.5 49.5 1 1.2 0.00 0        (2.83)
D 98.5 98.5 51.9 51.9 1 1.2 0.00 0        (2.26)
D 97.8 97.8 51.5 51.5 1 1.2 0.10 0        (1.79)
E 123.0 123.0 77.0 77.0 20 24.2 0.00 0    (1.37)
E 122.3 122.3 73.9 73.9 20 24.2 0.00 0    (1.31)
E 123.0 123.0 77.1 77.1 20 24.2 0.00 0    (1.37)
F 117.5 117.5 74.4 71.4 20 24.3 0.00 0    (1.39)
F 116.5 116.5 70.5 70.5 20 24.2 0.00 0    (1.99)
F 116.4 116.4 70.4 70.4 20 24.2 0.00 0    (1.89)
G 122.4 122.4 80.9 77.9 1 1.2 0.00 0      (1.27)
G 122.2 122.2 80.8 77.8 1 1.2 0.00 0      (1.36)
G 122.0 122.0 78.0 75.0 1 1.2 0.00 0      (1.60)
```

Paired pre-change build (`f5582a5`, no footprint column in that script), RSS MB / CPU %: A 144.9 /
11.8, 141.0 / 11.8, 140.4 / 11.8 (load_start 8.0, 5.2, 3.9); B 103.2, 104.2, 104.5 (0.00%); D
99.5, 101.2, 99.8 (0.00%).

## Re-running

```
bash dev/perf/sessions.sh                      # everything, 3 runs each (about 35 minutes)
SCENARIOS=A RUNS=1 WARMUP=10 bash dev/perf/sessions.sh   # quick check
```
