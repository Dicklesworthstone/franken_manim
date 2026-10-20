"use strict";
const {test} = require("node:test");
const assert = require("node:assert/strict");
const {webcrypto} = require("node:crypto");
const {Transport, Player, metadata, sampleAtFrame} = require("../crates/fmn-studio/src/studio_audio.js");
const digest = async bytes => Buffer.from(await webcrypto.subtle.digest("SHA-256", bytes)).toString("hex");
const info = (overrides = {}) => metadata({schema:"fmn.studio.audio", version:1, source:"a".repeat(64),
  worker_generation:3, sample_rate:48000, channels:2, sample_frames:120000, fps:30,
  picture_frames:60, max_chunk_frames:65536, ...overrides});
const flush = async () => { for (let n=0; n<8; n++) await Promise.resolve(); };
function deferred() { let resolve, reject; const promise = new Promise((yes,no) => {resolve=yes;reject=no;}); return {promise,resolve,reject}; }
async function response(m, start, frames, overrides = {}) {
  const bytes = new Uint8Array(frames * m.channels * 4), view = new DataView(bytes.buffer);
  for (let i=0;i<frames;i++) for(let c=0;c<m.channels;c++) view.setFloat32((i*m.channels+c)*4, (c? -1:1) * ((i+start)%127)/128, true);
  return new Response(bytes, {headers:{"Content-Type":"application/vnd.frankenmanim.pcm-f32le",
    "X-FMN-Audio-Source":m.source,"X-FMN-Worker-Generation":String(m.worker_generation),
    "X-FMN-Audio-Start":String(start),"X-FMN-Audio-Frames":String(frames),"X-FMN-Audio-Next":String(start+frames),
    "X-FMN-Audio-End":String(start+frames===m.sample_frames),"X-FMN-SHA256":await digest(bytes), ...overrides}});
}
class Context {
  constructor() { this.currentTime=0; this.state="suspended"; this.nodes=[]; this.buffers=[]; this.destination={}; }
  createGain() { return {gain:{value:1},connect(){}}; }
  resume() { this.state="running";return Promise.resolve(); }
  close() { this.state="closed";return Promise.resolve(); }
  createBuffer(channels, frames, rate) {
    const value={channels,frames,rate,values:[],copyToChannel(data,index){this.values[index]=data.slice();}};
    this.buffers.push(value);return value;
  }
  createBufferSource() {
    const node={stopped:0,disconnected:0, connect(){},disconnect(){this.disconnected++;},stop(){this.stopped++;},
      start(when){this.when=when;}};this.nodes.push(node);return node;
  }
}
function fixture(m=info(), options={}) {
  const calls=[], context=new Context(), states=[], frames=[];
  let constructed=0;
  const transport={info:async()=>m,chunk:async(m,start,count)=> {
    calls.push({start,count}); const frames=Math.min(count,m.sample_frames-start);
    return {start,frames,channels:Array.from({length:m.channels},(_,c)=>Float32Array.from({length:frames},(_,i)=>(c?-1:1)*((start+i)%127)/128))};
  }};
  const player=new Player({transport,context:()=>{constructed++;return context;},onState:s=>states.push(s),onFrame:f=>frames.push(f),...options});
  return {player,transport,context,calls,states,frames,get constructed(){return constructed;}};
}

