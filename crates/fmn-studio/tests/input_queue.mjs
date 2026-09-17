// Exercise the shipped controller, not a parallel event-queue implementation.
// DOM/HTTP fixtures isolate admission, ordering and ownership. browser.mjs is the
// independent real Chrome + native subprocess/pixel acceptance suite.
import assert from 'node:assert/strict';
import {test} from 'node:test';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';

const source = readFileSync(new URL('../src/studio.js', import.meta.url), 'utf8');
const start = source.indexOf('  async function run(');
const end = source.indexOf('  // FrameHub parts', start);
assert.ok(start >= 0 && end > start, 'production input controller boundaries');
const controller = source.slice(start, end);

class Element {
  constructor() { this.events = new Map(); this.style = {}; this.checked = true; this.capture = new Set(); }
  addEventListener(name, callback) {
    if (!this.events.has(name)) this.events.set(name, []);
    this.events.get(name).push(callback);
  }
  emit(name, values = {}) {
    const event = {key:'', button:0, buttons:0, pointerId:1, clientX:48, clientY:27,
      ctrlKey:false, shiftKey:false, altKey:false, metaKey:false, preventDefault() {}, ...values};
    for (const callback of this.events.get(name) || []) callback(event);
  }
  getBoundingClientRect() { return {left:0, top:0, width:96, height:54}; }
  focus() {}
  setPointerCapture(id) { this.capture.add(id); }
  hasPointerCapture(id) { return this.capture.has(id); }
  releasePointerCapture(id) { this.capture.delete(id); this.emit('lostpointercapture', {pointerId:id}); }
}
function fixture() {
  const elements = new Map(), $ = id => {
    if (!elements.has(id)) elements.set(id, new Element());
    return elements.get(id);
  };
  const window = new Element(), document = new Element();
  let frame = 5, revision = 0, generation = 3, release, first = true;
  const gate = new Promise(resolve => { release = resolve; });
  const calls = [], errors = [], control = {fail:false, conflict:false};
  const state = {snapshot:null, shown:null, expected:null, connected:true, generation,
    inputs:[], restartPending:false, busy:false, pending:null, playing:false,
    collapsed:new Set(), refreshNeeded:false};
  const refresh = async () => {
    state.generation = generation;
    state.snapshot = {view:{frame_index:frame, frame_count:61, width:96, height:54,
      scale:6, origin:[48,27], fps:30, input_events:true, input_revision:revision}};
  };
  void refresh();
  const api = async (path, fields) => {
    calls.push({path, ...fields});
    if (first) { first = false; await gate; }
    if (path === '/api/event') {
      if (control.fail) throw new Error('native refusal');
      assert.equal(Number(fields.worker_generation), generation, 'original worker owner');
      assert.equal(Number(fields.revision), revision, 'optimistic revision');
      revision += control.conflict ? 2 : 1;
    } else if (path === '/api/restart') generation++;
    else if (path === '/api/scrub') { frame = Number(fields.frame); if (fields.commit === 'true') revision++; }
    return {frame_index:frame, sha256:'f'.repeat(64), worker_generation:generation,
      reused_entries:revision, replayed_entries:0, reexecuted_entries:0};
  };
  const context = vm.createContext({$,window,document,state,api,refresh,
    clearError() {}, report(error) { errors.push(error.message); }, displayState() {},
    matchesFrame() { return true; }, requestAnimationFrame() {}});
  vm.runInContext(controller + '\n globalThis.queue = {enqueueInput,seek,heldKeys};', context);
  return {$,window,document,state,calls,errors,control,release,queue:context.queue,
    async settled() {
      release();
      for (let i=0; i<1000 && (state.busy || state.inputs.length || state.restartPending || state.pending); i++) {
        await new Promise(resolve => setImmediate(resolve));
      }
      assert.equal(state.busy, false, 'controller must settle');
    }};
}
const eventCalls = f => f.calls.filter(call => call.path === '/api/event');
function gesture(f) {
  const preview = f.$('preview');
  preview.emit('keydown', {key:'g'});
  preview.emit('pointerdown', {buttons:1});
  preview.emit('pointermove', {buttons:1, clientX:50});
  preview.emit('pointermove', {buttons:1, clientX:53});
  preview.emit('pointerup', {clientX:55});
  f.window.emit('keyup', {key:'g'});
}

