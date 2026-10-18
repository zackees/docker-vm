# Diskless private desktop design

## Data boundary

```text
Chromium DOM/profile (container tmpfs only)
  -> Xvnc framebuffer updates -> authenticated loopback WebSocket/RFB bridge
  -> packaged WebKitGTK/noVNC canvas -> screen pixels
keyboard + absolute pointer -> bridge -> Xvnc
```

The WebKitGTK application loads only its packaged `index.html` and a pinned noVNC
submodule. It is not a general browser for the remote session. It does not
navigate to remote pages and it exposes no filesystem, shell, Docker, or
arbitrary-network API to its JavaScript. One command returns the live session's
in-memory connection details. A second accepts only a validated numeric display
scale (0.5–4), atomically publishing it in the session's verified tmpfs directory.
The container reads that setting through its existing read-only session mount;
its DPI widget applies desktop settings or a session-local manual override.
Framebuffer dimensions independently follow physical webview pixels, capped at
4K pixel count, without passing remote DOM or browser state across the boundary.

The native bridge binds `127.0.0.1` on an ephemeral port, verifies the expected
packaged-viewer Origin and a single high-entropy capability carried as a WebSocket
subprotocol, then proxies binary RFB to the un-published container address. The
capability is not in a URL, command line, environment variable, Docker metadata,
or persistent keyring. VNC has an independent generated password over the
container-only connection. VNC listens only on the Docker bridge network; it
has no host-published port.

Clipboard is disabled at TigerVNC in both directions; the custom noVNC canvas
contains no clipboard/file-transfer/download/reconnect/settings UI. A disconnect
is terminal: the viewer never resumes or reconnects to a stale desktop.

## Persistence controls

The desktop container has a read-only root, no Docker socket, no host display,
no host home, no restart policy, no published ports, and Docker logging
disabled. It has no GPU device unless GPU rendering applies (below). Its writable home/profile/downloads/cache, runtime directory,
temporary directories, shared memory, Xvnc logs, VNC password hash, and Chromium
profile are tmpfs mounts. The host bind mount holds only the generated password
file in the viewer-verified `XDG_RUNTIME_DIR` tmpfs directory and is read-only in
the container.

A folder mounted through Read & write file sharing with **Allow changes** is
the deliberate exception: files changed in that host folder persist after the
session ends. The tier and per-mount choice must both allow writes. Directory
FUSE walks use directory file descriptors and `O_NOFOLLOW` at every component;
the mount remains owner-only, `nodev`, `nosuid`, and `noexec`. New files cannot
receive executable bits from the container.

## GPU rendering

GPU rendering is automatic. At launch, off the UI thread, the viewer checks that
`/dev/kvm` is openable read-write (KVM is enabled only selectively on some hosts)
and that `docker info` lists an NVIDIA CDI device (`nvidia.com/gpu=N`). If both
hold, a per-session compose override attaches the lowest-numbered GPU (never
`=all`) and sets `DESKTOP_GPU=nvidia`; Chromium then rasterizes and runs WebGL/WebGPU through
ANGLE on the driver's Vulkan device. Compositing stays in software: Xvnc has
no GPU, and a GPU-composited window cannot be presented to it (it stays black),
so finished frames are handed to X in software. `tests/gpu_render.py` checks
the framebuffer pixels, not just `chrome://gpu`.
The RFB transport is unchanged. If any check fails, or the container cannot
start with the device, the session uses the CPU path: software rendering with
Chromium's GPU process disabled, exactly as before. `DOCKER_VM_GPU=off` forces
the CPU path.

This deliberately widens the attack surface: remote Chromium content reaches
the host's NVIDIA kernel driver through the GPU process. The device nodes and
driver libraries come from the host's CDI spec; nothing else is added.

### Wayland GPU desktop (default in GPU mode)

Xvnc has no GPU, so the GPU path above still composites and encodes on the CPU.
In GPU mode the viewer therefore starts a Wayland desktop instead
(`DESKTOP_DISPLAY=wayland`; `DOCKER_VM_WAYLAND=off` opts out):

- **Compositor**: headless labwc (a stacking wlroots compositor) renders on the
  NVIDIA driver through GBM, with the Xfce panel and desktop on top: windows
  have title bars and can be moved, resized and launched from the menu. (A bare
  tiling compositor left one borderless browser and nothing to move.) The CDI spec omits NVIDIA's GBM backend, so the viewer bind-mounts the
  host's own `libnvidia-allocator` read-only as `gbm/nvidia-drm_gbm.so` (it
  always matches the kernel driver). Chromium runs natively on Wayland with
  full GPU compositing.
- **Frames**: `video-stream` asks xdg-desktop-portal (wlr backend, configured
  to pick the headless output with no chooser) for a screencast and captures it
  over PipeWire as DMA-BUFs: compositor buffer -> EGLImage -> CUDA -> NVENC,
  never through the CPU. The portal offers buffer layouts it cannot always
  allocate, so `video-stream` tries the layouts GStreamer GL can import one at a
  time and logs which one streams; system memory is the last resort.
  2560x1600 animation: ~55-60 fps, versus ~7-13 delivered by Xtigervnc/RFB.
- **Input and fallback frames**: wayvnc. noVNC and wayvnc share only Apple
  Remote Desktop authentication (Diffie-Hellman + AES-encrypted credentials),
  so wayvnc gets user `desktop` and the session's VNC password with
  `relax_encryption`; the RFB stream itself is unencrypted, as with Xvnc, and
  is reachable only through the capability-gated loopback bridge.
- **Resize and DPI**: wayvnc resizes the headless output on the client's RFB
  `SetDesktopSize` (verified; it answers status 4 but applies the size and
  announces it). The DPI scale is applied with `wlr-randr`. A resize silently
  stalls a live screencast, so `wayland-session` watches the output's actual
  mode and bumps a generation file on any change; `video-stream` then renews
  the screencast.