test("metadata refuses malformed clocks, unsafe cursors, generations and channel layouts",()=>{
  for(const override of [{schema:"other"},{version:2},{source:"a"},{worker_generation:0},{worker_generation:2**54},
    {sample_rate:0},{sample_rate:Infinity},{channels:3},{sample_frames:-1},{sample_frames:2**54},
    {fps:0},{picture_frames:0},{max_chunk_frames:0},{max_chunk_frames:65537},{picture_frames:2**52}]) {
    assert.throws(()=>info(override), /metadata/);
  }
});
test("absolute frame cuts partition fractional audio clocks including half-sample ties",()=>{
  const m=info({sample_rate:10,fps:3});
  assert.deepEqual([0,1,2,3].map(frame=>sampleAtFrame(m,frame)),[0,3,7,10]);
  assert.equal(sampleAtFrame(info({sample_rate:9,fps:2}),1),5);
});
test("transport authenticates headers only and verifies little-endian stereo channel order",async()=>{
  const m=info(), calls=[];
  const transport=new Transport(async(url,options)=>{calls.push({url,options});return response(m,137,13);},"secret-capability",webcrypto);
  const chunk=await transport.chunk(m,137,13,new AbortController().signal);
  assert.equal(chunk.frames,13); assert.equal(chunk.channels[0][0],10/128);assert.equal(chunk.channels[1][0],-10/128);
  assert(!calls[0].url.includes("secret")); assert.equal(calls[0].options.headers["X-FMN-Capability"],"secret-capability");
  assert.equal(calls[0].options.redirect,"error");assert.equal(calls[0].options.cache,"no-store");
  assert.equal(new URL(calls[0].url,"http://localhost").searchParams.get("worker_generation"),"3");
});
test("every response ownership and cursor header is checked before playback",async()=>{
  const m=info();
  for(const fields of [{"X-FMN-Audio-Source":"b".repeat(64)},{"X-FMN-Worker-Generation":"4"},
    {"X-FMN-Audio-Start":"2"},{"X-FMN-Audio-Frames":"8"},{"X-FMN-Audio-Next":"10"},
    {"X-FMN-Audio-End":"true"},{"X-FMN-Audio-Start":""},{"X-FMN-Audio-Next":"9007199254740992"},
    {"X-FMN-SHA256":"c".repeat(64)},{"Content-Type":"audio/wav"}]) {
    const transport=new Transport(()=>response(m,0,7,fields),"secret",webcrypto);
    await assert.rejects(transport.chunk(m,0,7,new AbortController().signal));
  }
});
test("terminal chunks shorten exactly and EOF is empty",async()=>{
  const m=info({sample_frames:5});
  const transport=new Transport(url=>{const start=Number(new URL(url,"http://localhost").searchParams.get("start"));return response(m,start,5-start);},"cap",webcrypto);
  const tail=await transport.chunk(m,3,7,new AbortController().signal); assert.equal(tail.frames,2);
  const eof=await transport.chunk(m,5,1,new AbortController().signal);assert.equal(eof.frames,0);
  await assert.rejects(transport.chunk(m,6,1,new AbortController().signal));
});
test("streaming response bounds apply before ownership allocations without Content-Length",async()=>{
  let cancelled=false;
  const body=new ReadableStream({start(c){c.enqueue(new Uint8Array(128));},cancel(){cancelled=true;}});
  const transport=new Transport(async()=>new Response(body,{headers:{"Content-Type":"application/vnd.frankenmanim.pcm-f32le"}}),"cap",webcrypto);
  await assert.rejects(transport.chunk(info(),0,1,new AbortController().signal),/budget/);assert(cancelled);
});
test("checksummed nonfinite PCM still refuses instead of entering Web Audio",async()=>{
  const m=info(), bytes=new Uint8Array(8);new DataView(bytes.buffer).setFloat32(0,NaN,true);
  const valid=await response(m,0,1);const headers=Object.fromEntries(valid.headers);headers["x-fmn-sha256"]=await digest(bytes);
  const transport=new Transport(async()=>new Response(bytes,{headers}),"cap",webcrypto);
  await assert.rejects(transport.chunk(m,0,1,new AbortController().signal),/Non-finite/);
});
test("HTTP refusal is not decoded and never echoes secret response bodies",async()=>{
  const transport=new Transport(async()=>new Response("secret-capability",{status:403}),"secret-capability",webcrypto);
  await assert.rejects(transport.info(new AbortController().signal),e=>e.status===403 && !e.message.includes("secret-capability"));
});
test("metadata probe never creates or resumes an audio device",async()=>{
  const f=fixture();await f.player.probe();assert.equal(f.constructed,0);assert.equal(f.context.nodes.length,0);
});
test("first click resumes the device before the metadata request settles",async()=>{
  const f=fixture(), pending=deferred();f.transport.info=()=>pending.promise;
  const playing=f.player.play();assert.equal(f.constructed,1);assert.equal(f.context.state,"running");
  assert.equal(f.context.nodes.length,0);pending.resolve(info());assert(await playing);
});
test("bounded lookahead schedules adjacent chunks on absolute sample boundaries",async()=>{
  const f=fixture();assert(await f.player.play());
  for(let i=0;i<30;i++){f.player.pump();await flush();}
  assert.equal(f.context.nodes.length,3);assert.equal(f.calls.length,3);assert.equal(f.player.slots.size,3);
  assert.deepEqual(f.context.nodes.map(n=>n.when),[0.06,0.56,1.06]);
  assert.deepEqual(f.calls.map(c=>c.start),[0,24000,48000]);
  assert(f.context.buffers.every(b=>b.rate===48000&&b.frames<=65536&&b.channels===2));
});
test("ended buffers are released even when browser onended callbacks are delayed",async()=>{
  const f=fixture();await f.player.play();for(let i=0;i<3;i++){f.player.pump();await flush();}
  f.context.currentTime=0.6;f.player.pump();await flush();assert.equal(f.player.slots.size,3);
  assert(f.context.nodes[0].disconnected);assert.equal(f.calls.at(-1).start,72000);
});
test("pause and resume retain exact sample position rather than the displayed frame",async()=>{
  const f=fixture();await f.player.play(3);assert.equal(f.calls[0].start,4800);
  f.context.currentTime=0.185;f.player.pause();assert.equal(f.player.position,10800);
  assert(f.context.nodes.every(n=>n.stopped&&n.disconnected));assert(await f.player.play());
  assert.equal(f.calls.at(-1).start,10800);
});
test("seeking clears all scheduled sound and resumes on the absolute selected boundary",async()=>{
  const f=fixture();await f.player.play();f.player.pump();await flush();f.player.seek(25);
  assert.equal(f.player.position,40000);assert.equal(f.player.slots.size,0);assert(await f.player.play());
  assert.equal(f.calls.at(-1).start,40000);
});
test("cancelled metadata cannot start playback after a pause",async()=>{
  const f=fixture(), pending=deferred();f.transport.info=()=>pending.promise;
  const playing=f.player.play();f.player.pause();pending.resolve(info());assert.equal(await playing,false);
  assert.equal(f.calls.length,0);assert.equal(f.context.nodes.length,0);
});
test("a late verified chunk from a prior run cannot sound after seeking",async()=>{
  const f=fixture();await f.player.play();const pending=deferred(), original=f.transport.chunk;
  f.transport.chunk=()=>pending.promise;f.player.pump();f.player.seek(12);
  f.transport.chunk=original;assert(await f.player.play());const count=f.context.nodes.length;
  pending.resolve(await original(info(),24000,24000));await flush();assert.equal(f.context.nodes.length,count);
  assert.equal(f.player.anchorSample,19200);
});
test("buffer starvation pauses at the last queued sample and never skips unheard PCM",async()=>{
  const f=fixture();await f.player.play();f.transport.chunk=()=>new Promise(()=>{});
  f.player.pump();f.context.currentTime=0.61;f.player.pump();
  assert.equal(f.player.status,"paused");assert.equal(f.player.position,24000);assert.equal(f.player.slots.size,0);
  assert.match(f.states.at(-1).message,/without skipping/);
});
test("audio clock drives latest stored frame with bounded picture request cadence",async()=>{
  const f=fixture();await f.player.play();
  for(const time of [0.061,0.063,0.07,0.16,0.161,0.3]) {f.context.currentTime=time;f.player.pump();await flush();}
  assert.deepEqual(f.frames,[0,3,7]);assert(f.frames.every(n=>Number.isSafeInteger(n)&&n<60));
});
test("long audio tails hold the final picture and stop only at the soundtrack end",async()=>{
  const f=fixture(info({sample_frames:12000,picture_frames:3}));await f.player.play();
  f.context.currentTime=0.24;f.player.pump();assert.equal(f.frames.at(-1),2);assert.equal(f.player.status,"playing");
  f.context.currentTime=0.32;f.player.pump();assert.equal(f.player.status,"paused");assert.equal(f.player.position,12000);
});
test("short narration leaves a silent picture suffix without fake PCM or requests past EOF",async()=>{
  const f=fixture(info({sample_frames:2400,picture_frames:30}));await f.player.play();
  f.context.currentTime=0.56;f.player.pump();await flush();assert.equal(f.player.status,"playing");assert.equal(f.frames.at(-1),15);
  assert.equal(f.calls.length,1);f.context.currentTime=1.1;f.player.pump();assert.equal(f.player.position,48000);
});
test("starting after audio EOF advances silent pictures without constructing an empty buffer",async()=>{
  const f=fixture(info({sample_frames:2400}));await f.player.play(40);assert.equal(f.calls.length,0);assert.equal(f.context.nodes.length,0);
  f.context.currentTime=0.07;f.player.pump();assert.equal(f.frames.at(-1),40);
});
test("worker replacement invalidates even an already-complete soundtrack during silent pictures",async()=>{
  const f=fixture(info({sample_frames:2400,picture_frames:90}));await f.player.play();
  f.transport.info=async()=>info({sample_frames:2400,picture_frames:90,worker_generation:4});
  f.context.currentTime=1.2;f.player.pump();await flush();assert.equal(f.player.status,"paused");assert.equal(f.player.info,null);
});
test("restarting against a different recording never silently retains an old audio cursor",async()=>{
  const f=fixture();await f.player.play(30);f.player.pause();f.transport.info=async()=>info({source:"b".repeat(64)});
  assert.equal(await f.player.play(),false);assert.equal(f.player.position,0);assert.match(f.states.at(-1).message,/changed/);
});
test("device suspension is a pause rather than a wall-time catch-up",async()=>{
  const f=fixture();await f.player.play();f.context.currentTime=0.2;f.context.state="suspended";f.player.pump();
  assert.equal(f.player.status,"paused");assert.equal(f.player.slots.size,0);assert.match(f.states.at(-1).message,/suspended/);
});
test("volume updates the dedicated gain and does not alter stored samples",async()=>{
  const f=fixture();f.player.setVolume(0.25);await f.player.play();assert.equal(f.player.gain.gain.value,0.25);
  const before=f.context.buffers[0].values[0].slice();f.player.setVolume(0);assert.equal(f.player.gain.gain.value,0);
  assert.deepEqual(f.context.buffers[0].values[0],before);for(const v of [-1,2,NaN])assert.throws(()=>f.player.setVolume(v));
});
test("a picture callback pausing playback cannot cause more network work in that pump",async()=>{
  const f=fixture();await f.player.play();f.player.onFrame=()=>f.player.pause();f.context.currentTime=0.1;
  f.player.pump();await flush();assert.equal(f.calls.length,1);
});
test("unsupported device rates fail visibly and leave no scheduled audio",async()=>{
  const f=fixture();f.context.createBuffer=()=>{throw new Error("sample rate unsupported");};
  assert.equal(await f.player.play(),false);assert.equal(f.player.slots.size,0);assert.match(f.states.at(-1).message,/unsupported/);
});
test("closing the page cancels outstanding work and closes the device",async()=>{
  const f=fixture();await f.player.play();await f.player.dispose();assert.equal(f.context.state,"closed");assert.equal(f.player.slots.size,0);
});


