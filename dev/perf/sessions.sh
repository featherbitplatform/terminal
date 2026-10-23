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
SCENARIOS=${SCENARIOS:-A B C D}
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

# run_one <scenario> <tabs> <visible cmd> <idle cmd>; prints a result line
run_one() {
  local name=$1 tabs=$2 cmd=$3
  local cfg
  cfg=$(mktemp -d)
  local load; load=$(uptime | sed 's/.*load averages*: //')
  if [ "$tabs" -gt 1 ] && [ "$name" != B ]; then
    # 19 idle tabs + 1 different visible tab: two windows are not allowed, so the idle tabs
    # are the first N-1 and the visible one is the last. A wrapper picks the command by tab:

    # mkdir is atomic: the Nth shell to start claims slot N.
    cmd="n=1; while ! mkdir '$cfg/slot.'\$n 2>/dev/null; do n=\$((n+1)); done; if [ \$n -lt $tabs ]; then exec sleep 100000; else $cmd; fi"
  fi
  FBTERM_CONFIG_DIR="$cfg" "$BIN" --perf-tabs "$tabs" --perf-cmd "$cmd" \
    --perf-exit-after $((WARMUP + SAMPLE + 15)) 2>"$cfg/stderr" &
  local pid=$!
  sleep "$WARMUP"
  if ! kill -0 "$pid" 2>/dev/null; then echo "$name: fbterm died" >&2; cat "$cfg/stderr" >&2; return 1; fi
  local rss crss nproc c0 c1
  rss=$(ps -o rss= -p "$pid" | tr -d ' ')
  crss=$(children_rss "$pid")
  nproc=$(ps -A -o pid=,ppid= | awk -v r="$pid" '{pp[$1]=$2} END{for(p in pp){q=p; while(q in pp && q!=r && q>1) q=pp[q]; if(q==r && p!=r) n++} print n+0}')
  c0=$(cpu_secs "$pid")
  sleep "$SAMPLE"
  c1=$(cpu_secs "$pid")
  local cpu; cpu=$(awk -v a="$c0" -v b="$c1" -v s="$SAMPLE" 'BEGIN{printf "%.2f", (b-a)/s*100}')
  local rss1; rss1=$(ps -o rss= -p "$pid" | tr -d ' ')
  # clean stop
  wait "$pid" 2>/dev/null || true
  # any stragglers (should be none)
  local left; left=$({ pgrep -f "sleep 100000" || true; } | wc -l | tr -d " ")
  pkill -f "$cfg" 2>/dev/null || true
  rm -rf "$cfg"
  printf '%s\t%s\ttabs=%s\tfbterm_rss_mb=%.1f\tfbterm_rss_end_mb=%.1f\tchildren=%s\tchildren_rss_mb=%.1f\tcpu_pct=%s\tload=%s\tstragglers=%s\n' \
    "$name" "$cmd_label" "$tabs" "$(awk -v k="$rss" 'BEGIN{print k/1024}')" "$(awk -v k="$rss1" 'BEGIN{print k/1024}')" \
    "$nproc" "$(awk -v k="$crss" 'BEGIN{print k/1024}')" "$cpu" "$load" "$left"
}

for s in $SCENARIOS; do
  for r in $(seq "$RUNS"); do
    printf 'run %s ' "$r" >&2
    case $s in
      A) cmd_label="idle+log-stream"; run_one A "$TABS" "$STREAM" ;;
      B) cmd_label="all-idle";        run_one B "$TABS" "exec sleep 100000" ;;
      C) cmd_label="idle+yes";        run_one C "$TABS" "exec yes" ;;
      D) cmd_label="single-idle";     run_one D 1 "exec sleep 100000" ;;
    esac
  done
done
