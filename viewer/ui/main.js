// This is deliberately not noVNC's configurable vnc.html application. It has
// no URL parameters, settings UI, reconnect loop, clipboard UI, file transfer,
// downloads, navigation, or popup surface.
import RFB from './novnc/core/rfb.js';
import { physicalRFB } from './physical-rfb.mjs';
import * as Log from './novnc/core/util/logging.js';

Log.initLogging('error');

const status = document.querySelector('#status');
const target = document.querySelector('#screen');

async function connection() {
  // Returns an in-memory, one-use bridge capability.
  // It does not expose Docker, filesystem, shell, or arbitrary network APIs.
  return window.__TAURI__.core.invoke('connection');
}

try {
  // Bound desktop UI DPI independently of physical framebuffer density.
  const passScale = scale => window.__TAURI__.core.invoke('set_display_scale', {
    scale: Math.min(4, Math.max(0.5, Number.isFinite(scale) ? scale : 1)),
  });
  await passScale(window.devicePixelRatio);
  const { websocket_url, capability, vnc_password } = await connection();
  // The capability is a WebSocket subprotocol, never a URL fragment/query,
  // command-line argument, environment variable, or persisted preference.
  const PhysicalRFB = physicalRFB(RFB, window, scale => {
    passScale(scale).catch(() => {
      status.textContent = 'Unable to update desktop DPI.';
      status.style.display = 'block';
    });
  });
  const rfb = new PhysicalRFB(target, websocket_url, {
    credentials: { password: vnc_password },
    shared: false,
    wsProtocols: ['binary', capability],
  });
  rfb.scaleViewport = true;
  // Match physical pixels, including live resizes and display-density changes.
  // Viewport scaling remains a fallback while the server applies the change.
  rfb.resizeSession = true;
  rfb.viewOnly = false;
  rfb.addEventListener('connect', () => document.documentElement.classList.add('connected'));
  rfb.addEventListener('disconnect', event => {
    document.documentElement.classList.remove('connected');
    status.textContent = event.detail.clean ? 'Session ended.' : 'Session lost; refusing reconnect.';
  });
} catch (_) {
  status.textContent = 'Unable to start the private session.';
}
