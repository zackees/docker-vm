#!/usr/bin/env bash
# Benchmark RFB settings end to end in separate viewer instances.
# Usage: bench.sh VIEWER_BIN "label:ENV=V ENV=V" ...
# Example: tests/perf/bench.sh result/bin/docker-vm "base:X=1" "q4:DOCKER_VM_RFB_QUALITY=4"
# BENCH_GDK_SCALE (default 2) sets viewer density; logs go to $BENCH_LOG_DIR.
# Each run: launch with DOCKER_VM_STATS=1 plus the env, wait for the session,
# run the full-screen animation, then report the viewer's content fps / decode
# time / RFB bandwidth (averaged over the sample window) and per-process CPU.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
logs=${BENCH_LOG_DIR:-$(mktemp -d -t docker-vm-bench.XXXXXX)}
echo "logs: $logs"
bin=$1; shift
for spec in "$@"; do
  label=${spec%%:*}; envs=${spec#*:}
  log="$logs/run-$label.log"
  # shellcheck disable=SC2086
  (cd ~ && exec env DOCKER_VM_STATS=1 GDK_SCALE=${BENCH_GDK_SCALE:-2} $envs "$bin") >"$log" 2>&1 &
  launcher=$!
  timeout 300 tail -n +1 -f "$log" | grep -m1 -qE "desktop-1 Started|private session failed" || true
  container=$(grep -oE "docker-vm-[0-9a-f]{32}-desktop-1" "$log" | head -1)
  timeout 12 tail -f /dev/null || true
  viewer=$launcher  # the wrappers exec, so this is the viewer itself
  echo "===== $label ($envs) container=$container"
  mark=$(wc -l <"$log")
  env $envs VIEWER_PID=$viewer bash "$here/profile.sh" "$container" 20 2>&1 | grep -v "^WM_NAME"
  tail -n +"$mark" "$log" | grep "^stats:" | tail -8 | awk '
    {fps+=$2; dec+=$6; mb+=$8; n++}
    END {if (n) printf "viewer avg over %d reports: %.1f content fps, decode %.0f ms/s, %.1f Mbit/s\n", n, fps/n, dec/n, mb/n; else print "no stats reports"}'
  kill -TERM "$viewer" 2>/dev/null || true
  timeout 40 tail --pid="$viewer" -f /dev/null || true
  wait "$launcher" 2>/dev/null || true
done