test('rapid held-key drag preserves every admitted transition while rendering is in flight', async () => {
  const f = fixture(); gesture(f);
  assert.equal(f.calls.length, 1);
  await f.settled();
  const calls = eventCalls(f);
  assert.deepEqual(calls.map(x => x.type), ['key_press','mouse_press','mouse_drag','mouse_drag','mouse_release','key_release']);
  assert.deepEqual(calls.map(x => x.revision), ['0','1','2','3','4','5']);
  assert.ok(calls.every(x => x.frame === '5' && x.worker_generation === '3'));
  assert.deepEqual(calls.filter(x => x.type === 'mouse_drag').map(x => x.dx), [1/3, 1/2]);
  assert.equal(f.errors.length, 0);
});

test('restart waits for accepted gesture transitions and synthetic releases', async () => {
  const f = fixture(), preview = f.$('preview');
  preview.emit('keydown', {key:'g'});
  preview.emit('pointerdown', {buttons:1});
  preview.emit('pointermove', {buttons:1, clientX:60});
  f.$('restart').emit('click');
  await f.settled();
  assert.deepEqual(f.calls.map(x => x.type || x.path), ['key_press','mouse_press','mouse_drag','mouse_release','key_release','/api/restart']);
  assert.equal(f.state.generation, 4);
  assert.equal(f.errors.length, 0);
});

test('scrub releases held native edit modes before changing frame ownership', async () => {
  const f = fixture(); f.$('preview').emit('keydown', {key:'t'});
  f.queue.seek(20);
  await f.settled();
  assert.deepEqual(f.calls.map(x => x.type || x.path), ['key_press','key_release','/api/scrub']);
  assert.equal(f.state.snapshot.view.frame_index, 20);
});

for (const trigger of ['window-blur','canvas-blur','hidden','disable','cancel','lost']) {
  test(`${trigger} releases captured input exactly once`, async () => {
    const f = fixture(), preview = f.$('preview');
    preview.emit('keydown', {key:'g'}); preview.emit('pointerdown', {buttons:1});
    if (trigger === 'window-blur') f.window.emit('blur');
    if (trigger === 'canvas-blur') preview.emit('blur');
    if (trigger === 'hidden') { f.document.hidden = true; f.document.emit('visibilitychange'); }
    if (trigger === 'disable') { f.$('input-events').checked = false; f.$('input-events').emit('change'); }
    if (trigger === 'cancel') preview.emit('pointercancel');
    if (trigger === 'lost') preview.emit('lostpointercapture');
    f.window.emit('keyup', {key:'g'}); preview.emit('pointerup');
    await f.settled();
    assert.deepEqual(eventCalls(f).map(x => x.type), ['key_press','mouse_press','mouse_release','key_release']);
    assert.equal(f.errors.length, 0);
  });
}

test('bounded queue preserves release capacity without dropping earlier accepted input', async () => {
  const f = fixture(), preview = f.$('preview');
  preview.emit('keydown', {key:'g'}); preview.emit('pointerdown', {buttons:1});
  for (let i=0; i<400; i++) preview.emit('pointermove', {buttons:1, clientX:50});
  assert.equal(f.state.inputs.length, 256);
  preview.emit('pointerup'); f.window.emit('keyup', {key:'g'});
  assert.equal(f.state.inputs.length, 258, 'only owned release transitions may use reserved capacity');
  await f.settled();
  assert.equal(eventCalls(f).length, 259);
  assert.deepEqual(eventCalls(f).slice(-2).map(x => x.type), ['mouse_release','key_release']);
});

for (const mode of ['fail','conflict']) {
  test(`${mode} discards later actions instead of retrying against another scene revision`, async () => {
    const f = fixture(); gesture(f); f.control[mode] = true;
    await f.settled();
    assert.equal(eventCalls(f).length, 1);
    assert.equal(f.state.inputs.length, 0);
    assert.equal(f.$('input-events').checked, false);
    assert.equal(f.errors.length, 1);
  });
}

test('stale admitted owner is rejected locally without sending it to a replacement worker', async () => {
  const f = fixture();
  f.queue.enqueueInput({type:'key_press', key:'g'}, {generation:2,frame:5});
  await f.settled();
  assert.equal(f.calls.length, 0);
  assert.equal(f.errors.length, 1);
});
