import test, {afterEach} from 'node:test';
import {createHash} from 'node:crypto';
import assert from 'node:assert/strict';
import {PictureWorkerRuntime} from './picture-worker-core.mjs';
import {WorkerTimelineSession, transferredSoundtrack} from './worker-session.mjs';
import {WORKER_PROTOCOL, metadata, audioInfo} from './worker-protocol.mjs';
import {prepareSoundtrack} from './audio-playback.mjs';

// A canonical framed envelope with explicit fixture payload, not native scene
// bytes. The native-API double verifies the real SHA-256 envelope before return.
function recording(payload = [1]) {
  const body = new Uint8Array(24 + payload.length);
  body.set([70,77,84,76]); body.set(payload, 24);
  new DataView(body.buffer).setBigUint64(16, BigInt(payload.length), true);
  const bytes = new Uint8Array(body.length + 32); bytes.set(body);
  bytes.set(createHash('sha256').update(body).digest(), body.length);
  return bytes;
}
function verifyEnvelope(bytes) {
  const actual = createHash('sha256').update(bytes.subarray(0, -32)).digest();
  if (!Buffer.from(bytes.subarray(-32)).equals(actual)) throw new Error('Native checksum refusal.');
}
const sessions = new Set();
afterEach(() => { for (const session of sessions) session.dispose(); sessions.clear(); });
const flush = async () => { for (let i = 0; i < 24; i++) await Promise.resolve(); };
const deferred = () => { let resolve, reject; const promise = new Promise((a,b) => {resolve=a;reject=b;}); return {promise,resolve,reject}; };
const expectedPixels = (frame, length = 32) => Uint8Array.from({length}, (_,i) => (frame * 31 + i) % 256);
function native(options = {}) {
  const {sound = false, frames = 16, channels = 2, rate = 8, frameCount = 12, fps = 4} = options;
  const facts = {frees: 0, pcmFrees: 0, copies: [], renders: [], inputs: []};
  const samples = Array.from({length: channels}, (_,c) => Float32Array.from({length: frames}, (_,i) => (c ? -1 : 1) * (i % 127) / 128));
  const pcm = {channels, sample_frames: frames, sample_rate: rate,
    free() { facts.pcmFrees++; }, copy_channel_into(c, start, out) {
      facts.copies.push([c,start,out.length]);
      if (options.copyFailure) throw new Error('native copy failure');
      out.set(samples[c].subarray(start, start + out.length));
    }};
  const player = {frame_count: frameCount, fps, duration_seconds: frameCount / fps,
    engine_version: 'fixture:exact', has_camera_track: true, has_soundtrack: sound,
    set_viewport(w,h) { this.width=w; this.height=h; }, labels() { return ['origin','tail']; },
    frame_of_label(name) { return name === 'origin' ? 0 : frameCount - 1; },
    take_soundtrack() { this.has_soundtrack=false; return pcm; }, seek_frame(n) { this.current_frame=n; },
    render_into(frame, out) { facts.renders.push(frame); if (frame === options.failFrame) throw new Error('native render failure'); out.set(expectedPixels(frame,out.length)); },
    free() { facts.frees++; },
  };
  return {facts, player, samples, pcm};
}
class FakeWorker {
  onmessage = null; onerror = null; onmessageerror = null;
  stopped = 0; requests = []; held = []; holdRender = false; editReply = value => value;
  constructor(factory) {
    this.runtime = new PictureWorkerRuntime(factory, (message, transfer) => {
      const value = structuredClone(message, {transfer});
      queueMicrotask(() => { if (!this.stopped) this.onmessage?.({data: this.editReply(value)}); });
    });
  }
  postMessage(message, transfer) {
    const value = structuredClone(message, {transfer});
    this.requests.push({kind: value.kind, frame: value.frame, id: value.id});
    const run = () => this.runtime.receive(value);
    if (value.kind === 'render' && this.holdRender) this.held.push(run);
    else queueMicrotask(run);
  }
  async finish() { await this.held.shift()?.(); await flush(); }
  terminate() { this.stopped++; this.runtime.dispose(); }
}
function rig(options = {}, extra = {}) {
  const workers = [], natives = [], frames = [], errors = [], audio = [];
  const session = new WorkerTimelineSession({width: 4, height: 2,
    createWorker() {
      const value = native(options); natives.push(value);
      const worker = new FakeWorker(async bytes => { verifyEnvelope(bytes); value.facts.inputs.push([...bytes]); return value.player; });
      workers.push(worker); return worker;
    },
    prepareAudio: options.sound ? player => {
      const pcm = player.take_soundtrack();
      const value = {pcm, disposed:0, pauses:0, dispose(){this.disposed++;pcm.free();}, pause(){this.pauses++;}, play(){}, seek(){}, frameAt(){}};
      audio.push(value); return value;
    } : null,
    onFrame: frame => frames.push({index: frame.index, bytes: frame.rgba.slice()}),
    onError: error => errors.push(error), ...extra});
  sessions.add(session);
  return {session, workers, natives, frames, errors, audio,
    load(bytes = recording([70,77,84,76])) {
      const snapshot = bytes.slice(); let first = true;
      return session.load(async () => { const next = first ? bytes : snapshot.slice(); first = false; return next; });
    }};
}

