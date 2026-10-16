# Benchmark record

No benchmark result is implied by this template. Record only a completed run
with retained screenshots/logs and exact image digests.

## Required record

| Field | Value |
| --- | --- |
| Date / operator | |
| Host GPU UUID, PCI ID, driver | |
| Docker / Compose / Wolf image digest / desktop image digest | |
| Client GPU, OS, Moonlight version | |
| Connection (wired LAN/VPN), resolution, refresh, codec, bitrate | |
| Browser renderer and WebGL evidence | |
| Browser media-decode evidence | |
| Server encoder evidence | |
| Client hardware decoder evidence | |

## 1080p60 acceptance run

Run a continuous ten-minute 1920x1080/60 Hz workload: text editing, rapid
scrolling, video, WebGL, audio, window changes, and download/persistence
checks. Save Moonlight Statistics and controller logs. Record distributions—not
only averages—for encode and decode time, network latency/jitter, frame drops,
and CPU/GPU load. The acceptance target is sustained 60 fps, under 1% dropped
frames, and p95 encode/decode below 16.7 ms where those distributions exist.

Moonlight statistics do not measure full input-to-photon latency. The below-50 ms
target requires an external, documented measurement method; leave it unclaimed
when that equipment/method is unavailable. Repeat at 1440p60 and 1080p120 only
after the baseline passes. HEVC and AV1 each need a fresh end-to-end hardware
proof.

## Evidence status on the NixOS reference host

Host inspection observed NVIDIA GeForce RTX 3060 (PCI `0000:06:00.0`, UUID
`GPU-b4e0b439-9095-b6cd-3c07-3db8fbc05acd`), driver `595.71.05`, Docker
`29.7.2`, Compose `5.4.0`, and `/dev/dri/renderD128` mapped to that PCI device.
This is host capability evidence only. The legacy Docker `--gpus all` CUDA probe
failed with `AMD CDI spec not found`, but the selected CDI form
`--device=nvidia.com/gpu=0` successfully exposed the RTX 3060 and driver inside
the CUDA container. This proves only basic container GPU visibility—not browser
rendering, stream encoding, remote presentation, client decoding, or any
performance target. The final Debian Chromium acceptance run is pending the
host repair described below; no 10-minute or input-to-photon result is recorded.

The final Debian Chromium image was built but not live-validated: during the
final transition the host NVIDIA userspace changed while kernel driver 595.71.05
remained loaded and both Docker CDI spec directories disappeared. Docker could
no longer resolve `nvidia.com/gpu=0`. Do not repair driver/CDI state from this
project; regenerate the host's CDI configuration after completing the host driver
update, then rerun preflight, pairing, and all live gates.

## VNC baseline (2026-10-16)

`tests/perf/bench.sh` with the viewer's `DOCKER_VM_STATS=1` counters: a
full-screen canvas animation in a GPU-mode session (RTX 3060, ANGLE/Vulkan
raster, software compositing), host load average ~45-50. "Content fps" is the
number of distinct page frames the viewer actually painted (frame-stamp block),
not RFB updates.

| Framebuffer | Setting | Page fps (container) | Content fps (viewer) | Xvnc CPU | Chromium GPU proc | Viewer WebKit |
|---|---|---|---|---|---|---|
| 1280x800 | defaults (q6, c2) | 58.8 | 26.5 | 57% | 35% | 45% |
| 2560x1600 | defaults (q6, c2) | 24.0 | 12.6 | 90% | 75% | 85% |
| 2560x1600 | quality 4 | 31.5 | 9.4 | 79% | 70% | 65% |
| 2560x1600 | quality 2 | 22.3 | 10.5 | 79% | 71% | 68% |
| 2560x1600 | quality 8 | 29.0 | 9.3 | 80% | 71% | 60% |
| 2560x1600 | compression 6 | 31.0 | 10.3 | 79% | 71% | 63% |
| 2560x1600 | Xvnc CompareFB=0 | 28.0 | 10.1 | 78% | 67% | 67% |

At HiDPI, no RFB quality/compression or Xvnc setting moves delivered frames
meaningfully: Xvnc encoding, Chromium's software compositing (needed to reach
Xvnc at all) and noVNC's JavaScript decoding each consume most of a core. Only
fewer pixels helps (about 2x at 1280x800). Compression level 1 at 2560x1600
stalled delivery entirely in two runs. Defaults are unchanged; the next step is
a hardware-encoded video transport, measured with the same harness.