test("completion delivers the final picture even when the preview interval has not elapsed",async()=>{
  const f=fixture(info({sample_frames:1600,picture_frames:1}));await f.player.play();
  f.context.currentTime=0.11;f.player.pump();assert.deepEqual(f.frames,[0]);assert.equal(f.player.status,"paused");
});
test("closing then explicitly restarting creates a new device without reviving stopped sources",async()=>{
  const f=fixture();await f.player.play();await f.player.dispose();assert.equal(f.player.context,null);
  assert(await f.player.play());assert.equal(f.constructed,2);assert(f.context.nodes[0].stopped);
});

test("picture ownership is validated before fetching or scheduling the first audio sample",async()=>{
  const f=fixture();f.player.onReady=()=>{throw new Error("wrong picture generation");};
  assert.equal(await f.player.play(),false);assert.equal(f.calls.length,0);assert.equal(f.context.nodes.length,0);
  assert.match(f.states.at(-1).message,/generation/);
});

test("pause after a stalled tick resumes at the first unqueued sample",async()=>{
  const f=fixture();assert(await f.player.play(3));
  f.context.currentTime=10;
  assert.equal(f.player.current(),28800);
  f.player.pause("Tab lost focus");
  assert.equal(f.player.position,28800);assert.equal(f.states.at(-1).sample,28800);
  assert(await f.player.play());assert.equal(f.calls.at(-1).start,28800);
});
test("a tick delayed beyond both track ends reports underrun, never false completion",async()=>{
  const f=fixture();assert(await f.player.play());f.context.currentTime=10;f.player.pump();
  assert.equal(f.player.status,"paused");assert.equal(f.player.position,24000);
  assert.match(f.states.at(-1).message,/without skipping/);assert.deepEqual(f.frames,[]);
  assert.equal(f.player.slots.size,0);assert.equal(f.calls.length,1);
});
test("device suspension cannot advance past narration that was never queued",async()=>{
  const f=fixture();assert(await f.player.play());
  f.context.currentTime=10;f.context.state="suspended";f.player.pump();
  assert.equal(f.player.position,24000);assert.match(f.states.at(-1).message,/suspended/);
  assert(await f.player.play());assert.equal(f.calls.at(-1).start,24000);
});
test("late generation-check failures preserve the audible cursor without a timely pump",async()=>{
  const f=fixture();assert(await f.player.play());
  for(let n=0;n<2;n++){f.player.pump();await flush();}
  const pending=deferred(), chunk=deferred();
  f.transport.info=()=>pending.promise;f.transport.chunk=()=>chunk.promise;
  f.context.currentTime=1.1;f.player.pump();
  f.context.currentTime=10;pending.reject(new Error("connection unavailable"));await flush();
  assert.equal(f.player.position,72000);assert.equal(f.player.status,"paused");
  assert.match(f.states.at(-1).message,/connection/);assert.equal(f.player.slots.size,0);
});
test("repeated stalls neither skip nor duplicate narration across resume cycles",async()=>{
  const f=fixture();const heard=[];
  for(let n=0;n<5;n++){
    assert(await f.player.play());
    heard.push(...f.context.buffers.at(-1).values[0]);
    f.context.currentTime+=10;f.player.pump();
    assert.equal(f.player.position,(n+1)*24000);
    assert.equal(f.player.status,"paused");
  }
  assert.deepEqual(f.calls.map(c=>c.start),[0,24000,48000,72000,96000]);
  assert.deepEqual(heard,Array.from({length:120000},(_,i)=>(i%127)/128));
  assert.match(f.states.at(-1).message,/finished/);assert.deepEqual(f.frames,[59]);
});
test("a fully queued short track may advance through its intentional silent suffix",async()=>{
  const f=fixture(info({sample_frames:100,picture_frames:90}));assert(await f.player.play());
  f.context.currentTime=10;f.player.pump();
  assert.equal(f.player.position,144000);assert.match(f.states.at(-1).message,/finished/);
  assert.equal(f.calls.length,1);assert.deepEqual(f.frames,[89]);
});
test("an empty soundtrack still runs the picture clock without requesting PCM",async()=>{
  const f=fixture(info({sample_frames:0,picture_frames:60}));assert(await f.player.play());
  f.context.currentTime=10;f.player.pump();
  assert.equal(f.player.position,96000);assert.deepEqual(f.frames,[59]);assert.equal(f.calls.length,0);
});
test("readiness callbacks can cancel loading before any audio fetch",async()=>{
  const f=fixture();f.player.onReady=()=>f.player.pause("picture ownership changed");
  assert.equal(await f.player.play(12),false);
  assert.equal(f.player.status,"paused");assert.equal(f.player.position,0);
  assert.equal(f.calls.length,0);assert.equal(f.context.nodes.length,0);
  assert.match(f.states.at(-1).message,/ownership/);
});

