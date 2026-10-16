# syntax=docker/dockerfile:1
# Reviewed Debian 13 slim root filesystem; update this digest deliberately.
FROM debian:13-slim@sha256:d7e12182ce18b85b93007c1dedf31f2d29e01ccf3182cc4017c709b6259bc132

ENV DEBIAN_FRONTEND=noninteractive \
    HOME=/home/desktop \
    XDG_RUNTIME_DIR=/run/user/1000 \
    XDG_CACHE_HOME=/home/desktop/.cache \
    XDG_CONFIG_HOME=/home/desktop/.config \
    XDG_DATA_HOME=/home/desktop/.local/share \
    CHROMIUM_USER_DATA_DIR=/home/desktop/.config/chromium \
    LIBGL_ALWAYS_SOFTWARE=1

# Package layers run from most to least stable, so adding a package rebuilds
# only its own layer. The apt caches are BuildKit cache mounts: rebuilt layers
# reuse downloaded .debs, and nothing from apt ends up in the image.
RUN rm -f /etc/apt/apt.conf.d/docker-clean

# Desktop: browser, session, VNC server, diagnostics.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update \
 && apt-get install -y --no-install-recommends \
      chromium chromium-sandbox dbus-x11 dumb-init tigervnc-standalone-server \
      tigervnc-tools xauth xfce4 xfce4-terminal python3-gi gir1.2-gtk-3.0 x11-xserver-utils \
      gdb x11-utils

# GPU mode: GL/Vulkan loaders, the GPU X server, and the NVENC video service.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update \
 && apt-get install -y --no-install-recommends \
      libegl1 libgles2 libgl1 libvulkan1 \
      xserver-xorg-core tigervnc-xorg-extension \
      gir1.2-gstreamer-1.0 gir1.2-gst-plugins-base-1.0 gstreamer1.0-plugins-base \
      gstreamer1.0-plugins-bad gstreamer1.0-x

# Wayland GPU desktop: headless sway on the NVIDIA driver (GBM), wayvnc for
# input and RFB fallback, PipeWire screencast for zero-copy NVENC capture.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update \
 && apt-get install -y --no-install-recommends \
      sway wayvnc xwayland pipewire wireplumber xdg-desktop-portal xdg-desktop-portal-wlr \
      gstreamer1.0-pipewire gstreamer1.0-gl python3-dbus

# Mount points for the host's NVIDIA X driver (GPU X server only). The GBM
# allocator mounts (Wayland GPU desktop) need none: Docker creates them, and
# empty placeholders there broke the DMA-BUF screencast negotiation.
RUN install -d /opt/nvidia-xorg/drivers /opt/nvidia-xorg/extensions \
 && touch /opt/nvidia-xorg/drivers/nvidia_drv.so /opt/nvidia-xorg/extensions/libglxserver_nvidia.so \
 && useradd --create-home --uid 1000 --shell /bin/bash desktop \
 && install -d -o desktop -g desktop -m 0700 /home/desktop

# Keep frequently edited session/UI files after the stable dependency layers.
COPY --chmod=0755 images/desktop/ephemeral-session /usr/local/bin/ephemeral-session
COPY --chmod=0755 images/desktop/crash-watch /usr/local/bin/crash-watch
COPY images/desktop/browser-backtrace.gdb /usr/local/share/browser-backtrace.gdb
COPY --chmod=0755 images/desktop/display-scale /usr/local/bin/display-scale
COPY --chmod=0755 images/desktop/video-stream /usr/local/bin/video-stream
COPY --chmod=0755 images/desktop/display-size /usr/local/bin/display-size
COPY --chmod=0755 images/desktop/wayland-session /usr/local/bin/wayland-session
COPY images/desktop/display-scale.desktop /usr/share/applications/display-scale.desktop
# GPU mode only: the NVIDIA CDI spec injects the driver libraries and device
# nodes but not these loader registrations, so glvnd/Vulkan would never pick
# the driver. In CPU mode no library they name exists and nothing loads them.
COPY images/desktop/nvidia-egl-vendor.json /usr/share/glvnd/egl_vendor.d/10_nvidia.json
COPY images/desktop/nvidia-vulkan-icd.json /usr/share/vulkan/icd.d/nvidia_icd.json

USER desktop
EXPOSE 5900
ENTRYPOINT ["/usr/bin/dumb-init", "--", "/usr/local/bin/ephemeral-session"]
