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
