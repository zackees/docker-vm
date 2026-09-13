# Setup and validation

This is a trusted, single-user LAN/VPN desktop. Wolf receives Docker access and
is therefore host-administration equivalent; do not expose its ports to an
untrusted network.

## Before starting

Run the read-only host report from the repository root:

```bash
./scripts/preflight
```

For the reference NVIDIA host, this must identify the selected RTX GPU by UUID
and PCI address, the NVIDIA driver, `renderD128` mapping, Docker context, and
CDI/runtime integration. A render node existing on its own is not a GPU pass.
This NixOS reference host uses CDI selection (`nvidia.com/gpu=0`); its legacy
`--gpus all` path is not a valid readiness test. If the selected GPU access fails
(for example an absent or stale CDI spec), fix host integration first; do not
replace it with software rendering.

Before a fresh launch, verify that the NVIDIA userspace and loaded kernel driver
match and that a CDI spec is present in Docker's configured CDI directories.
`nvidia-smi` reporting an NVML/driver mismatch or Docker reporting an
unresolvable `nvidia.com/gpu=0` device is a host-maintenance blocker, not an
image problem. Do not reboot or regenerate CDI configuration while the user is
using the host desktop unless they explicitly authorize that maintenance.

Install a native Moonlight client on the machine used as the client. On NixOS,
a disposable test client can be started without changing the system profile:

```bash
nix shell nixpkgs#moonlight-qt --command moonlight
```

Use `scripts/connect HOST` for the repository baseline. It deliberately starts
Moonlight windowed with absolute desktop mouse coordinates and does not capture
system keys. It does not request fullscreen or relative/FPS mouse mode. In
Moonlight 6.1, pointer-region locking is enabled only for fullscreen (or an
explicit user toggle); normal windowed absolute mode leaves the pointer region
unrestricted. Verify the client pointer crosses the Moonlight window boundary
before treating the setup as acceptable.
The launcher also sets `SDL_MOUSE_AUTO_CAPTURE=0`; Moonlight 6.1's absolute
mode uses direct coordinates rather than relative/FPS input, and this prevents
SDL from auto-capturing on window entry. Do not use a launcher that omits it.

The desktop host itself can be the client for local testing, but that only proves
the local network path and its decoder; it is not a substitute for a second
client if remote-network behavior matters.

## Start and pair

Install `docker`, Compose v2, `jq`, and `realpath`, then configure, build, and
start the stack:

```bash
./scripts/configure
./scripts/build
docker compose -f compose.yaml -f compose.nvidia.yaml up -d
```

Do not put pairing state or browser profiles in Git.

In Moonlight, add the host's LAN/VPN address, choose **Desktop**, and complete
the displayed PIN pairing flow. Wolf logs an `Insert pin at
http://HOST:47989/pin/#...` URL; open that URL only on the trusted host/LAN and
enter the four-digit code displayed by Moonlight. Use 1920x1080 at 60 Hz, H.264, and 20 Mbps as
test inputs—not guaranteed performance settings. Do not run a second Desktop
session against the same writable Chromium profile.

## Evidence gate

With the stream connected, run:

```bash
./scripts/doctor --wolf docker-vm-wolf --desktop <live-worker-name> --strict
```

The worker container is created by Wolf and its generated name may differ;
pass that exact name to `--desktop`. Strict mode intentionally fails until the
following independent evidence is recorded:

| Layer | Required live evidence |
| --- | --- |
| Browser render | Visible `chrome://gpu`: hardware renderer, compositing and WebGL |
| Browser video decode | `chrome://media-internals` while playing a known test video |
| Server encoding | Wolf/Moonlight negotiated H.264 encoder, with no software encoder fallback |
| Client decode | Moonlight Statistics: hardware decoder, decode timing and dropped frames |

GPU visibility or NVENC alone proves none of the other three layers. A software
renderer/encoder is a failed accelerated gate, never a passing fallback.

For a manual per-session evidence file, retain the corresponding screenshot and
record only observations from that same worker/controller session:

```text
DESKTOP_VISIBLE=PASS
BROWSER_RENDER=PASS
BROWSER_DECODE=PASS
CLIENT_DECODE=PASS
```

Pass it with `--evidence path/to/session.evidence`. `DESKTOP_VISIBLE` requires
the actual streamed Chromium window, not a process listing; the other entries
require the pages/statistics described above. The controller H.264 hardware
encoder is checked from its recent live log.

The current image contains Debian Chromium 152 at `/usr/bin/chromium`, launched
with Wayland Ozone. Its live GUI verification is pending host NVIDIA/CDI repair.
`chrome://sandbox` is supporting evidence; it does not replace the visible GPU
and media pages above.

## Functional and persistence checks

While viewing the actual Moonlight presentation, open three normal Chromium
windows, resize/switch them, use a dialog, type and scroll, play audio, and
download a small non-sensitive file into `Downloads`. Capture a screenshot of
the Moonlight window plus a statistics screenshot. Verify the download and
profile state after a controlled worker recreation.

Test an ordinary transport disconnect separately from explicit Moonlight **Quit**:
reconnect using the same paired client identity and inspect whether the same
desktop session remains. The selected Wolf release uses distinct pause and stop
paths, but this repository makes no seamless-resume claim until observed. A host
reboot is disruptive to the active host desktop and must be scheduled separately;
until then only restart-with-profile persistence is validated.
