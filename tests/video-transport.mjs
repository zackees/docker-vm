import { test } from 'node:test';
import assert from 'node:assert/strict';
import { codecFromAccessUnit, FrameReader, startNativeVideo } from '../viewer/ui/video.mjs';

const sps = [0, 0, 0, 1, 0x67, 0x64, 0x00, 0x33, 0xac, 0x2b];
const idr = [0, 0, 1, 0x65, 0x88, 0x84];

test('native video keeps a small RFB request pending for resize acknowledgements', async () => {
  const saved = { ResizeObserver: globalThis.ResizeObserver, window: globalThis.window,
    setInterval: globalThis.setInterval, clearInterval: globalThis.clearInterval };
  let poll;
  let disconnect;
  const requests = [];
  const continuous = [];
  const sock = {};
  const RFB = { messages: {
    fbUpdateRequest: (...args) => requests.push(args),
    enableContinuousUpdates: (...args) => continuous.push(args),
  } };
  const rfb = {
    _sock: sock, _fbWidth: 1280, _fbHeight: 800,
    _canvas: { getBoundingClientRect: () => ({ left: 0, top: 0, width: 1280, height: 800 }) },
    _updateContinuousUpdates: () => RFB.messages.enableContinuousUpdates(sock, true, 0, 0, 1280, 800),
    addEventListener: (_, callback) => { disconnect = callback; },
  };
  try {
    globalThis.ResizeObserver = class { observe() {} disconnect() {} };
    globalThis.window = { addEventListener() {}, removeEventListener() {} };
    globalThis.setInterval = callback => { poll = callback; return 1; };
    globalThis.clearInterval = () => {};
    const invoke = async command => command === 'native_video_start' ? true
      : command === 'native_video_state' ? { state: 'live', frames: 1 } : undefined;
    assert.equal(await startNativeVideo(RFB, rfb, invoke), true);
    await poll();
    assert.equal(continuous.at(-1)[1], false, 'full-frame continuous updates should be disabled');
    assert.deepEqual(requests.at(-1), [sock, true, 0, 0, 1, 1],
      'the server still needs a pending request to deliver resize/control rectangles');
    // noVNC requests the next update after processing a framebuffer response.
    RFB.messages.fbUpdateRequest(sock, true, 0, 0, 1920, 1080);
    assert.deepEqual(requests.at(-1), [sock, true, 0, 0, 1, 1]);
    disconnect();
    assert.deepEqual(requests.at(-1), [sock, false, 0, 0, 1280, 800],
      'stopping native video must restore the full RFB framebuffer');
  } finally {
    for (const [name, value] of Object.entries(saved)) {
      if (value === undefined) delete globalThis[name];
      else globalThis[name] = value;
    }
  }
});

test('codec string comes from the SPS profile, constraints and level', () => {
  assert.equal(codecFromAccessUnit(Uint8Array.from([...sps, ...idr])), 'avc1.640033');
  assert.equal(codecFromAccessUnit(Uint8Array.from([0, 0, 1, 0x67, 0x42, 0xc0, 0x1f, 0, 0])), 'avc1.42c01f');
  assert.equal(codecFromAccessUnit(Uint8Array.from(idr)), null);
});

function framed(keyframe, timestamp, payload) {
  const bytes = new Uint8Array(13 + payload.length);
  const view = new DataView(bytes.buffer);
  view.setUint32(0, payload.length);
  view.setUint8(4, keyframe ? 1 : 0);
  view.setBigUint64(5, BigInt(timestamp));
  bytes.set(payload, 13);
  return bytes;
}

test('frames reassemble across arbitrary chunk boundaries', () => {
  const stream = Uint8Array.from([...framed(true, 1000, [1, 2, 3]), ...framed(false, 2000, [4, 5])]);
  const pieces = new FrameReader();
  const all = [];
  for (let i = 0; i < stream.length; i += 4) all.push(...pieces.push(stream.subarray(i, i + 4)));
  assert.deepEqual(all.map(f => [f.keyframe, f.timestamp, [...f.data]]), [[true, 1000, [1, 2, 3]], [false, 2000, [4, 5]]]);
});

test('Annex-B access units become an avcC description and length-prefixed samples', async () => {
  const { splitNals, avcDescription, toAvc } = await import('../viewer/ui/video.mjs');
  const sps = [0x67, 0x42, 0xc0, 0x33, 0xaa];
  const pps = [0x68, 0xce, 0x3c];
  const slice = [0x65, 0x88, 0x84, 0x00, 0x21];
  // 4- and 3-byte start codes; slice data that contains a zero byte.
  const unit = Uint8Array.from([0, 0, 0, 1, ...sps, 0, 0, 1, ...pps, 0, 0, 0, 1, ...slice]);
  const nals = splitNals(unit);
  assert.deepEqual(nals.map(nal => [...nal]), [sps, pps, slice]);
  assert.deepEqual([...avcDescription(nals)], [1, 0x42, 0xc0, 0x33, 0xff, 0xe1, 0, 5, ...sps, 1, 0, 3, ...pps]);
  assert.deepEqual([...toAvc(nals)], [0, 0, 0, 5, ...sps, 0, 0, 0, 3, ...pps, 0, 0, 0, 5, ...slice]);
  assert.equal(avcDescription(splitNals(Uint8Array.from([0, 0, 1, ...slice]))), null);
});

test('NV12 pixel sampling inverts BT.601 limited range', async () => {
  const { nv12Pixel } = await import('../viewer/ui/video.mjs');
  // 2x2 frame: Y plane then interleaved UV. Pure red is Y=81, U=90, V=240.
  const bytes = Uint8Array.from([81, 81, 81, 81, 90, 240]);
  const layout = [{ offset: 0, stride: 2 }, { offset: 4, stride: 2 }];
  const [r, g, b] = nv12Pixel({ bytes, layout }, 1, 1);
  assert.ok(r > 245 && g < 10 && b < 10, `${r},${g},${b}`);
});
