import test from 'node:test';
import assert from 'node:assert/strict';
import {readBundleStream, readBundleFile, readBundleResponse} from './bundle-input.mjs';
const stream = chunks => new ReadableStream({start(c){for(const chunk of chunks)c.enqueue(Uint8Array.from(chunk));c.close();}});

test('known and unknown sizes preserve exact chunked recording bytes',async()=>{
  for(const expectedSize of [null,4])assert.deepEqual(await readBundleStream(stream([[1],[2,3],[],[4]]),{expectedSize,maxBytes:4}),new Uint8Array([1,2,3,4]));
});
test('declared-size truncation, overflow and unknown-size overrun are refused',async()=>{
  for(const [chunks,expectedSize,maxBytes] of [[[[1,2]],3,4],[[[1,2,3]],2,4],[[[1,2],[3,4,5]],null,4]]){
    await assert.rejects(readBundleStream(stream(chunks),{expectedSize,maxBytes}));
  }
});
test('byte limit is enforced during streaming and cancels without draining the tail',async()=>{
  let cancelled=0,reads=0;
  const input=new ReadableStream({pull(c){reads++;c.enqueue(new Uint8Array(6));},cancel(){cancelled++;}});
  await assert.rejects(readBundleStream(input,{maxBytes:4}),/budget/);assert.equal(cancelled,1);assert(reads<=2);assert.equal(input.locked,false);
});
test('aborting a pending stream read settles immediately and releases its reader',async()=>{
  let cancelled=0;const input=new ReadableStream({cancel(){cancelled++;}}), controller=new AbortController();
  const read=readBundleStream(input,{signal:controller.signal});controller.abort();
  await assert.rejects(read,e=>e.name==='AbortError');assert.equal(cancelled,1);assert.equal(input.locked,false);
});
test('an already cancelled read never waits for bytes',async()=>{
  const controller=new AbortController();controller.abort();let cancelled=0;
  await assert.rejects(readBundleStream(new ReadableStream({cancel(){cancelled++;}}),{signal:controller.signal}),e=>e.name==='AbortError');assert.equal(cancelled,1);
});
test('file admission precedes stream acquisition and exact file lengths are verified',async()=>{
  let opened=0;const file={size:5,stream(){opened++;return stream([[1,2,3,4,5]]);}};
  assert.throws(()=>readBundleFile(file,undefined,4));assert.equal(opened,0);
  assert.deepEqual(await readBundleFile(file,undefined,5),new Uint8Array([1,2,3,4,5]));assert.equal(opened,1);
});
test('response admission refuses missing bodies, bad status and malformed lengths',async()=>{
  for(const response of [new Response(null),new Response('bad',{status:404}),new Response('x',{headers:{'Content-Length':'1e3'}}),new Response('x',{headers:{'Content-Length':'9007199254740992'}})]){
    await assert.rejects(readBundleResponse(response,undefined,4));
  }
});
test('response and local Blob readers work with real standard stream implementations',async()=>{
  const bytes=new Uint8Array([70,77,84,76]);
  assert.deepEqual(await readBundleResponse(new Response(bytes,{headers:{'Content-Length':'4'}})),bytes);
  assert.deepEqual(await readBundleFile(new Blob([bytes])),bytes);
});
test('a stream of empty chunks hits the work bound instead of looping forever',async()=>{
  let reads=0,cancelled=0;const input=new ReadableStream({pull(c){reads++;c.enqueue(new Uint8Array());},cancel(){cancelled++;}});
  await assert.rejects(readBundleStream(input),/excessive/);assert.equal(cancelled,1);assert(reads<=65538);
});
test('unknown-size chunks cannot retain unrelated backing storage',async()=>{
  const input=new Uint8Array([9,1,2,3,8]);const body=new ReadableStream({start(c){c.enqueue(input.subarray(1,4));c.close();}});
  assert.deepEqual(await readBundleStream(body,{maxBytes:3}),new Uint8Array([1,2,3]));
});
