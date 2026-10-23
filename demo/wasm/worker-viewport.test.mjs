import test, {afterEach} from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {PictureWorkerRuntime} from './picture-worker-core.mjs';
import {WorkerTimelineSession} from './worker-session.mjs';
import {WORKER_PROTOCOL} from './worker-protocol.mjs';

// Explicit native-API fixture, not a Wasm render oracle. Real structuredClone
// transfers prove buffer ownership; independent expected pixels bind geometry
// to both the requested frame and viewport, rather than mere nonempty output.
const flush = async () => { for (let i = 0; i < 8; i++) await new Promise(resolve => setImmediate(resolve)); };
const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r; }); return {promise, resolve}; };
function recording() {
  const body = new Uint8Array(25); body.set([70, 77, 84, 76]);
  new DataView(body.buffer).setBigUint64(16, 1n, true);
  const bytes = new Uint8Array(57); bytes.set(body);
  bytes.set(createHash('sha256').update(body).digest(), 25); return bytes;
}
function expected(frame, width, height) {
  return Uint8Array.from({length: width * height * 4}, (_, n) => (n + frame * 7 + width * 11 + height * 13) % 256);
}
const sessions = [];
afterEach(() => { for (const session of sessions.splice(0)) session.dispose(); });
function rig({sound = false, prepare, onFrame, timeoutMs = 2000} = {}) {
  const workers = [], facts = [], frames = [], errors = [], audioOwners = [];
  const f = {decodes: 0, reads: 0, nextDecode: null};
  const session = new WorkerTimelineSession({width: 4, height: 2, streamAudio: true, timeoutMs,
    onFrame(frame) {
      frames.push({...frame, rgba: frame.rgba.slice()});
      onFrame?.(frame, session);
    }, onError(error) { errors.push(error); },
    createWorker() {
      const state = {width: 0, height: 0, frees: 0, pcmFrees: 0, viewports: [], renders: [], copies: [], taken: 0};
      facts.push(state);
      const pcm = {sample_rate: 48000, channels: 2, sample_frames: 480000,
        copy_channel_into(channel, start, out) {
          state.copies.push({channel, start, count: out.length});
          for (let n = 0; n < out.length; n++) out[n] = (channel ? -1 : 1) * ((start + n) % 512) / 512;
        }, free() { state.pcmFrees++; }};
      const player = {frame_count: 100, fps: 25, duration_seconds: 4,
        engine_version: 'viewport-fixture', has_camera_track: true, has_soundtrack: sound,
        set_viewport(width, height) { state.viewports.push([width, height]); state.width = width; state.height = height; },
        render_into(frame, out) {
          if (state.failRender) throw new Error('fixture raster failure');
          assert.equal(out.byteLength, state.width * state.height * 4);
          state.renders.push([frame, state.width, state.height]);
          out.set(expected(frame, state.width, state.height));
        }, seek_frame() {}, labels() { return ['intro']; }, frame_of_label() { return 0; },
        take_soundtrack() { state.taken++; return pcm; }, free() { state.frees++; }};
      const worker = {onmessage: null, onerror: null, onmessageerror: null,
        requests: [], outgoingBytes: [], held: [], hold: null, stopped: 0, edit: value => value,
        postMessage(message, transfer) {
          this.requests.push({kind: message.kind, frame: message.frame, width: message.width, height: message.height,
            bytes: message.pixels?.byteLength, start: message.start, count: message.count});
          const copy = structuredClone(message, {transfer});
          for (const buffer of transfer) assert.equal(buffer.byteLength, 0);
          const send = () => runtime.receive(copy);
          if (this.hold === message.kind) this.held.push(send); else queueMicrotask(send);
        }, terminate() { this.stopped++; runtime.dispose(); },
        async finish() { assert(this.held.length); await this.held.shift()(); await flush(); }};
      const gate = f.nextDecode; f.nextDecode = null;
      const runtime = new PictureWorkerRuntime(async bytes => {
        f.decodes++;
        assert.deepEqual(Buffer.from(bytes.subarray(-32)), createHash('sha256').update(bytes.subarray(0, -32)).digest());
        if (gate) await gate;
        return player;
      }, (message, transfer) => {
        const copy = structuredClone(message, {transfer});
        worker.outgoingBytes.push(copy.pixels?.byteLength ?? 0);
        for (const buffer of transfer) assert.equal(buffer.byteLength, 0);
        queueMicrotask(() => { if (!worker.stopped) worker.onmessage?.({data: worker.edit(copy)}); });
      });
      workers.push(worker); return worker;
    },
    prepareAudio: prepare ?? (player => {
      const pcm = player.take_soundtrack();
      const audio = {pcm, frame: 13, positionSeconds: 0.525, playing: true, disposed: 0, pauses: 0,
        play() { this.playing = true; }, frameAt() { return this.frame; },
        pause() { this.pauses++; this.playing = false; }, seek(frame) { this.frame = frame; },
        dispose() { this.disposed++; pcm.free(); }};
      audioOwners.push(audio); return audio;
    }),
  });
  sessions.push(session);
  return Object.assign(f, {session, workers, facts, frames, errors, audioOwners,
    load: () => session.load(async () => { f.reads++; return recording(); })});
}
function checkLast(f, frame, width, height) {
  const value = f.frames.at(-1); assert(value);
  assert.deepEqual([value.index, value.width, value.height], [frame, width, height]);
  assert.deepEqual(value.rgba, expected(frame, width, height));
}

