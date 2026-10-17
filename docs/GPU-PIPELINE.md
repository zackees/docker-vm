# GPU pipeline: container desktop → viewer, on one shared NVIDIA GPU

How a frame gets from Chromium inside the desktop container to the viewer
window, what runs where, what is copied, and how each piece fails. Everything
here was measured on the reference host (RTX 3060 driving the host displays,
RTX 2080 Ti idle, driver 595.99.02, Docker 29 with the NVIDIA CDI spec,
KDE Plasma on Wayland, 2560x1600 test framebuffer, full-screen canvas
animation; host load 40-100 from other work). Re-measure with
`tests/perf/bench.py --scale 2`.

## Stage map (Wayland GPU desktop, the default in GPU mode)

| # | Stage | Process | API / device | Memory | Copy? |
|---|---|---|---|---|---|
| 1 | Page raster + compositing | Chromium (container, `--ozone-platform=wayland`) | ANGLE/GL on NVIDIA EGL | GPU | no |
| 2 | Desktop compositing | labwc + Xfce panel/desktop, headless wlroots backend | GLES 3.2, NVIDIA EGL device | GPU | no |
| 3 | Screen capture | xdg-desktop-portal-wlr → PipeWire | wlr-screencopy, DMA-BUF `XR24:0x0300000000e080xx` | GPU | no |
| 4 | Import | video-stream: `pipewiresrc ! glupload` | EGLImage from DMA-BUF | GPU | no |
| 5 | Colour convert | `glcolorconvert` → NV12 | GL | GPU | no |
| 6 | Encode | `nvh264enc` (p1, ultra-low-latency, CBR, 2 s GOP) | CUDA-GL interop → NVENC | GPU | no |
| 7 | Transport | TCP 5901 (container network) → viewer | length-framed Annex-B access units | system, ~2-4 MB/s | small |
| 8 | Decode | viewer | NVDEC (`nvh264dec`) | GPU | — |
| 9 | Present | viewer | see "Viewer side" | — | **this is where it stalls** |

Input never uses this path: keyboard and pointer go over RFB to wayvnc (ARD
auth), which also serves full RFB frames whenever video is not live.

Container side, measured: stages 1-6 sustain **54-56 fps at 2560x1600**,
including a mid-stream resize; the compositor ~40-60%, the encoder process ~20%, Chromium
GPU process ~12% of one core.

## Requirements the CDI spec does not cover

| Need | Why | Provided by |
|---|---|---|
| glvnd EGL vendor + Vulkan ICD JSON for NVIDIA | the CDI spec injects the libraries but not their loader registrations; without them EGL/Vulkan never pick the driver | baked into the image (`nvidia-egl-vendor.json`, `nvidia-vulkan-icd.json`) |
| every `/dev/nvidiaN` node, CUDA pinned to one GPU | with only the chosen GPU's node visible, `NvEncOpenEncodeSessionEx` fails with "unsupported device (2)" | viewer adds the other nodes; `CUDA_DEVICE_ORDER=PCI_BUS_ID` + index |
| host `libnvidia-allocator` as `gbm/nvidia-drm_gbm.so` | NVIDIA's GBM backend for wlroots/PipeWire buffer allocation | viewer bind-mounts the host's copy (matches the kernel driver) |
| host NVIDIA X driver (opt-in Xorg only) | headless Xorg on the GPU | viewer bind-mounts `nvidia_drv.so` + `libglxserver_nvidia.so` |

## Failure signatures

| Symptom | Cause | Fix in tree |
|---|---|---|
| Black Chromium window, `chrome://gpu` says hardware accelerated | GPU-composited window presented to Xvnc, which has no GPU | `--disable-gpu-compositing` on the Xvnc GPU path; Wayland path composites on the GPU |
| NVENC "unsupported device (2)" | not all `/dev/nvidiaN` visible in the container | other GPU nodes added |
| `nvh264enc` missing in GStreamer | same as above (registration probes a session) | same |
| Screen collapses to 8x8 | client RFB resize on an output-less headless Xorg screen | host-side resize (`display-size`) for Xorg |
| `pipewiresrc` "unhandled format" / "Format negotiation failed" | portal offers DMA-BUF layouts (`...606010`) its allocator cannot create; pipewiresrc picks the first | layouts glupload can import tried one by one (`e080xx` first) |
| Screencast silent after a resize | output mode change stalls the portal stream without an error | `wayland-session` watches the output mode → generation file → `video-stream` renews |
| Second portal session refused (code 2) | request/session tokens reused across sessions | process-wide token counter |
| WebCodecs `EncodingError: Decode error` | WebKitGTK decodes AVC (length-prefixed + `avcC`) only, never Annex-B | viewer builds `avcC`, length-prefixes NAL units |
| No mouse input reaches the desktop while video is live | `gtkglsink`'s GL area has its own event window; overlay pass-through alone does not cover it | empty input shape on the overlay window and its children |
| Desktop gets half the pixels at 2x zoom on a HiDPI host | viewer forced onto XWayland (`GDK_BACKEND=x11`) | GTK's native backend; on Wayland the viewer sets `GST_GL_API=opengl3` (otherwise `gtkglsink` context creation fails) |
| GStreamer GTK widget never shows a frame | `gtkglsink` cannot preroll until its widget is realized | widget realized up front, transparent until the first frame |

## Viewer side

The viewer reads the stream itself (`native_video.rs`): `appsrc ! h264parse !
nvh264dec ! glsinkbin(gtkglsink)` in a GTK overlay placed on noVNC's canvas.
The overlay is input pass-through, so keyboard and pointer still reach noVNC;
while it is live the UI stops requesting RFB frames. If it fails, the UI falls
back to WebCodecs → WebGL in the page, then to plain RFB.

That only works if the viewer is a GPU client. The wrapper used to force
software GL (`LIBGL_ALWAYS_SOFTWARE=1`, `WEBKIT_DISABLE_COMPOSITING_MODE=1`),
which put every GL context in the viewer (WebKit's WebGL, the GTK GL area) on
llvmpipe. Same container, same 2560x1600 animation at ~58 fps:

| Viewer GL | Presenter | Content fps shown | Viewer GPU clients |
|---|---|---|---|
| software (old wrapper) | WebCodecs → WebGL | ~12 (38-50 ms upload per frame, WebKitWebProcess ~100% CPU) | none |
| software (old wrapper) | native gtkglsink | 22.6 | none |
| hardware (current wrapper) | native gtkglsink | **58.4** (= page fps), RFB 0, viewer ~22% CPU | WebKitWebProcess, docker-vm-viewer |

Nothing crashed or went blank without the software flags. With labwc and the
native Wayland viewer: 53.0 fps shown of 58.0 rendered at 2560x1600. Check a host with `nvidia-smi` while the viewer runs: both processes
should be listed. The viewer log line `native video: first frame presented`
shows the native path is live.

## Audio and input, measured

Through the real viewer (Xvfb host display, Wayland GPU desktop):

| Check | Result |
|---|---|
| Audio vs video, real Chromium (white flash + beep together, 10 pairs, twice) | sound 29-30 ms after the picture (median), every pair within ±52 ms |
| Beep in the container → viewer's output stream | ~225 ms (GPU desktop), ~262 ms (X desktop, CPU mode) |
| Ctrl+L / Ctrl+A / Ctrl+C / Ctrl+V, typing | work (text copied and pasted twice) |
| Alt+Tab between two windows | works |

On a host desktop, shortcuts the host compositor itself takes (e.g. Alt+Tab
under KDE) never reach the viewer window.