test('one worker owns decode, first picture and all later raster work', async () => {
  const f = rig(), bytes = recording([70,77,84,76]);
  assert(await f.load(bytes)); assert.equal(bytes.byteLength,0);
  assert.equal(f.frames.length,0); f.session.present();
  f.session.render(4); await flush();
  assert.deepEqual(f.natives[0].facts.renders,[0,4]);
  assert.deepEqual(f.frames.map(f=>f.index),[0,4]);
  assert.deepEqual(f.frames[1].bytes,expectedPixels(4));
  assert.equal(f.session.current.metadata.hasCameraTrack,true);
  f.session.dispose(); assert.equal(f.workers[0].stopped,1); assert.equal(f.natives[0].facts.frees,1);
});
test('subarray loads transfer only the selected input bytes, not unrelated storage',async()=>{
  const f=rig(), bytes=recording([70,77,84,76]), all=new Uint8Array(bytes.length+2).fill(99);
  all.set(bytes,1);assert(await f.load(all.subarray(1,1+bytes.length)));assert.equal(all.byteLength,bytes.length+2);
  assert.deepEqual(f.natives[0].facts.inputs,[[...bytes]]);f.session.dispose();
});
test('ten thousand seeks retain only one raster and the latest pending index',async()=>{
  const f=rig();await f.load();f.workers[0].holdRender=true;
  f.session.render(1);for(let i=0;i<10000;i++)f.session.render(2+i%10);
  assert.equal(f.workers[0].requests.length,2);assert.equal(f.workers[0].held.length,1);
  await f.workers[0].finish();assert.equal(f.workers[0].requests.length,3);
  await f.workers[0].finish();assert.deepEqual(f.natives[0].facts.renders,[0,1,11]);
  assert.deepEqual(f.frames.map(f=>f.index),[1,11]);f.session.dispose();
});
test('an explicit seek suppresses the old raster but recycles its returned storage',async()=>{
  const f=rig();await f.load();f.workers[0].holdRender=true;
  f.session.render(5);f.session.invalidate();f.session.render(2);
  await f.workers[0].finish();assert.equal(f.frames.length,0);
  await f.workers[0].finish();assert.deepEqual(f.frames.map(f=>f.index),[2]);
  assert.deepEqual(f.frames[0].bytes,expectedPixels(2));f.session.dispose();
});
test('seeking back to the displayed frame cannot reuse a stale cancelled raster',async()=>{
  const f=rig();await f.load();f.workers[0].holdRender=true;
  f.session.render(5);f.session.invalidate();f.session.render(0);
  await f.workers[0].finish();assert.deepEqual(f.workers[0].requests.map(x=>x.frame),[undefined,5,0]);
  await f.workers[0].finish();assert.deepEqual(f.frames[0],{index:0,bytes:expectedPixels(0)});f.session.dispose();
});
test('an observer may enqueue another frame without detaching its borrowed view early',async()=>{
  const seen=[];let session;
  const f=rig({}, {onFrame(frame){const copy=frame.rgba.slice();if(frame.index===1)session.render(2);assert.deepEqual(frame.rgba,copy);seen.push(frame.index);}});
  session=f.session;await f.load();session.render(1);await flush();assert.deepEqual(seen,[1,2]);session.dispose();
});
test('out-of-range seeks leave the active request and previous movie intact',async()=>{
  const f=rig();await f.load();f.workers[0].holdRender=true;f.session.render(3);
  for(const index of [-1,12,1.5,NaN,Infinity])assert.throws(()=>f.session.render(index));
  await f.workers[0].finish();assert.equal(f.frames[0].index,3);f.session.dispose();
});
test('native PCM crosses ownership once with bounded copies and exact stereo bits',async()=>{
  const f=rig({sound:true,frames:65539});await f.load();
  assert.deepEqual(f.natives[0].facts.copies,[[0,0,65536],[0,65536,3],[1,0,65536],[1,65536,3]]);
  assert.equal(f.natives[0].facts.pcmFrees,1);
  for(let c=0;c<2;c++){const dest=new Float32Array(65539);f.audio[0].pcm.copy_channel_into(c,0,dest);assert.deepEqual(dest,f.natives[0].samples[c]);}
  f.session.render(8);await flush();assert.equal(f.natives[0].facts.copies.length,4);
  f.session.dispose();assert.equal(f.audio[0].disposed,1);assert.throws(()=>f.audio[0].pcm.copy_channel_into(0,0,new Float32Array(1)),/freed/);
});
test('transferred PCM uses the existing windowed playback scheduler without another native player',async()=>{
  const device={state:'suspended',sampleRate:48000,currentTime:0,destination:{},closed:0,
    resume(){this.state='running';return Promise.resolve();},close(){this.closed++;this.state='closed';return Promise.resolve();},
    createBuffer(c,n,sr){const arrays=Array.from({length:c},()=>new Float32Array(n));return {duration:n/sr,getChannelData:i=>arrays[i]};},
    createBufferSource(){return {connect(){},start(){},stop(){},disconnect(){}}}};
  const f=rig({sound:true,frames:150000,rate:48000},{prepareAudio:player=>prepareSoundtrack(player,()=>device)});
  await f.load();const audio=f.session.current.audio;assert(await audio.play());
  device.currentTime=0.2;assert.equal(audio.frameAt(),0);audio.seek(2);assert(await audio.play());
  assert.equal(f.natives.length,1);f.session.dispose();assert.equal(device.closed,1);
});
test('invalid PCM windows do not mutate caller destinations',()=>{
  const pcm=transferredSoundtrack(audioInfo({sampleRate:8,channels:1,sampleFrames:2}),[new Float32Array([.25,.5]).buffer]);
  for(const [c,start,len] of [[1,0,1],[0,-1,1],[0,2,1],[0,0,3]]){const dst=new Float32Array(len).fill(99);assert.throws(()=>pcm.copy_channel_into(c,start,dst));assert(dst.every(n=>n===99));}
  pcm.free();pcm.free();
});
test('failure of native decode, first render or audio preparation preserves the previous movie',async()=>{
  for(const stage of ['decode','picture','audio']){
    let n=0;const workers=[];
    const session=new WorkerTimelineSession({width:4,height:2,
      createWorker(){const value=native({sound:stage==='audio',failFrame:n++===1&&stage==='picture'?0:undefined});
        const number=n;const worker=new FakeWorker(async()=>{if(number===2&&stage==='decode')throw new Error('decode refused');return value.player;});workers.push(worker);return worker;},
      prepareAudio(player){const pcm=player.take_soundtrack();if(n===2)throw new Error('audio refused');return {dispose(){pcm.free();},pause(){},play(){},seek(){},frameAt(){}};}});
    assert(await session.load(async()=>recording([1])));const prior=session.current;
    await assert.rejects(session.load(async()=>recording([2])));assert.equal(session.current,prior);
    assert.equal(workers[0].stopped,0);assert.equal(workers[1].stopped,1);session.render(2);await flush();session.dispose();
  }
});
test('a newer load terminates an old decoder and does not wait for it to finish',async()=>{
  const slow=deferred(), first=native(), second=native();const workers=[];
  const session=new WorkerTimelineSession({width:4,height:2,createWorker(){const index=workers.length;const w=new FakeWorker(()=>index?second.player:slow.promise);workers.push(w);return w;}});
  const old=session.load(async()=>recording([1]));await flush();
  const latest=session.load(async()=>recording([2]));assert(await latest);assert.equal(await old,false);
  assert.equal(workers[0].stopped,1);slow.resolve(first.player);await flush();assert.equal(first.facts.frees,1);
  assert.equal(session.current.metadata.engineVersion,'fixture:exact');session.dispose();
});
test('superseded input reads receive cancellation and cannot create late workers',async()=>{
  const f=rig(), read=deferred();let signal;
  const old=f.session.load(s=>{signal=s;return read.promise;});await flush();assert(await f.load());assert(signal.aborted);
  read.resolve(recording([9]));assert.equal(await old,false);assert.equal(f.workers.length,1);f.session.dispose();
});
test('dispose terminates a hung load and releases current audio without late resurrection',async()=>{
  const f=rig({sound:true});await f.load();const read=deferred();const pending=f.session.load(()=>read.promise);
  f.session.dispose();read.resolve(recording([1]));assert.equal(await pending,false);assert.equal(f.audio[0].disposed,1);
  await assert.rejects(f.load(),/disposed/);assert.equal(f.session.current,null);
});
test('worker deadline rejects a hung candidate and keeps the previous generation',async()=>{
  const wait=deferred();let count=0;const workers=[];
  const session=new WorkerTimelineSession({width:4,height:2,timeoutMs:15,createWorker(){const value=native();const which=count++;const w=new FakeWorker(()=>which?wait.promise:value.player);workers.push(w);return w;}});
  await session.load(async()=>recording([1]));const prior=session.current;
  await assert.rejects(session.load(async()=>recording([2])),/timed out/);assert.equal(session.current,prior);assert.equal(workers[1].stopped,1);
  session.dispose();wait.resolve(native().player);await flush();
});
test('worker protocol corruption stops audio and never publishes wrong pixels',async()=>{
  for(const edit of [v=>({...v,id:v.id+1}),v=>({...v,frame:v.frame+1}),v=>({...v,width:9}),v=>({...v,pixels:new ArrayBuffer(0)}),v=>({...v,protocol:'other'})]){
    const f=rig({sound:true});await f.load();f.workers[0].editReply=edit;f.session.render(3);await flush();
    assert.equal(f.frames.length,0);assert.equal(f.audio[0].pauses,1);assert.equal(f.errors.length,1);assert.equal(f.workers[0].stopped,1);
    assert.throws(()=>f.session.render(4));f.session.dispose();
  }
});
test('native render errors and worker exceptions stop the current audio owner',async()=>{
  for(const mode of ['native','error','messageerror']){
    const f=rig({sound:true,failFrame:2});await f.load();
    if(mode==='native')f.session.render(2);else f.workers[0][`on${mode}`]({message:'worker crash'});
    await flush();assert.equal(f.errors.length,1);assert.equal(f.audio[0].pauses,1);assert.equal(f.frames.length,0);f.session.dispose();
  }
});
test('invalid or oversized input is refused before allocating any worker',async()=>{
  const f=rig({}, {maxBytes:4});for(const value of [new Uint8Array(5),new ArrayBuffer(4),null])await assert.rejects(f.session.load(async()=>value));
  assert.equal(f.workers.length,0);f.session.dispose();
});
test('native audio byte admission and channel-copy failures release every candidate',async()=>{
  for(const kind of ['budget','copy']){
    const f=rig({sound:true,copyFailure:kind==='copy'},{maxAudioBytes:kind==='budget'?4:1024});await assert.rejects(f.load());
    assert.equal(f.natives[0].facts.frees,1);assert.equal(f.natives[0].facts.pcmFrees,1);assert.equal(f.session.current,null);f.session.dispose();
  }
});
test('malformed ready metadata and transferred channels refuse candidate publication',async()=>{
  for(const edit of [v=>({...v,metadata:{...v.metadata,frameCount:0}}),v=>({...v,channels:[]}),v=>({...v,channels:[new ArrayBuffer(4),new ArrayBuffer(4)]}),v=>({...v,metadata:{...v.metadata,labels:[{name:'bad',index:999}]}})]){
    const f=rig({sound:true});const pending=f.load();await Promise.resolve();f.workers[0].editReply=edit;
    await assert.rejects(pending);assert.equal(f.session.current,null);assert.equal(f.workers[0].stopped,1);f.session.dispose();
  }
});
test('runtime refuses reinitialization, concurrent commands and invalid viewport before native work',async()=>{
  for(const bad of [{width:0},{maxBundleBytes:0},{bytes:new ArrayBuffer(9),maxBundleBytes:8},{id:2},{protocol:'other'}]){
    let created=0;const messages=[];const runtime=new PictureWorkerRuntime(()=>{created++;return native().player;},v=>messages.push(v));
    await runtime.receive({protocol:WORKER_PROTOCOL,id:1,kind:'load',width:4,height:2,maxBundleBytes:8,maxAudioBytes:1024,bytes:new ArrayBuffer(4),...bad});
    assert.equal(created,0);assert.equal(messages[0].kind,'error');
  }
  const value=native(), messages=[];const runtime=new PictureWorkerRuntime(()=>value.player,v=>messages.push(v));
  const load={protocol:WORKER_PROTOCOL,id:1,kind:'load',width:4,height:2,maxBundleBytes:128,maxAudioBytes:1024,bytes:recording().buffer};
  await runtime.receive(load);assert.equal(messages.at(-1).kind,'ready');await runtime.receive({...load,id:2});assert.equal(messages.at(-1).kind,'error');assert.equal(value.facts.frees,1);
});
test('metadata and channel admission reject impossible clocks and repeated backing arrays',()=>{
  const base={frameCount:4,fps:2,durationSeconds:2,engineVersion:'v1',hasCameraTrack:false,hasSoundtrack:false,audio:null,labels:[]};
  for(const changes of [{fps:0},{frameCount:2**40},{durationSeconds:Infinity},{engineVersion:''},{hasSoundtrack:true},{labels:[{name:'x',index:4}]}])assert.throws(()=>metadata({...base,...changes}));
  const info=audioInfo({sampleRate:8,channels:2,sampleFrames:2}), same=new ArrayBuffer(8);
  assert.throws(()=>transferredSoundtrack(info,[same,same]));
});