test('live resize preserves the decoded recording, audio owner, and fractional device position', async () => {
  const f = rig({sound: true}); await f.load();
  const current = f.session.current, audio = current.audio;
  f.session.render(13); await flush();
  assert.equal(f.session.resize(12, 7), true); await flush(); checkLast(f, 13, 12, 7);
  assert.equal(f.session.current, current); assert.equal(f.decodes, 1); assert.equal(f.reads, 1);
  assert.equal(f.audioOwners.length, 1); assert.equal(audio.positionSeconds, 0.525);
  assert.equal(audio.pauses, 0); assert.equal(audio.playing, true); assert.equal(audio.disposed, 0);
  assert.equal(f.facts[0].taken, 1); assert.equal(f.facts[0].pcmFrees, 0);
  const chunks = await audio.pcm.read_window(67, 32);
  assert.equal(chunks[0][0], 67 / 512); assert.equal(chunks[1][31], -98 / 512);
});

test('duplicate sizes are no-ops and subsequent renders use the resized buffer', async () => {
  const f = rig(); await f.load();
  assert.equal(f.session.resize(4, 2), false); assert.equal(f.workers[0].requests.length, 1);
  f.session.resize(8, 5); await flush(); f.session.render(9); await flush();
  checkLast(f, 9, 8, 5);
  assert.deepEqual(f.workers[0].requests.map(r => [r.kind, r.bytes]), [['load', undefined], ['resize', 32], ['render', 160]]);
});

test('equal-area portrait rotation renders native geometry, not a stretched old bitmap', async () => {
  const f = rig(); await f.load(); f.session.resize(2, 4); await flush();
  checkLast(f, 0, 2, 4); assert.deepEqual(f.facts[0].viewports, [[4, 2], [2, 4]]);
});

test('invalid sizes refuse before scheduling or changing the next load', async () => {
  const f = rig({sound: true}); await f.load(); const previous = f.session.current;
  for (const [w, h] of [[0, 2], [-1, 1], [1, 4097], [4097, 1], [NaN, 1], [Infinity, 1], [1.2, 2], ['4', 2], [true, 2]]) {
    assert.throws(() => f.session.resize(w, h), /viewport/);
  }
  assert.equal(f.workers[0].requests.length, 1); assert.equal(f.session.current, previous);
  assert.equal(previous.audio.pauses, 0); await f.load();
  assert.deepEqual(f.facts[1].viewports, [[4, 2]]);
});

