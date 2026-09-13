// Integration adapter for pinned noVNC v1.6.0. Keep its CSS layout and input
// transforms untouched; only SetDesktopSize uses physical pixels. Revalidate
// these private hooks and tests/physical-pixels.mjs when updating noVNC.
export const MAX_PIXELS = 3840 * 2160; // 4K; ~32 MiB per RGBA buffer.
export const MAX_DIMENSION = 8192;

export function physicalSize({ w, h }, density) {
  if (![w, h, density].every(Number.isFinite) || w <= 0 || h <= 0 || density <= 0) return null;
  const width = w * density;
  const height = h * density;
  if (!Number.isFinite(width) || !Number.isFinite(height)) return null;
  const factor = Math.min(1, MAX_DIMENSION / width, MAX_DIMENSION / height,
    Math.sqrt(MAX_PIXELS / width / height));
  // Round normally (<= 0.5 physical pixel error). Floor capped sizes to stay
  // inside the memory budget; retain aspect ratio for fallback scaling.
  const round = factor < 1 ? Math.floor : Math.round;
  const result = { w: round(width * factor), h: round(height * factor) };
  return result.w >= 32 && result.h >= 32 ? result : null;
}

export function physicalRFB(RFB, environment = window, densityChanged = () => {}) {
  return class PhysicalRFB extends RFB {
    constructor(...args) {
      super(...args);
      let query;
      const watch = () => {
        query?.removeEventListener('change', changed);
        query = environment.matchMedia(`(resolution: ${environment.devicePixelRatio}dppx)`);
        query.addEventListener('change', changed);
      };
      const changed = () => {
        watch(); // A new query is needed after each monitor-density change.
        densityChanged(environment.devicePixelRatio);
        this._requestRemoteResize();
      };
      watch();
      this.addEventListener('disconnect', () => {
        query.removeEventListener('change', changed);
        clearTimeout(this._resizeTimeout);
      }, { once: true });
    }

    _requestRemoteResize() {
      if (!this._resizeSession || this._viewOnly || !this._supportsSetDesktopSize ||
          this._rfbConnectionState !== 'connected' || this._pendingRemoteResize) return;
      const size = physicalSize(this._screenSize(), environment.devicePixelRatio);
      if (!size || (size.w === this._fbWidth && size.h === this._fbHeight)) return;

      // Preserve upstream's one-in-flight and 100ms rate limit. Its extended
      // desktop-size acknowledgement handler retries the latest target on
      // success, but does not loop on a refusal. Re-evaluate density and size
      // in the timer, rather than sending a stale captured target.
      const elapsed = Date.now() - this._lastResize;
      if (elapsed < 100) {
        clearTimeout(this._resizeTimeout);
        this._resizeTimeout = setTimeout(() => this._requestRemoteResize(), 100 - elapsed);
        return;
      }
      this._resizeTimeout = null;
      this._pendingRemoteResize = true;
      this._lastResize = Date.now();
      RFB.messages.setDesktopSize(this._sock, size.w, size.h, this._screenID, this._screenFlags);
    }
  };
}