// Continuous preview is explicitly enabled by the embedded page. Embedders
// retaining the original pause-on-underrun policy still exercise every test above.
const continuous = (m=info()) => fixture(m,{autoRebuffer:true});

async function pendingRefill(f) {
  const original=f.transport.chunk, pending=deferred();
  const requests=[];
  f.transport.chunk=(m,start,count,signal)=> {
    requests.push({m,start,count,signal});return pending.promise;
  };
  f.player.pump();
  assert.equal(requests.length,1);
  return {requests,pending,original,async deliver() {
    const {m,start,count}=requests[0];
    pending.resolve(await original(m,start,count));await flush();
  }};
}

test("continuous playback holds an underrun cursor and keeps exactly one refill alive",async()=>{
  const f=continuous();await f.player.play();
  const refill=await pendingRefill(f);
  f.context.currentTime=0.7;f.player.pump();
  assert.equal(f.player.status,"buffering");assert.equal(f.player.current(),24000);
  assert.equal(refill.requests[0].signal.aborted,false);
  const pictures=f.frames.length;
  for(let n=0;n<40;n++){f.context.currentTime+=0.1;f.player.pump();await flush();}
  assert.equal(refill.requests.length,1);assert.equal(f.player.slots.size,0);
  assert.equal(f.player.current(),24000);assert.equal(f.frames.length,pictures);
  f.player.pause();assert.equal(refill.requests[0].signal.aborted,true);
});

