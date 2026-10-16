// This is deliberately not noVNC's configurable vnc.html application. It has
// no URL parameters, settings UI, reconnect loop, clipboard UI, file transfer,
// downloads, navigation, or popup surface.
import RFB from './novnc/core/rfb.js';
import { physicalRFB } from './physical-rfb.mjs';
import * as Log from './novnc/core/util/logging.js';

Log.initLogging('error');

window.__viewerLoaded = true;
const status = document.querySelector('#status');
const stage = document.querySelector('#stage');
const log = document.querySelector('#log');
const target = document.querySelector('#screen');

// Docker work runs behind the window; show each step and the latest build or
// compose output until the session is ready or has failed.
async function ready() {
  for (;;) {
    const current = await window.__TAURI__.core.invoke('status');
    if (current.state === 'ready') return;
    stage.textContent = current.state === 'failed' ? `Unable to start the private session: ${current.error}` : current.stage;
    log.textContent = current.log.join('\n');
    if (current.state === 'failed') throw new Error(current.error);
    await new Promise(resolve => setTimeout(resolve, 250));
  }
}

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
  await ready();
  stage.textContent = 'Connecting to the desktop…';
  log.textContent = '';
  const { websocket_url, capability, vnc_password } = await connection();
  // The capability is a WebSocket subprotocol, never a URL fragment/query,
  // command-line argument, environment variable, or persisted preference.
  const PhysicalRFB = physicalRFB(RFB, window, scale => {
    passScale(scale).catch(() => {
      stage.textContent = 'Unable to update desktop DPI.';
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
    document.documentElement.classList.add('ended');
    stage.textContent = event.detail.clean ? 'Session ended.' : 'Session lost; refusing reconnect.';
  });
} catch (error) {
  document.documentElement.classList.add('failed');
  if (!stage.textContent.startsWith('Unable')) stage.textContent = `Unable to start the private session: ${error?.message ?? error}`;
}
