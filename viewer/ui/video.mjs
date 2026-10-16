// Hardware video transport: H.264 access units from the container's NVENC
// service, decoded with WebCodecs and drawn into noVNC's display. While video
// is live, RFB framebuffer updates are confined to one pixel, so Xvnc stops
// encoding frames; keyboard, pointer, cursor and resize stay on RFB. Any
// failure restores full RFB updates. Wraps pinned noVNC v1.6.0 internals.

const HEADER_BYTES = 13; // u32 length | u8 flags | u64 pts (us), big-endian
const KEYFRAME = 1;
// WebKit's GStreamer decoder keeps several frames in flight; only a real
// backlog (~0.5 s at 60 fps) means skipping ahead to the next keyframe.
const MAX_DECODE_QUEUE = 30;

const NAL_SPS = 7;
const NAL_PPS = 8;

// NAL units of an Annex-B access unit, without start codes.
export function splitNals(bytes) {
  const starts = [];
  for (let i = 0; i + 2 < bytes.length; i++) {
    if (bytes[i] === 0 && bytes[i + 1] === 0 && bytes[i + 2] === 1) {
      starts.push({ code: i > 0 && bytes[i - 1] === 0 ? i - 1 : i, payload: i + 3 });
      i += 2;
    }
  }
  return starts.map((start, index) => bytes.subarray(start.payload, starts[index + 1]?.code ?? bytes.length));
}

// Codec string from the SPS's profile, constraint flags and level.
export function codecFromNals(nals) {
  const sps = nals.find(nal => (nal[0] & 0x1f) === NAL_SPS);
  if (!sps || sps.length < 4) return null;
  const hex = value => value.toString(16).padStart(2, '0');
  return `avc1.${hex(sps[1])}${hex(sps[2])}${hex(sps[3])}`;
}

export const codecFromAccessUnit = bytes => codecFromNals(splitNals(bytes));

// avcC decoder configuration record (ISO 14496-15) from the SPS and PPS.
// WebKitGTK's WebCodecs decodes AVC (length-prefixed) only, never Annex-B.
export function avcDescription(nals) {
  const sps = nals.find(nal => (nal[0] & 0x1f) === NAL_SPS);
  const pps = nals.find(nal => (nal[0] & 0x1f) === NAL_PPS);
  if (!sps || !pps) return null;
  const record = new Uint8Array(11 + sps.length + pps.length);
  record.set([1, sps[1], sps[2], sps[3], 0xff, 0xe1, sps.length >> 8, sps.length & 0xff]);
  record.set(sps, 8);
  record.set([1, pps.length >> 8, pps.length & 0xff], 8 + sps.length);
  record.set(pps, 11 + sps.length);
  return record;
}

// The access unit as 4-byte length-prefixed NAL units.
export function toAvc(nals) {
  const sample = new Uint8Array(nals.reduce((total, nal) => total + 4 + nal.length, 0));
  const view = new DataView(sample.buffer);
  let offset = 0;
  for (const nal of nals) {
    view.setUint32(offset, nal.length);
    sample.set(nal, offset + 4);
    offset += 4 + nal.length;
  }
  return sample;
}

const sameBytes = (a, b) => a.length === b.length && a.every((value, index) => value === b[index]);

// Reassembles length-framed access units from arbitrary WebSocket chunks.
export class FrameReader {
  constructor() { this.buffer = new Uint8Array(0); }

  push(chunk) {
    const merged = new Uint8Array(this.buffer.length + chunk.length);
    merged.set(this.buffer);
    merged.set(chunk, this.buffer.length);
    this.buffer = merged;
    const frames = [];
    while (this.buffer.length >= HEADER_BYTES) {
      const view = new DataView(this.buffer.buffer, this.buffer.byteOffset);
      const length = view.getUint32(0);
      if (this.buffer.length < HEADER_BYTES + length) break;
      frames.push({
        keyframe: (view.getUint8(4) & KEYFRAME) !== 0,
        timestamp: Number(view.getBigUint64(5)),
        data: this.buffer.slice(HEADER_BYTES, HEADER_BYTES + length),
      });
      this.buffer = this.buffer.slice(HEADER_BYTES + length);
    }
    return frames;
  }
}

