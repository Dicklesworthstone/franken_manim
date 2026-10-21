import assert from 'node:assert/strict';
import test from 'node:test';
import { prepareSoundtrack } from './audio-playback.mjs';

const value = (frame, channel) => Math.fround(((frame % 251) - 125) / 128 * (channel ? -1 : 1));
const near = (actual, expected) => assert(Math.abs(actual - expected) < 1e-9, `${actual} != ${expected}`);
function deferred() {
  let resolve, reject;
  const promise = new Promise((a, b) => { resolve = a; reject = b; });
  return { promise, resolve, reject };
}
function fixture({ frames = 300123, channels = 2, rate = 44100, fps = 29, count = 200 } = {}) {
  const copies = [], buffers = [], sources = [], scheduled = [], live = new Set();
  let maximumLive = 0, creates = 0;
  const pcm = {
    sample_frames: frames, channels, sample_rate: rate, freed: 0,
    free() { this.freed++; assert.equal(this.freed, 1, 'native PCM freed more than once'); },
    copy_channel_into(channel, start, destination) {
      assert.equal(this.freed, 0, 'native PCM used after free');
      assert(Number.isInteger(start) && start >= 0 && start + destination.length <= frames);
      assert(destination.length <= 65536, 'unbounded bridge copy');
      copies.push({ channel, start, frames: destination.length });
      for (let i = 0; i < destination.length; i++) destination[i] = value(start + i, channel);
      destination.sourceStart = start;
    },
  };
  const context = {
    state: 'suspended', currentTime: 10, closed: 0, resumes: 0, destination: {},
    async resume() { this.resumes++; this.state = 'running'; },
    async close() { this.closed++; this.state = 'closed'; },
    createBuffer(channels, frames, rate) {
      assert(frames > 0 && frames <= 65536, `whole-track device allocation: ${frames} frames`);
      const data = Array.from({ length: channels }, () => new Float32Array(frames));
      const buffer = { data, length: frames, sampleRate: rate, duration: frames / rate,
        getChannelData(channel) { return data[channel]; } };
      buffers.push(buffer); return buffer;
    },
    createBufferSource() {
      const node = { stopped: 0, disconnected: 0, buffer: null,
        connect(destination) { assert.equal(destination, context.destination); },
        start(when, offset, duration) {
          assert(when >= context.currentTime, 'source scheduled in the past');
          const { buffer } = this;
          const readStart = buffer.data[0].sourceStart;
          const first = Math.floor(offset * buffer.sampleRate + 1e-7);
          const end = duration === undefined ? buffer.length : Math.round((offset + duration) * buffer.sampleRate);
          const start = readStart + first;
          this.record = { start, frames: end - first, when,
            offset: offset - first / buffer.sampleRate, rate: buffer.sampleRate,
            data: buffer.data.map(channel => channel.slice(first, end)) };
          scheduled.push(this.record); live.add(this); maximumLive = Math.max(maximumLive, live.size);
        },
        stop(when) {
          if (when !== undefined) {
            this.stopTime = when;
            const frames = Math.round((when - this.record.when) * this.record.rate);
            this.record.frames = frames;
            this.record.data = this.record.data.map(channel => channel.slice(0, frames));
          } else { this.stopped++; live.delete(this); }
        },
        disconnect() { this.disconnected++; live.delete(this); },
      };
      sources.push(node); return node;
    },
  };
  const player = { fps, frame_count: count, take_soundtrack() { return pcm; } };
  return { pcm, player, context, copies, buffers, sources, scheduled, live,
    get maximumLive() { return maximumLive; }, get creates() { return creates; },
    prepare(maxBytes) { return prepareSoundtrack(player, () => { creates++; return context; }, maxBytes); } };
}
function exactSchedule(f) {
  let next = 0;
  for (const window of f.scheduled) {
    assert.equal(window.start, next, 'skipped or repeated native samples');
    near(window.offset, 0);
    for (let c = 0; c < window.data.length; c++) {
      for (let i = 0; i < window.frames; i++) assert.equal(window.data[c][i], value(next + i, c));
    }
    next += window.frames;
  }
  assert.equal(next, f.pcm.sample_frames);
  assert(f.maximumLive <= 3);
  assert(f.buffers.every(buffer => buffer.length <= 65536));
}

