#!/usr/bin/env python3
"""Exercise the shipping picture worker/session in real Chromium.

The native decoder is an explicit deterministic API fixture served only at its
fixed module URL in this test. Scheduling, transfer, cancellation, module execution and audio preparation
are real. Imports are bound to in-memory Blob URLs; no network is used. This is NOT native Wasm render evidence.
Requires Python Playwright and Chromium as test tools, not product dependencies.
"""
import argparse
import hashlib
import json
import shutil
import sys
from pathlib import Path

FIXTURE = r"""
export default async function init() {}
export class FmnPlayer {
  static from_bundle_with_audio(bytes) {
    if (bytes[24] === 255) throw new Error('fixture decoder refusal');
    const until = performance.now() + 160;
    while (performance.now() < until) {}
    return new FmnPlayer(bytes[24] === 2 || bytes[24] === 3, bytes[24] === 3);
  }
  constructor(sound, fail = false) {
    this.fail = fail;
    this.fps=8;this.frame_count=32;this.duration_seconds=4;
    this.engine_version='explicit-decoder-fixture';this.has_camera_track=true;this.has_soundtrack=sound;
  }
  set_viewport(w,h){this.width=w;this.height=h;}
  labels(){return ['begin','middle'];}
  frame_of_label(n){return n==='begin'?0:16;}
  seek_frame(n){this.current_frame=n;}
  render_into(frame,out){
    if(this.fail && frame===13) throw new Error('fixture native render trap');
    const until=performance.now()+(frame?120:0);while(performance.now()<until){}
    for(let i=0;i<out.length;i++)out[i]=(frame*31+i)%256;
  }
  take_soundtrack(){this.has_soundtrack=false;return {channels:2,sample_frames:150000,sample_rate:48000,
    copy_channel_into(c,start,dst){for(let i=0;i<dst.length;i++)dst[i]=(c?-1:1)*((start+i)%127)/128;},free(){}};}
  free(){}
}
"""
def fixture_recording(marker):
    body = bytearray(25)
    body[:4] = b'FMTL'
    body[16:24] = (1).to_bytes(8, 'little')
    body[24] = marker
    return bytes(body) + hashlib.sha256(body).digest()