const VERTEX_SHADER = `#version 300 es
in vec2 position;
out vec2 uv;
void main() {
  uv = vec2((position.x + 1.0) / 2.0, (1.0 - position.y) / 2.0);
  gl_Position = vec4(position, 0.0, 1.0);
}`;

// BT.601 limited-range NV12 to RGB, which is what nvh264enc produces.
const FRAGMENT_SHADER = `#version 300 es
precision mediump float;
in vec2 uv;
uniform sampler2D luma;
uniform sampler2D chroma;
out vec4 color;
void main() {
  float y = 1.1643 * (texture(luma, uv).r - 0.0625);
  vec2 c = texture(chroma, uv).rg - 0.5;
  color = vec4(y + 1.5958 * c.y, y - 0.39173 * c.x - 0.81290 * c.y, y + 2.017 * c.x, 1.0);
}`;

// Paints NV12 VideoFrames with WebGL: the planes are copied out once and the
// colour conversion runs in a shader. WebKitGTK's drawImage/texImage2D of a
// VideoFrame converts on the CPU instead (~50 ms per 2560x1600 frame).
export class Nv12Presenter {
  constructor(screen, target) {
    this.screen = screen;
    this.target = target;
    this.canvas = document.createElement('canvas');
    // Input still lands on noVNC's canvas underneath.
    Object.assign(this.canvas.style, { position: 'absolute', pointerEvents: 'none' });
    const gl = this.canvas.getContext('webgl2', { alpha: false, antialias: false, desynchronized: true });
    if (!gl) throw new Error('WebGL2 unavailable');
    this.gl = gl;
    const program = gl.createProgram();
    for (const [type, source] of [[gl.VERTEX_SHADER, VERTEX_SHADER], [gl.FRAGMENT_SHADER, FRAGMENT_SHADER]]) {
      const shader = gl.createShader(type);
      gl.shaderSource(shader, source);
      gl.compileShader(shader);
      if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(shader));
      gl.attachShader(program, shader);
    }
    gl.bindAttribLocation(program, 0, 'position');
    gl.linkProgram(program);
    gl.useProgram(program);
    gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    this.textures = [0, 1].map(unit => {
      const texture = gl.createTexture();
      gl.activeTexture(gl.TEXTURE0 + unit);
      gl.bindTexture(gl.TEXTURE_2D, texture);
      for (const key of [gl.TEXTURE_MIN_FILTER, gl.TEXTURE_MAG_FILTER]) gl.texParameteri(gl.TEXTURE_2D, key, gl.LINEAR);
      for (const key of [gl.TEXTURE_WRAP_S, gl.TEXTURE_WRAP_T]) gl.texParameteri(gl.TEXTURE_2D, key, gl.CLAMP_TO_EDGE);
      return texture;
    });
    gl.uniform1i(gl.getUniformLocation(program, 'luma'), 0);
    gl.uniform1i(gl.getUniformLocation(program, 'chroma'), 1);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    this.buffer = new Uint8Array(0);
  }

  // Copies the frame's planes into textures and draws. Returns the frame's
  // NV12 bytes and layout so callers can sample it (stats only).
  async present(frame) {
    if (frame.format !== 'NV12') throw new Error(`unexpected frame format ${frame.format}`);
    const size = frame.allocationSize();
    if (this.buffer.length !== size) this.buffer = new Uint8Array(size);
    const layout = await frame.copyTo(this.buffer);
    const { codedWidth: w, codedHeight: h } = frame;
    const { gl } = this;
    if (this.canvas.width !== w || this.canvas.height !== h) {
      this.canvas.width = w;
      this.canvas.height = h;
      gl.viewport(0, 0, w, h);
    }
    gl.activeTexture(gl.TEXTURE0);
    gl.pixelStorei(gl.UNPACK_ROW_LENGTH, layout[0].stride);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.R8, w, h, 0, gl.RED, gl.UNSIGNED_BYTE, this.buffer, layout[0].offset);
    gl.activeTexture(gl.TEXTURE1);
    gl.pixelStorei(gl.UNPACK_ROW_LENGTH, layout[1].stride / 2);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RG8, w / 2, h / 2, 0, gl.RG, gl.UNSIGNED_BYTE, this.buffer, layout[1].offset);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    this.align();
    return { bytes: this.buffer, layout, width: w };
  }

  // Track noVNC's canvas box (it is centred and CSS-scaled in its screen).
  align() {
    const { target, canvas } = this;
    if (!canvas.parentNode) {
      this.screen.style.position = 'relative';
      this.screen.appendChild(canvas);
      target.style.opacity = '0';
    }
    canvas.style.left = `${target.offsetLeft}px`;
    canvas.style.top = `${target.offsetTop}px`;
    canvas.style.width = `${target.clientWidth}px`;
    canvas.style.height = `${target.clientHeight}px`;
  }

  remove() {
    this.target.style.opacity = '';
    this.canvas.remove();
    this.gl.getExtension('WEBGL_lose_context')?.loseContext();
  }
}

