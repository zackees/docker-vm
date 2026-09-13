#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
test -x "$root/scripts/preflight"
test -x "$root/scripts/doctor"
test -x "$root/scripts/connect"
bash -n "$root/scripts/preflight" "$root/scripts/doctor" "$root/scripts/connect"
grep -q -- '--absolute-mouse' "$root/scripts/connect"
grep -q -- '--capture-system-keys never' "$root/scripts/connect"
grep -q 'SDL_MOUSE_AUTO_CAPTURE=0' "$root/scripts/connect"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin"
printf '%s\n' '#!/usr/bin/env bash' \
  'case "$1" in' \
  '  info) exit "${DOCKER_INFO_STATUS:-0}";;' \
  '  inspect)' \
  '    [[ "${DOCKER_EXISTS:-1}" = 1 ]] || exit 1' \
  '    case "$3" in' \
  "      '{{.State.Running}}') printf '%s\\n' \"\${DOCKER_RUNNING:-true}\";;" \
  "      '{{range .HostConfig.Devices}}{{.PathOnHost}} {{end}}') printf '%s\\n' \"\${DOCKER_DEVICES:-/dev/dri/renderD128}\";;" \
  "      '{{range .Config.Env}}{{println .}}{{end}}') printf '%s\\n' \"\${DOCKER_ENV:-}\";;" \
  '    esac;;' \
  '  exec)' \
  '    case "$*" in' \
  "      *'chromium --version'*) printf '%s\\n' \"\${DOCKER_CHROMIUM:-Chromium 152}\";;" \
  "      *'chrome-sandbox'*) printf '%s\\n' \"\${DOCKER_SANDBOX:-4755 root root}\";;" \
  "      *) printf '%s\\n' \"\${DOCKER_EXEC_OUTPUT:-NVIDIA GPU}\";;" \
  '    esac;;' \
  "  logs) printf '%s\\n' \"\${DOCKER_LOGS:-}\";;" \
  '  *) exit 0;;' \
  'esac' >"$tmp/bin/docker"
chmod +x "$tmp/bin/docker"
set +e
PATH="$tmp/bin:$PATH" "$root/scripts/doctor" --desktop >/dev/null 2>&1
test $? -eq 2 || { echo 'missing option value did not return usage error'; exit 1; }
PATH="$tmp/bin:$PATH" "$root/scripts/doctor" --unknown >/dev/null 2>&1
test $? -eq 2 || { echo 'unknown option did not return usage error'; exit 1; }
DOCKER_INFO_STATUS=1 PATH="$tmp/bin:$PATH" "$root/scripts/doctor" >/dev/null 2>&1
test $? -eq 1 || { echo 'unreachable Docker did not fail'; exit 1; }
DOCKER_LOGS='NVENC encoder available' PATH="$tmp/bin:$PATH" "$root/scripts/doctor" --strict >"$tmp/availability" 2>&1
test $? -eq 1 || { echo 'availability-only encoder evidence passed strict mode'; exit 1; }
grep -q 'GAP   \[encode\]' "$tmp/availability" || { echo 'availability-only encoder was not GAP'; exit 1; }
DOCKER_LOGS='Using h264 encoder: nvcodec' PATH="$tmp/bin:$PATH" "$root/scripts/doctor" >"$tmp/nvcodec" 2>&1
test $? -eq 0 || { echo 'active nvcodec H.264 was not accepted'; exit 1; }
grep -q 'PASS  \[encode\]' "$tmp/nvcodec" || { echo 'nvcodec H.264 was not classified as hardware'; exit 1; }
DOCKER_LOGS='Using h264 encoder: x264' PATH="$tmp/bin:$PATH" "$root/scripts/doctor" >"$tmp/fallback" 2>&1
test $? -eq 1 || { echo 'active software encoder did not fail'; exit 1; }
grep -q 'FAIL  \[encode\]' "$tmp/fallback" || { echo 'software encoder not classified as failure'; exit 1; }
printf '%s\n' DESKTOP_VISIBLE=PASS BROWSER_RENDER=PASS BROWSER_DECODE=PASS CLIENT_DECODE=PASS >"$tmp/complete.evidence"
DOCKER_LOGS='Using h264 encoder: nvcodec' PATH="$tmp/bin:$PATH" "$root/scripts/doctor" --strict --evidence "$tmp/complete.evidence" >"$tmp/complete" 2>&1
test $? -eq 0 || { echo 'complete live evidence did not pass strict mode'; cat "$tmp/complete"; exit 1; }
grep -q 'PASS: all configured evidence gates are present' "$tmp/complete" || { echo 'complete evidence did not report PASS'; exit 1; }
set -e
echo 'validation behavior tests: PASS'
