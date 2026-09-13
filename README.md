# docker-vm

A Linux-first, ephemeral Chromium desktop. Chromium runs in a CPU-only Xfce
desktop container; the packaged Tauri/WebKitGTK client displays it through a private
RFB/noVNC canvas. This is a container desktop, not a VM.

The viewer never receives the remote browser DOM, profile, URLs, cookies, or
DevTools traffic. It receives only RFB framebuffer updates and ordinary
keyboard/absolute-pointer input.

## Security model

New sessions are intentionally diskless: the container root filesystem is
read-only and all session-writable paths are tmpfs. The viewer refuses to start
unless its per-session directory under `XDG_RUNTIME_DIR` resolves to tmpfs.
Session secrets are created there with mode 0600, never supplied through
Docker environment, labels, command lines, or persistent configuration.

This does not promise forensic RAM erasure, anonymity, protection from a
compromised administrator, or the absence of all OS operational metadata.
Strict host policy—including no disk-backed swap/hibernation/crash dumping—is
checked separately and must be satisfied before making the diskless claim.

### Crash diagnostics

Chromium's main process runs under batch GDB. On browser exit or a fatal
signal, a report is saved to `/run/user/1000/crash/browser-backtrace.log` inside
the container, and the desktop remains running with a notification. **Keep the
viewer open until you have inspected the report**: closing it destroys the
container and its RAM-only diagnostics. No browser restart happens automatically.
Normal browser closure also leaves the desktop and reports available.

While Chromium is running, `crash/browser-output.log` retains the newest 2 MiB
of output and `crash/resources.json` records task/memory counters and filesystem
space once per second, with the last 32 counter changes, 120 recent samples,
and filesystem free-space low-water marks since monitoring started. These RAM-only snapshots
help diagnose tab/resource failures that do not terminate the main process.
The container allows 2048 tasks (threads count too); the former 512-task limit
was exhausted by a 48-tab synthetic test. The same test at 2048 loaded all 48
tabs with 987 tasks and no task-limit or OOM events on the validation host.
Private shared memory has a 2 GiB ceiling (allocated on demand). A 24-tab
4096×4096 canvas test produced 18 child-process crash dumps at 512 MiB and
none at 2 GiB, with no cgroup OOM or task-limit events in either run. This
raises headroom; it does not make an unlimited number of tabs safe.

Find the live session name with `docker ps --filter name=docker-vm`, then inspect:

```bash
docker exec <session-container> cat /run/user/1000/crash/browser-backtrace.log
```

The report retains at most the last 2 MiB of debugger/browser output, including
up to 32 frames per thread and shared-library load addresses. Locals and frame
arguments are omitted, but reports can still contain sensitive data; inspect
privately and do not upload them unreviewed. Docker logging stays disabled, core
dumps are disabled, and no crash-upload or symbol-download service is enabled
by this wrapper. It does not disable Chromium's sandbox or add ptrace privileges.

This captures the **main browser process**, not every sandboxed renderer crash,
host OOM/SIGKILL, viewer crash, or container/daemon loss. Distribution Chromium
is stripped: some frames are addresses rather than function names; full source
symbolization requires matching debug symbols. Reports do not survive a host
restart or container stop. GDB adds some startup/runtime overhead.

To validate capture in an isolated, networkless container after building:

```bash
docker run --rm -i --network none --read-only --cap-drop ALL \
  --security-opt seccomp=unconfined --ulimit core=0 \
  --pids-limit 2048 \
  --tmpfs /tmp:mode=1777,size=128m \
  --tmpfs /run/user/1000:uid=1000,gid=1000,mode=700,size=64m \
  --tmpfs /home/desktop:uid=1000,gid=1000,mode=700,size=128m \
  --shm-size 256m --entrypoint python3 docker-vm-desktop:local \
  - < tests/crash-capture.py
```

The test covers SIGTRAP/SIGABRT/SIGSEGV capture, bounded private reports, normal
exit, harmless SIGCONT delivery, and preserving a real Chromium crash report
while the supervisor lives.
`tests/tab-stress.py` is a separate synthetic multi-site stress test. Run it with
the same isolated Docker command, a 256 MiB `/tmp`, 2 GiB shared memory and a
6 GiB memory limit. Its test-only loopback DevTools endpoint is not enabled in
the interactive desktop application.
Set `-e STRESS_GRAPHICS=1 -e STRESS_TABS=24` for the canvas workload. The test
checks child-process crash dumps as well as tab titles: a crashed tab can
retain its title. Use `--shm-size 512m` to reproduce the previous limit failure.

## Build and run

```bash
git submodule update --init --recursive
./scripts/preflight --strict
./scripts/build
cd viewer/src-tauri
cargo build --release
cd .. && ./launch
```

Launch the bundled `docker-vm-viewer` application through `viewer/launch`. The
launcher reports a missing shared library in a native dialog before the dynamic
loader can fail silently. The viewer creates one fresh Docker Compose project,
an in-memory VNC password, and an authenticated loopback bridge. Closing it
removes only that project.

### Resolution and HiDPI

The viewer automatically requests a remote framebuffer sized in physical pixels
(CSS content size × webview device-pixel ratio, rounded to the nearest pixel).
Window resizing and display-density changes do not reconnect the session. CSS
layout and pointer coordinates remain independent of framebuffer resolution.
The software-rendered desktop is capped at 8,294,400 pixels (4K) and 8192 pixels
per axis; larger windows use aspect-preserving scaling. A server that refuses
resizing keeps its existing framebuffer and is scaled to fit.

The client also sends its display scale to the container (96 × scale DPI), so
text, browser controls, and the Xfce panels stay appropriately sized at the
higher resolution. The small **Desktop DPI** window stays above other windows.
Uncheck **Follow host display** to choose 50–400% manually; recheck it to resume
automatic monitor scaling. Closing it or clicking **Hide to dock** hides only
the window. Click the display-settings icon in the bottom dock to reopen it.
The DPI watcher keeps running while hidden; manual overrides last for this
ephemeral session only. No browser restart or reconnect is needed.

On Linux, use WebKitGTK/Wry. Fractional-scale XWayland can additionally resample
the host window: correct RFB dimensions alone cannot remove compositor blur.
Check the host's legacy-X11 application scaling policy if text is still soft;
the viewer does not change desktop-wide display settings.
In particular, KDE XWayland may report one global device-pixel ratio even when
the window moves to another monitor. In that case no density-change event is
available to the viewer: the compositor performs the final scaling. The manual
container DPI control remains available; true per-monitor density changes are
handled when the webview reports them.

Run `node --test tests/physical-pixels.mjs` (Node 22+) for the issue #3 regression
tests, or `bash tests/validation.sh` for repository validation. The tests execute
the pinned noVNC resize-acknowledgement and display/input methods; revalidate the
private integration hooks in `viewer/ui/physical-rfb.mjs` on a noVNC upgrade.

Do not run `docker compose up` by hand: `SESSION_RUNTIME_DIR` is deliberately
required and is created and verified by the viewer.

See [DESIGN.md](DESIGN.md) and [docs/SETUP.md](docs/SETUP.md) for host policy,
verification, and limitations.
# Building from source

The canonical build is `nix build .#docker-vm`. It uses the pinned Soldr
release as Cargo's compiler front door, with its compile cache enabled, and a
Nix-vendored copy of every Cargo dependency. `nix develop` provides the same
Soldr, Rust, GTK, WebKit, and GStreamer environment for iterative builds.
