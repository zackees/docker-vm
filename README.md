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