test("a successful delayed refill resumes from its exact sample with a new device-clock anchor",async()=>{
  const f=continuous();await f.player.play();
  const refill=await pendingRefill(f);
  f.context.currentTime=5;f.player.pump();assert.equal(f.player.status,"buffering");
  await refill.deliver();
  assert.equal(f.player.status,"playing");assert.equal(f.player.anchorSample,24000);
  assert.equal(f.context.nodes.at(-1).when,5.06);assert.equal(f.player.current(),24000);
  assert.equal(f.player.queuedEnd,48000);assert.equal(f.constructed,1);
  assert.equal(f.context.buffers.at(-1).values[0][0],(24000%127)/128);
  assert.equal(f.context.buffers.at(-1).values[1][0],-(24000%127)/128);
  assert.equal(f.player.slots.size,1);f.player.pause();
});

test("a refill that arrives after its deadline before the next tick also rebuffers",async()=>{
  const f=continuous();await f.player.play();const refill=await pendingRefill(f);
  f.context.currentTime=2;await refill.deliver();
  assert.equal(f.player.status,"playing");assert.equal(f.context.nodes.at(-1).when,2.06);
  assert.equal(f.player.anchorSample,24000);assert.equal(f.player.current(),24000);
  assert(f.states.some(s=>s.status==="buffering"));f.player.pause();
});

