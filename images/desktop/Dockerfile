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

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      chromium chromium-sandbox dbus-x11 dumb-init tigervnc-standalone-server \
      tigervnc-tools xauth xfce4 xfce4-terminal python3-gi gir1.2-gtk-3.0 x11-xserver-utils \
      gdb x11-utils \
      libegl1 libgles2 libgl1 libvulkan1 \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --uid 1000 --shell /bin/bash desktop \
 && install -d -o desktop -g desktop -m 0700 /home/desktop

# Keep frequently edited session/UI files after the stable dependency layer.
COPY --chmod=0755 images/desktop/ephemeral-session /usr/local/bin/ephemeral-session
COPY --chmod=0755 images/desktop/crash-watch /usr/local/bin/crash-watch
COPY images/desktop/browser-backtrace.gdb /usr/local/share/browser-backtrace.gdb
COPY --chmod=0755 images/desktop/display-scale /usr/local/bin/display-scale
COPY images/desktop/display-scale.desktop /usr/share/applications/display-scale.desktop
# GPU mode only: the NVIDIA CDI spec injects the driver libraries and device
# nodes but not these loader registrations, so glvnd/Vulkan would never pick
# the driver. In CPU mode no library they name exists and nothing loads them.
COPY images/desktop/nvidia-egl-vendor.json /usr/share/glvnd/egl_vendor.d/10_nvidia.json
COPY images/desktop/nvidia-vulkan-icd.json /usr/share/vulkan/icd.d/nvidia_icd.json

USER desktop
EXPOSE 5900
ENTRYPOINT ["/usr/bin/dumb-init", "--", "/usr/local/bin/ephemeral-session"]
