# docker-vm

A Linux-first, ephemeral Chromium desktop. Chromium runs in a CPU-only Xfce
desktop container; the packaged Tauri/CEF client displays it through a private
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
cargo tauri build
```

Launch the bundled `docker-vm-viewer` application. It creates one fresh Docker
Compose project, an in-memory VNC password, an authenticated loopback bridge,
and a fresh CEF off-the-record context. Closing it removes only that project.

Do not run `docker compose up` by hand: `SESSION_RUNTIME_DIR` is deliberately
required and is created and verified by the viewer.

See [DESIGN.md](DESIGN.md) and [docs/SETUP.md](docs/SETUP.md) for host policy,
verification, and limitations.
