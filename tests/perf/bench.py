#!/usr/bin/env python3
"""End-to-end frame-delivery benchmark for the private desktop viewer.

Each variant launches its own viewer (never touching other sessions) with
DOCKER_VM_STATS=1 plus the variant's environment, waits for the session, runs a
full-screen canvas animation in a kiosk Chromium inside the container, and
reports:

* page fps      - frames the page itself draws (requestAnimationFrame)
* content fps   - distinct page frames the viewer actually painted; the page
                  stamps its frame counter as the colour of a 32x32 block at
                  framebuffer (0,0), which the viewer's stats hook reads back
* decode, Mbit  - noVNC decode time per second and RFB bandwidth
* CPU per process in the container (Xvnc, Chromium) and the host viewer

Usage:
  tests/perf/bench.py VIEWER_BIN SPEC [SPEC...]
  SPEC = label[:NAME=VALUE[,NAME=VALUE...]]
  e.g. tests/perf/bench.py result/bin/docker-vm base q4:DOCKER_VM_RFB_QUALITY=4
       tests/perf/bench.py result/bin/docker-vm cmp:XVNC_SET=CompareFB=0

XVNC_SET=Param=Value is applied live inside the container with vncconfig
rather than passed to the viewer. --scale sets GDK_SCALE (viewer density).
"""
from __future__ import annotations

import argparse
import base64
import os
import re
import signal
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path

PAGE_HTML = """<title>anim</title>
<body style="margin:0;overflow:hidden;background:#fff"><canvas id=c></canvas><script>
const c = document.getElementById("c"), x = c.getContext("2d");
let n = 0, k = 0, t0 = performance.now();
function f(t) {
  if (c.width != innerWidth || c.height != innerHeight) { c.width = innerWidth; c.height = innerHeight; }
  x.fillStyle = "#fff"; x.fillRect(0, 0, c.width, c.height);
  for (let i = 0; i < 40; i++) {
    x.fillStyle = "hsl(" + ((t / 10 + i * 9) % 360) + ",80%,50%)";
    x.fillRect((Math.sin(t / 700 + i) * 0.5 + 0.5) * (innerWidth - 220),
               (Math.cos(t / 900 + i * 1.3) * 0.5 + 0.5) * (innerHeight - 160) + 40, 220, 160);
  }
  // Frame stamp read back by the viewer's DOCKER_VM_STATS hook.
  n++; k++;
  x.fillStyle = "rgb(" + ((n % 8) * 32 + 16) + "," + (((n >> 3) % 8) * 32 + 16) + ",128)";
  x.fillRect(0, 0, 32, 32);
  if (t - t0 > 1000) { document.title = "fps " + (k * 1000 / (t - t0)).toFixed(1); k = 0; t0 = t; }
  requestAnimationFrame(f);
}
requestAnimationFrame(f);
</script></body>"""
PAGE_URL = "data:text/html;base64," + base64.b64encode(PAGE_HTML.encode()).decode()

GPU_FLAG = re.compile(r"--(?:ignore-gpu-blocklist|use-angle=\w+|enable-features=\S+|disable-gpu-compositing|disable-gpu)(?=\s|$)")
STATS_LINE = re.compile(r"^stats: ([\d.]+) content fps, decode ([\d.]+) ms/s, ([\d.]+) Mbit/s")
CONTAINER_NAME = re.compile(r"docker-vm-[0-9a-f]{32}-desktop-1")
PROCESS_LABELS = ("Xtigervnc", "type=gpu-process", "type=renderer", "type=utility", "type=zygote",
                  "WebKitWebProcess", "WebKitNetworkProcess", "docker-vm-viewer", "xfwm4", "xfce4-panel")


@dataclass(frozen=True)
class EnvSetting:
    name: str
    value: str


@dataclass(frozen=True)
class Variant:
    label: str
    viewer_env: tuple[EnvSetting, ...] = ()
    xvnc_settings: tuple[str, ...] = ()

    @classmethod
    def parse(cls, spec: str) -> Variant:
        label, _, rest = spec.partition(":")
        viewer_env: list[EnvSetting] = []
        xvnc: list[str] = []
        for item in filter(None, rest.split(",")):
            name, _, value = item.partition("=")
            if name == "XVNC_SET":
                xvnc.append(value)
            else:
                viewer_env.append(EnvSetting(name, value))
        return cls(label, tuple(viewer_env), tuple(xvnc))


@dataclass(frozen=True)
class ProcessSample:
    pid: int
    label: str
    cpu_ticks: int


@dataclass(frozen=True)
class CpuShare:
    label: str
    percent: float


@dataclass(frozen=True)
class StatsReport:
    content_fps: float
    decode_ms_per_s: float
    mbit_per_s: float


