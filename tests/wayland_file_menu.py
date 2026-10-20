#!/usr/bin/env python3
"""Check native picker sizing and input on a private KDE Wayland desktop.

Run on a host with KWin, Xvfb, kdotool and ImageMagick:
  nix shell nixpkgs#xdotool -c dbus-run-session -- python3 tests/wayland_file_menu.py \
    --viewer /path/to/bin/docker-vm --workdir /path/to/empty/scratch

Use a workdir on shared tmpfs (for example a new directory in $XDG_RUNTIME_DIR).
The private bus, display, settings and in-memory GSettings backend keep this
test away from the user's desktop and saved GTK file-chooser geometry.
"""
from __future__ import annotations

import argparse
import os
import re
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

from file_sharing_e2e import Lab, command, require, until


@dataclass(frozen=True)
class Rectangle:
    x: int
    y: int
    width: int
    height: int


@dataclass
class WaylandLab(Lab):
    compositor: subprocess.Popen | None = None
    socket: str = "docker-vm-input-test"

    @property
    def env(self) -> dict[str, str]:
        return {**os.environ, "DISPLAY": self.display, "WAYLAND_DISPLAY": self.socket,
                "GDK_BACKEND": "wayland", "GSETTINGS_BACKEND": "memory",
                "DOCKER_VM_STATS": "1", "NO_AT_BRIDGE": "1"}

    def start_display(self) -> None:
        output = (self.workdir / "xvfb.log").open("wb")
        self.xvfb = subprocess.Popen(["Xvfb", self.display, "-screen", "0", "2160x1560x24"],
                                     stdout=output, stderr=subprocess.STDOUT)
        until(lambda: Path(f"/tmp/.X11-unix/X{self.display.lstrip(':')}").exists(), "Xvfb did not start", 5)
        output = (self.workdir / "kwin.log").open("wb")
        self.compositor = subprocess.Popen(
            ["kwin_wayland", f"--x11-display={self.display}", f"--socket={self.socket}",
             "--width=2100", "--height=1500", "--scale=1.5", "--no-global-shortcuts",
             "--no-kactivities", "--no-lockscreen"],
            env={**self.env, "WAYLAND_DISPLAY": "", "QT_QPA_PLATFORM": "xcb", "KWIN_COMPOSE": "Q",
                 "XDG_CONFIG_HOME": str(self.workdir / "kwin-config")},
            stdout=output, stderr=subprocess.STDOUT)
        until(lambda: (Path(os.environ["XDG_RUNTIME_DIR"]) / self.socket).exists(),
              "private KWin did not create its Wayland socket", 15)

    def kwin(self, *args: str) -> str:
        result = command("kdotool", *args, env=self.env)
        require(result.returncode == 0, f"kdotool {args}: {result.stderr}")
        return result.stdout.strip()

    def window(self, title: str) -> str:
        result = command("kdotool", "search", "--title", f"^{re.escape(title)}$", env=self.env)
        return result.stdout.strip().splitlines()[-1] if result.stdout.strip() else ""

    def rectangle(self, window: str) -> Rectangle:
        value = self.kwin("getwindowgeometry", window)
        position = re.search(r"Position: (-?\d+),(-?\d+)", value)
        size = re.search(r"Geometry: (\d+)x(\d+)", value)
        require(position is not None and size is not None, f"unrecognized geometry: {value}")
        return Rectangle(*map(int, (*position.groups(), *size.groups())))

    def open_picker(self) -> str:
        self.click(20, 41)
        self.hover(80, 69)
        self.click(210, 69)
        time.sleep(0.2)
        if not self.window("Mount Folder"):
            self.click(210, 69)
        until(lambda: bool(self.window("Mount Folder")), "File → Mount → Folder did not open a picker", 5)
        until(lambda: self.kwin("getactivewindow", "getwindowname") == "Mount Folder", "picker did not take focus", 5)
        return self.window("Mount Folder")

    def cleanup(self) -> None:
        try:
            for session in reversed(self.sessions):
                if session.process.poll() is None:
                    session.stop("signal")
        finally:
            for process in (self.compositor, self.xvfb):
                if process and process.poll() is None:
                    process.terminate()
                    process.wait(timeout=15)


