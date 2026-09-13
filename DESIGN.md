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
no host home, no GPU device, no restart policy, no published ports, and Docker
logging disabled. Its writable home/profile/downloads/cache, runtime directory,
temporary directories, shared memory, Xvnc logs, VNC password hash, and Chromium
profile are tmpfs mounts. The host bind mount holds only the generated password
file in the viewer-verified `XDG_RUNTIME_DIR` tmpfs directory and is read-only in
the container.

The native WebKitGTK shell loads only the local viewer UI and uses an
off-the-record request context. Chromium inside the desktop container keeps its
own renderer sandbox; Docker's default seccomp profile cannot be used because it
blocks the namespace setup required by that sandbox. The container therefore
uses `seccomp=unconfined`, while still running as an unprivileged user with all
Linux capabilities dropped, a read-only root, and no host mounts or device
access. Host journald/coredump capture remains a strict-preflight concern.

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

Each launch creates a UUID-scoped Compose project. Viewer close stops/removes
only that project and signals the loopback bridge to close. Container loss makes
the bridge fail; there is no reconnect. Docker-daemon loss cannot guarantee
container removal within a timeout, so the viewer fails closed immediately and
cleanup is retried only when the daemon is reachable. No unrelated container or
legacy Wolf state is removed.