CASES = r"""async ({sessionURL, audioURL, workerURL, recordings}) => {
  const encoded=marker=>new Uint8Array(recordings[marker]);
  const {WorkerTimelineSession}=await import(sessionURL);
  const {prepareSoundtrack}=await import(audioURL);
  const check=(ok,message)=>{if(!ok)throw new Error(message);};
  const wait=async condition=>{const start=performance.now();while(!condition()){
    if(performance.now()-start>5000)throw new Error('browser worker test timed out');
    await new Promise(resolve=>setTimeout(resolve,5));}};
  let heartbeat=0;const timer=setInterval(()=>heartbeat++,5);
  const workers=[],sent=[],frames=[],errors=[],devices=[];
  let copied=0;
  const session=new WorkerTimelineSession({width:8,height:4,timeoutMs:5000,
    createWorker(){const w=new Worker(workerURL);
      const post=w.postMessage.bind(w),terminate=w.terminate.bind(w);
      const record={terminated:0};workers.push(record);
      w.postMessage=(m,t)=>{sent.push({kind:m.kind,frame:m.frame});post(m,t);};
      w.terminate=()=>{record.terminated++;terminate();};return w;},
    onFrame(frame){frames.push({index:frame.index,bytes:[...frame.rgba]});},onError(e){errors.push(e.message);},
    prepareAudio(player){const pcm=player.take_soundtrack();
      for(let c=0;c<2;c++){const dst=new Float32Array(pcm.sample_frames);pcm.copy_channel_into(c,0,dst);
        for(let i=0;i<dst.length;i++)check(dst[i]===(c?-1:1)*(i%127)/128,'transferred PCM bit drift');copied+=dst.length;}
      const context=new AudioContext({sampleRate:48000});devices.push(context);
      return prepareSoundtrack({fps:player.fps,frame_count:player.frame_count,take_soundtrack:()=>pcm},()=>context);
    }});
  try {
    let before=heartbeat;const bytes=encoded(1);
    check(await session.load(async()=>bytes),'silent load failed');
    const decodingTicks=heartbeat-before;check(decodingTicks>0,'native decode blocked UI heartbeat');
    check(bytes.byteLength===0,'bundle did not transfer ownership');session.present();
    check(frames[0].bytes.every((v,i)=>v===i%256),'first frame changed');
    before=heartbeat;session.render(1);for(let i=0;i<10000;i++)session.render(2+i%30);
    check(sent.length===2,'seek storm sent unbounded messages');await wait(()=>frames.at(-1).index===11);
    const renderingTicks=heartbeat-before;check(renderingTicks>0,'raster blocked UI heartbeat');
    check(frames.length===3&&sent.length===3,'preview did not coalesce to latest index');
    check(frames.at(-1).bytes.every((v,i)=>v===(11*31+i)%256),'worker changed raster bytes');
    session.render(25);session.invalidate();session.render(4);const previous=frames.length;
    await wait(()=>frames.at(-1).index===4);check(frames.length===previous+1,'cancelled raster leaked into canvas');
    const prior=session.current;
    let refused=false;try{await session.load(async()=>encoded(255));}catch{refused=true;}
    check(refused&&session.current===prior,'failed replacement destroyed previous movie');
    session.render(7);await wait(()=>frames.at(-1).index===7);
    check(await session.load(async()=>encoded(2)),'audible replacement failed');
    check(copied===300000,'missing transferred audio samples');check(workers[0].terminated===1,'old picture worker survived replacement');
    check(!session.current.audio.playing,'load started audio without a gesture');
    check(errors.length===0,'unexpected active-worker error before restart checks');
    let marker=3;
    check(await session.load(async()=>encoded(marker)),'recovery fixture failed');
    const audio=session.current.audio;audio.seek(6);const phase=audio.positionSeconds, copies=copied;
    session.render(13);await wait(()=>errors.length===1);
    check(!!session.current.renderer.error,'native render trap did not fail the connection');
    check(await session.restart(),'restart failed');session.present();
    check(session.current.audio===audio&&audio.positionSeconds===phase&&!audio.playing,'restart replaced or retimed paused narration');
    check(copied===copies,'restart transferred audio again');check(frames.at(-1).index===6,'restart lost the selected picture');
    marker=2;let changed=false;try{await session.restart();}catch(e){changed=e.message.includes('Recording changed');}
    check(changed&&session.current.audio===audio,'changed source was not refused atomically');
    marker=3;check(await session.restart(),'retry after changed source failed');
    session.dispose();await wait(()=>devices.every(c=>c.state==='closed'));
    check(workers.every(w=>w.terminated===1),'worker disposal count differs');
    check(errors.length===1,'unexpected recovery error');
    return {decodingTicks,renderingTicks,copiedSamples:copied,workers:workers.length,
      renderedIndices:frames.map(f=>f.index), recovery:['native trap', 'same audio owner', 'exact paused phase', 'no repeated PCM transfer', 'changed source refusal', 'retry'],nativeDecoder:'explicit fixture',passed:true};
  } finally {clearInterval(timer);session.dispose();}
}"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--chromium', default=shutil.which('chromium'))
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    names = ['worker-session.mjs', 'worker-protocol.mjs', 'picture-worker-core.mjs', 'picture-worker.mjs', 'audio-playback.mjs']
    sources = {name: (root / 'demo/wasm' / name).read_text() for name in names}
    from playwright.sync_api import sync_playwright
    expression = """async sources => {
      const urls = [];
      const module = text => {const url=URL.createObjectURL(new Blob([text],{type:'text/javascript'}));urls.push(url);return url;};
      const protocol = module(sources['worker-protocol.mjs']);
      const audioURL = module(sources['audio-playback.mjs']);
      const decoder = module(sources.fixture);
      const core = module(sources['picture-worker-core.mjs'].replace("'./worker-protocol.mjs'",JSON.stringify(protocol)));
      const entry = module(sources['picture-worker.mjs'].replace("'./picture-worker-core.mjs'",JSON.stringify(core)).replace("'./pkg/fmn_wasm.js'",JSON.stringify(decoder)));
      // Opaque-origin pages cannot start module Workers in Chromium. Import
      // the production module from a classic worker; buffer its one init event.
      const workerURL = module(`const pending=[]; self.onmessage=e=>pending.push(e); import(${JSON.stringify(entry)}).then(()=>{const receive=self.onmessage;for(const event of pending)receive(event);}).catch(error=>{self.postMessage({protocol:'fmn.picture-worker.v2',id:pending[0]?.data.id,kind:'error',message:String(error)});});`);
      const sessionURL = module(sources['worker-session.mjs'].replace("'./worker-protocol.mjs'",JSON.stringify(protocol)));
      try {return await (""" + CASES + """)({sessionURL,audioURL,workerURL,recordings:sources.recordings});}
      finally {for(const url of urls)URL.revokeObjectURL(url);}
    }"""
    with sync_playwright() as p:
        options = {'headless': True}
        if args.chromium:
            options['executable_path'] = args.chromium
        browser = p.chromium.launch(**options)
        try:
            page = browser.new_page()
            page.on('console', lambda msg: print('browser:', msg.type, msg.text, file=sys.stderr))
            page.on('pageerror', lambda err: print('pageerror:', err, file=sys.stderr))
            page.set_content('<!doctype html><title>Picture-worker browser acceptance</title>')
            result = page.evaluate(expression, {**sources, 'fixture': FIXTURE, 'recordings': {str(n): list(fixture_recording(n)) for n in [1, 2, 3, 255]}})
            result.update(schema='fmn.picture-worker-browser', version=1, browser=browser.version,
                source_sha256={name: hashlib.sha256(text.encode()).hexdigest() for name, text in sources.items()})
            print(json.dumps(result, sort_keys=True))
        finally:
            browser.close()


if __name__ == '__main__':
    main()