def exercise_file_menu(lab: WaylandLab, session, parent_id: str) -> None:
    parent = lab.rectangle(parent_id)

    picker = lab.open_picker()
    time.sleep(0.5)
    bounds = lab.rectangle(picker)
    lab.screenshot("wayland-picker")
    require(bounds.width <= parent.width and bounds.height <= parent.height,
            f"picker {bounds.width}x{bounds.height} exceeds viewer {parent.width}x{parent.height}")
    before = session.output().count("stats: ")
    until(lambda: session.output().count("stats: ") > before, "viewer IPC stopped while picker was open", 6)
    lab.click(bounds.x + 45, bounds.y + 23)  # Native header-bar Cancel.
    until(lambda: not lab.window("Mount Folder"), "mouse Cancel did not close the picker", 5)
    lab.note("Wayland picker", f"focused {bounds.width}x{bounds.height} picker fits its parent; mouse Cancel and IPC work")

    source = lab.workdir / "shared"
    source.mkdir()
    (source / "hello.txt").write_text("Wayland picker marker\n")
    picker = lab.open_picker()
    bounds = lab.rectangle(picker)
    lab.click(bounds.x + 70, bounds.y + 105)  # Leave the Recent view.
    lab.key("ctrl+l", "ctrl+a")
    lab.type(str(source))
    lab.key("Return")
    time.sleep(0.3)
    if lab.window("Mount Folder"):
        bounds = lab.rectangle(picker)
        lab.click(bounds.x + bounds.width - 45, bounds.y + 23)
    lab.screenshot("wayland-after-selection")
    target = "/home/desktop/mnt/shared/hello.txt"
    until(lambda: session.exists(target), "native Wayland picker did not hot-mount the folder", 8)
    require(session.exec("cat", target).stdout == "Wayland picker marker\n", "shared contents differ")
    time.sleep(0.5)
    lab.key("Return")  # Acknowledge the mount result.
    lab.screenshot("wayland-after-mount-ack")
    lab.note("Wayland mount", "selected folder hot-mounted through the native picker")