test('restart restores a crashed silent worker without another file-selection step', async () => {
  const f=rig();await f.load();f.session.render(5);await flush();
  const old=f.session.current;const source=old.renderer.source;
  assert.equal(source.byteLength,60);assert.match(source.checksum,/^[0-9a-f]{64}$/);
  f.workers[0].onerror({message:'native trap'});
  assert(old.renderer.error);assert.equal(f.workers[0].stopped,1);
  assert(await f.session.restart());assert.notEqual(f.session.current.renderer,old.renderer);
  assert.deepEqual(f.natives[1].facts.renders,[5]);assert.equal(f.workers[1].requests[0].kind,'restore');
  assert.equal(f.session.current.renderer.frame,5);assert.equal(f.session.current.renderer.error,null);
  f.session.present();assert.deepEqual(f.frames.at(-1),{index:5,bytes:expectedPixels(5)});
  f.session.render(2);await flush();assert.equal(f.frames.at(-1).index,2);
  f.session.dispose();assert(f.workers.every(w=>w.stopped===1));
});

test('restart keeps the exact paused audio owner and never copies its PCM a second time', async () => {
  const device={state:'suspended',sampleRate:48000,currentTime:10,destination:{},closed:0,
    resume(){this.state='running';return Promise.resolve();},close(){this.closed++;this.state='closed';return Promise.resolve();},
    createBuffer(c,n,sr){const arrays=Array.from({length:c},()=>new Float32Array(n));return {duration:n/sr,getChannelData:i=>arrays[i]};},
    createBufferSource(){return {connect(){},start(){},stop(){},disconnect(){}}}};
  for(const samples of [48000,150000]) {
    device.currentTime=10;device.state='suspended';device.closed=0;let prepares=0;
    const f=rig({sound:true,frames:samples,rate:48000},{prepareAudio:p=>{prepares++;return prepareSoundtrack(p,()=>device);}});
    await f.load();const audio=f.session.current.audio;await audio.play();device.currentTime=10.312345;
    f.workers[0].onerror({message:'trap'});const phase=audio.positionSeconds,index=audio.frame;
    assert(phase>0&&phase<1);assert.equal(audio.playing,false);
    assert(await f.session.restart());assert.equal(f.session.current.audio,audio);assert.equal(prepares,1);
    assert.equal(audio.positionSeconds,phase);assert.equal(audio.playing,false);
    assert.deepEqual(f.natives[1].facts.renders,[index]);assert.deepEqual(f.natives[1].facts.copies,[]);
    assert.equal(f.natives[1].facts.pcmFrees,1);assert.equal(device.closed,0);
    device.currentTime=20;await audio.play();assert.equal(audio.positionSeconds,phase);audio.pause();
    f.session.dispose();assert.equal(device.closed,1);
  }
});

