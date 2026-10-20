import assert from 'node:assert/strict';
import test from 'node:test';
import { prepareSoundtrack } from './audio-playback.mjs';

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function fixture({ frames = 8, channels = 2, rate = 8, fps = 8, count = 4 } = {}) {
  const copies = [], buffers = [], sources = [];
  const pcm = {
    sample_frames: frames, channels, sample_rate: rate, freed: 0,
    free() { this.freed++; },
    copy_channel_into(channel, start, destination) {
      copies.push([channel, start, destination.length]);
      for (let i = 0; i < destination.length; ++i) destination[i] = (start + i + 1) / 16 * (channel ? -1 : 1);
    },
  };
  const context = {
    state: 'suspended', currentTime: 10, closed: 0, resumes: 0, destination: {},
    async resume() { this.resumes++; this.state = 'running'; },
    async close() { this.closed++; this.state = 'closed'; },
    createBuffer(ch, n, sr) {
      const data = Array.from({ length: ch }, () => new Float32Array(n));
      const buffer = { data, duration: n / sr, getChannelData(c) { return data[c]; } };
      buffers.push(buffer); return buffer;
    },
    createBufferSource() {
      const source = {
        starts: [], stops: 0, disconnects: 0,
        connect(destination) { assert.equal(destination, context.destination); },
        start(when, offset) { this.starts.push([when, offset]); },
        stop() { this.stops++; }, disconnect() { this.disconnects++; },
      };
      sources.push(source); return source;
    },
  };
  const player = { fps, frame_count: count, take_soundtrack() { return pcm; } };
  return { pcm, context, player, copies, buffers, sources,
    prepare(maxBytes) { return prepareSoundtrack(player, () => context, maxBytes); } };
}

test('preparation copies exact channels once, releases Wasm PCM, and never starts audio', () => {
  const f = fixture(); const audio = f.prepare();
  assert.equal(f.pcm.freed, 1); assert.equal(f.context.resumes, 0); assert.equal(f.sources.length, 0);
  assert.deepEqual(f.copies, [[0, 0, 8], [1, 0, 8]]);
  assert.deepEqual([...f.buffers[0].data[0]], [1,2,3,4,5,6,7,8].map(x => x / 16));
  assert.deepEqual([...f.buffers[0].data[1]], [1,2,3,4,5,6,7,8].map(x => -x / 16));
  assert.equal(audio.durationSeconds, 1); assert.equal(audio.playing, false);
  audio.dispose(); audio.dispose(); assert.equal(f.context.closed, 1);
});

test('bridge copies are bounded to 65536 frames even for a longer mono track', () => {
  const f = fixture({ frames: 65537, channels: 1 }); const audio = f.prepare();
  assert.deepEqual(f.copies, [[0,0,65536], [0,65536,1]]); audio.dispose();
});

test('input budget and malformed metadata refuse before creating a context', () => {
  for (const override of [{ sample_frames: 9 }, { channels: 3 }, { sample_rate: 0 }, { sample_frames: NaN }]) {
    const f = fixture(); Object.assign(f.pcm, override); let creates = 0;
    assert.throws(() => prepareSoundtrack(f.player, () => { creates++; return f.context; }, 64));
    assert.equal(creates, 0); assert.equal(f.pcm.freed, 1);
  }
});

test('buffer or channel-copy refusal releases candidate device and PCM', () => {
  for (const kind of ['buffer', 'copy']) {
    const f = fixture();
    if (kind === 'buffer') f.context.createBuffer = () => { throw new Error('rate refused'); };
    else f.pcm.copy_channel_into = () => { throw new Error('copy refused'); };
    assert.throws(() => f.prepare(), /refused/);
    assert.equal(f.context.closed, 1); assert.equal(f.pcm.freed, 1); assert.equal(f.sources.length, 0);
  }
});

test('the device clock chooses pictures and holds the last one through the audio tail', async () => {
  const f = fixture(); const a = f.prepare();
  assert.equal(await a.play(), true); assert.deepEqual(f.sources[0].starts, [[10, 0]]);
  f.context.currentTime = 10.25; assert.equal(a.frameAt(999999999), 2);
  f.context.currentTime = 10.75; assert.equal(a.frameAt(0), 3); assert.equal(a.playing, true);
  f.context.currentTime = 11; assert.equal(a.frameAt(), 3); assert.equal(a.playing, false);
  assert.equal(f.sources[0].stops, 1); assert.equal(f.sources[0].disconnects, 1);
  f.context.currentTime = 12; await a.play(); assert.deepEqual(f.sources[1].starts, [[12, 0]]);
  a.dispose();
});

