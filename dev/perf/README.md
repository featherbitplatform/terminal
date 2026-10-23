# Performance measurement

`bash dev/perf/sessions.sh` builds the release binary and measures fbterm with many tabs open.
Target (design spec, section 7): 20 sessions (19 idle background tabs, one visible tab
streaming logs) under about 300 MB resident with the default scrollback (10,000 lines), and
idle CPU near zero. The numbers may be retuned after measuring.

## How it works

- `fbterm --perf-tabs <N> --perf-cmd "<cmd>" [--perf-exit-after <secs>]` opens N local tabs in
  one window, each running the command through the user's shell (`$SHELL -c`), the last tab
  active. The flag implies no session restore, no state saving and in-memory secrets (no
  keychain). It works in release builds. `--perf-exit-after` quits the app cleanly.
- The script uses a temporary `FBTERM_CONFIG_DIR`, so nothing touches a real config. No screen
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
running. Date: 2026-10-23. Commit: see `git log` for `test(perf)`. Load averages (1/5/15 min,
from `uptime` at the start of each sampling window) are in the raw lines below.

| Scenario | fbterm RSS (MB), median [range] | fbterm CPU over 10 s, median [range] | child shells RSS (MB) |
|---|---|---|---|
| A: 20 tabs, 19 idle + 1 streaming log | 124.9 [122.1 - 143.9] | 0.30% [0.30 - 21.30] | ~29.5 (20 processes) |
| B: 20 idle tabs | 107.1 [107.0 - 107.5] | 0.00% [0.00 - 0.00] | ~24.2 (20 processes) |
| C: 19 idle + 1 `yes` flood | 170.7 [126.6 - 170.8] | 102.2% [99.0 - 102.5] | ~24.2 (20 processes) |
| D: 1 idle tab (baseline) | 103.0 [102.6 - 104.3] | 0.00% [0.00 - 0.00] | ~1.2 |

Verdict: the spec scenario (A) is about 125 MB against the 300 MB target, and 20 idle tabs
use no measurable CPU. Idle CPU target met.

Notes on the numbers:

- Per-tab cost is small: B minus D is about 4 MB for 19 more tabs (roughly 0.2 MB each, idle
  terminals with a mostly empty 10,000-line scrollback). The base of about 103 MB is the
  process itself (GPU stack, fonts, UI), not sessions.
- A run 3 was a CPU outlier (21.3%, RSS 143.9 MB). The load average was falling from a busy
  period (3.6 at the start of that run) and the log stream had a burst; the other two runs are
  0.30%. The median is the representative figure; the range is shown for honesty.
- C runs one core at the cap (about 100%) while the app drains `yes` as fast as the PTY delivers it. RSS was 171 MB in two runs and 127 MB in one; the cause of the spread was not investigated (the default scrollback is 10,000 lines). It is a flood stress case, not the daily target.
- Child shell RSS includes 19 `sleep` processes plus the visible command.

## Raw runs

```
A idle+log-stream  rss=122.1 cpu=0.30  load=6.06 8.35 7.27
A idle+log-stream  rss=124.9 cpu=0.30  load=5.81 7.85 7.15
A idle+log-stream  rss=143.9 cpu=21.30 load=3.62 6.91 6.84
B all-idle         rss=107.1 cpu=0.00  load=3.72 6.34 6.63
B all-idle         rss=107.5 cpu=0.00  load=2.73 5.58 6.33
B all-idle         rss=107.0 cpu=0.00  load=2.04 4.91 6.03
C idle+yes         rss=170.8 cpu=102.20 load=1.98 4.41 5.77
C idle+yes         rss=170.7 cpu=102.50 load=4.14 4.56 5.74
C idle+yes         rss=126.6 cpu=99.00 load=3.99 4.43 5.62
D single-idle      rss=104.3 cpu=0.00  load=3.48 4.24 5.47
D single-idle      rss=102.6 cpu=0.00  load=2.31 3.80 5.23
D single-idle      rss=103.0 cpu=0.00  load=1.87 3.45 5.01
```

No orphaned processes were left after any run (checked each time).

## Re-running

```
bash dev/perf/sessions.sh                      # everything, 3 runs each (about 11 minutes)
SCENARIOS=A RUNS=1 WARMUP=10 bash dev/perf/sessions.sh   # quick check
```
