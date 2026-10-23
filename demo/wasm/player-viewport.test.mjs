import test, {after, afterEach} from 'node:test';
import assert from 'node:assert/strict';
import * as playerUI from './player-ui.mjs';
const {choosePlayerViewport, mountTimelinePlayer} = playerUI;

const ImageDataBefore = globalThis.ImageData;
globalThis.ImageData = class { constructor(data, width, height) { Object.assign(this, {data, width, height}); } };
after(() => { globalThis.ImageData = ImageDataBefore; });
const owners = [];
afterEach(() => { for (const owner of owners.splice(0)) owner.dispose(); });
const flush = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
class Element extends EventTarget {
  width = 640; height = 360; clientWidth = 320; style = {}; value = 'auto';
  disabled = false; textContent = ''; dataset = {}; children = []; files = [];
  classList = {toggle() {}};
  draws = [];
  getContext() { return {putImageData: image => this.draws.push([image.width, image.height])}; }
  appendChild(child) { this.children.push(child); }
  replaceChildren() { this.children = []; }
}
async function rig({density = 2, observer = true, sound = false} = {}) {
  const names = ['canvas', 'status', 'scrub', 'play', 'labels', 'restartWorker', 'frameLabel', 'bundleFile', 'loadDemo', 'resolution', 'viewportLabel'];
  const elements = Object.fromEntries(names.map(n => [n, new Element()]));
  const document = Object.assign(new EventTarget(), {hidden: false,
    getElementById: id => elements[id], createElement: () => new Element()});
  const scheduled = new Map(), queries = []; let next = 0, observerOwner = null, session;
  const window = Object.assign(new EventTarget(), {devicePixelRatio: density, performance: {now: () => 10},
    requestAnimationFrame(fn) { const id = ++next; scheduled.set(id, fn); return id; },
    cancelAnimationFrame(id) { scheduled.delete(id); },
    matchMedia(text) {
      const query = Object.assign(new EventTarget(), {text, listeners: 0});
      const add = query.addEventListener.bind(query), remove = query.removeEventListener.bind(query);
      query.addEventListener = (...args) => { query.listeners++; add(...args); };
      query.removeEventListener = (...args) => { query.listeners--; remove(...args); };
      queries.push(query); return query;
    }});
  if (observer) window.ResizeObserver = class {
    constructor(callback) { this.callback = callback; this.disconnected = false; observerOwner = this; }
    observe(target) { assert.equal(target, elements.canvas); }
    disconnect() { this.disconnected = true; }
  };
  const audio = {frame: 5, positionSeconds: .625, playing: false, pauses: 0, seeks: 0,
    pause() { this.pauses++; this.playing = false; }, seek() { this.seeks++; },
    frameAt() { return this.frame; }, async play() { this.playing = true; return true; }, dispose() {}};
  const owner = mountTimelinePlayer({document, window, readDemo: async () => new Uint8Array([1]),
    createSession(options) {
      session = {current: null, width: options.width, height: options.height, resizes: [], renders: [], pending: null,
        async load(read) {
          await read(); this.current = {audio: sound ? audio : null, renderer: {frame: 0, error: null},
            metadata: {frameCount: 20, fps: 8, durationSeconds: 2.5, labels: [], engineVersion: 'fixture',
              audio: sound ? {channels: 2, sampleRate: 48000} : null, hasCameraTrack: true}};
          return true;
        },
        resize(width, height) {
          this.resizes.push([width, height]); const changed = this.width !== width || this.height !== height;
          this.width = width; this.height = height;
          if (changed && this.current) this.pending = this.current.renderer.frame;
          return changed;
        },
        render(frame) { this.renders.push(frame); this.pending = frame; },
        present() { this.deliver(this.current.renderer.frame); },
        deliver(frame = this.pending) {
          assert.notEqual(frame, null); this.pending = null; this.current.renderer.frame = frame;
          options.onFrame({index: frame, width: this.width, height: this.height, rgba: new Uint8Array(this.width * this.height * 4)});
        },
        invalidate() { this.pending = null; }, dispose() { this.current = null; }, cancelRestart() { return false; }};
      return session;
    }});
  owners.push(owner); await flush();
  const frame = () => { const callbacks = [...scheduled.values()]; scheduled.clear(); for (const fn of callbacks) fn(10); };
  frame();
  return {owner, elements, document, window, session, audio, queries, scheduled, observer: observerOwner, frame,
    select(value) { elements.resolution.value = value; elements.resolution.dispatchEvent(new Event('change')); }};
}

test('automatic sizing follows density, exact aspect, quantization, and the 1080p cap', () => {
  for (const [width, density, output] of [[320, 2, [640, 360]], [500, 1, [496, 279]],
    [960, 2, [1920, 1080]], [1000000, 1000, [1920, 1080]], [1, .1, [16, 9]]]) {
    assert.deepEqual(choosePlayerViewport('auto', width, density), output);
  }
  assert.equal(choosePlayerViewport('auto', 0, 2), null);
});

test('explicit presets select landscape, portrait and square without reading layout', () => {
  for (const [mode, output] of [['640x360', [640, 360]], ['1280x720', [1280, 720]],
    ['1920x1080', [1920, 1080]], ['1080x1920', [1080, 1920]], ['1080x1080', [1080, 1080]]]) {
    assert.deepEqual(choosePlayerViewport(mode, NaN, NaN), output);
  }
  const old = choosePlayerViewport('640x360', 0); old[0] = 1;
  assert.deepEqual(choosePlayerViewport('640x360', 0), [640, 360]);
});