test('a changed recording cannot pair new pictures with retained narration', async () => {
  const f=rig({sound:true});let source=recording([1]);
  await f.session.load(async()=>source.slice());const prior=f.session.current;
  source=recording([2]);await assert.rejects(f.session.restart(7),/Recording changed/);
  assert.equal(f.session.current,prior);assert.equal(f.audio[0].disposed,0);assert.equal(f.workers[0].stopped,0);
  assert.equal(f.workers[1].stopped,1);assert.equal(f.natives[1].facts.renders.length,0);
  source=recording([1]);assert(await f.session.restart(7));assert.equal(f.session.current.audio,prior.audio);
  f.session.dispose();assert.equal(f.audio[0].disposed,1);
});

test('matching checksum trailers cannot bypass native revalidation of altered payloads', async () => {
  const f=rig({sound:true});let source=recording([1]);
  await f.session.load(async()=>source.slice());const prior=f.session.current;
  source[24]^=1;await assert.rejects(f.session.restart(),/Native checksum refusal/);
  assert.equal(f.session.current,prior);assert.equal(f.audio[0].disposed,0);assert.equal(f.natives[1].facts.renders.length,0);
  assert.equal(f.workers[1].stopped,1);f.session.dispose();
});

test('failed restart metadata, frame, identity and audio-ownership replies preserve the old movie',async()=>{
  for(const edit of [
    v=>({...v,metadata:{...v.metadata,fps:9}}),v=>({...v,metadata:{...v.metadata,engineVersion:'changed-engine'}}),
    v=>({...v,metadata:{...v.metadata,hasCameraTrack:false}}),v=>({...v,metadata:{...v.metadata,audio:{...v.metadata.audio,sampleRate:9}}}),
    v=>({...v,source:{...v.source,checksum:'f'.repeat(64)}}),v=>({...v,source:{...v.source,byteLength:v.source.byteLength+1}}),
    v=>({...v,frame:v.frame+1}),v=>({...v,audioReused:false}),v=>({...v,channels:[new ArrayBuffer(64),new ArrayBuffer(64)]})
  ]) {
    const f=rig({sound:true});await f.load();const prior=f.session.current;
    const pending=f.session.restart(4);await Promise.resolve();f.workers[1].editReply=edit;
    await assert.rejects(pending);assert.equal(f.session.current,prior);assert.equal(f.audio[0].disposed,0);
    assert.equal(f.workers[1].stopped,1);assert.equal(f.session.restarting,false);
    assert(await f.session.restart(3));assert.equal(f.session.current.audio,prior.audio);f.session.dispose();
  }
});

