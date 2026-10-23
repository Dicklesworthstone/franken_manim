import test, {afterEach} from 'node:test';
import assert from 'node:assert/strict';
import {prepareSoundtrack} from './audio-playback.mjs';
const tick = async () => { for (let i = 0; i < 8; i++) await new Promise(resolve => setImmediate(resolve)); };
const value = (i, c) => ((i % 1009) - 504) / 1024 * (c ? -1 : 1);
const near = (a, b) => assert(Math.abs(a - b) < 1e-9, `${a} != ${b}`);
const cleanups = [];
afterEach(() => { for (const cleanup of cleanups.splice(0)) cleanup(); });
function fixture({frames = 200123, rate = 48000, device = rate, count = 200, fps = 29} = {}) {
  const reads = [], buffers = [], nodes = [], held = [], live = new Set();
  let hold = false, failure = null, corrupt = false, ignoreAbort = false, active = 0, peak = 0;
  const pcm = {channels: 2, sample_rate: rate, sample_frames: frames, freed: 0,
    free() { this.freed++; },
    read_window(start, count, signal) {
      assert.equal(this.freed, 0); assert(count <= 65536); assert(start + count <= frames);
      reads.push({start, count}); active++; peak = Math.max(peak, active);
      return new Promise((resolve, reject) => {
        let settled = false;
        const done = (error, data) => {
          if (settled) return; settled = true; active--;
          signal.removeEventListener('abort', abort);
          if (error) reject(error); else resolve(data);
        };
        const abort = () => { if (!ignoreAbort) done(new DOMException('cancelled', 'AbortError')); };
        const finish = () => {
          if (settled) return;
          if (failure) { const error = failure; failure = null; return done(error); }
          const data = Array.from({length: 2}, (_, c) => Float32Array.from({length: count}, (_, i) => value(start + i, c)));
          done(null, corrupt ? [data[0]] : data);
        };
        signal.addEventListener('abort', abort, {once: true});
        if (signal.aborted) abort();
        if (hold) held.push(finish); else queueMicrotask(finish);
      });
    },
  };
  const context = {state: 'suspended', currentTime: 0, sampleRate: device, destination: {}, closes: 0, resumes: 0,
    async resume() { this.resumes++; this.state = 'running'; },
    async close() { this.closes++; this.state = 'closed'; },
    createBuffer(channels, length, sampleRate) {
      assert(length <= 65536); const data = Array.from({length: channels}, () => new Float32Array(length));
      const buffer = {length, sampleRate, duration: length / sampleRate, data, getChannelData: c => data[c]};
      buffers.push(buffer); return buffer;
    },
    createBufferSource() {
      const node = {buffer: null, stopped: false, disconnected: false, connect() {},
        start(when, offset) {
          assert(when >= context.currentTime); this.when = when; this.offset = offset;
          this.data = this.buffer.data.map(a => a.slice()); this.rate = this.buffer.sampleRate;
          live.add(this);
        }, stop(when) { if (when === undefined) { this.stopped = true; live.delete(this); } else this.until = when; },
        disconnect() { this.disconnected = true; live.delete(this); }};
      nodes.push(node); return node;
    },
  };
  const f = {pcm, context, reads, buffers, nodes, live, held,
    get active() { return active; }, get peak() { return peak; },
    hold(value) { hold = value; }, fail(error = new Error('window read failed')) { failure = error; },
    corrupt(value = true) { corrupt = value; }, ignoreAbort(value = true) { ignoreAbort = value; },
    async deliver() { held.shift()?.(); await tick(); },
    async prepare() {
      const audio = await prepareSoundtrack({fps, frame_count: count, take_soundtrack: () => pcm}, () => context);
      cleanups.push(() => audio.dispose()); return audio;
    }};
  return f;
}
async function prime(f, audio) {
  for (let i = 0; i < 5; i++) { await tick(); audio.frameAt(); }
  assert(f.live.size <= 3); assert(f.peak <= 1);
}

test('async preparation admits only the first bounded window, without resuming the device', async () => {
  const f = fixture({frames: 32_000_000}); const audio = await f.prepare();
  assert.equal(f.reads.length, 1); assert.equal(f.buffers.length, 1);
  assert.equal(f.reads[0].count, 24032); assert.equal(f.nodes.length, 0); assert.equal(f.context.resumes, 0);
  assert.equal(audio.positionSeconds, 0); audio.dispose(); assert.equal(f.pcm.freed, 1); assert.equal(f.context.closes, 1);
});

test('a small streamed track uses windows too, and an empty track never requests samples', async () => {
  for (const frames of [0, 17]) {
    const f = fixture({frames}); const audio = await f.prepare(); assert(await audio.play());
    await prime(f, audio); assert.equal(f.reads.length, frames ? 1 : 0);
    assert.equal(f.nodes.length, frames ? 1 : 0); audio.dispose(); assert.equal(f.pcm.freed, 1);
  }
});