test('viewport can be selected before admission and at both dimension boundaries', async () => {
  const f = rig(); assert(f.session.resize(1, 4096)); assert.equal(f.workers.length, 0);
  await f.load(); f.session.present(); checkLast(f, 0, 1, 4096);
  f.session.resize(4096, 1); await flush(); checkLast(f, 0, 4096, 1);
});

test('ten thousand resizes coalesce behind one raster and suppress the old viewport', async () => {
  const f = rig(); await f.load(); const worker = f.workers[0]; worker.hold = 'render';
  f.session.render(7);
  for (let n = 0; n < 10000; n++) f.session.resize(10 + n % 100, 8);
  assert.equal(worker.requests.length, 2); assert.equal(worker.held.length, 1);
  await worker.finish(); checkLast(f, 7, 109, 8);
  assert.equal(f.frames.length, 1);
  assert.deepEqual(worker.requests.map(r => r.kind), ['load', 'render', 'resize']);
});

test('a new size and seek replace an already running resize without stale presentation', async () => {
  const f = rig(); await f.load(); const worker = f.workers[0]; worker.hold = 'resize';
  f.session.resize(20, 10); f.session.render(42); f.session.resize(30, 17);
  await worker.finish(); assert.equal(f.frames.length, 0); assert.equal(worker.held.length, 1);
  await worker.finish(); checkLast(f, 42, 30, 17);
  assert.deepEqual(worker.requests.map(r => [r.kind, r.frame]), [['load', undefined], ['resize', 0], ['resize', 42]]);
});

test('changing back to the original size still rejects the obsolete raster', async () => {
  const f = rig(); await f.load(); const worker = f.workers[0]; worker.hold = 'render';
  f.session.render(5); f.session.resize(20, 10); f.session.resize(4, 2);
  await worker.finish(); checkLast(f, 5, 4, 2);
  assert.equal(f.frames.length, 1); assert.equal(worker.requests.length, 2);
});

test('audio retains priority across viewport and seek storms', async () => {
  const f = rig({sound: true}); await f.load(); const worker = f.workers[0]; worker.hold = 'render';
  f.session.render(3); f.session.resize(20, 10);
  const audio = f.audioOwners[0].pcm.read_window(0, 8);
  f.session.render(19); f.session.resize(11, 13);
  await worker.finish(); await audio; await flush(); checkLast(f, 19, 11, 13);
  assert.deepEqual(worker.requests.map(r => r.kind), ['load', 'render', 'audio', 'resize']);
  assert.equal(f.audioOwners[0].pauses, 0);
});

test('a resize invoked by a frame observer waits until the borrowed pixels return', async () => {
  let changed = false;
  const f = rig({onFrame(frame, session) {
    if (changed) return; changed = true;
    const before = frame.rgba.slice(); session.resize(8, 9); session.render(frame.index);
    assert.deepEqual(frame.rgba, before); assert.notEqual(frame.rgba.byteLength, 0);
  }});
  await f.load(); f.session.present(); await flush(); checkLast(f, 0, 8, 9);
  assert.equal(f.frames.length, 2);
});

test('resize during native decode keeps first-response correlation and latest dimensions', async () => {
  const f = rig(), gate = deferred(); f.nextDecode = gate.promise;
  const pending = f.load(); await flush(); f.session.resize(7, 11); f.session.resize(9, 12);
  gate.resolve(); assert(await pending); await flush(); f.session.present();
  checkLast(f, 0, 9, 12); assert.equal(f.decodes, 1); assert.equal(f.errors.length, 0);
});

test('resize while the file is being read applies before worker creation', async () => {
  const f = rig(), gate = deferred();
  const pending = f.session.load(() => gate.promise);
  f.session.resize(17, 9); gate.resolve(recording()); await pending; f.session.present();
  checkLast(f, 0, 17, 9); assert.equal(f.workers[0].requests.length, 1);
});

test('failed replacement leaves the existing resized movie and audio usable', async () => {
  const f = rig({sound: true}); await f.load(); const previous = f.session.current;
  const gate = deferred(), pending = f.session.load(() => gate.promise);
  f.session.resize(17, 8); await flush(); gate.resolve(new Uint8Array(1));
  await assert.rejects(pending); assert.equal(f.session.current, previous);
  checkLast(f, 0, 17, 8); assert.equal(previous.audio.disposed, 0);
});

