import test, {after, afterEach} from 'node:test';
import assert from 'node:assert/strict';
import {mountTimelinePlayer} from './player-ui.mjs';

// Only DOM and worker-session boundaries are controlled here. The production
// page controller and its presentation clock execute unchanged; the Chromium
// check separately exercises the actual session, workers and audio device.
const originalImageData = globalThis.ImageData;
globalThis.ImageData = class {
  constructor(data, width, height) { Object.assign(this, {data, width, height}); }
};
after(() => { globalThis.ImageData = originalImageData; });
const owners = new Set();
afterEach(() => { for (const owner of owners) owner.dispose(); owners.clear(); });
const flush = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return {promise, resolve, reject};
};
class Element extends EventTarget {
  disabled = true; textContent = ''; value = '0'; dataset = {}; children = [];
  width = 4; height = 2; files = []; hidden = false;
  classList = {toggle() {}};
  getContext() { return {putImageData() {}}; }
  appendChild(child) { this.children.push(child); }
  replaceChildren() { this.children.length = 0; }
  click() { if (!this.disabled) this.dispatchEvent(new Event('click')); }
}
function movie(audio = null) {
  return {audio, metadata: {frameCount: 32, fps: 8, durationSeconds: 4,
    engineVersion: 'explicit-session-fixture', hasCameraTrack: true,
    audio: audio ? {channels: 2, sampleRate: 48000} : null,
    labels: [{name: 'middle', index: 16}, {name: 'unresolved', index: null}]},
    renderer: {error: null, frame: 0}};
}
function device() {
  const audio = {playing: false, starting: false, frame: 5, positionSeconds: 0.734567,
    pauses: 0, disposals: 0, starts: 0, generation: 0, resumeGate: null,
    pause() { this.pauses++; this.generation++; this.starting = false; this.playing = false; },
    seek(frame) { this.pause(); this.frame = frame; this.positionSeconds = frame / 8; },
    frameAt() { return this.frame; },
    dispose() { this.pause(); this.disposals++; },
    async play() {
      const generation = ++this.generation; this.starting = true;
      if (this.resumeGate) await this.resumeGate.promise;
      if (generation !== this.generation) return false;
      this.starting = false; this.playing = true; this.starts++; return true;
    }};
  return audio;
}
async function fixture(audio = null) {
  const ids = ['canvas', 'status', 'scrub', 'play', 'labels', 'restartWorker', 'frameLabel', 'bundleFile', 'loadDemo'];
  const elements = Object.fromEntries(ids.map(id => [id, new Element()]));
  const document = Object.assign(new EventTarget(), {hidden: false,
    getElementById: id => elements[id], createElement: () => new Element()});
  const callbacks = new Map(); let nextFrame = 0;
  const window = Object.assign(new EventTarget(), {performance: {now: () => 100},
    requestAnimationFrame(fn) { const id = ++nextFrame; callbacks.set(id, fn); return id; },
    cancelAnimationFrame(id) { callbacks.delete(id); }});
  let session;
  const owner = mountTimelinePlayer({document, window, readDemo: async () => new Uint8Array([1]),
    createSession(options) {
      session = {current: null, restartGate: null, failRestart: null, restartCalls: [], cancels: 0,
        renders: [], invalidations: 0, epoch: 0, pending: false, disposed: false, presented: [],
        async load(read) {
          const epoch = ++this.epoch; this.pending = false; await read(new AbortController().signal);
          if (this.disposed || epoch !== this.epoch) return false;
          this.current = movie(this.current ? null : audio); return true;
        },
        async restart(frame) {
          this.restartCalls.push(frame); const epoch = ++this.epoch;
          const old = this.current; this.pending = true; old.audio?.pause();
          try {
            if (this.restartGate) await this.restartGate.promise;
            if (this.failRestart) throw this.failRestart;
            if (this.disposed || epoch !== this.epoch) return false;
            this.current = {...old, renderer: {error: null, frame: frame ?? old.audio.frame}};
            return true;
          } finally { if (epoch === this.epoch) this.pending = false; }
        },
        cancelRestart() { if (!this.pending) return false; this.cancels++; this.epoch++; this.pending = false; return true; },
        render(frame) { this.renders.push(frame); this.current.renderer.frame = frame; this.present(); },
        present() {
          const frame = this.current.renderer.frame; this.presented.push(frame);
          options.onFrame({index: frame, width: 4, height: 2, rgba: new Uint8Array(32)});
        },
        invalidate() { this.invalidations++; },
        crash() { this.current.renderer.error = new Error('worker stopped'); options.onError(this.current.renderer.error); },
        dispose() { this.disposed = true; this.epoch++; this.current?.audio?.dispose(); this.current = null; },
      };
      return session;
    }});
  owners.add(owner); await flush();
  return {owner, session, document, window, elements, callbacks};
}

