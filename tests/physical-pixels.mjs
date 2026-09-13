// Issue #3: run with node --test tests/physical-pixels.mjs (Node 22+).
import assert from 'node:assert/strict';
import { readFileSync, existsSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

// Execute the pinned upstream methods, not a reimplementation of its sizing.
const source = readFileSync(new URL('../viewer/ui/novnc/core/rfb.js', import.meta.url), 'utf8');
function method(name, code = source) {
  const start = code.indexOf(`    ${name}(`);
  assert.ok(start >= 0);
  const end = code.indexOf('\n    }', start) + 6;
  return code.slice(start, end);
}
const requests = [];
const queries = new Set();
const environment = { devicePixelRatio: 1, matchMedia: () => {
  const query = new EventTarget();
  queries.add(query);
  return query;
} };
const Upstream = vm.runInNewContext(`(class RFB extends EventTarget {
  ${method('_screenSize')}
  ${method('_requestRemoteResize')}
  ${method('_handleExtendedDesktopSize')}
  static messages = { setDesktopSize: (...args) => requests.push(args.slice(1, 3)) };
})`, { EventTarget, requests, Log: { Debug() {}, Warn() {} }, Date, setTimeout, clearTimeout });
const adapter = new URL('../viewer/ui/physical-rfb.mjs', import.meta.url);
const RFB = existsSync(adapter) ? (await import(adapter)).physicalRFB(Upstream, environment) : Upstream;
function client(w, h) {
  const rfb = new RFB();
  Object.assign(rfb, {
    _screen: { getBoundingClientRect: () => ({ width: w, height: h }) },
    _resizeSession: true, _viewOnly: false, _supportsSetDesktopSize: true,
    _rfbConnectionState: 'connected',
    _pendingRemoteResize: false, _lastResize: 0, _fbWidth: 1280, _fbHeight: 800,
    _sock: { rQwait: () => false, rQpeek8: () => 1, rQskipBytes() {}, rQshift32: () => 0 },
    _resize(width, height) { this._fbWidth = width; this._fbHeight = height; },
  });
  return rfb;
}

function acknowledge(rfb, width, height, status = 0, reason = 1) {
  rfb._FBU = { x: reason, y: status, width, height };
  assert.equal(rfb._handleExtendedDesktopSize(), true);
}

test('initial capability, pending request, resize/maximize/restore and ack converge', () => {
  environment.devicePixelRatio = 1.75;
  requests.length = 0;
  const rfb = client(1000, 600);
  rfb._supportsSetDesktopSize = false;
  rfb._requestRemoteResize();
  assert.equal(requests.length, 0);
  acknowledge(rfb, 1280, 800, 0, 0);
  assert.equal(requests.length, 1);
  rfb._requestRemoteResize();
  assert.equal(requests.length, 1);
  for (const [w, h] of [[1100, 700], [1400, 780], [1000, 600]]) {
    rfb._screen.getBoundingClientRect = () => ({ width: w, height: h });
    rfb._lastResize = 0;
    acknowledge(rfb, ...requests.at(-1));
    assert.equal(requests.at(-1)[0], Math.round(w * 1.75));
  }
  const count = requests.length;
  acknowledge(rfb, ...requests.at(-1));
  rfb._requestRemoteResize();
  assert.equal(requests.length, count);
  rfb.dispatchEvent(new Event('disconnect'));
});

test('DPI-only changes rearm the media query and listeners stop on disconnect', () => {
  environment.devicePixelRatio = 1.25;
  requests.length = 0;
  const rfb = client(1000, 600);
  for (const dpr of [1.5, 1.75, 2, 1]) {
    const query = [...queries].at(-1);
    environment.devicePixelRatio = dpr;
    rfb._lastResize = 0;
    query.dispatchEvent(new Event('change'));
    assert.equal(requests.at(-1)[0], 1000 * dpr);
    acknowledge(rfb, ...requests.at(-1));
    assert.notEqual([...queries].at(-1), query);
  }
  const count = requests.length;
  rfb.dispatchEvent(new Event('disconnect'));
  environment.devicePixelRatio = 2;
  [...queries].at(-1).dispatchEvent(new Event('change'));
  assert.equal(requests.length, count);
});

test('refusal does not loop; disconnected, disabled, view-only and unsupported do not send', () => {
  requests.length = 0;
  const rfb = client(1000, 600);
  rfb._requestRemoteResize();
  acknowledge(rfb, 1280, 800, 1);
  assert.equal(requests.length, 1);
  assert.equal(rfb._fbWidth, 1280);
  for (const [key, value] of [['_resizeSession', false], ['_viewOnly', true],
    ['_supportsSetDesktopSize', false], ['_rfbConnectionState', 'disconnected']]) {
    const previous = rfb[key];
    rfb[key] = value;
    rfb._requestRemoteResize();
    assert.equal(requests.length, 1);
    rfb[key] = previous;
  }
  rfb.dispatchEvent(new Event('disconnect'));
});

test('rate-limited timer uses the latest density and geometry', async () => {
  requests.length = 0;
  const rfb = client(1000, 600);
  rfb._lastResize = Date.now();
  rfb._requestRemoteResize();
  assert.equal(requests.length, 0);
  environment.devicePixelRatio = 1.5;
  rfb._screen.getBoundingClientRect = () => ({ width: 1200, height: 700 });
  await new Promise(resolve => setTimeout(resolve, 140));
  assert.deepEqual(Array.from(requests[0]), [1800, 1050]);
  rfb.dispatchEvent(new Event('disconnect'));
});

test('invalid/minimized sizes and resource budgets', async () => {
  const { physicalSize, MAX_PIXELS, MAX_DIMENSION } = await import(adapter);
  for (const size of [{ w: 0, h: 500 }, { w: -1, h: 5 }, { w: NaN, h: 1 }, { w: Infinity, h: 5 }]) {
    assert.equal(physicalSize(size, 2), null);
  }
  assert.equal(physicalSize({ w: 100, h: 100 }, 0), null);
  for (const size of [{ w: 10000, h: 10000 }, { w: 100000, h: 1000 }, { w: 7680, h: 4320 }]) {
    const result = physicalSize(size, 2);
    assert.ok(result.w * result.h <= MAX_PIXELS);
    assert.ok(result.w <= MAX_DIMENSION && result.h <= MAX_DIMENSION);
  }
});

const displaySource = readFileSync(new URL('../viewer/ui/novnc/core/display.js', import.meta.url), 'utf8');
const Display = vm.runInNewContext(`(class Display {
  ${['absX', 'absY', 'autoscale', '_rescale'].map(name => method(name, displaySource)).join('\n')}
})`, { toSigned32bit: value => value | 0 });

for (const density of [1, 1.25, 1.5, 1.75, 2]) {
  test(`pinned display maps corners, center and drag without double scaling at ${density}x`, () => {
    const display = new Display();
    display._viewportLoc = { x: 0, y: 0, w: 1000 * density, h: 600 * density };
    display._target = { style: {} };
    display.autoscale(1000, 600);
    assert.equal(display._target.style.width, '1000px');
    assert.equal(display._target.style.height, '600px');
    for (const [x, y] of [[0, 0], [999, 0], [0, 599], [999, 599], [500, 300], [100, 200], [200, 250]]) {
      assert.ok(Math.abs(display.absX(x) - x * density) <= 1);
      assert.ok(Math.abs(display.absY(y) - y * density) <= 1);
    }
  });
}

for (const density of [1, 1.25, 1.5, 1.75, 2]) {
  test(`issue #3: physical resize at ${density}x, keeping CSS geometry unchanged`, () => {
    environment.devicePixelRatio = density;
    requests.length = 0;
    const rfb = client(1000.25, 600.25);
    rfb._requestRemoteResize();
    assert.deepEqual(requests.map(value => Array.from(value)), [[Math.round(1000.25 * density), Math.round(600.25 * density)]]);
    assert.equal(rfb._screenSize().w, 1000.25);
    rfb.dispatchEvent(new Event('disconnect'));
  });
}
