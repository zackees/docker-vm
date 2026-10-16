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
cd viewer/src-tauri && cargo build --release
cd .. && ./launch
```

The viewer uses Tauri's WebKitGTK runtime. Distribute it with the matching
WebKitGTK system runtime (or a packaged runtime closure); `viewer/launch` shows
a native error dialog if an ELF dependency is missing before the app can start.

Run the packaged viewer, not a system browser and not `docker compose up`.
The viewer creates a unique session and removes it when the window closes.

Read-only archive mounts (`--mount type=zip,...`, see the README) additionally
need the setuid `fusermount3` helper on `PATH` and shared mount propagation for
`XDG_RUNTIME_DIR` (the systemd default); the viewer refuses to start without
them. `user_allow_other` in `/etc/fuse.conf` is not required and should not be
enabled for this purpose.

## Validation checklist

For a release candidate, collect sanitized evidence that:

- `docker inspect` shows `ReadonlyRootfs=true`, `LogConfig.Type=none`, no
  restart policy, no Docker socket/host-home mounts, and no published VNC port;
  devices are empty in CPU mode and exactly one `nvidia.com/gpu=N` CDI device
  in GPU mode.
- GPU mode engages automatically with `/dev/kvm` usable and an NVIDIA CDI
  device present (`chrome://gpu` in the session reports hardware acceleration
  on ANGLE/Vulkan), and `DOCKER_VM_GPU=off` or an unusable `/dev/kvm` falls
  back to CPU rendering.
- on a GPU host, `tests/gpu_render.py` passes: the Xvnc framebuffer shows the
  page and its WebGL canvas, drawn by the NVIDIA renderer (not just
  `chrome://gpu` claiming acceleration).
- with a synthetic encrypted archive mounted, the target is read-only in the
  container, reads as UID 0 or any other UID fail with `Permission denied`, the
  host mount options show no `allow_other`, and the mount and its directory are
  gone after the viewer closes.
- all container profile, downloads, temporary, runtime, Xvnc, and shared-memory
  writes resolve to tmpfs; the local WebKitGTK viewer uses an off-the-record
  request context and never navigates to the remote browser.
- Chromium's renderer sandbox starts successfully inside the desktop container.
  This requires `seccomp=unconfined` because Docker's default seccomp profile
  blocks Chromium's required namespace setup; verify the remaining container
  hardening controls rather than adding `--no-sandbox`.
- synthetic URLs, cookies, form values, IndexedDB/local-storage values, download
  names, and clipboard canaries do not appear in persistent paths or survive a
  fresh launch. Do not use real secrets for this test.
- typing, scrolling, dialogs, multiple windows and pointer boundary crossing
  work without pointer lock/warping. Attempts at reconnect, stale capability
  reuse, second connection, clipboard, file transfer, popup, navigation, and
  DevTools fail closed.

The framebuffer necessarily exists in volatile viewer/container RAM. These
checks do not claim secure RAM erasure or erase host operational metadata.
