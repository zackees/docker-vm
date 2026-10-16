#!/usr/bin/env bash
# GPU-mode render check: the pixels VNC would send must show the page.
#
# Runs the desktop image with the host's NVIDIA CDI GPU and Chromium's GPU-mode
# flags (read from images/desktop/ephemeral-session, so they cannot drift),
# opens a green page holding a WebGL canvas cleared to red, samples the Xvnc
# framebuffer, and asks WebGL which renderer drew it. chrome://gpu alone is not
# enough: it reported "Hardware accelerated" while the window was black.
#
# Skips (exit 0) when Docker exposes no NVIDIA CDI GPU.
# Usage: tests/gpu-render.sh [image]   (default docker-vm-desktop:local)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
image=${1:-docker-vm-desktop:local}

device=$(docker info -f '{{json .DiscoveredDevices}}' | grep -oE 'nvidia\.com/gpu=[0-9]+' | sort -t= -k2 -n | head -1 || true)
if [[ -z $device ]]; then
  echo "gpu-render: SKIP (no NVIDIA CDI GPU exposed to Docker)"
  exit 0
fi

flags=$(sed -n 's/^ *export CHROMIUM_GPU_FLAGS="\(--ignore-gpu-blocklist[^"]*\)"$/\1/p' "$root/images/desktop/ephemeral-session")
[[ -n $flags ]] || { echo "gpu-render: cannot read GPU-mode flags from ephemeral-session" >&2; exit 1; }

probe=$(cat <<'PROBE'
set -eu
export HOME=/home/desktop DISPLAY=:0
unset LIBGL_ALWAYS_SOFTWARE
Xtigervnc :0 -geometry 1280x800 -SecurityTypes None >/tmp/xvnc.log 2>&1 &
sleep 2
page='data:text/html,<body style="margin:0;background:%2300ff00"><canvas id=c width=300 height=300></canvas><script>const g=document.getElementById("c").getContext("webgl");g.clearColor(1,0,0,1);g.clear(g.COLOR_BUFFER_BIT);const d=g.getExtension("WEBGL_debug_renderer_info");document.title=d?g.getParameter(d.UNMASKED_RENDERER_WEBGL):"no-webgl"</script></body>'
# shellcheck disable=SC2086
/usr/lib/chromium/chromium --user-data-dir=/tmp/profile --no-first-run --no-default-browser-check \
  --window-position=0,0 --window-size=1000,700 --remote-debugging-port=9222 --remote-allow-origins='*' \
  $CHROMIUM_GPU_FLAGS "$page" >/tmp/chromium.log 2>&1 &
sleep 10
python3 - <<'EOF'
import json, sys, urllib.request
import gi
gi.require_version("Gdk", "3.0")
from gi.repository import Gdk

root = Gdk.get_default_root_window()
pb = Gdk.pixbuf_get_from_window(root, 0, 0, 1000, 700)
px, stride, n = pb.get_pixels(), pb.get_rowstride(), pb.get_n_channels()
counts = {"black": 0, "green": 0, "red": 0}
for y in range(0, 700, 10):
    for x in range(0, 1000, 10):
        r, g, b = px[y * stride + x * n: y * stride + x * n + 3]
        if r + g + b < 30: counts["black"] += 1
        elif g > 200 and r < 60 and b < 60: counts["green"] += 1
        elif r > 200 and g < 60 and b < 60: counts["red"] += 1
title = next(t["title"] for t in json.load(urllib.request.urlopen("http://127.0.0.1:9222/json")) if t["type"] == "page")
print(f"pixels {counts}; WebGL renderer: {title}")
ok = counts["green"] > 3000 and counts["red"] > 500 and counts["black"] < 100 and "NVIDIA" in title
sys.exit(0 if ok else 1)
EOF
PROBE
)

if docker run --rm --read-only --tmpfs /tmp:mode=1777 --tmpfs /home/desktop:uid=1000 --shm-size 1g \
     --user 1000:1000 --security-opt seccomp=unconfined --device "$device" \
     -e CHROMIUM_GPU_FLAGS="$flags" --entrypoint sh "$image" -c "$probe"; then
  echo "gpu-render: PASS ($device)"
else
  echo "gpu-render: FAIL ($device): the GPU-mode window does not reach the VNC framebuffer" >&2
  exit 1
fi