test('failed worker disables playback but keeps a usable restart control', async () => {
  const f = await fixture(); f.owner.seek(7); f.session.crash();
  assert(f.elements.play.disabled && f.elements.scrub.disabled);
  assert.equal(f.elements.restartWorker.disabled, false);
  f.elements.restartWorker.click(); await flush();
  assert.deepEqual(f.session.restartCalls, [7]); assert.equal(f.session.current.renderer.error, null);
  assert.equal(f.elements.frameLabel.textContent, '7 / 31');
  assert.equal(f.elements.play.disabled, false); assert.equal(f.elements.play.textContent, 'Play');
  assert.match(f.elements.status.textContent, /restarted at frame 7/);
  assert(f.elements.labels.children[1].disabled);
});

test('narrated restart preserves the audio owner and fractional phase without seeking or autoplay', async () => {
  const audio = device(), f = await fixture(audio), phase = audio.positionSeconds;
  assert(await f.owner.restart());
  assert.deepEqual(f.session.restartCalls, [null]); assert.equal(f.owner.current.audio, audio);
  assert.equal(audio.positionSeconds, phase); assert.equal(audio.playing, false); assert.equal(audio.starts, 0);
  assert.equal(f.elements.frameLabel.textContent, '5 / 31'); assert.equal(audio.disposals, 0);
  f.elements.play.click(); await flush(); assert.equal(audio.starts, 1);
});

test('recovery holds playback and labels until the requested picture is ready', async () => {
  const f = await fixture(); const gate = deferred(); f.session.restartGate = gate;
  const pending = f.owner.restart(); assert.equal(f.owner.restarting, true);
  assert.equal(f.elements.restartWorker.textContent, 'Cancel restart');
  assert.equal(f.elements.restartWorker.disabled, false);
  assert(f.elements.play.disabled && f.elements.scrub.disabled);
  assert(f.elements.labels.children.every(button => button.disabled));
  f.owner.seek(8); assert.deepEqual(f.session.renders, []);
  gate.resolve(); assert(await pending);
  assert.equal(f.owner.restarting, false); assert.equal(f.elements.play.disabled, false);
  assert.equal(f.elements.labels.children[0].disabled, false);
});

test('second restart click cancels rather than allocating another replacement', async () => {
  const f = await fixture(), gate = deferred(); f.session.restartGate = gate;
  const current = f.owner.current;
  f.elements.restartWorker.click(); assert(f.owner.restarting);
  f.elements.restartWorker.click(); assert.equal(f.owner.restarting, false);
  assert.equal(f.session.cancels, 1); assert.equal(f.session.restartCalls.length, 1);
  const status = f.elements.status.textContent;
  gate.resolve(); await flush(); assert.equal(f.owner.current, current);
  assert.equal(f.elements.status.textContent, status); assert.match(status, /cancelled/);
  assert.equal(f.elements.restartWorker.textContent, 'Restart picture worker');
  assert.equal(f.elements.play.disabled, false);
});