@dataclass
class RunResult:
    variant: Variant
    framebuffer: str = "?"
    display_server: str = "?"
    page_fps: str = "?"
    container_cpu: list[CpuShare] = field(default_factory=list)
    viewer_cpu: list[CpuShare] = field(default_factory=list)
    stats: list[StatsReport] = field(default_factory=list)
    viewer_gpu_clients: list[str] = field(default_factory=list)
    error: str = ""

    def stats_average(self) -> StatsReport | None:
        if not self.stats:
            return None
        count = len(self.stats)
        return StatsReport(sum(s.content_fps for s in self.stats) / count,
                           sum(s.decode_ms_per_s for s in self.stats) / count,
                           sum(s.mbit_per_s for s in self.stats) / count)


def run(*args: str, check: bool = False) -> str:
    return subprocess.run(args, capture_output=True, text=True, check=check).stdout


WAYLAND_ENV = ("-e", "XDG_RUNTIME_DIR=/run/user/1000", "-e", "WAYLAND_DISPLAY=wayland-1")


def docker_exec(container: str, *args: str) -> str:
    return run("docker", "exec", "-e", "DISPLAY=:0", *WAYLAND_ENV, container, *args)


def swaymsg(container: str, *args: str) -> str:
    # swaymsg needs the compositor's IPC socket, whose name includes its PID.
    return run("docker", "exec", *WAYLAND_ENV, container, "sh", "-c",
               'SWAYSOCK=$(ls /run/user/1000/sway-ipc.*.sock) exec swaymsg "$@"', "swaymsg", *args)


def label_for(cmdline: str) -> str:
    for key in PROCESS_LABELS:
        if key in cmdline:
            return key
    if "chromium" in cmdline:
        return "chromium (browser)"
    return cmdline.split(" ")[0].rsplit("/", 1)[-1][:30]


def sample(pids: list[int]) -> list[ProcessSample]:
    samples = []
    for pid in pids:
        try:
            stat = Path(f"/proc/{pid}/stat").read_text()
            cmdline = Path(f"/proc/{pid}/cmdline").read_text().replace("\0", " ")
        except OSError:
            continue
        fields = stat[stat.rindex(")") + 2:].split()
        samples.append(ProcessSample(pid, label_for(cmdline), int(fields[11]) + int(fields[12])))
    return samples


def cpu_shares(before: list[ProcessSample], after: list[ProcessSample], seconds: float) -> list[CpuShare]:
    start = {s.pid: s.cpu_ticks for s in before}
    per_label: list[CpuShare] = []
    for label in sorted({s.label for s in after}):
        ticks = sum(s.cpu_ticks - start[s.pid] for s in after if s.label == label and s.pid in start)
        percent = 100 * ticks / os.sysconf("SC_CLK_TCK") / seconds
        if percent >= 1:
            per_label.append(CpuShare(label, percent))
    return sorted(per_label, key=lambda share: -share.percent)


def gpu_clients(pids: list[int]) -> list[str]:
    """Which of these processes hold an NVIDIA GPU context (nvidia-smi)."""
    listed = {int(pid) for pid in re.findall(r"^\|\s+\d+\s+\S+\s+\S+\s+(\d+)\s", run("nvidia-smi"), re.MULTILINE)}
    return sorted({label_for(Path(f"/proc/{pid}/cmdline").read_text().replace("\0", " "))
                   for pid in pids if pid in listed and Path(f"/proc/{pid}").exists()})


def container_pids(container: str) -> list[int]:
    return [int(line) for line in run("docker", "top", container, "-eo", "pid").split()[1:] if line.isdigit()]


def host_pids(viewer: int) -> list[int]:
    children = run("pgrep", "-P", str(viewer)).split()
    return [viewer, *map(int, children)]


def wait_for_session(log: Path, build_s: float, run_s: float) -> str:
    """Two phases: the image build/pull (container creation starts only after
    it) gets its own long limit; the session itself must start within run_s."""
    deadline = time.monotonic() + build_s
    started_run = None
    while time.monotonic() < deadline:
        text = log.read_text(errors="replace") if log.exists() else ""
        if "private session failed" in text:
            raise RuntimeError("session failed to start")
        if started_run is None and "desktop-1 Creating" in text:
            started_run = time.monotonic()
            deadline = started_run + run_s
        if "desktop-1 Started" in text and (match := CONTAINER_NAME.search(text)):
            return match.group(0)
        time.sleep(1)
    phase = "image build" if started_run is None else "session start"
    raise TimeoutError(f"{phase} did not finish in time")


def stats_after(log: Path, offset: int) -> list[StatsReport]:
    reports = []
    for line in log.read_text(errors="replace")[offset:].splitlines():
        if match := STATS_LINE.match(line):
            reports.append(StatsReport(*map(float, match.groups())))
    return reports[-8:]