test('a cancelled restart cannot replace the current movie after a delayed read',async()=>{
  const f=rig({sound:true}),gate=deferred();let reads=0,signal;
  await f.session.load(async s=>{if(reads++){signal=s;return gate.promise;}return recording();});
  const prior=f.session.current,pending=f.session.restart(6);
  assert.equal(f.session.restarting,true);assert.throws(()=>f.session.render(2),/restart is in progress/);
  assert.equal(await f.session.restart(3),false);assert.equal(f.session.cancelRestart(),true);assert(signal.aborted);
  assert.equal(f.session.cancelRestart(),false);assert.equal(f.session.restarting,false);
  gate.resolve(recording());assert.equal(await pending,false);assert.equal(f.session.current,prior);
  assert.equal(f.workers.length,1);f.session.render(2);await flush();assert.equal(f.frames.at(-1).index,2);
  f.session.dispose();assert.equal(f.audio[0].disposed,1);
});

test('a newer recording or disposal cancels an outstanding restart without sharing audio lifetimes',async()=>{
  for(const action of ['load','dispose']) {
    const f=rig({sound:true}),gate=deferred();let reads=0;
    await f.session.load(async()=>reads++?gate.promise:recording());const original=f.session.current;
    const pending=f.session.restart();
    if(action==='load')assert(await f.load(recording([2])));else f.session.dispose();
    gate.resolve(recording());assert.equal(await pending,false);
    assert.notEqual(f.session.current,original);assert.equal(f.audio[0].disposed,1);
    if(action==='load'){assert.equal(f.audio[1].disposed,0);f.session.dispose();assert.equal(f.audio[1].disposed,1);}
  }
});