test('failed restart keeps the failure visible and offers another explicit attempt', async () => {
  const f = await fixture(device()), current = f.owner.current;
  f.session.crash(); f.session.failRestart = new Error('Recording changed');
  assert.equal(await f.owner.restart(), false); assert.equal(f.owner.current, current);
  assert.match(f.elements.status.textContent, /restart failed: Error: Recording changed/);
  assert(f.elements.play.disabled); assert.equal(f.elements.restartWorker.disabled, false);
  assert.equal(current.audio.disposals, 0);
  f.session.failRestart = null; assert(await f.owner.restart());
  assert.equal(f.elements.play.disabled, false); assert.equal(current.audio.playing, false);
});

test('stop, visibility loss and page hiding cancel recovery without late status changes', async () => {
  for (const action of ['stop', 'hidden', 'pagehide']) {
    const f = await fixture(device()), gate = deferred(); f.session.restartGate = gate;
    const current = f.owner.current, pending = f.owner.restart();
    if (action === 'stop') f.owner.stop();
    if (action === 'hidden') { f.document.hidden = true; f.document.dispatchEvent(new Event('visibilitychange')); }
    if (action === 'pagehide') { const event = new Event('pagehide'); event.persisted = true; f.window.dispatchEvent(event); }
    assert.equal(f.session.cancels, 1); assert.equal(f.owner.restarting, false);
    const status = f.elements.status.textContent;
    gate.reject(new Error('late decode refusal')); assert.equal(await pending, false);
    assert.equal(f.elements.status.textContent, status); assert.equal(f.owner.current, current);
    assert.equal(current.audio.disposals, 0);
  }
});

test('a new recording supersedes recovery and owns the final controls and status', async () => {
  for (const failure of [false, true]) {
    const f = await fixture(), gate = deferred(); f.session.restartGate = gate;
    const pending = f.owner.restart();
    assert(await f.owner.load(async () => new Uint8Array([2]), 'new.fmtl'));
    const current = f.owner.current, status = f.elements.status.textContent;
    if (failure) gate.reject(new Error('late candidate failure')); else gate.resolve();
    assert.equal(await pending, false); assert.equal(f.owner.current, current);
    assert.equal(f.elements.status.textContent, status); assert.match(status, /^new\.fmtl:/);
    assert.equal(f.elements.play.disabled, false); assert.equal(f.session.cancels, 1);
  }
});

test('disposal cancels recovery and releases the one paused audio owner', async () => {
  const audio = device(), f = await fixture(audio), gate = deferred(); f.session.restartGate = gate;
  const pending = f.owner.restart(); f.owner.dispose(); gate.resolve();
  assert.equal(await pending, false); assert.equal(audio.disposals, 1); assert.equal(f.owner.current, null);
  assert(f.elements.play.disabled && f.elements.restartWorker.disabled);
  f.elements.restartWorker.dispatchEvent(new Event('click')); await flush();
  assert.equal(f.session.restartCalls.length, 1);
  assert.equal(await f.owner.restart(), false);
});

test('restart invalidates a pending audio resume and leaves no stale animation callback', async () => {
  const audio = device(), f = await fixture(audio), gate = deferred(); audio.resumeGate = gate;
  f.elements.play.click(); await flush(); assert(audio.starting);
  assert(await f.owner.restart()); gate.resolve(); await flush();
  assert.equal(audio.playing, false); assert.equal(audio.starting, false); assert.equal(audio.starts, 0);
  assert.equal(f.callbacks.size, 0); assert.equal(f.elements.play.textContent, 'Play');
});

test('restart is unavailable during source loading and failed loading restores recovery controls', async () => {
  const f = await fixture(), gate = deferred();
  const pending = f.owner.load(() => gate.promise, 'pending.fmtl');
  assert.equal(f.elements.restartWorker.disabled, true); assert.equal(await f.owner.restart(), false);
  gate.reject(new Error('read failure')); assert.equal(await pending, false);
  assert.equal(f.elements.restartWorker.disabled, false); assert.equal(f.session.restartCalls.length, 0);
  assert(await f.owner.restart());
});