test('long-track preparation owns native PCM but copies only one device window without autoplay', () => {
  const f = fixture(), a = f.prepare();
  assert.equal(f.context.resumes, 0); assert.equal(f.sources.length, 0); assert.equal(f.pcm.freed, 0);
  assert.equal(f.buffers.length, 1);
  assert.deepEqual(f.copies, [{ channel: 0, start: 0, frames: 22082 }, { channel: 1, start: 0, frames: 22082 }]);
  assert.equal(a.audio.sampleFrames, 300123); assert.equal(a.audio.sampleRate, 44100);
  a.dispose(); a.dispose(); assert.equal(f.pcm.freed, 1); assert.equal(f.context.closed, 1);
});

test('a full retained-PCM budget needs only a half-second device allocation', () => {
  const f = fixture({ frames: 256 * 1024 * 1024 / 8, rate: 48000 });
  const a = f.prepare();
  assert.equal(f.buffers.length, 1); assert.equal(f.buffers[0].length, 24032);
  assert.equal(f.copies.length, 2); a.dispose();
});

test('retained PCM byte limits and bad clocks still refuse before ownership transfers', () => {
  const f = fixture();
  assert.throws(() => f.prepare(64), /budget/); assert.equal(f.creates, 0); assert.equal(f.pcm.freed, 1);
  for (const key of ['fps', 'frame_count']) {
    const f = fixture(); f.player[key] = 0;
    assert.throws(() => f.prepare(), /integer/);
    assert.equal(f.pcm.freed, 1); assert.equal(f.context.closed, 1); assert.equal(f.sources.length, 0);
  }
});

test('unsupported device buffers and failed initial bridge copies preserve candidate cleanup', () => {
  for (const failure of ['buffer', 'copy']) {
    const f = fixture();
    if (failure === 'buffer') f.context.createBuffer = () => { throw new Error('rate refused'); };
    else f.pcm.copy_channel_into = () => { throw new Error('bridge refused'); };
    assert.throws(() => f.prepare(), /refused/);
    assert.equal(f.pcm.freed, 1); assert.equal(f.context.closed, 1); assert.equal(f.context.resumes, 0);
  }
});

test('first play uses the prepared window and schedules exactly three contiguous buffers', async () => {
  const f = fixture(), a = f.prepare(); assert(await a.play());
  assert.equal(f.buffers.length, 3); assert.equal(f.scheduled.length, 3); assert.equal(f.copies.length, 6);
  assert.deepEqual(f.scheduled.map(w => w.start), [0, 22050, 44100]);
  for (let i = 0; i < 3; i++) { near(f.scheduled[i].when, 10.06 + i / 2); near(f.scheduled[i].offset, 0); }
  for (let i = 0; i < 100; i++) a.frameAt();
  assert.equal(f.buffers.length, 3); a.dispose(); assert.equal(f.live.size, 0);
});

test('continuous playback plays every stereo sample once with bounded windows and halo copies', async () => {
  const f = fixture(), a = f.prepare(); await a.play();
  for (let tick = 1; tick <= 100 && a.playing; tick++) {
    f.context.currentTime = 10.06 + tick / 10;
    a.frameAt(); assert(f.live.size <= 3);
  }
  assert.equal(a.playing, false); assert.equal(a.frame, 199); exactSchedule(f);
  const copied = f.copies.reduce((sum, copy) => sum + copy.frames, 0);
  assert(copied >= f.pcm.sample_frames * 2);
  assert(copied <= f.pcm.sample_frames * 2 + f.scheduled.length * 64 * 2);
  for (let i = 1; i < f.scheduled.length; i++) {
    near(f.scheduled[i].when, f.scheduled[i - 1].when + f.scheduled[i - 1].frames / 44100);
  }
  assert(f.sources.every(node => node.buffer === null)); a.dispose();
});

