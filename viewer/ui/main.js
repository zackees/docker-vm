// This is deliberately not noVNC's configurable vnc.html application. It has
// no URL parameters, settings UI, reconnect loop, clipboard UI, file transfer,
// downloads, navigation, or popup surface.
import RFB from './novnc/core/rfb.js';
import * as Log from './novnc/core/util/logging.js';

Log.initLogging('error');

const status = document.querySelector('#status');
const target = document.querySelector('#screen');

async function connection() {
  // The sole Tauri command returns an in-memory, one-use bridge capability.
  // It does not expose Docker, filesystem, shell, or arbitrary network APIs.
  return window.__TAURI__.core.invoke('connection');
}

try {
  const { websocket_url, capability, vnc_password } = await connection();
  // The capability is a WebSocket subprotocol, never a URL fragment/query,
  // command-line argument, environment variable, or persisted preference.
  const rfb = new RFB(target, websocket_url, {
    credentials: { password: vnc_password },
    shared: false,
    wsProtocols: ['binary', capability],
  });
  rfb.scaleViewport = true;
  rfb.resizeSession = false;
  rfb.viewOnly = false;
  rfb.addEventListener('connect', () => document.documentElement.classList.add('connected'));
  rfb.addEventListener('disconnect', event => {
    status.textContent = event.detail.clean ? 'Session ended.' : 'Session lost; refusing reconnect.';
  });
} catch (_) {
  status.textContent = 'Unable to start the private session.';
}