// RGB of one pixel of an NV12 frame (BT.601 limited range), for the stats
// frame-stamp counter.
export function nv12Pixel({ bytes, layout }, x, y) {
  const luma = 1.1643 * (bytes[layout[0].offset + y * layout[0].stride + x] - 16);
  const chroma = layout[1].offset + (y >> 1) * layout[1].stride + (x & ~1);
  const u = bytes[chroma] - 128, v = bytes[chroma + 1] - 128;
  const clamp = value => Math.max(0, Math.min(255, Math.round(value)));
  return [clamp(luma + 1.5958 * v), clamp(luma - 0.39173 * u - 0.81290 * v), clamp(luma + 2.017 * u)];
}

// While `active()` is true, RFB carries no frames: update requests are not
// sent and continuous updates are switched off. (Confining them to one pixel
// is not enough: wayvnc ignores the region and keeps encoding full frames.)
// Input, resize and the cursor need no framebuffer updates; wayvnc draws the
// cursor into the video frames.
function quietRfb(RFB, active) {
  const messages = RFB.messages;
  const request = messages.fbUpdateRequest;
  const continuous = messages.enableContinuousUpdates;
  messages.fbUpdateRequest = (...args) => { if (!active()) request(...args); };
  messages.enableContinuousUpdates = (sock, enable, x, y, w, h) =>
    continuous(sock, active() ? false : enable, x, y, w, h);
  return () => { messages.fbUpdateRequest = request; messages.enableContinuousUpdates = continuous; };
}

// Native presentation: the viewer process decodes and draws the stream on
// the GPU in a layer over noVNC's canvas (no CPU copy per frame, which the
// webview path cannot avoid). This only places that layer, quiets RFB while
// it shows frames, and restores RFB if it stops. Returns false when native
// presentation is unavailable, so the caller can use startVideo instead.
export async function startNativeVideo(RFB, rfb, invoke, onStop = () => {}, onFrames = null) {
  if (!(await invoke('native_video_start'))) return false;
  let live = false;
  let stopped = false;
  let frames = 0;
  const restore = quietRfb(RFB, () => live);
  const place = () => {
    const box = rfb._canvas.getBoundingClientRect();
    invoke('native_video_place', { x: Math.round(box.left), y: Math.round(box.top),
                                   width: Math.round(box.width), height: Math.round(box.height) });
  };
  const observer = new ResizeObserver(place);
  observer.observe(rfb._canvas);
  window.addEventListener('resize', place);
  place();
  const stop = reason => {
    if (stopped) return;
    stopped = true;
    live = false;
    clearInterval(timer);
    observer.disconnect();
    window.removeEventListener('resize', place);
    restore();
    RFB.messages.fbUpdateRequest(rfb._sock, false, 0, 0, rfb._fbWidth, rfb._fbHeight);
    rfb._updateContinuousUpdates();
    onStop(reason);
  };
  const timer = setInterval(async () => {
    const state = await invoke('native_video_state').catch(() => ({ state: 'failed', reason: 'state unavailable' }));
    if (state.state === 'live') {
      onFrames?.(state.frames - frames);
      frames = state.frames;
      if (!live) {
        live = true;
        place();
        rfb._updateContinuousUpdates();
      }
    } else if (state.state === 'failed' || state.state === 'off') {
      stop(state.reason ?? state.state);
    }
  }, 250);
  rfb.addEventListener('disconnect', () => stop('session ended'), { once: true });
  return true;
}

