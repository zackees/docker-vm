// This is deliberately not noVNC's configurable vnc.html application. It has
// no URL parameters, settings UI, reconnect loop, clipboard UI, file transfer,
// downloads, navigation, or popup surface.
import RFB from './novnc/core/rfb.js';
import { physicalRFB } from './physical-rfb.mjs';
import { startVideo } from './video.mjs';
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

// DOCKER_VM_STATS=1 only: count painted frames and time spent decoding RFB
// updates. Wraps pinned noVNC v1.6.0 internals that are looked up per call.
function countFrames(rfb) {
  // `frames` counts content frames: benchmark pages stamp a frame counter as
  // the colour of the framebuffer's top-left 32x32 block, so each distinct
  // value seen after a paint is one new page frame, however it was split into
  // RFB updates.
  let frames = 0, decodeMs = 0, since = performance.now(), last = -1;
  const display = rfb._display;
  const flip = display.flip.bind(display);
  display.flip = (...args) => {
    const result = flip(...args);
    const [r, g] = display._drawCtx.getImageData(16, 16, 1, 1).data;
    const stamp = (r >> 5) | ((g >> 5) << 3);
    if (stamp !== last) { frames++; last = stamp; }
    return result;
  };
  const update = rfb._framebufferUpdate.bind(rfb);
  rfb._framebufferUpdate = () => {
    const start = performance.now();
    try { return update(); } finally { decodeMs += performance.now() - start; }
  };
  setInterval(() => {
    const now = performance.now();
    window.__TAURI__.core.invoke('report_stats', { frames, decodeMs, seconds: (now - since) / 1000 });
    frames = 0; decodeMs = 0; since = now;
  }, 2000);
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
  const session = await connection();
  const { websocket_url, capability, vnc_password } = session;
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
  const tuning = await window.__TAURI__.core.invoke('rfb_tuning');
  if (tuning.quality !== null) rfb.qualityLevel = tuning.quality;
  if (tuning.compression !== null) rfb.compressionLevel = tuning.compression;
  if (await window.__TAURI__.core.invoke('stats_enabled')) countFrames(rfb);
  const videoEvent = message => window.__TAURI__.core.invoke('video_event', { message }).catch(() => {});
  rfb.addEventListener('connect', async () => {
    document.documentElement.classList.add('connected');
    // NVENC video when offered and decodable; RFB frames otherwise.
    const started = await startVideo(RFB, rfb, session, reason => videoEvent(`stopped, using VNC frames: ${reason}`))
      .catch(error => { videoEvent(`failed to start: ${error.message}`); return false; });
    videoEvent(started ? 'hardware H.264 stream started' : 'not offered or not decodable; using VNC frames');
  }, { once: true });
  rfb.addEventListener('disconnect', event => {
    document.documentElement.classList.remove('connected');
    document.documentElement.classList.add('ended');
    stage.textContent = event.detail.clean ? 'Session ended.' : 'Session lost; refusing reconnect.';
  });
} catch (error) {
  document.documentElement.classList.add('failed');
  if (!stage.textContent.startsWith('Unable')) stage.textContent = `Unable to start the private session: ${error?.message ?? error}`;
}