test('invalid display inputs and unrecognized resolution modes fail precisely', () => {
  for (const mode of ['__proto__', 'constructor', '4k', null]) assert.throws(() => choosePlayerViewport(mode, 10), /resolution/);
  for (const [w, d] of [[-1, 1], [NaN, 1], [Infinity, 1], ['320', 1], [1, 0], [1, NaN], [1, Infinity]]) {
    assert.throws(() => choosePlayerViewport('auto', w, d), /display/);
  }
});

test('page does not clear the current canvas until the replacement picture arrives', async () => {
  const f = await rig(); const before = f.elements.canvas.draws.length;
  f.select('1080x1920'); assert.deepEqual([f.elements.canvas.width, f.elements.canvas.height], [640, 360]);
  assert.equal(f.elements.canvas.draws.length, before); assert.match(f.elements.viewportLabel.textContent, /pending/);
  f.session.deliver(); assert.deepEqual([f.elements.canvas.width, f.elements.canvas.height], [1080, 1920]);
  assert.equal(f.elements.canvas.style.aspectRatio, '1080 / 1920');
  assert.equal(f.elements.viewportLabel.textContent, '1080 × 1920');
});

test('resolution control neither pauses nor seeks a running narration', async () => {
  const f = await rig({sound: true}); f.audio.playing = true;
  const old = f.owner.current, phase = f.audio.positionSeconds;
  f.select('1280x720'); f.session.deliver();
  assert.equal(f.owner.current, old); assert.equal(f.audio.positionSeconds, phase);
  assert.equal(f.audio.pauses, 0); assert.equal(f.audio.seeks, 0); assert(f.audio.playing);
  assert.equal(f.elements.frameLabel.textContent, '5 / 19');
});

test('observer resize storms coalesce to one display measurement per animation frame', async () => {
  const f = await rig(); const before = f.session.resizes.length;
  for (let i = 0; i < 10000; i++) { f.elements.canvas.clientWidth = 320 + i % 200; f.observer.callback(); }
  assert.equal(f.scheduled.size, 1); assert.equal(f.session.resizes.length, before);
  f.frame(); assert.equal(f.session.resizes.length, before + 1);
  assert.deepEqual(f.session.resizes.at(-1), [1040, 585]);
});

test('window resize remains functional without ResizeObserver', async () => {
  const f = await rig({observer: false}); f.elements.canvas.clientWidth = 160;
  f.window.dispatchEvent(new Event('resize')); f.frame(); assert.deepEqual(f.session.resizes.at(-1), [320, 180]);
});

test('explicit resolutions survive changing CSS width and device density', async () => {
  const f = await rig(); f.select('1280x720'); f.session.deliver(); const count = f.session.renders.length;
  f.elements.canvas.clientWidth = 100; f.window.devicePixelRatio = 4;
  f.window.dispatchEvent(new Event('resize')); f.frame();
  assert.deepEqual(f.session.resizes.at(-1), [1280, 720]); assert.equal(f.session.renders.length, count);
});

test('density notifications are rearmed and detached without retaining old listeners', async () => {
  const f = await rig(); f.window.devicePixelRatio = 3; f.queries[0].dispatchEvent(new Event('change')); f.frame();
  assert.equal(f.queries[0].listeners, 0); assert.equal(f.queries[1].listeners, 1);
  assert.deepEqual(f.session.resizes.at(-1), [960, 540]);
  f.owner.dispose(); assert.equal(f.queries[1].listeners, 0); assert(f.observer.disconnected);
});

test('hidden zero-width containers wait for a usable viewport', async () => {
  const f = await rig(); const count = f.session.resizes.length;
  f.elements.canvas.clientWidth = 0; f.observer.callback(); f.frame(); assert.equal(f.session.resizes.length, count);
  f.elements.canvas.clientWidth = 400; f.document.dispatchEvent(new Event('visibilitychange')); f.frame();
  assert.deepEqual(f.session.resizes.at(-1), [800, 450]);
});

test('pause during pending resize reissues only the selected picture without resuming audio', async () => {
  const f = await rig({sound: true}); f.select('1280x720'); assert.equal(f.session.pending, 5);
  f.owner.stop(); assert.equal(f.session.pending, null); f.frame(); assert.equal(f.session.pending, 5);
  f.session.deliver(); assert.equal(f.audio.playing, false); assert.equal(f.audio.seeks, 0);
  assert.equal(f.elements.viewportLabel.textContent, '1280 × 720');
});

test('a refused UI resolution preserves the image and narration', async () => {
  const f = await rig({sound: true}); const count = f.session.resizes.length; f.audio.playing = true;
  f.select('16384x16384'); assert.equal(f.session.resizes.length, count); assert(f.audio.playing);
  assert.equal(f.elements.canvas.width, 640); assert.match(f.elements.status.textContent, /resolution refused/);
});

test('disposing cancels resize callbacks and removes every notification source', async () => {
  const f = await rig(); f.observer.callback(); const late = [...f.scheduled.values()][0];
  const count = f.session.resizes.length; f.owner.dispose();
  assert.equal(f.scheduled.size, 0); assert(f.elements.resolution.disabled);
  late(0); f.select('1280x720'); f.window.dispatchEvent(new Event('resize')); f.observer.callback();
  assert.equal(f.scheduled.size, 0); assert.equal(f.session.resizes.length, count);
});