test('restarting a healthy worker suppresses its in-flight picture and preserves a failed candidate retry',async()=>{
  const f=rig();await f.load();f.workers[0].holdRender=true;f.session.render(8);
  assert(await f.session.restart(2));assert.equal(f.workers[0].stopped,1);
  await f.workers[0].finish();assert.equal(f.frames.length,0);
  f.session.present();assert.equal(f.frames.at(-1).index,2);f.session.dispose();
});

test('invalid restart positions do not interrupt audio or allocate a candidate',async()=>{
  const f=rig({sound:true});await assert.rejects(f.session.restart(),/No recording/);await f.load();
  for(const frame of [-1,12,0.1,NaN,Infinity])await assert.rejects(f.session.restart(frame));
  assert.equal(f.audio[0].pauses,0);assert.equal(f.workers.length,1);
  f.session.dispose();await assert.rejects(f.session.restart(),/disposed/);
});

test('timeouts of a restarting worker keep the original failure and audio available for another attempt',async()=>{
  const f=rig({sound:true},{timeoutMs:20});await f.load();const prior=f.session.current;
  f.workers[0].onerror({message:'original crash'});const originalError=prior.renderer.error;
  const gate=deferred();
  // Hold the whole restore request in the transport, just like a wedged worker.
  const pending=f.session.restart(2);await Promise.resolve();
  f.workers[1].runtime.receive=()=>gate.promise;
  await assert.rejects(pending,/timed out/);assert.equal(prior.renderer.error,originalError);
  assert.equal(f.session.current,prior);assert.equal(f.audio[0].disposed,0);assert.equal(f.workers[1].stopped,1);
  gate.resolve();assert(await f.session.restart(2));assert.equal(f.session.current.audio,prior.audio);
  f.session.dispose();
});

