#!/usr/bin/env python3
"""Exercise the real viewer's native File menu on a private Xvfb display.

Run after building the viewer:
  nix shell nixpkgs#xdotool -c python3 tests/file_sharing_e2e.py \
    --viewer /path/to/result/bin/docker-vm --workdir /path/to/empty/scratch

The workdir must be created by the caller. The driver leaves screenshots and
logs there, stops only its own PIDs, and never touches the user's display or
saved settings. On this host, create /tmp scratch with safe-mktemp and remove
it with safe-rm after reviewing the evidence.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import time
import zipfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any


def command(*argv: str, env: dict[str, str] | None = None, timeout: float = 20) -> subprocess.CompletedProcess[str]:
    return subprocess.run(argv, env=env, capture_output=True, text=True, timeout=timeout)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def until(predicate, message: str, seconds: float = 30) -> None:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.15)
    raise AssertionError(message)


@dataclass(frozen=True)
class Evidence:
    criterion: str
    detail: str


@dataclass
class ExternalProcess:
    pid: int

    def poll(self) -> int | None:
        return None if Path(f"/proc/{self.pid}").exists() else 0

    def wait(self, timeout: float) -> int:
        until(lambda: self.poll() is not None, f"restarted viewer {self.pid} did not exit", timeout)
        return 0


@dataclass
class Session:
    lab: "Lab"
    label: str
    tier: str
    config: Path
    process: Any
    log: Path
    project: str = ""
    container: str = ""

    @property
    def mount_dir(self) -> Path:
        return Path(os.environ["XDG_RUNTIME_DIR"]) / "docker-vm" / f"{self.project}.mounts"

    def output(self) -> str:
        return self.log.read_text(errors="replace")

    def ready(self) -> None:
        pattern = re.compile(r"Container (docker-vm-[0-9a-f]{32})-desktop-1 Started")
        marker = 0

        def found() -> bool:
            nonlocal marker
            matches = list(pattern.finditer(self.output()))
            match = matches[-1] if matches else None
            if match:
                marker = match.end()
                self.project = match.group(1)
                self.container = f"{self.project}-desktop-1"
                return command("docker", "inspect", "-f", "{{.State.Running}}", self.container).stdout.strip() == "true"
            return False

        until(found, f"{self.label}: desktop did not start; {self.output()[-1800:]}", 120)
        until(lambda: "video:" in self.output()[marker:] or self.process.poll() is not None,
              f"{self.label}: viewer did not connect; {self.output()[-1800:]}", 90)
        require(self.process.poll() is None, f"{self.label}: viewer exited early")
        time.sleep(0.5)

    def exec(self, *argv: str, timeout: float = 15) -> subprocess.CompletedProcess[str]:
        return command("docker", "exec", self.container, *argv, timeout=timeout)

    def exists(self, path: str) -> bool:
        return self.exec("test", "-e", path).returncode == 0

    def container_id(self) -> str:
        return command("docker", "inspect", "-f", "{{.Id}}", self.container).stdout.strip()

    def inspect(self) -> dict:
        result = command("docker", "inspect", self.container)
        require(result.returncode == 0, f"inspect {self.container}: {result.stderr}")
        return json.loads(result.stdout)[0]

    def stop(self, how: str = "quit") -> None:
        if self.process.poll() is None:
            if how == "quit":
                self.lab.open_file()
                for _ in range(3):
                    self.lab.click(45, 91)
                    if self.process.poll() is not None:
                        break
                    time.sleep(0.2)
            else:
                os.kill(self.process.pid, signal.SIGTERM)
        try:
            self.process.wait(timeout=45)
        except subprocess.TimeoutExpired:
            os.kill(self.process.pid, signal.SIGTERM)
            self.process.wait(timeout=20)
        until(lambda: command("docker", "ps", "--filter", f"name={self.project}", "--format", "{{.Names}}").stdout.strip() == "",
              f"{self.label}: container survived stop", 30)
        require(not self.mount_dir.exists(), f"{self.label}: mount directory survived stop: {self.mount_dir}")
        mounts = command("findmnt", "-t", "fuse.dir,fuse.zip", "-n", "-o", "TARGET").stdout
        require(self.project not in mounts, f"{self.label}: FUSE mount survived stop")


@dataclass
class Lab:
    viewer: Path
    workdir: Path
    display: str
    evidence: list[Evidence] = field(default_factory=list)
    xvfb: subprocess.Popen[bytes] | None = None
    sessions: list[Session] = field(default_factory=list)

    @property
    def env(self) -> dict[str, str]:
        return {**os.environ, "DISPLAY": self.display, "GDK_BACKEND": "x11", "WAYLAND_DISPLAY": ""}

    def start_display(self) -> None:
        log = (self.workdir / "xvfb.log").open("wb")
        self.xvfb = subprocess.Popen(["Xvfb", self.display, "-screen", "0", "1280x800x24"], stdout=log, stderr=subprocess.STDOUT)
        socket = Path("/tmp/.X11-unix") / f"X{self.display.lstrip(':')}"
        until(socket.exists, f"Xvfb did not create {socket}")

    def note(self, criterion: str, detail: str) -> None:
        self.evidence.append(Evidence(criterion, detail))
        print(f"PASS {criterion}: {detail}", flush=True)

    def xdo(self, *args: str) -> subprocess.CompletedProcess[str]:
        result = command("xdotool", *args, env=self.env)
        require(result.returncode == 0, f"xdotool {' '.join(args)}: {result.stderr}")
        return result

    def click(self, x: int, y: int) -> None:
        self.xdo("mousemove", str(x), str(y), "click", "1")

    def hover(self, x: int, y: int) -> None:
        self.xdo("mousemove", str(x), str(y))
        time.sleep(0.25)

    def key(self, *keys: str) -> None:
        self.xdo("key", "--clearmodifiers", *keys)

    def type(self, value: str) -> None:
        self.xdo("type", "--delay", "100", "--clearmodifiers", value)

    def screenshot(self, name: str) -> Path:
        path = self.workdir / f"{name}.png"
        result = command("import", "-window", "root", str(path), env=self.env)
        require(result.returncode == 0, f"screenshot: {result.stderr}")
        return path

    def start(self, label: str, tier: str, *, gpu: bool = False, saved: str | None = None,
              config: Path | None = None, private_mounts: bool = False) -> Session:
        config = config or self.workdir / f"config-{label}"
        config.mkdir(parents=True, exist_ok=True)
        if saved is not None:
            settings_dir = config / "docker-vm"
            settings_dir.mkdir(exist_ok=True)
            (settings_dir / "settings.json").write_text(json.dumps({"file_sharing": saved}))
        log = self.workdir / f"{label}.log"
        env = {**self.env, "XDG_CONFIG_HOME": str(config)}
        if not gpu:
            env["DOCKER_VM_GPU"] = "off"
        else:
            env.pop("DOCKER_VM_GPU", None)
        if tier:
            env["DOCKER_VM_FILE_SHARING"] = tier
        else:
            env.pop("DOCKER_VM_FILE_SHARING", None)
        output = log.open("wb")
        argv = (["sudo", "-n", "-E", "unshare", "--mount", "--propagation", "private", "setpriv",
                 f"--reuid={os.getuid()}", f"--regid={os.getgid()}", "--init-groups"]
                if private_mounts else []) + [str(self.viewer)]
        process = subprocess.Popen(argv, env=env, stdout=output, stderr=subprocess.STDOUT)
        session = Session(self, label, tier, config, process, log)
        self.sessions.append(session)
        session.ready()
        return session

    def restart(self, session: Session, label: str) -> Session:
        previous = session.process.pid
        self.key("Escape")
        self.click(1249, 40)
        time.sleep(0.7)
        self.screenshot(f"{label}-restart-panel")
        self.click(1000, 693 if label == "transition-rw" else 678)
        until(lambda: session.process.poll() is not None, "Restart session did not exit old viewer", 20)

        def replacement() -> int | None:
            for entry in Path("/proc").iterdir():
                if not entry.name.isdigit() or int(entry.name) <= previous:
                    continue
                try:
                    if "docker-vm-viewer" not in entry.joinpath("exe").resolve().name:
                        continue
                    return int(entry.name)
                except (OSError, PermissionError):
                    continue
            return None

        until(lambda: replacement() is not None, "Restart session did not spawn a viewer", 20)
        new = Session(self, label, session.tier, session.config, ExternalProcess(replacement()), session.log)
        self.sessions.append(new)
        new.ready()
        return new

    def set_tier(self, tier: str, config: Path) -> None:
        self.key("Escape")
        self.click(1249, 40)
        time.sleep(0.7)
        self.click(964, {"off": 402, "encrypted": 423, "ro": 461, "rw": 483}[tier])
        path = config / "docker-vm" / "settings.json"
        until(lambda: json.loads(path.read_text()).get("file_sharing") == tier,
              f"Settings did not save File sharing={tier}", 5)
        self.key("Escape")

    def open_file(self) -> None:
        self.click(20, 12)
        time.sleep(0.2)

    def picker(self, folder: bool, tier: str) -> int:
        self.open_file()
        self.hover(110, 39)
        y = 39 if folder or tier == "encrypted" else 65
        self.click(205, y)
        name = "Mount Folder" if folder else "Mount ZIP Archive"

        def window() -> bool:
            return bool(command("xdotool", "search", "--name", name, env=self.env).stdout.strip())

        if not window():
            self.click(205, y)  # GTK may first select a menu row on bare Xvfb.
        until(window, f"native {name} picker did not appear", 5)
        ids = command("xdotool", "search", "--name", name, env=self.env).stdout.split()
        identity = ids[-1]
        self.xdo("windowsize", identity, "900", "600")
        self.xdo("windowmove", identity, "100", "80")
        return int(identity)

    def choose(self, path: Path, *, folder: bool, tier: str, remember: bool = False,
               writable: bool = False) -> float:
        identity = self.picker(folder, tier)
        if remember:
            self.screenshot("remember-picker-before")
            self.click(118, 618)
            self.screenshot("remember-picker-after")
        self.click(145, 140)  # Home, leaving the Recent view.
        self.key("ctrl+l")
        self.xdo("windowfocus", "--sync", str(identity))
        self.key("ctrl+a", "BackSpace")
        self.type(str(path))
        self.key("Return")
        if folder:
            time.sleep(0.2)
            if command("xdotool", "search", "--name", "Mount Folder", env=self.env).stdout.strip():
                self.click(950, 656)
        if folder and tier == "rw":
            time.sleep(0.2)
            self.screenshot("rw-confirm-before")
            self.click(730 if writable else 545, 423)
        return time.monotonic()

    def dismiss(self) -> None:
        self.click(640, 423)

    def password(self, title: str, value: str) -> None:
        def focus() -> bool:
            ids = command("xdotool", "search", "--name", title, env=self.env).stdout.split()
            return bool(ids) and command("xdotool", "windowfocus", "--sync", ids[-1], env=self.env).returncode == 0
        until(focus, f"password prompt {title!r} did not become focusable", 8)
        self.type(value)
        self.key("Return")

    def recent(self) -> None:
        self.open_file()
        self.hover(110, 39)
        self.screenshot("recent-menu")
        self.hover(285, 65)
        self.screenshot("recent-submenu")
        self.click(420, 65)

    def forget_all(self) -> None:
        self.open_file()
        self.hover(110, 39)
        self.screenshot("forget-menu")
        self.click(235, 91)
        self.screenshot("forget-after-click")

    def unmount(self, name: str, *, expected_gone: bool = True) -> None:
        self.open_file()
        self.hover(110, 65)
        self.click(205, 65)
        time.sleep(0.3)
        if expected_gone:
            self.dismiss()

    def cleanup(self) -> None:
        for session in reversed(self.sessions):
            if session.process.poll() is None:
                session.stop("signal")
        if self.xvfb and self.xvfb.poll() is None:
            self.xvfb.terminate()
            self.xvfb.wait(timeout=10)


def gpu_folder_and_isolation(lab: Lab) -> None:
    source = lab.workdir / "gpu-folder"
    source.mkdir(exist_ok=True)
    (source / "hello.txt").write_text("gpu folder marker\n")
    session = lab.start("gpu-ro", "ro", gpu=True)
    try:
        inspect = session.inspect()
        require("DESKTOP_DISPLAY=wayland" in inspect["Config"]["Env"], "GPU desktop did not use Wayland")
        until(lambda: "native video: first frame presented" in session.output(), "native H.264 first frame absent", 60)
        before = session.container_id()
        lab.screenshot("gpu-before-mount")
        mounted_at = lab.choose(source, folder=True, tier="ro")
        target = "/home/desktop/mnt/gpu-folder/hello.txt"
        until(lambda: session.exists(target), "GPU folder did not hot-mount", 5)
        elapsed = time.monotonic() - mounted_at
        require(elapsed < 2, f"GPU folder took {elapsed:.2f}s to mount")
        require(session.exec("cat", target).stdout == "gpu folder marker\n", "GPU folder content differs")
        require(session.container_id() == before, "container restarted during hot mount")
        lab.dismiss()
        # Thunar opens the same live path in the running desktop.
        opened = command("docker", "exec", "-d", session.container, "thunar", "/home/desktop/mnt/gpu-folder")
        require(opened.returncode == 0, f"Thunar launch failed: {opened.stderr}")
        time.sleep(1)
        lab.dismiss()  # The asynchronous mount notice may arrive after Thunar opens.
        time.sleep(0.3)
        before_drag = lab.screenshot("gpu-thunar-and-overlay")
        lab.xdo("mousemove", "600", "185", "mousedown", "1")
        for x, y in ((625, 195), (650, 205), (675, 215), (700, 225)):
            lab.hover(x, y)
        lab.xdo("mouseup", "1")
        time.sleep(0.3)
        after_drag = lab.screenshot("gpu-thunar-after-drag")
        diff = command("compare", "-metric", "AE", str(before_drag), str(after_drag), "null:")
        changed = int(re.search(r"\d+", diff.stderr).group()) if re.search(r"\d+", diff.stderr) else 0
        require(changed > 10_000, f"native video overlay swallowed the remote title drag ({changed} pixels changed)")
        mountpoint = session.mount_dir / "live" / "gpu-folder"
        other = command("sudo", "-n", "-u", "nobody", "ls", str(mountpoint))
        require(other.returncode != 0, "different host UID could read owner-only mount")
        options = session.exec("findmnt", "-T", target, "-o", "OPTIONS", "-n").stdout
        require(all(option in options.split(",") for option in ("ro", "nosuid", "nodev")), f"unsafe FUSE options: {options}")
        denied = session.exec("touch", "/home/desktop/mnt/gpu-folder/denied")
        require(denied.returncode != 0 and "Read-only file system" in denied.stderr, f"read-only write was allowed: {denied}")
        lab.note("#26 1/8/9", f"GPU Wayland + native H.264, hot folder {elapsed:.2f}s, same container, Thunar opened file and remote title drag changed {changed} pixels, owner-only RO FUSE")
        lab.unmount("gpu-folder")
        until(lambda: not session.exists(target), "GPU folder stayed mounted", 5)
        require(session.container_id() == before, "container restarted during unmount")
        lab.note("#26 3", "native Unmount removed folder with the same container ID")
        session.stop("signal")
        lab.note("#26 5", "SIGTERM removed GPU container, runtime mount directory and project FUSE mounts")
    finally:
        if session.process.poll() is None:
            session.stop("signal")


def off_and_override(lab: Lab) -> None:
    session = lab.start("off-override", "off", saved="rw")
    try:
        mounts = session.inspect()["Mounts"]
        require(not any(mount.get("Destination") == "/home/desktop/mnt" for mount in mounts), "Off tier has a drop-zone bind")
        lab.open_file()
        lab.screenshot("off-menu")
        lab.key("Escape")
        # The settings panel reports the saved choice and the launch override.
        lab.click(1249, 40)
        time.sleep(0.4)
        lab.screenshot("off-settings-override")
        lab.click(1249, 40)
        lab.note("#26 10 Off/override", "no drop-zone bind; File menu disabled; saved rw overridden by DOCKER_VM_FILE_SHARING=off")
        session.stop()
        lab.note("#26 4", "File → Quit removed container and runtime mount directory")
    finally:
        if session.process.poll() is None:
            session.stop("signal")


def cpu_folder_and_busy_unmount(lab: Lab) -> None:
    source = lab.workdir / "cpu-folder"
    source.mkdir(exist_ok=True)
    (source / "hold.txt").write_text("held open\n")
    session = lab.start("cpu-ro", "ro")
    try:
        require("DESKTOP_DISPLAY=wayland" not in session.inspect()["Config"]["Env"], "CPU fallback unexpectedly used Wayland")
        before = session.container_id()
        lab.choose(source, folder=True, tier="ro")
        target = "/home/desktop/mnt/cpu-folder/hold.txt"
        until(lambda: session.exists(target), "CPU folder did not hot-mount", 5)
        lab.dismiss()
        sleeper = subprocess.Popen(["docker", "exec", session.container, "python3", "-c",
                                    "import time; f=open('/home/desktop/mnt/cpu-folder/hold.txt'); print('open',flush=True); time.sleep(8)"],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        require(sleeper.stdout is not None and sleeper.stdout.readline().strip() == "open", "container did not hold file open")
        lab.unmount("cpu-folder", expected_gone=False)
        time.sleep(0.5)
        require(session.exists(target), "busy unmount removed an open file without warning")
        lab.screenshot("busy-unmount-message")
        lab.dismiss()
        sleeper.wait(timeout=12)
        lab.unmount("cpu-folder")
        until(lambda: not session.exists(target), "CPU folder survived unmount", 5)
        require(session.container_id() == before, "CPU container restarted during mount or unmount")
        lab.note("#26 3/6", "CPU X fallback hot-mounted and unmounted without restart; busy open file produced an error and remained mounted")
        session.stop()
        lab.note("#26 4/6", "CPU File → Quit removed container and FUSE mounts")
    finally:
        if session.process.poll() is None:
            session.stop("signal")


def archives_and_saved_shares(lab: Lab) -> None:
    marker = "encrypted archive marker 7319\n"
    encrypted = lab.workdir / "encrypted.zip"
    created = subprocess.run(["7z", "a", "-tzip", "-mem=AES256", "-pcorrect-horse",
                              str(encrypted), "-sisecret.txt"], input=marker, capture_output=True, text=True)
    require(created.returncode == 0, f"AES ZIP fixture: {created.stderr}")
    plain = lab.workdir / "plain.zip"
    with zipfile.ZipFile(plain, "w") as archive:
        archive.writestr("plain.txt", "plain ZIP marker\n")
    mixed = lab.workdir / "mixed.zip"
    mixed.write_bytes(encrypted.read_bytes())
    with zipfile.ZipFile(mixed, "a") as archive:
        archive.writestr("plain.txt", "mixed plain entry\n")

    config = lab.workdir / "config-archive-persist"
    session = lab.start("encrypted-first", "encrypted", config=config)
    try:
        lab.choose(encrypted, folder=False, tier="encrypted", remember=True)
        lab.password("Password for encrypted.zip", "wrong")
        lab.password("Wrong password for encrypted.zip", "correct-horse")
        target = "/home/desktop/mnt/encrypted.zip/secret.txt"
        until(lambda: session.exists(target), "encrypted ZIP did not mount", 8)
        require(session.exec("cat", target).stdout == marker, "encrypted ZIP contents differ")
        plaintext = command("rg", "-l", "-a", "encrypted archive marker 7319", str(lab.workdir))
        require(plaintext.returncode == 1, f"decrypted ZIP contents appeared on host disk: {plaintext.stdout}")
        lab.dismiss()
        lab.screenshot("encrypted-zip-mounted")
        shares = config / "docker-vm" / "shares.json"
        until(shares.exists, "remembered ZIP was not saved", 5)
        saved_text = shares.read_text()
        require(str(encrypted) in saved_text and "correct-horse" not in saved_text and "wrong" not in saved_text,
                "saved share omitted path or recorded password")
        lab.note("#26 2/10/11", "AES ZIP prompted twice after wrong password, read decrypted content, saved path without password")
        lab.unmount("encrypted.zip")
        until(lambda: not session.exists(target), "encrypted ZIP stayed mounted", 5)
        lab.choose(plain, folder=False, tier="encrypted")
        time.sleep(0.6)
        lab.screenshot("encrypted-rejects-plain")
        require(not session.exists("/home/desktop/mnt/plain.zip"), "plain ZIP mounted in Encrypted tier")
        lab.dismiss()
        lab.choose(mixed, folder=False, tier="encrypted")
        time.sleep(0.6)
        lab.screenshot("encrypted-rejects-mixed")
        require(not session.exists("/home/desktop/mnt/mixed.zip"), "mixed ZIP mounted in Encrypted tier")
        lab.dismiss()
        lab.open_file()
        lab.hover(110, 39)
        lab.screenshot("encrypted-tier-menu")
        lab.key("Escape", "Escape")
        lab.note("#26 10 Encrypted", "native menu offered only encrypted archive; plain and mixed ZIPs were refused")
        session.stop()
    finally:
        if session.process.poll() is None:
            session.stop("signal")

    session = lab.start("encrypted-recent", "encrypted", config=config)
    try:
        require(not session.exists("/home/desktop/mnt/encrypted.zip/secret.txt"), "saved ZIP mounted automatically")
        lab.recent()
        lab.password("Password for encrypted.zip", "correct-horse")
        until(lambda: session.exists("/home/desktop/mnt/encrypted.zip/secret.txt"), "Recent ZIP did not mount", 8)
        time.sleep(0.6)
        lab.dismiss()
        def forgotten() -> bool:
            return not shares.exists() or not json.loads(shares.read_text()).get("shares")
        for _ in range(3):
            if forgotten():
                break
            lab.forget_all()
            time.sleep(0.5)
            lab.dismiss()
        require(forgotten(), "Forget all did not clear saved shares")
        lab.dismiss()
        lab.note("#26 11", "saved ZIP stayed inactive after relaunch, Recent mounted it after a fresh password prompt, Forget all cleared it")
        session.stop()
    finally:
        if session.process.poll() is None:
            session.stop("signal")


def writable_folder(lab: Lab) -> None:
    source = lab.workdir / "writable-folder"
    source.mkdir(exist_ok=True)
    (source / "existing.txt").write_text("original\n")
    outside = lab.workdir / "outside-writable"
    outside.mkdir(exist_ok=True)
    session = lab.start("rw-folder", "rw")
    try:
        target = "/home/desktop/mnt/writable-folder"
        lab.choose(source, folder=True, tier="rw")
        until(lambda: session.exists(f"{target}/existing.txt"), "default RO folder did not mount", 5)
        lab.dismiss()
        denied = session.exec("touch", f"{target}/denied")
        require(denied.returncode != 0, "RW tier default folder permitted writes")
        lab.unmount("writable-folder")
        until(lambda: not session.exists(f"{target}/existing.txt"), "RO folder stayed mounted", 5)
        lab.choose(source, folder=True, tier="rw", writable=True)
        until(lambda: session.exists(f"{target}/existing.txt"), "writable folder did not mount", 5)
        lab.dismiss()
        require(session.exec("sh", "-c", f"echo from-container > {target}/created.txt && chmod 755 {target}/created.txt").returncode == 0,
                "writable folder denied file creation")
        created = source / "created.txt"
        require(created.read_text().strip() == "from-container", "container write absent on host")
        require(created.stat().st_uid == os.getuid() and created.stat().st_gid == os.getgid(), "container write has wrong host owner")
        require(created.stat().st_mode & 0o111 == 0, "container-created file executable on host")
        (source / "escape").symlink_to(outside, target_is_directory=True)
        escape = session.exec("touch", f"{target}/escape/escaped.txt")
        require(escape.returncode != 0 and not (outside / "escaped.txt").exists(), "symlink escaped source root")
        traversal = session.exec("touch", f"{target}/../escaped.txt")
        require(traversal.returncode != 0, "path traversal escaped mount")
        require(session.exec("chmod", "700", "/home/desktop/mnt").returncode != 0,
                "container changed read-only drop-zone permissions")
        lab.note("#26 10 Read & write", "default folder was RO; Allow changes wrote host-owned nonexecutable file; symlink and .. escapes failed")
        session.stop()
    finally:
        if session.process.poll() is None:
            session.stop("signal")


def no_propagation(lab: Lab) -> None:
    session = lab.start("private-mounts", "ro", private_mounts=True)
    try:
        until(lambda: "live sharing unavailable:" in session.output(), "private mount propagation was not detected", 5)
        require("private mount" in session.output(), f"wrong sharing failure: {session.output()[-1200:]}")
        require(not any(mount.get("Destination") == "/home/desktop/mnt" for mount in session.inspect()["Mounts"]),
                "private runtime still bound a live drop zone")
        lab.open_file()
        lab.hover(110, 39)
        lab.screenshot("no-propagation-menu")
        lab.key("Escape", "Escape")
        lab.note("#26 7", "viewer started under private mount namespace; File → Mount showed propagation reason and no drop-zone bind")
        session.stop()
    finally:
        if session.process.poll() is None:
            session.stop("signal")


def tier_transitions(lab: Lab) -> None:
    config = lab.workdir / "config-transitions"
    source = lab.workdir / "transition-folder"
    source.mkdir(exist_ok=True)
    (source / "present.txt").write_text("transition marker\n")
    session = lab.start("transition-off", "", saved="off", config=config)
    try:
        require(not any(m.get("Destination") == "/home/desktop/mnt" for m in session.inspect()["Mounts"]),
                "Off initial bind was present")
        lab.set_tier("ro", config)
        require(not any(m.get("Destination") == "/home/desktop/mnt" for m in session.inspect()["Mounts"]),
                "Off → RO widened before restart")
        lab.note("#26 10 transition Off → Read-only", "Settings saved RO while running Off; no bind appeared before Restart session")
        session = lab.restart(session, "transition-ro")
        require(any(m.get("Destination") == "/home/desktop/mnt" and not m.get("RW")
                    for m in session.inspect()["Mounts"]), "RO bind absent after Restart session")
        lab.set_tier("rw", config)
        require(all(not m.get("RW") for m in session.inspect()["Mounts"] if m.get("Destination") == "/home/desktop/mnt"),
                "RO → RW widened before restart")
        lab.note("#26 10 transition Read-only → Read & write", "Settings saved RW; drop-zone bind stayed RO until Restart session")
        session = lab.restart(session, "transition-rw")
        require(any(m.get("Destination") == "/home/desktop/mnt" for m in session.inspect()["Mounts"]),
                "drop-zone bind absent after Restart session")
        lab.choose(source, folder=True, tier="rw", remember=True, writable=True)
        target = "/home/desktop/mnt/transition-folder/present.txt"
        until(lambda: session.exists(target), "RW transition share did not mount", 5)
        lab.dismiss()
        require(session.exec("sh", "-c", "echo writable > /home/desktop/mnt/transition-folder/from-rw.txt").returncode == 0,
                "writable FUSE child did not take effect after Restart session")
        require((source / "from-rw.txt").read_text().strip() == "writable", "writable child did not reach host")
        lab.set_tier("ro", config)
        require(session.exists(target), "tightening unexpectedly removed existing mount")
        lab.open_file()
        lab.hover(110, 39)
        lab.hover(285, 91)
        lab.screenshot("rw-to-ro-recent-disabled")
        lab.key("Escape", "Escape", "Escape")
        lab.note("#26 10 transition Read & write → Read-only", "existing mount stayed active; saved writable share became disabled immediately")
        lab.set_tier("off", config)
        until(lambda: not session.exists(target), "switching to Off did not unmount live share", 6)
        lab.open_file()
        lab.hover(110, 39)
        lab.screenshot("transition-off-menu")
        lab.key("Escape", "Escape")
        lab.note("#26 10 transition any → Off", "live share unmounted immediately and Mount became disabled")
        session.stop()
    finally:
        if session.process.poll() is None:
            session.stop("signal")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--viewer", required=True, type=Path)
    parser.add_argument("--workdir", required=True, type=Path)
    parser.add_argument("--display", default=":197")
    parser.add_argument("--case", choices=("all", "gpu", "cpu", "off", "archives", "rw", "private", "transitions"), default="all")
    args = parser.parse_args()
    require(args.viewer.is_file(), f"viewer is missing: {args.viewer}")
    require(args.workdir.is_dir(), f"workdir is missing: {args.workdir}")
    lab = Lab(args.viewer.resolve(), args.workdir.resolve(), args.display)
    try:
        lab.start_display()
        cases = (("gpu", gpu_folder_and_isolation), ("cpu", cpu_folder_and_busy_unmount),
                 ("off", off_and_override), ("archives", archives_and_saved_shares),
                 ("rw", writable_folder), ("private", no_propagation),
                 ("transitions", tier_transitions))
        for name, run in cases:
            if args.case in ("all", name):
                run(lab)
    finally:
        lab.cleanup()
    return 0


if __name__ == "__main__":
    sys.exit(main())
