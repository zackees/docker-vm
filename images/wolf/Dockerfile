# syntax=docker/dockerfile:1.7
# Keep this in lock-step with compose.yaml's pinned Wolf controller image.
FROM ghcr.io/games-on-whales/wolf@sha256:edb348d7440e5716081830c241e536a17ea8c5b9f5a6275dfda6c775b9a2204f

ARG NVRTC_VERSION=12.2.140~12.2.2-2build1

USER root

# Ubuntu's libnvrtc12 is the CUDA runtime compiler, and its exact dependency
# pulls libnvrtc-builtins12.2.  Wolf's GStreamer CUDA plugin dlopens the
# unversioned SONAME, whereas the runtime package deliberately ships only the
# versioned ABI name (the unversioned development symlink is in a -dev package).
# Provide only that loader alias; do not install a CUDA toolkit or GPU driver.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends "libnvrtc12=${NVRTC_VERSION}"; \
    nvrtc_path=$(ldconfig -p | awk '/libnvrtc\.so\.12 / {print $NF; exit}'); \
    builtins_path=$(ldconfig -p | awk '/libnvrtc-builtins\.so\.12\.2 / {print $NF; exit}'); \
    test -n "$nvrtc_path"; \
    test -n "$builtins_path"; \
    # /usr/local/nvidia is supplied by an NVIDIA CDI mount on some hosts and
    # therefore cannot hold an image-owned alias. /usr/local/lib remains in
    # this image and is searched by ldconfig before GStreamer dlopens NVRTC.
    ln -sfn "$nvrtc_path" /usr/local/lib/libnvrtc.so; \
    ln -sfn "$builtins_path" /usr/local/lib/libnvrtc-builtins.so; \
    ldconfig; \
    rm -rf /var/lib/apt/lists/*

# NVIDIA's CDI mount provides libEGL_nvidia.so.0, but does not provide the
# GLVND vendor manifest on this NixOS host. Without it EGL selects Mesa and
# Wolf cannot initialize the NVIDIA Wayland EGL display.
COPY nvidia-egl.json /usr/share/glvnd/egl_vendor.d/10_nvidia.json

# CDI mounts NixOS external-platform JSON files under /etc that contain host
# store paths unavailable in the container. NVIDIA's EGL loader honors this
# search-path override, selecting the distro JSON with bare SONAMEs instead.
ENV __EGL_EXTERNAL_PLATFORM_CONFIG_DIRS=/usr/share/egl/egl_external_platform.d

# On the reference CDI runtime the NVIDIA GBM backend lives here, while the
# distro GBM directory contains Mesa's dri_gbm only.  Select it explicitly for
# Wolf's DMA-BUF allocator.
ENV GBM_BACKEND=nvidia-drm \
    GBM_BACKENDS_PATH=/usr/local/nvidia/lib/gbm

LABEL org.opencontainers.image.title="docker-vm Wolf controller" \
      org.opencontainers.image.description="Pinned Wolf controller with CUDA NVRTC runtime for GStreamer CUDA conversion" \
      org.opencontainers.image.base.name="ghcr.io/games-on-whales/wolf@sha256:edb348d7440e5716081830c241e536a17ea8c5b9f5a6275dfda6c775b9a2204f" \
      org.opencontainers.image.nvrtc="libnvrtc12 12.2.140~12.2.2-2build1"
