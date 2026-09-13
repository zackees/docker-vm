FROM ghcr.io/games-on-whales/base-app@sha256:1ce23fc7c3b0a9f7ef8d13ad18211e034623c923841e9fcd1e25302531fc4a5a AS gow
FROM debian@sha256:abc9cb88a5587630d7f915f47b23b0668fe250fbfc6457aa4d52b534c1bbf73f
ARG CHROMIUM_VERSION=152.0.7977.82-1~deb13u1
ENV DEBIAN_FRONTEND=noninteractive PUID=1000 PGID=1000 UMASK=000 UNAME=retro HOME=/home/retro XDG_RUNTIME_DIR=/tmp/sockets CHROMIUM_USER_DATA_DIR=/home/retro/.config/chromium NIXOS_OZONE_WL=1 OZONE_PLATFORM=wayland XDG_CURRENT_DESKTOP=sway XDG_SESSION_DESKTOP=sway XDG_SESSION_TYPE=wayland __EGL_EXTERNAL_PLATFORM_CONFIG_DIRS=/usr/share/egl/egl_external_platform.d GBM_BACKEND=nvidia-drm GBM_BACKENDS_PATH=/usr/local/nvidia/lib/gbm
RUN sed -i 's/Components: main/Components: main contrib/' /etc/apt/sources.list.d/debian.sources && apt-get update && apt-get install -y --no-install-recommends bemenu dbus-x11 gosu chromium=${CHROMIUM_VERSION} chromium-sandbox=${CHROMIUM_VERSION} kitty thunar sway swaybg xwayland pulseaudio-utils xdg-desktop-portal xdg-desktop-portal-gtk xdg-utils ca-certificates curl psmisc libnvidia-egl-gbm1 libnvidia-egl-wayland1 && rm -rf /var/lib/apt/lists/* && install -d -m 1777 /var/lib/xkb /tmp/sockets
COPY --from=gow /entrypoint.sh /entrypoint.sh
COPY --from=gow /opt/gow/bash-lib /opt/gow/bash-lib
COPY --from=gow /opt/gow/ensure-groups /opt/gow/ensure-groups
COPY --from=gow /opt/gow/startup.sh /opt/gow/startup.sh
COPY --from=gow /opt/gow/launch-comp.sh /opt/gow/launch-comp.sh
COPY --from=gow /opt/gow/wait-x11 /opt/gow/wait-x11
COPY --from=gow /cfg/waybar /cfg/waybar
COPY --from=gow /etc/cont-init.d/ /etc/cont-init.d/
RUN chmod 0755 /entrypoint.sh /opt/gow/ensure-groups /opt/gow/startup.sh /opt/gow/launch-comp.sh /opt/gow/wait-x11 /etc/cont-init.d/*.sh
COPY --chmod=0755 browser /usr/local/bin/browser
COPY --chmod=0755 status-clock /usr/local/bin/status-clock
COPY --chmod=0755 desktop-session /opt/gow/desktop-session
COPY --chmod=0755 startup-app.sh /opt/gow/startup-app.sh
COPY sway-config /cfg/sway/config
COPY nvidia-egl.json /usr/share/glvnd/egl_vendor.d/10_nvidia.json
LABEL org.opencontainers.image.browser="chromium 152.0.7977.82-1~deb13u1"
ENTRYPOINT ["/entrypoint.sh"]