def exercise(lab: WaylandLab, dock_only: bool = False) -> None:
    session = lab.start("wayland-menu", "ro", gpu=True)
    until(lambda: "native video: first frame presented" in session.output(), "native GPU video did not present", 60)
    parent_id = lab.window("Private desktop canvas")
    require(bool(parent_id), "viewer window was not mapped")
    lab.kwin("windowmove", parent_id, "0", "0")
    lab.kwin("windowsize", parent_id, "900", "650")
    time.sleep(1)
    if not dock_only:
        exercise_file_menu(lab, session, parent_id)

    # A small centered button makes stale RFB pointer dimensions observable:
    # the video can look correct while clicks map outside the visible button.
    page = (b'<body style="margin:0;background:#334455"><button style="position:fixed;'
            b'left:50%;top:50%;transform:translate(-50%,-50%);width:100px;height:100px" '
            b'onclick="fetch(\'/input-ok\')">Click here</button></body>')
    server = ("from http.server import BaseHTTPRequestHandler,HTTPServer; from pathlib import Path\n"
              f"PAGE={page!r}\n"
              "class H(BaseHTTPRequestHandler):\n"
              " def do_GET(self):\n"
              "  if self.path == '/input-ok': Path('/tmp/viewer-pointer-request').write_text(self.path)\n"
              "  if self.path == '/probe': Path('/tmp/viewer-probe-loaded').touch()\n"
              "  self.send_response(200); self.end_headers(); self.wfile.write(PAGE)\n"
              "HTTPServer(('127.0.0.1',18765),H).serve_forever()")
    result = command("docker", "exec", "-d", session.container, "python3", "-c", server)
    require(result.returncode == 0, "could not start in-container pointer probe")
    lab.kwin("windowsize", parent_id, "1100", "750")
    time.sleep(0.5)
    lab.kwin("windowsize", parent_id, "900", "650")
    time.sleep(1)
    lab.click(400, 350)
    lab.key("ctrl+l")
    lab.type("http://127.0.0.1:18765/probe")
    lab.key("Return")
    until(lambda: session.exists("/tmp/viewer-probe-loaded"), "remote browser did not receive keyboard navigation", 10)
    time.sleep(0.5)
    lab.screenshot("wayland-before-button-click")
    lab.click(450, 420)
    until(lambda: session.exists("/tmp/viewer-pointer-request"), "remote click missed the visible button after resizing", 10)
    require(session.exec("cat", "/tmp/viewer-pointer-request").stdout == "/input-ok", "unexpected browser request")
    lab.screenshot("wayland-remote-input")
    lab.note("Wayland input", "remote Chromium received keyboard input and a precise button click after two live resizes")
    # Grow past the initial output size. Xfce 4.20.4 can leave its hidden
    # dock's reveal strip at the old bottom edge, over the page contents.
    lab.kwin("windowsize", parent_id, "1300", "900")
    time.sleep(2)
    screenshot = lab.screenshot("wayland-grown-desktop")
    pixels = subprocess.run(["convert", str(screenshot), "-crop", "1240x670+30+200",
                             "-depth", "8", "RGB:-"], capture_output=True, check=True).stdout
    longest = run = background = 0
    for offset in range(0, len(pixels), 3):
        if offset % (1240 * 3) == 0:
            run = 0
        r, g, b = pixels[offset:offset + 3]
        background += abs(r - 51) < 12 and abs(g - 68) < 12 and abs(b - 85) < 12
        run = run + 1 if min(r, g, b) > 180 and max(r, g, b) - min(r, g, b) < 20 else 0
        longest = max(longest, run)
    require(background > 1240 * 670 * 0.9, "resized video did not show the probe background")
    require(longest < 250, f"stray dock edge across the page after resize: {longest} bright pixels")
    lab.note("Wayland dock", "no floating dock edge after growing beyond the original output size")
    session.stop("signal")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--viewer", type=Path, required=True)
    parser.add_argument("--workdir", type=Path, required=True)
    parser.add_argument("--display", default=":196")
    parser.add_argument("--dock-only", action="store_true", help="run resize/input/dock checks without file pickers")
    args = parser.parse_args()
    require(args.workdir.is_dir() and args.viewer.is_file(), "viewer and workdir must exist")
    require(f"/run/user/{os.getuid()}/bus" not in os.environ.get("DBUS_SESSION_BUS_ADDRESS", ""),
            "run this test under dbus-run-session, away from the user's KWin bus")
    os.environ["XDG_RUNTIME_DIR"] = str(args.workdir.resolve())
    lab = WaylandLab(args.viewer.resolve(), args.workdir.resolve(), args.display)
    # The private bus was started before Python changed XDG_RUNTIME_DIR. Its
    # activated GTK/portal helpers must inherit this same isolated environment,
    # otherwise their accessibility sockets can replace the host's bus socket.
    activation = {"XDG_RUNTIME_DIR": str(lab.workdir), "XDG_CONFIG_HOME": str(lab.workdir / "service-config"),
                  "XDG_DATA_HOME": str(lab.workdir / "service-data"), "XDG_CACHE_HOME": str(lab.workdir / "service-cache"),
                  "WAYLAND_DISPLAY": lab.socket, "DISPLAY": lab.display, "GDK_BACKEND": "wayland",
                  "GSETTINGS_BACKEND": "memory", "NO_AT_BRIDGE": "1"}
    os.environ.update(activation)
    result = command("dbus-update-activation-environment", *activation)
    require(result.returncode == 0, f"could not isolate D-Bus service activation: {result.stderr}")
    try:
        lab.start_display()
        exercise(lab, args.dock_only)
    finally:
        lab.cleanup()


if __name__ == "__main__":
    main()
