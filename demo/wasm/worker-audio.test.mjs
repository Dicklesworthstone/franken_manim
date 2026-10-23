import test, {afterEach} from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {PictureWorkerRuntime} from './picture-worker-core.mjs';
import {WorkerTimelineSession} from './worker-session.mjs';
import {WORKER_PROTOCOL} from './worker-protocol.mjs';

// No native decoder is claimed. The fixture provides deterministic PCM samples
// through the actual bounded worker protocol and transferable ArrayBuffers.
function recording(marker = 1) {
  const body = new Uint8Array(25); body.set([70, 77, 84, 76]); body[24] = marker;
  new DataView(body.buffer).setBigUint64(16, 1n, true);
  const bytes = new Uint8Array(57); bytes.set(body);
  bytes.set(createHash('sha256').update(body).digest(), 25);
  return bytes;
}
const sample = (channel, index) => (channel ? -1 : 1) * (index % 1009) / 1024;
const flush = async () => { for (let n = 0; n < 12; n++) await new Promise(resolve => setImmediate(resolve)); };
const sessions = [];
afterEach(() => { for (const session of sessions.splice(0)) session.dispose(); });
function rig({frames = 32_000_000, channels = 2, adapter, timeoutMs = 2000} = {}) {
  const workers = [], natives = [], errors = [], pictures = [], audios = [];
  let marker = 1;
  const session = new WorkerTimelineSession({width: 4, height: 2, streamAudio: true, timeoutMs,
    onFrame: frame => pictures.push(frame.index), onError: error => errors.push(error),
    createWorker() {
      const facts = {copies: [], freed: 0, playerFreed: 0}; natives.push(facts);
      const pcm = {channels, sample_rate: 48000, sample_frames: frames,
        copy_channel_into(channel, start, out) {
          assert.equal(facts.freed, 0);
          facts.copies.push({channel, start, count: out.length});
          for (let i = 0; i < out.length; i++) out[i] = sample(channel, start + i);
        }, free() { facts.freed++; }};
      const player = {frame_count: 800, fps: 29, duration_seconds: 800 / 29,
        engine_version: 'explicit-native-fixture', has_camera_track: true, has_soundtrack: true,
        set_viewport() {}, labels() { return []; }, frame_of_label() { return null; },
        render_into(index, out) { if (index === 799) throw new Error('fixture raster trap'); out.fill(index % 255); },
        seek_frame() {}, take_soundtrack() { return pcm; }, free() { facts.playerFreed++; }};
      const worker = {onmessage: null, onerror: null, onmessageerror: null,
        requests: [], replies: [], held: [], hold: null, stopped: 0, edit: x => x,
        postMessage(message, transfer) {
          const copy = structuredClone(message, {transfer}); this.requests.push(copy);
          const send = () => runtime.receive(copy);
          if (message.kind === this.hold) this.held.push(send); else queueMicrotask(send);
        }, terminate() { this.stopped++; runtime.dispose(); },
        async finish() { await this.held.shift()?.(); await flush(); }};
      const runtime = new PictureWorkerRuntime(async bytes => {
        assert.deepEqual(Buffer.from(bytes.subarray(-32)), createHash('sha256').update(bytes.subarray(0, -32)).digest());
        return player;
      }, (message, transfer) => {
        const value = structuredClone(message, {transfer});
        worker.replies.push({kind: value.kind, bytes: value.channels?.reduce((n, b) => n + b.byteLength, 0) ?? 0});
        queueMicrotask(() => { if (!worker.stopped) worker.onmessage?.({data: worker.edit(value)}); });
      });
      workers.push(worker); return worker;
    },
    prepareAudio: adapter ?? (async player => {
      const pcm = player.take_soundtrack();
      assert.equal(typeof pcm.read_window, 'function');
      const audio = {pcm, frame: 0, positionSeconds: 0, disposed: 0,
        play() {}, frameAt() { return this.frame; }, pause() {},
        seek(frame) { this.frame = frame; this.positionSeconds = frame / player.fps; },
        dispose() { this.disposed++; pcm.free(); }};
      audios.push(audio); return audio;
    }),
  });
  sessions.push(session);
  return {session, workers, natives, errors, pictures, audios,
    setMarker(value) { marker = value; }, load: () => session.load(async () => recording(marker))};
}
function verify(channels, start, count) {
  assert.equal(channels.length, 2);
  for (let c = 0; c < 2; c++) {
    assert.equal(channels[c].length, count);
    for (let i = 0; i < count; i++) assert.equal(channels[c][i], sample(c, start + i));
  }
}

