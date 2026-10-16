import { test } from 'node:test';
import assert from 'node:assert/strict';
import { codecFromAccessUnit, FrameReader } from '../viewer/ui/video.mjs';

const sps = [0, 0, 0, 1, 0x67, 0x64, 0x00, 0x33, 0xac, 0x2b];
const idr = [0, 0, 1, 0x65, 0x88, 0x84];

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