test('repeated full-duration UI stalls recover at the first unheard sample rather than finishing early', async () => {
  const f = fixture({ count: 30 }), a = f.prepare(); await a.play();
  let recoveries = 0;
  while (a.playing && recoveries++ < 20) {
    const last = f.scheduled.at(-1), frontier = (last.start + last.frames) / last.rate;
    f.context.currentTime += 20;
    a.frameAt();
    near(a.positionSeconds, frontier);
    if (frontier < f.pcm.sample_frames / f.pcm.sample_rate) assert(a.playing);
  }
  assert(recoveries >= 4); assert.equal(a.playing, false); exactSchedule(f); a.dispose();
});

test('pause during starvation does not skip unqueued PCM and resumes at its exact boundary', async () => {
  const f = fixture(), a = f.prepare(); await a.play();
  f.context.currentTime = 100; a.pause(); near(a.positionSeconds, 1.5);
  assert.equal(f.scheduled.length, 3); assert.equal(f.live.size, 0);
  await a.play(); assert.equal(f.scheduled[3].start, 66150); near(f.scheduled[3].offset, 0);
  a.dispose();
});

test('pause/resume preserves fractional audio time rather than rounding to a frame or sample', async () => {
  const f = fixture(), a = f.prepare(); await a.play();
  f.context.currentTime = f.scheduled[0].when + 0.123456; a.pause();
  const position = a.positionSeconds; near(position, 0.123456);
  const before = f.scheduled.length; f.context.currentTime = 20; await a.play();
  const window = f.scheduled[before]; assert.equal(window.start, Math.floor(position * 44100));
  near(window.start / 44100 + window.offset, position); near(window.when, 20.06);
  a.dispose();
});

test('backward and non-dividing-FPS seeks read only the selected sample window', async () => {
  const f = fixture(), a = f.prepare();
  for (const index of [149, 31, 1, 0]) {
    a.seek(index); assert.equal(a.frameAt(), index);
    const before = f.scheduled.length; await a.play(); assert.equal(a.frameAt(), index);
    const window = f.scheduled[before]; assert.equal(window.start, Math.floor(index / 29 * 44100));
    near(window.start / 44100 + window.offset, index / 29);
    assert(f.scheduled.length - before <= 3); a.pause();
  }
  a.dispose();
});

test('late ended callbacks from an earlier seek do not release a new run', async () => {
  const f = fixture(), a = f.prepare(); await a.play();
  const callbacks = f.sources.map(node => node.onended);
  a.seek(100); await a.play(); const nodes = [...f.live];
  for (const callback of callbacks) callback();
  assert.deepEqual([...f.live], nodes); assert(nodes.every(node => !node.stopped && node.buffer)); a.dispose();
});

test('bridge failure during refill stops sound without freeing the retained track or skipping the failed window', async () => {
  const f = fixture(), a = f.prepare(); await a.play();
  const copy = f.pcm.copy_channel_into;
  f.pcm.copy_channel_into = () => { throw new Error('late bridge refusal'); };
  f.context.currentTime = 10.7;
  assert.throws(() => a.frameAt(), /late bridge refusal/);
  assert.equal(a.playing, false); assert.equal(f.live.size, 0); assert.equal(f.pcm.freed, 0);
  const position = a.positionSeconds; near(position, 0.64);
  f.pcm.copy_channel_into = copy; const before = f.scheduled.length;
  await a.play(); near(f.scheduled[before].start / 44100 + f.scheduled[before].offset, position);
  a.dispose();
});

test('partial source-start failure stops all earlier scheduled windows and can be explicitly retried', async () => {
  const f = fixture(), a = f.prepare(), create = f.context.createBufferSource;
  f.context.createBufferSource = () => {
    const node = create();
    if (f.sources.length === 2) node.start = () => { throw new Error('start refused'); };
    return node;
  };
  await assert.rejects(a.play(), /start refused/);
  assert.equal(a.playing, false); assert.equal(a.starting, false); assert.equal(f.live.size, 0);
  assert(f.sources.every(node => node.stopped && node.disconnected && node.buffer === null));
  f.context.createBufferSource = create; assert(await a.play()); assert.equal(f.scheduled[1].start, 0); a.dispose();
});