test('a 244 MiB soundtrack loads with zero full-track copies or channel transfers', async () => {
  const f = rig(); await f.load();
  assert.deepEqual(f.natives[0].copies, []);
  assert.equal(f.workers[0].replies[0].bytes, 0);
  assert.equal(f.natives[0].freed, 0);
  const pcm = f.audios[0].pcm;
  for (const [start, count] of [[0, 65536], [31_999_999, 1], [17, 211], [65500, 32]]) {
    verify(await pcm.read_window(start, count), start, count);
  }
  assert(f.natives[0].copies.every(copy => copy.count <= 65536));
  assert(f.workers[0].replies.every(reply => reply.bytes <= 512 * 1024));
  f.session.dispose(); assert.equal(f.natives[0].freed, 1); assert.equal(f.natives[0].playerFreed, 1);
});

test('audio takes the next worker slot ahead of a ten-thousand-seek storm', async () => {
  const f = rig(); await f.load(); const worker = f.workers[0]; worker.hold = 'render';
  f.session.render(1);
  for (let n = 0; n < 10000; n++) f.session.render(2 + n % 100);
  const window = f.audios[0].pcm.read_window(250, 32);
  assert.equal(worker.requests.length, 2);
  await worker.finish(); verify(await window, 250, 32);
  assert.deepEqual(worker.requests.map(row => row.kind), ['load', 'render', 'audio', 'render']);
  assert.equal(worker.held.length, 1); await worker.finish(); assert.equal(f.pictures.at(-1), 101);
});

test('invalid intervals fail before another request or native allocation', async () => {
  const f = rig(); await f.load(); const pcm = f.audios[0].pcm;
  for (const [start, count] of [[-1, 1], [0, 0], [0, 65537], [32_000_000, 1], [1.5, 4], [0, NaN]]) {
    await assert.rejects(pcm.read_window(start, count));
  }
  assert.equal(f.workers[0].requests.length, 1); assert.deepEqual(f.natives[0].copies, []);
});

test('only one requested audio window can wait behind a raster', async () => {
  const f = rig(); await f.load(); f.workers[0].hold = 'render'; f.session.render(1);
  const pending = f.audios[0].pcm.read_window(0, 10);
  await assert.rejects(f.audios[0].pcm.read_window(10, 10), /already pending/);
  await f.workers[0].finish(); verify(await pending, 0, 10);
});

test('cancelling queued audio removes it without cancelling the picture', async () => {
  const f = rig(); await f.load(); const worker = f.workers[0]; worker.hold = 'render'; f.session.render(5);
  const controller = new AbortController();
  const pending = f.audios[0].pcm.read_window(0, 20, controller.signal);
  const rejected = assert.rejects(pending, {name: 'AbortError'}); controller.abort(); await rejected;
  await worker.finish(); assert.deepEqual(worker.requests.map(r => r.kind), ['load', 'render']);
  assert.deepEqual(f.pictures, [5]); assert.equal(f.errors.length, 0);
});

test('cancelling in-flight audio discards its data and admits one replacement', async () => {
  const f = rig(); await f.load(); const worker = f.workers[0]; worker.hold = 'audio';
  const controller = new AbortController();
  const pending = f.audios[0].pcm.read_window(0, 20, controller.signal);
  const rejected = assert.rejects(pending, {name: 'AbortError'}); controller.abort(); await rejected;
  const replacement = f.audios[0].pcm.read_window(100, 10);
  assert.equal(worker.held.length, 1); await worker.finish(); assert.equal(worker.held.length, 1);
  await worker.finish(); verify(await replacement, 100, 10); assert.equal(f.errors.length, 0);
});

for (const [name, mutate] of [
  ['offset', m => ({...m, start: m.start + 1})],
  ['size', m => ({...m, count: m.count + 1})],
  ['channel count', m => ({...m, channels: m.channels.slice(1)})],
  ['aliased channels', m => ({...m, channels: [m.channels[0], m.channels[0]]})],
  ['truncated buffer', m => ({...m, channels: [new ArrayBuffer(4), m.channels[1]]})],
  ['oversized buffer', m => ({...m, channels: [new ArrayBuffer(65537 * 4), m.channels[1]]})],
]) test(`malformed ${name} audio response terminates the worker, never exposes samples`, async () => {
  const f = rig(); await f.load(); const worker = f.workers[0]; worker.edit = m => m.kind === 'audio' ? mutate(m) : m;
  await assert.rejects(f.audios[0].pcm.read_window(0, 20));
  assert.equal(worker.stopped, 1); assert.equal(f.errors.length, 1); assert.equal(f.natives[0].freed, 1);
});

