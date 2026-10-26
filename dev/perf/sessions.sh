#!/usr/bin/env bash
# 20-session memory and CPU measurement for fbterm (macOS; Linux uses journalctl -f).
#
#   bash dev/perf/sessions.sh            # all scenarios, 3 runs each
#   RUNS=1 WARMUP=10 bash dev/perf/sessions.sh
#
# Scenarios (each opens ONE window with TABS local tabs, the last one visible):
#   A  tabs-1 idle (sleep) + 1 visible tab streaming the system log   (the spec scenario)
#   B  all tabs idle
#   C  tabs-1 idle + 1 visible tab running `yes` (flood)
#   D  1 idle tab only (baseline, to derive the per-tab cost)
#   E  every tab fills its full 10,000-line scrollback (12,000 wide lines) then idles; the
#      last tab stays visible; soak SOAK_E s (default 120) before measuring
#   F  as E with short lines (16 characters)
#   G  as E, then at SOAK_E s the app closes 19 of the 20 tabs (--perf-close-after) and RSS is
#      read 10 s later
# Every run records RSS and, on macOS, footprint (`footprint <pid>`, phys_footprint: includes
# GPU memory that RSS misses); footprint is `na` where the tool is missing.
# The window must stay visible and unoccluded during runs (a hidden window may not render).
# Measures the fbterm process only (RSS after WARMUP s, mean CPU over SAMPLE s); the child
# shells are summed separately. Uses a temp FBTERM_CONFIG_DIR; --perf-tabs implies no
# restore, no state saving and in-memory secrets. No screen capture is involved.
set -euo pipefail
export LC_ALL=C

cd "$(dirname "$0")/../.."
TABS=${TABS:-20}
WARMUP=${WARMUP:-30}
SAMPLE=${SAMPLE:-10}
RUNS=${RUNS:-3}
SCENARIOS=${SCENARIOS:-A B C D E F G}
SOAK_E=${SOAK_E:-120}
BIN=target/release/fbterm

cargo build --release -p fbterm >&2

case "$(uname -s)" in
  Darwin) STREAM='/usr/bin/log stream --style compact' ;;
  *) STREAM='journalctl -f' ;;
esac

# cputime string ([[h:]m:]s[.cc]) to seconds
cpu_secs() {
  ps -o cputime= -p "$1" | awk '{n=split($1,a,":"); s=0; for(i=1;i<=n;i++) s=s*60+a[i]; printf "%.3f", s}'
}

# sum of RSS (KB) of all descendants of $1
children_rss() {
  ps -A -o pid=,ppid=,rss= | awk -v root="$1" '
    {ppid[$1]=$2; rss[$1]=$3; pid[NR]=$1}
    END{ for(i=1;i<=NR;i++){p=pid[i]; q=p; while(q in ppid && q!=root && q>1) q=ppid[q]; if(q==root && p!=root) s+=rss[p]} printf "%d", s }'
}


# footprint of $1 in MB (phys_footprint), or na
footprint_mb() {
  command -v footprint >/dev/null 2>&1 || { echo na; return; }
  footprint -f bytes --noCategories -p "$1" 2>/dev/null |
    awk '/phys_footprint:/ && !/peak/ {printf "%.1f", $2/1048576; f=1} END{if(!f) printf "na"}'
}

PID=""; CFG=""; KIDS=""
descendants() { # pids of all descendants of $1
  ps -A -o pid=,ppid= | awk -v r="$1" '{pp[$1]=$2} END{for(p in pp){q=p; while(q in pp && q!=r && q>1) q=pp[q]; if(q==r && p!=r) print p}}'
}
# Stop fbterm (bounded wait, then SIGKILL), kill every snapshotted descendant, remove the
# temp dir. Sets LEFT to the number of this run's processes still alive afterwards.
cleanup() {
  if [ -n "$PID" ]; then
    KIDS="$KIDS $(descendants "$PID" | tr '\n' ' ')"
    kill "$PID" 2>/dev/null || true
    for _ in $(seq 20); do kill -0 "$PID" 2>/dev/null || break; sleep 0.5; done
    kill -9 "$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
  fi
  for k in $KIDS; do kill "$k" 2>/dev/null || true; done
  sleep 1
  LEFT=0
  for k in $PID $KIDS; do
    if kill -0 "$k" 2>/dev/null; then kill -9 "$k" 2>/dev/null || true; sleep 0.2; kill -0 "$k" 2>/dev/null && LEFT=$((LEFT+1)); fi
  done
  [ -n "$CFG" ] && rm -rf "$CFG"
  PID=""; CFG=""; KIDS=""
}
trap 'cleanup' EXIT
trap 'cleanup; exit 130' INT TERM