test('full stereo playback across repeated delayed ticks schedules every sample exactly once', async () => {
  const f = fixture(); const audio = await f.prepare(); await audio.play(); await prime(f, audio);
  for (let n = 0; n < 20 && audio.playing; n++) {
    f.context.currentTime += 5; audio.frameAt(); await prime(f, audio);
  }
  let cursor = 0;
  for (const node of f.nodes) {
    const offset = Math.round(node.offset * node.rate);
    const frames = node.until === undefined ? node.data[0].length - offset : Math.round((node.until - node.when) * node.rate);
    for (let c = 0; c < 2; c++) for (let i = 0; i < frames; i++) assert.equal(node.data[c][offset + i], value(cursor + i, c));
    cursor += frames;
  }
  assert.equal(cursor, f.pcm.sample_frames); assert.equal(audio.playing, false); assert.equal(f.peak, 1);
});

test('a delayed transfer freezes at the first unqueued sample, then resumes from it', async () => {
  const f = fixture(); const audio = await f.prepare(); f.hold(true);
  await audio.play(); await tick(); assert.equal(f.held.length, 1);
  f.context.currentTime = 10; audio.frameAt(); near(audio.positionSeconds, .5);
  f.context.currentTime = 20; audio.frameAt(); near(audio.positionSeconds, .5);
  const count = f.nodes.length; await f.deliver();
  assert.equal(f.nodes.length, count, 'a transfer callback must not start sound');
  audio.frameAt(); assert.equal(f.nodes.length, count + 1); near(audio.positionSeconds, .5);
  f.context.currentTime += .2; audio.frameAt(); assert(audio.positionSeconds > .5);
});

for (const action of ['pause', 'seek', 'dispose']) test(`${action} invalidates a late transfer even if its provider ignores abort`, async () => {
  const f = fixture(); const audio = await f.prepare(); f.hold(true); f.ignoreAbort();
  await audio.play(); await tick(); const nodes = f.nodes.length, buffers = f.buffers.length;
  if (action === 'seek') audio.seek(31); else audio[action]();
  await f.deliver(); assert.equal(f.nodes.length, nodes); assert.equal(f.buffers.length, buffers);
  assert.equal(audio.playing, false); assert.equal(f.live.size, 0);
  if (action === 'seek') near(audio.positionSeconds, 31 / 29);
});

test('pause during initial play buffering returns false and cannot restart later', async () => {
  const f = fixture(); const audio = await f.prepare(); audio.seek(31); f.hold(true);
  const pending = audio.play(); await tick(); assert(audio.starting); audio.pause();
  assert.equal(await pending, false); await f.deliver(); assert.equal(audio.playing, false); assert.equal(f.nodes.length, 0);
});

test('seek with a fractional source sample preserves exact resume phase', async () => {
  const f = fixture(); const audio = await f.prepare(); audio.seek(31); await audio.play(); await prime(f, audio);
  const first = f.nodes[0]; const position = 31 / 29, sample = position * 48000;
  near(first.offset * 48000 - 32, sample - Math.floor(sample));
  f.context.currentTime = first.when + .1234567; audio.frameAt(); audio.pause();
  const paused = audio.positionSeconds; const old = f.nodes.length; await audio.play(); await prime(f, audio);
  near(f.nodes[old].offset * 48000 - 32, paused * 48000 - Math.floor(paused * 48000));
});

test('pending request failure stops playback at queued audio, with explicit retry', async () => {
  const f = fixture(); const audio = await f.prepare(); f.fail(); await audio.play(); await tick();
  f.context.currentTime = 10;
  assert.throws(() => audio.frameAt(), /window read failed/); near(audio.positionSeconds, .5);
  assert.equal(audio.playing, false); assert.equal(f.live.size, 0);
  assert(await audio.play()); await prime(f, audio); near(audio.positionSeconds, .5);
});

test('malformed first async window fails admission and frees source and device', async () => {
  const f = fixture(); f.corrupt();
  await assert.rejects(f.prepare(), /invalid asynchronous PCM/);
  assert.equal(f.pcm.freed, 1); assert.equal(f.context.closes, 1); assert.equal(f.nodes.length, 0);
});

test('suspended device during awaited startup does not publish playback', async () => {
  const f = fixture(); const audio = await f.prepare(); audio.seek(31); f.hold(true);
  const pending = audio.play(); const rejected = assert.rejects(pending, /suspended during buffering/);
  await tick(); f.context.state = 'suspended'; await f.deliver();
  await rejected; assert.equal(audio.playing, false); assert.equal(f.nodes.length, 0);
});

test('rapid explicit seeks retain one fetch at a time and the last selected frame', async () => {
  const f = fixture(); const audio = await f.prepare(); f.hold(true);
  for (let i = 0; i < 50; i++) {
    audio.seek(i + 1); const attempt = audio.play(); await tick(); audio.pause(); assert.equal(await attempt, false);
  }
  audio.seek(71); f.hold(false); assert(await audio.play()); await prime(f, audio);
  assert.equal(audio.frame, 71); assert.equal(f.peak, 1); assert(f.live.size <= 3);
});

test('an audio tail outlasts the final picture; a short track permits its silent picture suffix', async () => {
  for (const [frames, count] of [[120000, 1], [17, 200]]) {
    const f = fixture({frames, count}); const audio = await f.prepare(); await audio.play();
    await prime(f, audio);
    for (let n = 0; n < 30 && audio.playing; n++) { f.context.currentTime += 1; audio.frameAt(); await prime(f, audio); }
    near(audio.positionSeconds, Math.max(frames / 48000, count / 29)); assert.equal(audio.frame, count - 1);
  }
});