test('a nonrepeatable reader fails explicitly rather than pretending a detached buffer is the recording',async()=>{
  const f=rig(),once=recording();await f.session.load(async()=>once);const prior=f.session.current;
  assert.equal(once.byteLength,0);await assert.rejects(f.session.restart(),/framed recording/);
  assert.equal(f.session.current,prior);assert.equal(f.workers[1].stopped,1);f.session.dispose();
});


test('an old worker failure cannot abort a replacement that is already recovering it',async()=>{
  const f=rig({sound:true}),gate=deferred();let reads=0;
  await f.session.load(async()=>reads++?gate.promise:recording());
  const pending=f.session.restart(3);f.workers[0].onerror({message:'old worker died during recovery'});
  assert.equal(f.errors.length,0);assert.equal(f.session.restarting,true);
  gate.resolve(recording());assert(await pending);assert.equal(f.session.current.renderer.frame,3);
  assert.equal(f.audio[0].disposed,0);f.session.dispose();
});

test('cancelled candidate decoding cannot close retained audio or publish a late ready response',async()=>{
  const f=rig({sound:true});await f.load();const prior=f.session.current;
  const pending=f.session.restart(2);await Promise.resolve();
  const gate=deferred(),candidate=f.natives[1];
  f.workers[1].runtime=new PictureWorkerRuntime(()=>gate.promise,()=>{throw new Error('late reply escaped disposed runtime');});
  await flush();assert(f.session.cancelRestart());assert.equal(await pending,false);
  gate.resolve(candidate.player);await flush();assert.equal(candidate.facts.frees,1);
  assert.equal(f.session.current,prior);assert.equal(f.audio[0].disposed,0);f.session.dispose();
});