test('pause preserves fractional position, resume starts there, and seek uses the picture grid', async () => {
  const f = fixture(); const a = f.prepare(); await a.play();
  f.context.currentTime = 10.3125; a.pause(); assert.equal(a.positionSeconds, 0.3125);
  f.context.currentTime = 15; await a.play(); assert.deepEqual(f.sources[1].starts, [[15, 0.3125]]);
  f.context.currentTime = 15.125; assert.equal(a.frameAt(), 3);
  a.seek(1); assert.equal(a.positionSeconds, 0.125); assert.equal(a.playing, false);
  await a.play(); assert.deepEqual(f.sources[2].starts, [[15.125, 0.125]]); a.dispose();
});

test('short and empty soundtracks keep the complete picture duration without extra sources', async () => {
  for (const frames of [0, 1]) {
    const f = fixture({ frames }); const a = f.prepare(); assert.equal(a.durationSeconds, 0.5);
    a.seek(2); await a.play(); assert.equal(f.sources.length, 0);
    f.context.currentTime = 10.25; assert.equal(a.frameAt(), 3); assert.equal(a.playing, false);
    a.dispose(); assert.equal(f.context.closed, 1);
  }
});

test('pause, seek, or dispose cancels a pending resume without a delayed sound', async () => {
  for (const action of ['pause', 'seek', 'dispose']) {
    const f = fixture(), ready = deferred();
    f.context.resume = async () => { await ready.promise; f.context.state = 'running'; };
    const a = f.prepare(); const pending = a.play(); assert.equal(a.starting, true);
    assert.equal(await a.play(), false);
    if (action === 'seek') a.seek(1); else a[action]();
    ready.resolve(); assert.equal(await pending, false); assert.equal(f.sources.length, 0);
    assert.equal(a.playing, false); assert.equal(a.starting, false); a.dispose();
  }
});

test('failed resume can be retried and stale rejection cannot overwrite a newer play', async () => {
  const f = fixture(), ready = deferred();
  f.context.resume = () => ready.promise;
  const a = f.prepare(); const old = a.play(); a.pause();
  f.context.resume = async () => { f.context.state = 'running'; };
  assert.equal(await a.play(), true); ready.reject(new Error('old device refusal'));
  assert.equal(await old, false); assert.equal(a.playing, true); assert.equal(f.sources.length, 1);
  a.pause(); f.context.resume = async () => { throw new Error('blocked'); };
  await assert.rejects(a.play(), /blocked/); assert.equal(a.starting, false);
  f.context.resume = async () => { f.context.state = 'running'; };
  assert.equal(await a.play(), true); a.dispose();
});

test('invalid seeks do not stop active sound and a suspended device does not advance video', async () => {
  const f = fixture(); const a = f.prepare(); await a.play();
  for (const index of [-1, 4, Infinity, NaN, 0.5]) assert.throws(() => a.seek(index), RangeError);
  assert.equal(a.playing, true); assert.equal(f.sources[0].stops, 0);
  f.context.state = 'suspended'; assert.equal(a.frameAt(999999999), 0);
  f.context.state = 'running'; f.context.currentTime = 10.25; assert.equal(a.frameAt(), 2);
  f.context.currentTime = 10.125; assert.equal(a.frameAt(), 2);
  a.dispose(); await assert.rejects(a.play(), /disposed/); assert.throws(() => a.seek(0), /disposed/);
});

test('failed source start disconnects the node and never claims active playback', async () => {
  const f = fixture(); const create = f.context.createBufferSource;
  f.context.createBufferSource = () => {
    const node = create(); node.start = () => { throw new Error('device refused start'); }; return node;
  };
  const a = f.prepare(); await assert.rejects(a.play(), /refused start/);
  assert.equal(a.playing, false); assert.equal(a.starting, false); assert.equal(f.sources[0].disconnects, 1);
  a.dispose();
});

test('device teardown errors cannot prevent context disposal or leak a source', async () => {
  const f = fixture(); const a = f.prepare(); await a.play();
  f.sources[0].stop = () => { throw new Error('already closed'); };
  f.context.currentTime = NaN;
  assert.doesNotThrow(() => a.dispose());
  assert.equal(f.sources[0].disconnects, 1); assert.equal(f.context.closed, 1);
  assert.equal(a.playing, false); a.dispose(); assert.equal(f.context.closed, 1);
});

test('non-dividing frame rates retain exact sought indices before the device advances', async () => {
  for (const fps of [29, 30, 60]) {
    const f = fixture({ fps, count: 200 }); const a = f.prepare();
    for (let index = 199; index >= 0; --index) {
      a.seek(index); assert.equal(a.frame, index); assert.equal(a.frameAt(), index);
      await a.play(); assert.equal(a.frameAt(), index); a.pause();
    }
    a.dispose();
  }
});