// Returns false when hardware video is not offered or not decodable here.
export async function startVideo(RFB, rfb, { video_url: url, video_capability: capability }, onStop = () => {}, onFrame = null) {
  if (!url || !capability || typeof VideoDecoder === 'undefined') return false;
  let live = false;
  let stopped = false;
  // Per-stage counters, reported by onFrame's owner in stats mode.
  const counts = { received: 0, decoded: 0, presented: 0, skipped: 0, presentMs: 0 };
  startVideo.counts = counts;
  let codec = null;
  let description = null;
  let waitingForKeyframe = true;
  const restore = quietRfb(RFB, () => live);
  let presenter;
  try {
    presenter = new Nv12Presenter(rfb._screen, rfb._canvas);
  } catch (error) {
    restore();
    onStop(`no WebGL presenter: ${error.message}`);
    return false;
  }
  // Frames are presented in order, one at a time.
  let painting = Promise.resolve();

  const decoder = new VideoDecoder({
    output: frame => {
      counts.decoded++;
      painting = painting.then(async () => {
        try {
          if (stopped) return;
          const { displayWidth: w, displayHeight: h } = frame;
          if (w !== rfb._fbWidth || h !== rfb._fbHeight) return; // mid-resize
          const started = performance.now();
          const planes = await presenter.present(frame);
          counts.presentMs += performance.now() - started;
          counts.presented++;
          onFrame?.(planes);
          if (!live) {
            live = true;
            // Re-issue the standing update request at one pixel.
            rfb._updateContinuousUpdates();
          }
        } catch (error) {
          stop(`present: ${error.message}`);
        } finally {
          frame.close();
        }
      });
    },
    error: error => stop(`decoder: ${error.message}`),
  });

  const socket = new WebSocket(url, ['binary', capability]);
  socket.binaryType = 'arraybuffer';
  const reader = new FrameReader();

  function stop(reason) {
    if (stopped) return;
    stopped = true;
    live = false;
    restore();
    try { socket.close(); } catch (_) { /* already closed */ }
    if (decoder.state !== 'closed') decoder.close();
    presenter.remove();
    // Repaint everything from RFB.
    RFB.messages.fbUpdateRequest(rfb._sock, false, 0, 0, rfb._fbWidth, rfb._fbHeight);
    rfb._updateContinuousUpdates();
    onStop(reason);
  }

  async function decode(frame) {
    const nals = splitNals(frame.data);
    if (frame.keyframe) {
      const next = codecFromNals(nals);
      const nextDescription = avcDescription(nals);
      if (next && nextDescription && (next !== codec || !sameBytes(nextDescription, description))) {
        const config = { codec: next, description: nextDescription, optimizeForLatency: true,
                         // Software decode cannot keep up at 2560x1600 (~6 fps on
                         // a loaded host); NVDEC decodes ~100 fps and the WebGL
                         // presenter needs ~11 ms per frame.
                         hardwareAcceleration: 'prefer-hardware' };
        const { supported } = await VideoDecoder.isConfigSupported(config).catch(() => ({ supported: false }));
        if (!supported) return stop(`unsupported codec ${next}`);
        decoder.configure(config);
        codec = next;
        description = nextDescription;
      }
      waitingForKeyframe = !codec;
    }
    if (waitingForKeyframe || stopped) return;
    if (decoder.decodeQueueSize > MAX_DECODE_QUEUE) {
      // Behind: skip to the next keyframe rather than build latency.
      waitingForKeyframe = true;
      counts.skipped++;
      return;
    }
    decoder.decode(new EncodedVideoChunk({
      type: frame.keyframe ? 'key' : 'delta',
      timestamp: frame.timestamp,
      data: toAvc(nals),
    }));
  }

  let queue = Promise.resolve();
  socket.addEventListener('message', event => {
    for (const frame of reader.push(new Uint8Array(event.data))) {
      counts.received++;
      queue = queue.then(() => decode(frame)).catch(error => stop(`decode: ${error.message}`));
    }
  });
  socket.addEventListener('close', () => stop('video stream closed'));
  socket.addEventListener('error', () => stop('video stream error'));
  rfb.addEventListener('disconnect', () => stop('session ended'), { once: true });
  return true;
}