- **Cursor**: drawn locally by the host. The headless output has no cursor
  plane, so the compositor would paint the cursor into every captured frame
  and it would trail the hand by a full encode/decode round trip; the session
  therefore uses a transparent cursor theme and the viewer shows the host's
  arrow over the canvas (shape changes such as the text I-beam are not
  mirrored).
- **Audio**: `audio-stream` makes a null sink the default output of the
  session's PipeWire (PulseAudio API), Opus-encodes its monitor (48 kHz
  stereo, 10 ms frames) and serves it on TCP 5902 on the container network;
  the viewer plays it with GStreamer on the host's default output. Output
  only: there is no microphone path into the container. The X desktop (CPU
  mode and fallbacks) runs the same audio service.
- **Viewer**: decodes (NVDEC) and draws the stream natively in a GTK overlay
  on the noVNC canvas; see `docs/GPU-PIPELINE.md` for the measured pipeline.
- **Fallback**: if labwc or wayvnc cannot start, `wayland-session` exits 75 and
  `ephemeral-session` starts the Xtigervnc session described above.

`DOCKER_VM_XORG=on` instead selects a headless Xorg on the NVIDIA X driver; it
stays opt-in because reading its framebuffer back for VNC or capture is slower
than Xtigervnc.

Startup archive mounts are opt-in (`--mount type=zip`).
Each binds an empty directory beside the session directory read-only with
`rslave` propagation; after `compose up`, the viewer mounts a FUSE view of the
archive on that directory. Decryption happens in the viewer process on each
read, and plaintext is never written to disk. The FUSE mount is read-only,
`nodev`/`nosuid`, and has no `allow_other`/`allow_root`, so the kernel admits only
the session UID/GID: container root, other container UIDs, and host root cannot
read it, and Docker never has to access it. Passwords come from a 0600 file,
standard input, or a dialog, are verified before the container starts, and are
zeroed when dropped. Archive paths are sanitized: absolute and `..` entries and
symlinks are not exposed.

For live file sharing, the viewer binds an initially empty tmpfs drop zone to
`/home/desktop/mnt` when sharing is enabled. The parent bind stays read-only;
in Read & write, an explicitly writable folder gets its own writable FUSE
submount. Other live folder mounts remain read-only. The
container cannot name a host path to mount; that choice belongs to the native
viewer. If the host runtime mount cannot propagate later FUSE mounts, the
desktop still starts without live sharing.
The drop-zone backing directory is traversable but not writable by
the container. Its read-only bind prevents `..` and chmod from changing
the backing directory, while the viewer can still add and remove FUSE mount
points on the host. Writable `dirfs` shares map new
files to the host owner and strip executable bits. Its root-anchored path
lookup refuses symlink escapes.
The native File menu alone opens GTK pickers for host paths; the webview has no
path-accepting mount command. Menu items reflect the current tier and runtime
availability. ZIPs use the same decrypt-on-read FUSE view as startup mounts,
and the Encrypted tier checks every file entry before mounting. Unmount releases
the live FUSE mount, while Quit follows the window-close cleanup path.
The File sharing tier is a launch-time bind ceiling: Off has no drop-zone bind;
Encrypted and Read-only bind it read-only; Read & write also keeps the parent
read-only but permits writable FUSE children. Each folder defaults to
read-only. Tightening the tier rejects new mounts at
once, and switching to Off unmounts current shares. Widening needs a new
session. Explicitly remembered shares persist as paths only in a mode-0600
config file. They appear inactive under Recent and encrypted ZIPs prompt again.

The native WebKitGTK shell loads only the local viewer UI and uses an
off-the-record request context. Chromium inside the desktop container keeps its
own renderer sandbox; Docker's default seccomp profile cannot be used because it
blocks the namespace setup required by that sandbox. The container therefore
uses `seccomp=unconfined`, while still running as an unprivileged user with all
Linux capabilities dropped, a read-only root, and no host mounts (other than
the read-only session directory and opted-in archive views) or device access. Host journald/coredump capture remains a strict-preflight concern.

Deletion at exit is prompt cleanup only—not the privacy control. Volatile mounts
prevent the session data from being written in the first place.

## Host contract

Strict mode supports native Linux Docker Engine only. It rejects disk-backed or
unknown session runtime storage, active disk-backed swap, hibernation, enabled
core dumps/crash collectors, Docker Desktop/VM contexts, and unavailable Docker.
It is read-only: it never disables swap, changes hibernation, edits drivers, or
reboots the machine. An administrator can change host policy after launch, so
preflight is an eligibility check, not a defense against an administrator.

## Lifecycle

The browser runs under a main-process GDB supervisor. Any browser exit
captures a bounded (2 MiB) report in the container's runtime tmpfs and leaves
Xfce/Xvnc running for inspection; it does not restart the browser. A notification
warns that closing the viewer destroys the diagnostics. Locals/frame arguments,
core dumps, automatic report exports and debugger symbol downloads are disabled.
Live, bounded output/resource snapshots are retained in the same tmpfs so a tab
failure that leaves the browser alive can be investigated. The task limit is
2048, counting both threads and processes. Reports remain potentially sensitive
and require review before sharing. Renderer
crashes and host/container loss are outside this main-process capture boundary.

Each launch creates a UUID-scoped Compose project. Viewer close stops/removes
only that project and signals the loopback bridge to close. Container loss makes
the bridge fail; there is no reconnect. Docker-daemon loss cannot guarantee
container removal within a timeout, so the viewer fails closed immediately and
cleanup is retried only when the daemon is reachable. No unrelated container or
legacy Wolf state is removed.
