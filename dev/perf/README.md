# Performance measurement

`bash dev/perf/sessions.sh` builds the release binary and measures fbterm with many tabs open.
Target (design spec, section 7): 20 sessions (19 idle background tabs, one visible tab
streaming logs) under about 300 MB resident with the default scrollback (10,000 lines), and
idle CPU near zero. The numbers may be retuned after measuring.

## How it works

- `fbterm --perf-tabs <N> --perf-cmd "<cmd>" [--perf-exit-after <secs>]` opens N local tabs in
  one window, each running the command through the user's shell (`$SHELL -c`), the last tab
  active (each shell gets `FBTERM_PERF_TAB` and `FBTERM_PERF_TABS`). The flag implies no session restore, no state saving and in-memory secrets (no
  keychain). It works in release builds. `--perf-exit-after` quits the app cleanly.
- The script uses a temporary `FBTERM_CONFIG_DIR` (and `--perf-tabs` alone defaults to a fresh temp dir), so nothing touches a real config. No screen
  capture is involved.
- Per run: wait 30 s (warm-up), read RSS of the fbterm process (`ps -o rss=`), then mean CPU
  over 10 s from the process CPU-time delta (`ps -o cputime=`). The RSS and CPU are of the
  fbterm process only; child shells are separate processes and are summed separately.
- Scenarios: A is the spec scenario (19 idle `sleep` tabs, the visible last tab runs
  `/usr/bin/log stream --style compact`, the macOS unified log, roughly 100 lines per second
  and no privileges needed; Linux would use `journalctl -f`). B is 20 idle tabs. C is 19 idle
  tabs and a visible tab running `yes` (unthrottled flood). D is a single idle tab (baseline,
  to derive the per-tab cost).
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

## Re-running

```
bash dev/perf/sessions.sh                      # everything, 3 runs each (about 25 minutes)
SCENARIOS=A RUNS=1 WARMUP=10 bash dev/perf/sessions.sh   # quick check
```