def measure(variant: Variant, viewer_bin: Path, scale: str, seconds: float, logs: Path) -> RunResult:
    result = RunResult(variant)
    log = logs / f"run-{variant.label}.log"
    env = os.environ | {"DOCKER_VM_STATS": "1", "GDK_SCALE": scale} | {e.name: e.value for e in variant.viewer_env}
    with log.open("w") as out:
        # The wrappers exec, so this process becomes the viewer itself.
        viewer = subprocess.Popen([str(viewer_bin)], cwd=Path.home(), env=env, stdout=out, stderr=subprocess.STDOUT)
    try:
        container = wait_for_session(log, build_s=3600, run_s=120)
        time.sleep(12)
        processes = docker_exec(container, "ps", "-eo", "comm")
        wayland = "sway" in processes
        result.display_server = ("sway (GPU, Wayland)" if wayland else "Xorg (GPU)" if "Xorg" in processes
                                 else "Xtigervnc")
        if wayland:
            mode = re.search(r'"current_mode":\s*\{\s*"width":\s*(\d+),\s*"height":\s*(\d+)',
                             swaymsg(container, "-t", "get_outputs", "-r"))
            result.framebuffer = f"{mode.group(1)}x{mode.group(2)}" if mode else "?"
            flags = ["--ozone-platform=wayland", "--ignore-gpu-blocklist"]
        else:
            dims = re.search(r"dimensions:\s+(\S+)", docker_exec(container, "xdpyinfo"))
            result.framebuffer = dims.group(1) if dims else "?"
            browser = next((line for line in docker_exec(container, "ps", "-eo", "args").splitlines()
                            if line.startswith("/usr/lib/chromium/chromium --user-data-dir=/home")), "")
            flags = GPU_FLAG.findall(browser)
        for setting in variant.xvnc_settings:
            docker_exec(container, "vncconfig", "-display", ":0", "-set", setting)
        subprocess.Popen(["docker", "exec", "-e", "DISPLAY=:0", *WAYLAND_ENV, container, "/usr/lib/chromium/chromium",
                          "--user-data-dir=/tmp/bench", "--no-first-run", "--no-default-browser-check", "--kiosk",
                          *flags, PAGE_URL], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(6)
        offset = len(log.read_text(errors="replace"))
        inside, outside = container_pids(container), host_pids(viewer.pid)
        before_in, before_out = sample(inside), sample(outside)
        start = time.monotonic()
        time.sleep(seconds)
        elapsed = time.monotonic() - start
        result.container_cpu = cpu_shares(before_in, sample(inside), elapsed)
        result.viewer_cpu = cpu_shares(before_out, sample(outside), elapsed)
        result.viewer_gpu_clients = gpu_clients(outside)
        result.stats = stats_after(log, offset)
        service_log = docker_exec(container, "cat", "/run/user/1000/video-stream.log")
        (logs / f"video-stream-{variant.label}.log").write_text(service_log)
        titles = (swaymsg(container, "-t", "get_tree", "-r") if wayland
                  else docker_exec(container, "xwininfo", "-root", "-tree"))
        fps = re.search(r"fps [\d.]+", titles)
        result.page_fps = fps.group(0).removeprefix("fps ") if fps else "?"
    except (RuntimeError, TimeoutError) as error:
        result.error = str(error)
    finally:
        viewer.send_signal(signal.SIGTERM)
        try:
            viewer.wait(timeout=40)
        except subprocess.TimeoutExpired:
            viewer.kill()
    return result


def report(result: RunResult) -> None:
    settings = [f"{e.name}={e.value}" for e in result.variant.viewer_env] + \
               [f"XVNC_SET={s}" for s in result.variant.xvnc_settings]
    print(f"===== {result.variant.label} ({' '.join(settings) or 'defaults'})")
    if result.error:
        print(f"  error: {result.error}")
        return
    print(f"  framebuffer {result.framebuffer} on {result.display_server}, page fps {result.page_fps}")
    for title, shares in (("container", result.container_cpu), ("host viewer", result.viewer_cpu)):
        print(f"  -- {title}: " + ", ".join(f"{s.label} {s.percent:.0f}%" for s in shares))
    print(f"  -- viewer GPU clients: {', '.join(result.viewer_gpu_clients) or 'none (CPU only)'}")
    average = result.stats_average()
    if average is None:
        print("  no viewer stats reports")
    else:
        print(f"  viewer: {average.content_fps:.1f} content fps, decode {average.decode_ms_per_s:.0f} ms/s, "
              f"{average.mbit_per_s:.1f} Mbit/s (avg of {len(result.stats)} reports)")
    sys.stdout.flush()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("viewer_bin", type=Path)
    parser.add_argument("specs", nargs="+")
    parser.add_argument("--scale", default="2", help="GDK_SCALE for the viewer (default 2, i.e. HiDPI)")
    parser.add_argument("--seconds", type=float, default=20)
    parser.add_argument("--logs", type=Path, default=None)
    args = parser.parse_args()
    logs = args.logs or Path(tempfile.mkdtemp(prefix="docker-vm-bench."))
    logs.mkdir(parents=True, exist_ok=True)
    print(f"logs: {logs}")
    for spec in args.specs:
        report(measure(Variant.parse(spec), args.viewer_bin, args.scale, args.seconds, logs))
    return 0


if __name__ == "__main__":
    sys.exit(main())
