# Setup and verification

## Supported host policy

Use native Linux Docker Engine and a user `XDG_RUNTIME_DIR` mounted as tmpfs.
Before launch, disable disk-backed swap and hibernation and configure the host
to suppress core dumps/crash collectors for the viewer user. Docker Desktop,
remote Docker contexts, zram with writeback, and unknown crash/log collectors
are unsupported in strict mode.

```bash
./scripts/preflight --strict
git submodule update --init --recursive
./scripts/build
cd viewer/src-tauri && cargo tauri build
```

The Tauri CEF runtime is pinned to an upstream `feat/cef` commit; it is not a
stable standard Tauri webview path. The CEF bundle download is large and must be
reviewed/pinned before distribution.

Run the packaged viewer, not a system browser and not `docker compose up`.
The viewer creates a unique session and removes it when the window closes.

## Validation checklist

For a release candidate, collect sanitized evidence that:

- `docker inspect` shows `ReadonlyRootfs=true`, `LogConfig.Type=none`, no
  restart policy, no GPU/device/Docker socket/host-home mounts, and no published
  VNC port.
- all container profile, downloads, temporary, runtime, Xvnc, and shared-memory
  writes resolve to tmpfs; the viewer CEF root cache resolves to the same verified
  host tmpfs.
- the CEF renderer sandbox is active and software rendering is used; it has no
  default profile, CEF log, keyring, or system-browser reuse.
- synthetic URLs, cookies, form values, IndexedDB/local-storage values, download
  names, and clipboard canaries do not appear in persistent paths or survive a
  fresh launch. Do not use real secrets for this test.
- typing, scrolling, dialogs, multiple windows and pointer boundary crossing
  work without pointer lock/warping. Attempts at reconnect, stale capability
  reuse, second connection, clipboard, file transfer, popup, navigation, and
  DevTools fail closed.

The framebuffer necessarily exists in volatile viewer/container RAM. These
checks do not claim secure RAM erasure or erase host operational metadata.
