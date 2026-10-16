#!/usr/bin/env bash
# Profile a live docker-vm session under a full-screen animation.
# Usage: VIEWER_PID=<viewer pid> profile.sh <container-name> [seconds]
# XVNC_SET="Param=Value ..." applies Xvnc settings live via vncconfig first.
# The page stamps its frame counter into a 32x32 block at (0,0), which the
# viewer's DOCKER_VM_STATS counter reads back as delivered content fps.
# Prints framebuffer size, the page's own rAF fps, and CPU% per process for the
# container (Xvnc / Chromium processes) and the host viewer (WebKit processes).
set -euo pipefail
container=$1
seconds=${2:-10}
here=$(cd "$(dirname "$0")" && pwd)

page='data:text/html;base64,PHRpdGxlPmFuaW08L3RpdGxlPjxib2R5IHN0eWxlPSJtYXJnaW46MDtvdmVyZmxvdzpoaWRkZW47YmFja2dyb3VuZDojZmZmIj48Y2FudmFzIGlkPWM+PC9jYW52YXM+PHNjcmlwdD4KY29uc3QgYz1kb2N1bWVudC5nZXRFbGVtZW50QnlJZCgiYyIpLHg9Yy5nZXRDb250ZXh0KCIyZCIpO2xldCBuPTAsaz0wLHQwPXBlcmZvcm1hbmNlLm5vdygpOwpmdW5jdGlvbiBmKHQpe2lmKGMud2lkdGghPWlubmVyV2lkdGh8fGMuaGVpZ2h0IT1pbm5lckhlaWdodCl7Yy53aWR0aD1pbm5lcldpZHRoO2MuaGVpZ2h0PWlubmVySGVpZ2h0fQp4LmZpbGxTdHlsZT0iI2ZmZiI7eC5maWxsUmVjdCgwLDAsYy53aWR0aCxjLmhlaWdodCk7CmZvcihsZXQgaT0wO2k8NDA7aSsrKXt4LmZpbGxTdHlsZT0iaHNsKCIrKCh0LzEwK2kqOSklMzYwKSsiLDgwJSw1MCUpIjt4LmZpbGxSZWN0KChNYXRoLnNpbih0LzcwMCtpKSowLjUrMC41KSooaW5uZXJXaWR0aC0yMjApLChNYXRoLmNvcyh0LzkwMCtpKjEuMykqMC41KzAuNSkqKGlubmVySGVpZ2h0LTE2MCkrNDAsMjIwLDE2MCl9Cm4rKztrKys7eC5maWxsU3R5bGU9InJnYigiKygobiU4KSozMisxNikrIiwiKygoKG4+PjMpJTgpKjMyKzE2KSsiLDEyOCkiO3guZmlsbFJlY3QoMCwwLDMyLDMyKTsKaWYodC10MD4xMDAwKXtkb2N1bWVudC50aXRsZT0iZnBzICIrKGsqMTAwMC8odC10MCkpLnRvRml4ZWQoMSk7az0wO3QwPXR9cmVxdWVzdEFuaW1hdGlvbkZyYW1lKGYpfXJlcXVlc3RBbmltYXRpb25GcmFtZShmKTwvc2NyaXB0PjwvYm9keT4='

docker exec -e DISPLAY=:0 "$container" sh -c 'xdpyinfo | grep dimensions'
# A separate kiosk Chromium with the session's own GPU flags, so the page and
# its frame-stamp block sit at framebuffer (0,0).
flags=$(docker exec "$container" ps -eo args | grep -m1 -- '^/usr/lib/chromium/chromium --user-data-dir=/home' | grep -oE -- '--(ignore-gpu-blocklist|use-angle=[a-z]+|enable-features=[^ ]+|disable-gpu-compositing|disable-gpu)( |$)' | tr -d '\n')
echo "bench flags: $flags"
for setting in ${XVNC_SET:-}; do docker exec -e DISPLAY=:0 "$container" vncconfig -display :0 -set "$setting"; done
docker exec -e DISPLAY=:0 "$container" sh -c "/usr/lib/chromium/chromium --user-data-dir=/tmp/bench --no-first-run --no-default-browser-check --kiosk $flags '$page' >/dev/null 2>&1 &"
sleep 6

viewer=${VIEWER_PID:?set VIEWER_PID}
host_pids="$viewer $(pgrep -P "$viewer" | tr '\n' ' ')"
python3 "$here/cpu.py" "$seconds" "$container" $host_pids
docker exec -e DISPLAY=:0 "$container" sh -c 'xwininfo -root -tree | grep -oE "fps [0-9.]+" | head -1' || true