test('restart uses the latest viewport and preserves the same paused soundtrack', async () => {
  const f = rig({sound: true}); await f.load(); const audio = f.session.current.audio;
  f.session.resize(15, 16); await flush();
  assert(await f.session.restart()); f.session.present(); checkLast(f, 13, 15, 16);
  assert.equal(f.session.current.audio, audio); assert.equal(audio.positionSeconds, 0.525);
  assert.equal(f.audioOwners.length, 1); assert.equal(f.facts[0].pcmFrees, 1);
  assert.equal((await audio.pcm.read_window(3, 4))[0][0], 3 / 512);
});

test('a viewport change while restarting reaches the replacement as well', async () => {
  const f = rig({sound: true}); await f.load(); const gate = deferred(); f.nextDecode = gate.promise;
  const pending = f.session.restart(); await flush(); f.session.resize(11, 6);
  gate.resolve(); assert(await pending); await flush(); f.session.present();
  checkLast(f, 13, 11, 6); assert.equal(f.audioOwners.length, 1);
});

for (const [name, edit] of [
  ['dimensions', value => ({...value, width: value.width + 1})],
  ['buffer', value => ({...value, pixels: new ArrayBuffer(4)})],
  ['frame', value => ({...value, frame: value.frame + 1})],
  ['kind', value => ({...value, kind: 'ready'})],
]) test(`malformed resize ${name} stops without publishing a false picture`, async () => {
  const f = rig({sound: true}); await f.load(); f.workers[0].edit = edit;
  f.session.resize(8, 8); await flush();
  assert.equal(f.frames.length, 0); assert.equal(f.errors.length, 1);
  assert.equal(f.workers[0].stopped, 1); assert.equal(f.audioOwners[0].pauses, 1);
  assert.equal(f.facts[0].pcmFrees, 1);
});

test('native resize rendering failure remains restartable at the desired size', async () => {
  const f = rig({sound: true}); await f.load(); f.facts[0].failRender = true;
  f.session.resize(9, 5); await flush(); assert.equal(f.errors.length, 1);
  f.session.resize(12, 8); assert(await f.session.restart()); f.session.present();
  checkLast(f, 13, 12, 8); assert.equal(f.audioOwners.length, 1);
});

test('disposal terminates an in-flight resize and rejects further resizing', async () => {
  const f = rig(); await f.load(); f.workers[0].hold = 'resize'; f.session.resize(10, 8);
  f.session.dispose(); await f.workers[0].finish(); assert.equal(f.frames.length, 0);
  assert.equal(f.workers[0].stopped, 1); assert.equal(f.facts[0].frees, 1);
  assert.throws(() => f.session.resize(3, 4), /disposed/);
});

test('worker validates hostile resize requests before touching native viewport state', async () => {
  for (const bad of [{width: 0}, {height: 4097}, {frame: 100}, {pixels: new ArrayBuffer(1)}]) {
    const sizes = [], replies = [];
    const runtime = new PictureWorkerRuntime(async () => ({frame_count: 100, fps: 25, duration_seconds: 4,
      engine_version: 'fixture', has_camera_track: false, has_soundtrack: false,
      set_viewport(w, h) { sizes.push([w, h]); }, labels() { return []; },
      render_into() {}, seek_frame() {}, free() {}}), message => replies.push(message));
    await runtime.receive({protocol: WORKER_PROTOCOL, id: 1, kind: 'load', bytes: recording().buffer,
      width: 4, height: 2, maxBundleBytes: 1024, maxAudioBytes: 1024});
    await runtime.receive({protocol: WORKER_PROTOCOL, id: 2, kind: 'resize', frame: 0,
      width: 8, height: 9, pixels: new ArrayBuffer(32), ...bad});
    assert.deepEqual(sizes, [[4, 2]]); assert.equal(replies.at(-1).kind, 'error'); runtime.dispose();
  }
});