test("a long UI stall with no pending refill automatically requests the first unheard chunk",async()=>{
  const f=continuous();await f.player.play();f.context.currentTime=10;f.player.pump();await flush();
  assert.equal(f.player.status,"playing");assert.equal(f.calls.length,2);
  assert.equal(f.calls[1].start,24000);assert.equal(f.context.nodes[1].when,10.06);
  assert.equal(f.player.current(),24000);f.player.pause();
});

test("repeated automatic recoveries reconstruct both complete channels without gaps or duplicates",async()=>{
  const m=info({sample_frames:121003,picture_frames:78}), f=continuous(m);
  await f.player.play();
  let recoveries=0;
  while(f.player.queuedEnd<m.sample_frames) {
    f.context.currentTime+=10;f.player.pump();await flush();
    assert.equal(f.player.status,"playing");assert(f.player.slots.size<=3);
    assert.equal(f.player.current(),f.calls.at(-1).start);
    assert(++recoveries<10);
  }
  assert.equal(recoveries,5);
  assert.deepEqual(f.calls.map(c=>c.start),[0,24000,48000,72000,96000,120000]);
  for(let channel=0;channel<2;channel++) {
    const joined=Float32Array.from(f.context.buffers.flatMap(b=>Array.from(b.values[channel])));
    const expected=Float32Array.from({length:m.sample_frames},(_,i)=>(channel?-1:1)*(i%127)/128);
    assert.deepEqual(joined,expected);
  }
  f.context.currentTime+=10;f.player.pump();
  assert.equal(f.player.status,"paused");assert.match(f.states.at(-1).message,/finished/);
  assert.equal(f.player.current(),sampleAtFrame(m,m.picture_frames));
});

test("pause, seek, invalidation and disposal cancel a buffering refill permanently",async()=>{
  for(const stop of [f=>f.player.pause(),f=>f.player.seek(12),f=>f.player.invalidate(),f=>f.player.dispose()]) {
    const f=continuous();await f.player.play();const refill=await pendingRefill(f);
    f.context.currentTime=1;f.player.pump();assert.equal(f.player.status,"buffering");
    await stop(f);const count=f.context.nodes.length, cursor=f.player.current();
    await refill.deliver();
    assert.equal(f.player.status,"paused");assert.equal(f.context.nodes.length,count);
    assert.equal(f.player.current(),cursor);assert.equal(refill.requests[0].signal.aborted,true);
  }
});

test("a buffering notification may cancel without starting new work",async()=>{
  const f=continuous();await f.player.play();
  f.player.onState=event=>{if(event.status==="buffering")f.player.pause("cancelled buffering");};
  f.context.currentTime=1;f.player.pump();await flush();
  assert.equal(f.player.status,"paused");assert.equal(f.calls.length,1);
  assert.equal(f.player.current(),24000);assert.equal(f.context.nodes.length,1);
});

test("automatic recovery revalidates picture ownership before scheduling sound",async()=>{
  for(const cancel of [false,true]) {
    const f=continuous();await f.player.play();const refill=await pendingRefill(f);
    f.context.currentTime=1;f.player.pump();
    let checks=0;f.player.onReady=()=>{checks++;if(cancel)f.player.pause();else throw new Error("picture generation changed");};
    await refill.deliver();
    assert.equal(checks,1);assert.equal(f.player.status,"paused");assert.equal(f.context.nodes.length,1);
    assert.equal(f.player.current(),24000);assert.equal(f.player.slots.size,0);
  }
});

test("worker replacement while buffering invalidates the pending audio rather than resuming",async()=>{
  const f=continuous();await f.player.play();const refill=await pendingRefill(f);
  f.transport.info=async()=>info({worker_generation:4});
  f.context.currentTime=2;f.player.pump();await flush();
  assert.equal(f.player.status,"paused");assert.equal(f.player.info,null);
  await refill.deliver();assert.equal(f.context.nodes.length,1);assert.equal(f.player.status,"paused");
});

test("a rejected refill is terminal and never starts a retry loop",async()=>{
  const f=continuous();await f.player.play();const refill=await pendingRefill(f);
  f.context.currentTime=0.7;f.player.pump();
  refill.pending.reject(new Error("recording checksum changed"));await flush();
  assert.equal(f.player.status,"paused");assert.equal(f.player.current(),24000);
  for(let n=0;n<30;n++){f.context.currentTime++;f.player.pump();await flush();}
  assert.equal(refill.requests.length,1);assert.equal(f.context.nodes.length,1);
  assert.match(f.states.at(-1).message,/checksum/);
});

