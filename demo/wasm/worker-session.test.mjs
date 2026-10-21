import test from 'node:test';
import assert from 'node:assert/strict';
import {PictureWorkerRuntime} from './picture-worker-core.mjs';
import {WorkerTimelineSession, transferredSoundtrack} from './worker-session.mjs';
import {WORKER_PROTOCOL, metadata, audioInfo} from './worker-protocol.mjs';
import {prepareSoundtrack} from './audio-playback.mjs';

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
      const worker = new FakeWorker(async bytes => { value.facts.inputs.push([...bytes]); return value.player; });
      workers.push(worker); return worker;
    },
    prepareAudio: options.sound ? player => {
      const pcm = player.take_soundtrack();
      const value = {pcm, disposed:0, pauses:0, dispose(){this.disposed++;pcm.free();}, pause(){this.pauses++;}, play(){}, seek(){}, frameAt(){}};
      audio.push(value); return value;
    } : null,
    onFrame: frame => frames.push({index: frame.index, bytes: frame.rgba.slice()}),
    onError: error => errors.push(error), ...extra});
  return {session, workers, natives, frames, errors, audio,
    load: (bytes = new Uint8Array([70,77,84,76])) => session.load(async () => bytes)};
}

test('one worker owns decode, first picture and all later raster work', async () => {
  const f = rig(), bytes = new Uint8Array([70,77,84,76]);
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
  const f=rig(), all=new Uint8Array([99,70,77,84,76,98]);
  assert(await f.load(all.subarray(1,5)));assert.equal(all.byteLength,6);
  assert.deepEqual(f.natives[0].facts.inputs,[[70,77,84,76]]);f.session.dispose();
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
    assert(await session.load(async()=>new Uint8Array([1])));const prior=session.current;
    await assert.rejects(session.load(async()=>new Uint8Array([2])));assert.equal(session.current,prior);
    assert.equal(workers[0].stopped,0);assert.equal(workers[1].stopped,1);session.render(2);await flush();session.dispose();
  }
});
test('a newer load terminates an old decoder and does not wait for it to finish',async()=>{
  const slow=deferred(), first=native(), second=native();const workers=[];
  const session=new WorkerTimelineSession({width:4,height:2,createWorker(){const index=workers.length;const w=new FakeWorker(()=>index?second.player:slow.promise);workers.push(w);return w;}});
  const old=session.load(async()=>new Uint8Array([1]));await flush();
  const latest=session.load(async()=>new Uint8Array([2]));assert(await latest);assert.equal(await old,false);
  assert.equal(workers[0].stopped,1);slow.resolve(first.player);await flush();assert.equal(first.facts.frees,1);
  assert.equal(session.current.metadata.engineVersion,'fixture:exact');session.dispose();
});
test('superseded input reads receive cancellation and cannot create late workers',async()=>{
  const f=rig(), read=deferred();let signal;
  const old=f.session.load(s=>{signal=s;return read.promise;});await flush();assert(await f.load());assert(signal.aborted);
  read.resolve(new Uint8Array([9]));assert.equal(await old,false);assert.equal(f.workers.length,1);f.session.dispose();
});
test('dispose terminates a hung load and releases current audio without late resurrection',async()=>{
  const f=rig({sound:true});await f.load();const read=deferred();const pending=f.session.load(()=>read.promise);
  f.session.dispose();read.resolve(new Uint8Array([1]));assert.equal(await pending,false);assert.equal(f.audio[0].disposed,1);
  await assert.rejects(f.load(),/disposed/);assert.equal(f.session.current,null);
});
test('worker deadline rejects a hung candidate and keeps the previous generation',async()=>{
  const wait=deferred();let count=0;const workers=[];
  const session=new WorkerTimelineSession({width:4,height:2,timeoutMs:15,createWorker(){const value=native();const which=count++;const w=new FakeWorker(()=>which?wait.promise:value.player);workers.push(w);return w;}});
  await session.load(async()=>new Uint8Array([1]));const prior=session.current;
  await assert.rejects(session.load(async()=>new Uint8Array([2])),/timed out/);assert.equal(session.current,prior);assert.equal(workers[1].stopped,1);
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
  const load={protocol:WORKER_PROTOCOL,id:1,kind:'load',width:4,height:2,maxBundleBytes:8,maxAudioBytes:1024,bytes:new ArrayBuffer(4)};
  await runtime.receive(load);await runtime.receive({...load,id:2});assert.equal(messages.at(-1).kind,'error');assert.equal(value.facts.frees,1);
});
test('metadata and channel admission reject impossible clocks and repeated backing arrays',()=>{
  const base={frameCount:4,fps:2,durationSeconds:2,engineVersion:'v1',hasCameraTrack:false,hasSoundtrack:false,audio:null,labels:[]};
  for(const changes of [{fps:0},{frameCount:2**40},{durationSeconds:Infinity},{engineVersion:''},{hasSoundtrack:true},{labels:[{name:'x',index:4}]}])assert.throws(()=>metadata({...base,...changes}));
  const info=audioInfo({sampleRate:8,channels:2,sampleFrames:2}), same=new ArrayBuffer(8);
  assert.throws(()=>transferredSoundtrack(info,[same,same]));
});