test('pause, seek and dispose invalidate pending device resumes on the retained-PCM path', async () => {
  for (const action of ['pause', 'seek', 'dispose']) {
    const f = fixture(), a = f.prepare(), pending = deferred();
    f.context.resume = async () => { await pending.promise; f.context.state = 'running'; };
    const play = a.play(); assert(a.starting); assert.equal(await a.play(), false);
    if (action === 'seek') a.seek(31); else a[action]();
    pending.resolve(); assert.equal(await play, false); assert.equal(f.scheduled.length, 0);
    assert.equal(a.playing, false); a.dispose(); assert.equal(f.pcm.freed, 1);
  }
});

test('late rejected resume does not overwrite a newer successful run', async () => {
  const f = fixture(), a = f.prepare(), pending = deferred();
  f.context.resume = () => pending.promise; const old = a.play(); a.pause();
  f.context.resume = async () => { f.context.state = 'running'; };
  assert(await a.play()); pending.reject(new Error('old refusal'));
  assert.equal(await old, false); assert(a.playing); assert.equal(f.live.size, 3); a.dispose();
});

test('device suspension stops queued windows until an explicit user resume', async () => {
  const f = fixture(), a = f.prepare(); await a.play();
  f.context.currentTime = 10.2; f.context.state = 'suspended'; a.frameAt();
  near(a.positionSeconds, 0.14); assert.equal(a.playing, false); assert.equal(f.live.size, 0);
  await a.play(); near(f.scheduled[3].start / 44100 + f.scheduled[3].offset, a.positionSeconds); a.dispose();
});

test('native source lifetime survives seeks, completion and restart but ends exactly once on dispose', async () => {
  const f = fixture(), a = f.prepare(); await a.play();
  for (let i = 0; i < 20 && a.playing; i++) { f.context.currentTime += 20; a.frameAt(); }
  assert.equal(a.playing, false); assert.equal(f.pcm.freed, 0);
  const before = f.scheduled.length; await a.play(); assert.equal(f.scheduled[before].start, 0);
  a.dispose(); a.dispose(); assert.equal(f.pcm.freed, 1); assert.equal(f.context.closed, 1);
  await assert.rejects(a.play(), /disposed/); assert.throws(() => a.seek(0), /disposed/);
});

test('rate-dependent windows are capped at 65536 frames without changing the recorded sample rate', async () => {
  for (const rate of [8000, 44100, 48000, 192000, 384000]) {
    const f = fixture({ rate }), a = f.prepare(); await a.play();
    assert.equal(f.buffers[0].length, Math.min(65472, Math.ceil(rate / 2)) + 32);
    assert(f.buffers.every(buffer => buffer.sampleRate === rate)); assert.equal(f.live.size, 3); a.dispose();
  }
});

test('a long picture suffix after the soundtrack and an explicit seek beyond EOF need no fake audio', async () => {
  const f = fixture({ count: 2000 }), a = f.prepare();
  a.seek(300); const before = f.copies.length; await a.play();
  assert.equal(f.scheduled.length, 0); assert.equal(f.copies.length, before);
  f.context.currentTime += 1; a.frameAt(); assert(a.frame > 300); assert(a.playing);
  f.context.currentTime += 100; a.frameAt(); assert.equal(a.frame, 1999); assert.equal(a.playing, false); a.dispose();
});

test('slow native copies are prepared before anchoring the initial and recovery schedules', async () => {
  const f = fixture(), copy = f.pcm.copy_channel_into;
  f.pcm.copy_channel_into = function(...args) { const result = copy.apply(this, args); f.context.currentTime += 0.2; return result; };
  const a = f.prepare(); await a.play();
  assert(f.scheduled[0].when >= f.context.currentTime);
  for (let i = 0; i < 20 && a.playing; i++) { f.context.currentTime += 20; a.frameAt(); }
  assert.equal(a.playing, false); exactSchedule(f); a.dispose();
});

test('invalid seeks leave active windows alone and teardown still closes a device with failing nodes', async () => {
  const f = fixture(), a = f.prepare(); await a.play();
  for (const index of [-1, 200, NaN, Infinity, 0.1]) assert.throws(() => a.seek(index), RangeError);
  assert(a.playing); assert.equal(f.live.size, 3);
  for (const node of f.sources) node.stop = () => { throw new Error('device already gone'); };
  f.context.currentTime = NaN; assert.doesNotThrow(() => a.dispose());
  assert.equal(f.live.size, 0); assert.equal(f.pcm.freed, 1); assert.equal(f.context.closed, 1);
});

