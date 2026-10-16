#!/usr/bin/env python3
"""GPU-mode render check: the pixels VNC would send must show the page.

Runs the desktop image with the host's NVIDIA CDI GPU and Chromium's GPU-mode
flags (read from images/desktop/ephemeral-session, so they cannot drift), opens
a green page holding a WebGL canvas cleared to red, samples the Xvnc
framebuffer, and asks WebGL which renderer drew it. chrome://gpu alone is not
enough: it reported "Hardware accelerated" while the window was black.

Skips (exit 0) when Docker exposes no NVIDIA CDI GPU.
Usage: tests/gpu_render.py [--image docker-vm-desktop:local]
"""
from __future__ import annotations

import argparse
import json
import re
import shlex
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SESSION_SCRIPT = ROOT / "images/desktop/ephemeral-session"

# Runs inside the container (python3 -), as the session user.
PROBE = r'''
import json, os, subprocess, sys, time, urllib.request
import gi
gi.require_version("Gdk", "3.0")
os.environ.update(HOME="/home/desktop", DISPLAY=":0")
os.environ.pop("LIBGL_ALWAYS_SOFTWARE", None)
subprocess.Popen(["Xtigervnc", ":0", "-geometry", "1280x800", "-SecurityTypes", "None"],
                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(2)
page = ('data:text/html,<body style="margin:0;background:%2300ff00"><canvas id=c width=300 height=300></canvas>'
        '<script>const g=document.getElementById("c").getContext("webgl");g.clearColor(1,0,0,1);'
        'g.clear(g.COLOR_BUFFER_BIT);const d=g.getExtension("WEBGL_debug_renderer_info");'
        'document.title=d?g.getParameter(d.UNMASKED_RENDERER_WEBGL):"no-webgl"</script></body>')
subprocess.Popen(["/usr/lib/chromium/chromium", "--user-data-dir=/tmp/profile", "--no-first-run",
                  "--no-default-browser-check", "--window-position=0,0", "--window-size=1000,700",
                  "--remote-debugging-port=9222", "--remote-allow-origins=*",
                  *sys.argv[1].split(), page], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(10)
from gi.repository import Gdk
root = Gdk.get_default_root_window()
pb = Gdk.pixbuf_get_from_window(root, 0, 0, 1000, 700)
px, stride, n = pb.get_pixels(), pb.get_rowstride(), pb.get_n_channels()
black = green = red = 0
for y in range(0, 700, 10):
    for x in range(0, 1000, 10):
        r, g, b = px[y * stride + x * n: y * stride + x * n + 3]
        black += r + g + b < 30
        green += g > 200 and r < 60 and b < 60
        red += r > 200 and g < 60 and b < 60
title = next(t["title"] for t in json.load(urllib.request.urlopen("http://127.0.0.1:9222/json")) if t["type"] == "page")
print(json.dumps({"black": black, "green": green, "red": red, "renderer": title}))
'''


@dataclass(frozen=True)
class RenderProbe:
    black: int
    green: int
    red: int
    renderer: str

    @property
    def passed(self) -> bool:
        return self.green > 3000 and self.red > 500 and self.black < 100 and "NVIDIA" in self.renderer


def nvidia_device() -> str | None:
    devices = subprocess.run(["docker", "info", "-f", "{{json .DiscoveredDevices}}"],
                             capture_output=True, text=True).stdout
    indices = sorted(int(index) for index in re.findall(r"nvidia\.com/gpu=(\d+)", devices))
    return f"nvidia.com/gpu={indices[0]}" if indices else None


def gpu_flags() -> str:
    # The Xtigervnc GPU path (software compositing); the GPU X server path is
    # covered by its own check.
    match = re.search(r'^\s*export CHROMIUM_GPU_FLAGS="(--ignore-gpu-blocklist[^"]*--disable-gpu-compositing[^"]*)"$',
                      SESSION_SCRIPT.read_text(), re.MULTILINE)
    if not match:
        sys.exit("gpu-render: cannot read GPU-mode flags from ephemeral-session")
    return match.group(1)


def probe(image: str, device: str, flags: str) -> RenderProbe:
    command = ["docker", "run", "--rm", "-i", "--read-only", "--tmpfs", "/tmp:mode=1777",
               "--tmpfs", "/home/desktop:uid=1000", "--shm-size", "1g", "--user", "1000:1000",
               "--security-opt", "seccomp=unconfined", "--device", device,
               "--entrypoint", "python3", image, "-", flags]
    output = subprocess.run(command, input=PROBE, capture_output=True, text=True, timeout=180)
    lines = [line for line in output.stdout.splitlines() if line.startswith("{")]
    if not lines:
        sys.exit(f"gpu-render: probe produced no result: {shlex.join(command)}\n{output.stderr[-2000:]}")
    return RenderProbe(**json.loads(lines[-1]))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--image", default="docker-vm-desktop:local")
    args = parser.parse_args()
    device = nvidia_device()
    if device is None:
        print("gpu-render: SKIP (no NVIDIA CDI GPU exposed to Docker)")
        return 0
    result = probe(args.image, device, gpu_flags())
    print(f"pixels black={result.black} green={result.green} red={result.red}; WebGL renderer: {result.renderer}")
    if result.passed:
        print(f"gpu-render: PASS ({device})")
        return 0
    print(f"gpu-render: FAIL ({device}): the GPU-mode window does not reach the VNC framebuffer", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