# run_one <scenario> <label> <tabs> <visible-tab command> <idle-tab command> <warmup> [close-after]
# Tab i of N gets FBTERM_PERF_TAB=i, so the last (visible) tab runs the visible command and
# every other tab the idle one.
run_one() {
  local name=$1 label=$2 tabs=$3 vis=$4 idle=$5 warm=$6 closeafter=${7:-}
  local closeflag=(); [ -n "$closeafter" ] && closeflag=(--perf-close-after "$closeafter")
  CFG=$(mktemp -d)
  local load; load=$(uptime | sed 's/.*load averages*: //')
  local cmd="if [ \"\$FBTERM_PERF_TAB\" -lt \"\$FBTERM_PERF_TABS\" ]; then $idle; else $vis; fi"
  FBTERM_CONFIG_DIR="$CFG" "$BIN" --perf-tabs "$tabs" --perf-cmd "$cmd" \
    --perf-exit-after $((warm + SAMPLE + 60)) "${closeflag[@]+"${closeflag[@]}"}" 2>"$CFG/stderr" &
  PID=$!
  sleep "$warm"
  if ! kill -0 "$PID" 2>/dev/null; then echo "$name: fbterm died" >&2; cat "$CFG/stderr" >&2; cleanup; return 1; fi
  local rss crss nproc c0 c1
  rss=$(ps -o rss= -p "$PID" | tr -d ' ')
  local fp; fp=$(footprint_mb "$PID")
  KIDS=$(descendants "$PID" | tr '\n' ' ')
  nproc=$(echo $KIDS | wc -w | tr -d ' ')
  crss=0; for k in $KIDS; do crss=$((crss + $(ps -o rss= -p "$k" 2>/dev/null | tr -d ' ' || echo 0))); done
  local load2; load2=$(uptime | sed 's/.*load averages*: //')
  c0=$(cpu_secs "$PID")
  sleep "$SAMPLE"
  c1=$(cpu_secs "$PID")
  local cpu; cpu=$(awk -v a="$c0" -v b="$c1" -v s="$SAMPLE" 'BEGIN{printf "%.2f", (b-a)/s*100}')
  local rss1; rss1=$(ps -o rss= -p "$PID" | tr -d ' ')
  local fp1; fp1=$(footprint_mb "$PID")
  cleanup
  printf '%s\t%s\ttabs=%s\tfbterm_rss_mb=%.1f\tfbterm_rss_end_mb=%.1f\tfootprint_mb=%s\tfootprint_end_mb=%s\tchildren=%s\tchildren_rss_mb=%.1f\tcpu_pct=%s\tload_start=%s\tload_at_sample=%s\tstragglers=%s\n' \
    "$name" "$label" "$tabs" "$(awk -v k="$rss" 'BEGIN{print k/1024}')" "$(awk -v k="$rss1" 'BEGIN{print k/1024}')" "$fp" "$fp1" \
    "$nproc" "$(awk -v k="$crss" 'BEGIN{print k/1024}')" "$cpu" "$load" "$load2" "$LEFT"
  if [ "$LEFT" -ne 0 ]; then echo "FAIL: $LEFT processes of this run survived cleanup" >&2; exit 1; fi
}

FILL="seq 1 12000 | awk '{printf \"%05d xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\\n\", \$1}'; exec sleep 100000"
FILL_SHORT="seq 1 12000 | awk '{printf \"%05d xxxxxxxxxx\\n\", \$1}'; exec sleep 100000"
for s in $SCENARIOS; do
  for r in $(seq "$RUNS"); do
    printf 'run %s ' "$r" >&2
    case $s in
      A) run_one A idle+log-stream "$TABS" "$STREAM" "exec sleep 100000" "$WARMUP" ;;
      B) run_one B all-idle "$TABS" "exec sleep 100000" "exec sleep 100000" "$WARMUP" ;;
      C) run_one C idle+yes "$TABS" "exec yes" "exec sleep 100000" "$WARMUP" ;;
      D) run_one D single-idle 1 "exec sleep 100000" "exec sleep 100000" "$WARMUP" ;;
      E) run_one E all-full-scrollback "$TABS" "$FILL" "$FILL" "$SOAK_E" ;;
      F) run_one F all-full-short-lines "$TABS" "$FILL_SHORT" "$FILL_SHORT" "$SOAK_E" ;;
      G) run_one G full-then-19-closed "$TABS" "$FILL" "$FILL" $((SOAK_E + 10)) "$SOAK_E" ;;
    esac
  done
done
