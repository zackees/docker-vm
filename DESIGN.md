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

## GPU rendering

GPU rendering is automatic. At launch, off the UI thread, the viewer checks that
`/dev/kvm` is openable read-write (KVM is enabled only selectively on some hosts)
and that `docker info` lists an NVIDIA CDI device (`nvidia.com/gpu=N`). If both
hold, a per-session compose override attaches the lowest-numbered GPU (never
`=all`) and sets `DESKTOP_GPU=nvidia`; Chromium then rasterizes and runs WebGL/WebGPU through
ANGLE on the driver's Vulkan device. Compositing stays in software: Xvnc has
no GPU, and a GPU-composited window cannot be presented to it (it stays black),
so finished frames are handed to X in software. `tests/gpu-render.sh` checks
the framebuffer pixels, not just `chrome://gpu`.
The RFB transport is unchanged. If any check fails, or the container cannot
start with the device, the session uses the CPU path: software rendering with
Chromium's GPU process disabled, exactly as before. `DOCKER_VM_GPU=off` forces
the CPU path.

This deliberately widens the attack surface: remote Chromium content reaches
the host's NVIDIA kernel driver through the GPU process. The device nodes and
driver libraries come from the host's CDI spec; nothing else is added.

Archive mounts are opt-in (`--mount type=zip`) and are the only other host binds.
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