test("a suspended audio device during buffering needs an explicit user resume",async()=>{
  for(const tick of [false,true]) {
    const f=continuous();let resumed=0;const resume=f.context.resume.bind(f.context);
    f.context.resume=()=>{resumed++;return resume();};
    await f.player.play();const refill=await pendingRefill(f);
    f.context.currentTime=0.7;f.player.pump();f.context.state="suspended";
    if(tick)f.player.pump();await refill.deliver();
    assert.equal(resumed,1);assert.equal(f.player.status,"paused");assert.equal(f.player.current(),24000);
    assert.equal(f.context.nodes.length,1);assert.match(f.states.at(-1).message,/suspended/i);
  }
});

test("recovery keeps three-slot lookahead and never buffers intentional silence past EOF",async()=>{
  const f=continuous(info({sample_frames:74000,picture_frames:90}));await f.player.play();
  f.context.currentTime=3;f.player.pump();await flush();
  for(let n=0;n<50;n++){f.player.pump();await flush();assert(f.player.slots.size<=3);}
  assert.deepEqual(f.calls.map(c=>c.start),[0,24000,48000,72000,96000].slice(0,4));
  f.context.currentTime=4.8;f.player.pump();await flush();
  assert.equal(f.player.status,"playing");assert.equal(f.calls.length,4);
  f.context.currentTime=7;f.player.pump();assert.equal(f.player.status,"paused");
  assert.equal(f.player.current(),144000);assert.match(f.states.at(-1).message,/finished/);
});

test("a paused buffering run cannot revive when a new run finishes first",async()=>{
  const f=continuous();await f.player.play();const refill=await pendingRefill(f);
  f.context.currentTime=0.7;f.player.pump();f.player.seek(20);
  f.transport.chunk=refill.original;await f.player.play();
  const samples=f.player.queuedEnd,nodes=f.context.nodes.length;
  await refill.deliver();assert.equal(f.context.nodes.length,nodes);assert.equal(f.player.queuedEnd,samples);
  assert.equal(f.player.anchorSample,32000);assert.equal(f.player.status,"playing");f.player.pause();
});

// Execute the actual browser entry (not its CommonJS export) with minimal DOM
// event targets. WebCrypto and Response streams remain real; only the page,
// audio device clock and host HTTP responses are controlled by this harness.
function embeddedFixture() {
  const {runInNewContext}=require("node:vm"), {readFileSync}=require("node:fs");
  class Element extends EventTarget {
    constructor() {super();this.dataset={};this.disabled=false;this.hidden=false;this.textContent="";this.attributes=new Map();}
    setAttribute(name,value){this.attributes.set(name,value);}
    getAttribute(name){return this.attributes.get(name);}
    click(){this.dispatchEvent(new Event("click"));}
  }
  const nodes=new Map(), element=id=>{if(!nodes.has(id))nodes.set(id,new Element());return nodes.get(id);};
  Object.assign(element("display").dataset,{connected:"true",synchronized:"true",workerGeneration:"3",fps:"30",frame:"0"});
  Object.assign(element("timeline"),{max:"59",value:"0"});element("error").hidden=true;
  const document=new EventTarget();document.getElementById=element;document.querySelector=element;
  document.currentScript={src:"http://studio.test/studio.js?cap=test-capability"};document.hidden=false;
  const window=new EventTarget(), frames=[], observers=[], requests=[], pending=deferred();
  let context=null;
  window.AudioContext=class extends Context {constructor(){super();context=this;}};
  window.crypto=webcrypto;
  window.fetch=async(url,options)=> {
    requests.push({url,options});
    if(url==="/api/audio")return new Response(JSON.stringify(info()),{headers:{"Content-Type":"application/json"}});
    const query=new URL(url,"http://studio.test").searchParams,start=Number(query.get("start")),count=Number(query.get("frames"));
    if(start===24000)return pending.promise;
    return response(info(),start,Math.min(count,info().sample_frames-start));
  };
  class Observer {constructor(callback){this.callback=callback;observers.push(this);}observe(target){this.target=target;}}
  runInNewContext(readFileSync(require.resolve("../crates/fmn-studio/src/studio_audio.js"),"utf8"),{
    window,document,MutationObserver:Observer,requestAnimationFrame:callback=>frames.push(callback),
    URL,URLSearchParams,AbortController,AbortSignal,TextDecoder,Event,DataView,Uint8Array,Float32Array,
  });
  return {element,window,document,requests,pending,get context(){return context;},
    tick(time){context.currentTime=time;const callbacks=frames.splice(0);for(const callback of callbacks)callback();},
    mutate(target){for(const observer of observers)if(observer.target===element(target))observer.callback();},
    async deliver(headers={}){pending.resolve(await response(info(),24000,24000,headers));},
  };
}
async function settleUntil(predicate) {
  const deadline=Date.now()+5000;
  while(Date.now()<deadline){if(predicate())return;await new Promise(resolve=>setTimeout(resolve,1));}
  assert(predicate(),"embedded page never reached the expected state");
}
async function embeddedBuffering() {
  const f=embeddedFixture();await settleUntil(()=>f.element("audio-status").textContent.includes("available"));
  assert.equal(f.context,null);f.element("audio-play").click();
  await settleUntil(()=>f.element("audio-status").dataset.state==="playing");
  f.tick(0.1);await settleUntil(()=>f.requests.some(r=>r.url.includes("start=24000")));
  f.tick(0.7);assert.equal(f.element("audio-status").dataset.state,"buffering");
  return f;
}

