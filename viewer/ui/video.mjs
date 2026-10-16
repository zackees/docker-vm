// Hardware video transport: H.264 access units from the container's NVENC
// service, decoded with WebCodecs and drawn into noVNC's display. While video
// is live, RFB framebuffer updates are confined to one pixel, so Xvnc stops
// encoding frames; keyboard, pointer, cursor and resize stay on RFB. Any
// failure restores full RFB updates. Wraps pinned noVNC v1.6.0 internals.

const HEADER_BYTES = 13; // u32 length | u8 flags | u64 pts (us), big-endian
const KEYFRAME = 1;
const MAX_DECODE_QUEUE = 3;

// Codec string for an Annex-B access unit, from its SPS (NAL type 7).
export function codecFromAccessUnit(bytes) {
  for (let i = 0; i + 7 < bytes.length; i++) {
    const startCode = bytes[i] === 0 && bytes[i + 1] === 0 &&
      (bytes[i + 2] === 1 || (bytes[i + 2] === 0 && bytes[i + 3] === 1));
    if (!startCode) continue;
    const nal = i + (bytes[i + 2] === 1 ? 3 : 4);
    if ((bytes[nal] & 0x1f) === 7) {
      const hex = value => value.toString(16).padStart(2, '0');
      return `avc1.${hex(bytes[nal + 1])}${hex(bytes[nal + 2])}${hex(bytes[nal + 3])}`;
    }
  }
  return null;
}

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

// Confines RFB update requests to one pixel while `active()` is true.
function quietRfb(RFB, active) {
  const messages = RFB.messages;
  const request = messages.fbUpdateRequest;
  const continuous = messages.enableContinuousUpdates;
  messages.fbUpdateRequest = (sock, incremental, x, y, w, h) =>
    active() ? request(sock, incremental, 0, 0, 1, 1) : request(sock, incremental, x, y, w, h);
  messages.enableContinuousUpdates = (sock, enable, x, y, w, h) =>
    active() ? continuous(sock, enable, 0, 0, 1, 1) : continuous(sock, enable, x, y, w, h);
  return () => { messages.fbUpdateRequest = request; messages.enableContinuousUpdates = continuous; };
}

// Returns false when hardware video is not offered or not decodable here.
export async function startVideo(RFB, rfb, { video_url: url, video_capability: capability }, onStop = () => {}) {
  if (!url || !capability || typeof VideoDecoder === 'undefined') return false;
  let live = false;
  let stopped = false;
  let codec = null;
  let waitingForKeyframe = true;
  const restore = quietRfb(RFB, () => live);
  const display = rfb._display;

  const decoder = new VideoDecoder({
    output: frame => {
      try {
        if (stopped) return;
        const { displayWidth: w, displayHeight: h } = frame;
        if (w !== rfb._fbWidth || h !== rfb._fbHeight) return; // mid-resize
        display._drawCtx.drawImage(frame, 0, 0);
        display._damage(0, 0, w, h);
        display.flip();
        if (!live) {
          live = true;
          // Re-issue the standing update request at one pixel.
          rfb._updateContinuousUpdates();
        }
      } finally {
        frame.close();
      }
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
    // Repaint everything from RFB.
    RFB.messages.fbUpdateRequest(rfb._sock, false, 0, 0, rfb._fbWidth, rfb._fbHeight);
    rfb._updateContinuousUpdates();
    onStop(reason);
  }

  async function decode(frame) {
    if (frame.keyframe) {
      const next = codecFromAccessUnit(frame.data);
      if (next && next !== codec) {
        const config = { codec: next, optimizeForLatency: true, hardwareAcceleration: 'prefer-hardware' };
        const { supported } = await VideoDecoder.isConfigSupported(config).catch(() => ({ supported: false }));
        if (!supported) return stop(`unsupported codec ${next}`);
        decoder.configure(config);
        codec = next;
      }
      waitingForKeyframe = false;
    }
    if (waitingForKeyframe || stopped) return;
    if (decoder.decodeQueueSize > MAX_DECODE_QUEUE) {
      // Behind: skip to the next keyframe rather than build latency.
      waitingForKeyframe = true;
      return;
    }
    decoder.decode(new EncodedVideoChunk({
      type: frame.keyframe ? 'key' : 'delta',
      timestamp: frame.timestamp,
      data: frame.data,
    }));
  }

  let queue = Promise.resolve();
  socket.addEventListener('message', event => {
    for (const frame of reader.push(new Uint8Array(event.data))) {
      queue = queue.then(() => decode(frame)).catch(error => stop(`decode: ${error.message}`));
    }
  });
  socket.addEventListener('close', () => stop('video stream closed'));
  socket.addEventListener('error', () => stop('video stream error'));
  rfb.addEventListener('disconnect', () => stop('session ended'), { once: true });
  return true;
}