test('free and disposal reject pending audio and release native PCM exactly once', async () => {
  const f = rig(); await f.load(); f.workers[0].hold = 'audio';
  const pending = f.audios[0].pcm.read_window(0, 20);
  const rejected = assert.rejects(pending, {name: 'AbortError'}); f.session.dispose(); await rejected;
  await f.workers[0].finish(); f.session.dispose();
  assert.equal(f.natives[0].freed, 1); assert.equal(f.audios[0].disposed, 1);
  await assert.rejects(f.audios[0].pcm.read_window(0, 20), /freed/);
});

test('window request timeout terminates and releases the same native owners', async () => {
  const f = rig({timeoutMs: 25}); await f.load(); f.workers[0].hold = 'audio';
  await assert.rejects(f.audios[0].pcm.read_window(0, 20), /timed out/);
  assert.equal(f.natives[0].freed, 1); assert.equal(f.natives[0].playerFreed, 1);
});

test('worker restart rebinds the same audio source after a native render trap', async () => {
  const f = rig(); await f.load(); const audio = f.audios[0]; audio.seek(31);
  verify(await audio.pcm.read_window(100, 16), 100, 16);
  f.session.render(799); await flush(); assert.equal(f.errors.length, 1);
  assert(await f.session.restart()); assert.equal(f.session.current.audio, audio);
  assert.equal(audio.positionSeconds, 31 / 29); assert.equal(f.audios.length, 1);
  assert.equal(f.natives[1].copies.length, 0); verify(await audio.pcm.read_window(100, 16), 100, 16);
  assert.equal(f.natives[0].freed, 1); assert.equal(f.natives[1].freed, 0);
});

test('failed restart leaves the old PCM source readable', async () => {
  const f = rig(); await f.load(); const old = f.session.current; f.setMarker(2);
  await assert.rejects(f.session.restart(), /Recording changed/);
  assert.equal(f.session.current, old); verify(await f.audios[0].pcm.read_window(100, 16), 100, 16);
  assert.equal(f.natives[0].freed, 0);
});

test('asynchronous audio admission finishes before replacement of the previous movie', async () => {
  let release, first = true;
  const f = rig({adapter: async player => {
    const pcm = player.take_soundtrack(); await pcm.read_window(0, 16);
    if (!first) await new Promise(resolve => { release = resolve; }); first = false;
    return {dispose() { pcm.free(); }, pause() {}, play() {}, seek() {}, frameAt() { return 0; }};
  }});
  await f.load(); const old = f.session.current; const pending = f.load(); await flush();
  assert.equal(f.session.current, old); release(); assert(await pending);
  assert.notEqual(f.session.current, old); assert.equal(f.natives[0].freed, 1);
});

test('disposal during asynchronous admission cannot publish a late candidate', async () => {
  let release; const f = rig({adapter: async player => {
    const pcm = player.take_soundtrack(); await pcm.read_window(0, 1);
    await new Promise(resolve => { release = resolve; });
    return {dispose() { pcm.free(); }, pause() {}, play() {}, seek() {}, frameAt() { return 0; }};
  }});
  const pending = f.load(); await flush(); f.session.dispose(); release();
  assert.equal(await pending, false); assert.equal(f.session.current, null); assert.equal(f.natives[0].freed, 1);
});

test('worker independently rejects oversized and out-of-range windows before copying', async () => {
  for (const [start, count] of [[0, 65537], [32_000_000, 1], [-1, 1]]) {
    const f = rig(); await f.load();
    f.workers[0].postMessage({protocol: WORKER_PROTOCOL, id: 2, kind: 'audio', start, count}, []);
    await flush(); assert.deepEqual(f.natives[0].copies, []);
    assert.equal(f.workers[0].replies.at(-1).kind, 'error'); assert.equal(f.natives[0].freed, 1);
  }
});

test('pre-aborted audio windows send no request and do not poison the connection', async () => {
  const f = rig(); await f.load(); const controller = new AbortController(); controller.abort();
  await assert.rejects(f.audios[0].pcm.read_window(0, 2, controller.signal), {name: 'AbortError'});
  assert.equal(f.workers[0].requests.length, 1);
  verify(await f.audios[0].pcm.read_window(7, 2), 7, 2);
});

test('worker without negotiated window support cannot silently return eager PCM', async () => {
  const f = rig(); const pending = f.load(); await Promise.resolve();
  f.workers[0].edit = m => { if (m.kind === 'ready') { const n = {...m}; delete n.audioMode; return n; } return m; };
  await assert.rejects(pending, /bounded audio/); assert.equal(f.session.current, null);
  assert.equal(f.natives[0].freed, 1);
});