test("the embedded page enables automatic buffering and its Pause button cancels the pending refill",async()=>{
  const f=await embeddedBuffering();
  assert.equal(f.element("audio-play").textContent,"Pause narration");
  assert.equal(f.element("audio-play").getAttribute("aria-pressed"),"true");
  f.element("audio-play").click();
  assert.equal(f.element("audio-status").dataset.state,"paused");
  assert.equal(f.element("audio-play").getAttribute("aria-pressed"),"false");
  assert(f.requests.find(r=>r.url.includes("start=24000")).options.signal.aborted);
  await f.deliver();for(let n=0;n<5;n++)await new Promise(resolve=>setImmediate(resolve));
  assert.equal(f.context.nodes.length,1);assert.equal(f.element("audio-status").dataset.state,"paused");
});

test("the embedded page resumes a successful late response and rechecks the real DOM picture binding",async()=>{
  for(const changed of [false,true]) {
    const f=await embeddedBuffering();if(changed)f.element("display").dataset.fps="60";
    await f.deliver();await settleUntil(()=>f.element("audio-status").dataset.state!=="buffering");
    assert.equal(f.element("audio-status").dataset.state,changed?"paused":"playing");
    assert.equal(f.context.nodes.length,changed?1:2);
    if(changed)assert.match(f.element("audio-status").textContent,/clocks/);
    else assert.equal(f.context.nodes[1].when,0.76);
    f.window.dispatchEvent(new Event("pagehide"));
  }
});

test("every embedded edit and connection stop also cancels an automatic buffering state",async()=>{
  const actions=[
    f=>f.element("timeline").dispatchEvent(new Event("input")),
    f=>f.element("restart").click(),
    f=>f.element("preview").dispatchEvent(new Event("pointerdown")),
    f=>f.window.dispatchEvent(new Event("blur")),
    f=>f.window.dispatchEvent(new Event("offline")),
    f=>{f.document.hidden=true;f.document.dispatchEvent(new Event("visibilitychange"));},
    f=>f.window.dispatchEvent(new Event("pagehide")),
    f=>{f.element("display").dataset.workerGeneration="4";f.mutate("main");},
    f=>{f.element("display").dataset.connected="false";f.mutate("main");},
    f=>{f.element("error").hidden=false;f.mutate("error");},
  ];
  for(const action of actions) {
    const f=await embeddedBuffering();action(f);
    assert.equal(f.element("audio-status").dataset.state,"paused");
    await f.deliver();for(let n=0;n<5;n++)await new Promise(resolve=>setImmediate(resolve));
    assert.equal(f.context.nodes.length,1);assert.equal(f.element("audio-status").dataset.state,"paused");
    f.window.dispatchEvent(new Event("pagehide"));
  }
});

test("the embedded buffering path still rejects an actually checksummed-invalid transport response",async()=>{
  const f=await embeddedBuffering();await f.deliver({"X-FMN-SHA256":"b".repeat(64)});
  await settleUntil(()=>f.element("audio-status").dataset.state==="paused");
  assert.match(f.element("audio-status").textContent,/checksum/);assert.equal(f.context.nodes.length,1);
  f.tick(50);await flush();
  assert.equal(f.requests.filter(r=>r.url.includes("start=24000")).length,1);
  f.window.dispatchEvent(new Event("pagehide"));
});