// Exercise the actual transactional session owner, not a second test-only
// replacement implementation. Only decoding/rasterizing are fixture adapters.
import { TimelineSession } from './player-session.mjs';
function candidate(options) {
  const f = fixture(options);
  Object.assign(f.player, { freed: 0, has_soundtrack: true, has_camera_track: true,
    duration_seconds: f.player.frame_count / f.player.fps, engine_version: 'fixture',
    set_viewport() {}, render_into() {}, seek_frame() {}, labels() { return []; },
    free() { this.freed++; },
  });
  return f;
}
test('failed replacement preserves the current retained-PCM movie and frees only the candidate', async () => {
  for (const failure of ['copy', 'frame']) {
    const old = candidate(), next = candidate(), choices = [old, next];
    const session = new TimelineSession(() => choices.shift().player, 2, 2, 64,
      player => prepareSoundtrack(player, () => player === old.player ? old.context : next.context));
    await session.load(async () => new Uint8Array([1])); const previous = session.current;
    if (failure === 'copy') next.pcm.copy_channel_into = () => { throw new Error('copy refused'); };
    else next.player.render_into = () => { throw new Error('frame refused'); };
    await assert.rejects(session.load(async () => new Uint8Array([2])), /refused/);
    assert.equal(session.current, previous); assert.equal(old.pcm.freed, 0); assert.equal(old.player.freed, 0);
    assert.equal(next.pcm.freed, 1); assert.equal(next.player.freed, 1); assert.equal(next.context.closed, 1);
    assert(await session.current.audio.play()); session.dispose(); assert.equal(old.pcm.freed, 1);
  }
});

test('successful replacement cancels pending old playback and releases native PCM and picture owners once', async () => {
  const old = candidate(), next = candidate(), choices = [old, next], pending = deferred();
  const session = new TimelineSession(() => choices.shift().player, 2, 2, 64,
    player => prepareSoundtrack(player, () => player === old.player ? old.context : next.context));
  await session.load(async () => new Uint8Array([1]));
  old.context.resume = () => pending.promise; const play = session.current.audio.play();
  await session.load(async () => new Uint8Array([2])); pending.resolve();
  assert.equal(await play, false); assert.equal(old.scheduled.length, 0);
  assert.equal(old.pcm.freed, 1); assert.equal(old.player.freed, 1); assert.equal(old.context.closed, 1);
  assert(await session.current.audio.play()); assert.equal(next.pcm.freed, 0);
  session.dispose(); session.dispose(); assert.equal(next.pcm.freed, 1); assert.equal(next.player.freed, 1);
});


test('fractional seeks keep one source phase across device-grid-rounded window starts', async () => {
  for (const rate of [44100, 48000]) {
    const f = fixture({ rate }), a = (() => { f.context.sampleRate = 48000; return f.prepare(); })();
    a.seek(31); const position = a.positionSeconds; await a.play();
    const origin = f.scheduled[0].when;
    for (const window of f.scheduled) {
      near(window.start / rate + window.offset, position + window.when - origin);
      near(window.when * 48000, Math.round(window.when * 48000));
    }
    a.dispose();
  }
});

test('unrepresentable device-clock windows refuse before allocating or transferring PCM ownership', () => {
  const f = fixture({ rate: 384000 }); f.context.sampleRate = 44101;
  assert.throws(() => f.prepare(), /sample-rate ratio/);
  assert.equal(f.buffers.length, 0); assert.equal(f.sources.length, 0);
  assert.equal(f.pcm.freed, 1); assert.equal(f.context.closed, 1);
});

test('window size is a whole cycle of both clocks even for a high-rate source', async () => {
  const f = fixture({ rate: 384000 }); f.context.sampleRate = 44100;
  const a = f.prepare(); await a.play();
  assert.equal(f.scheduled[0].frames, 65280);
  for (let i = 1; i < f.scheduled.length; i++) {
    near((f.scheduled[i].when - f.scheduled[i - 1].when) * 44100, 7497);
  }
  assert(f.buffers.every(buffer => buffer.length <= 65536)); a.dispose();
});
