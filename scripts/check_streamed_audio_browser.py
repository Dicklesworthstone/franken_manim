#!/usr/bin/env python3
"""Check streamed PCM with real Web Audio and normally hosted module workers.

The native decoder/PCM is an explicit fixture; no native Wasm or hardware-audio
acceptance is claimed. Serve only fixed local modules from an ephemeral loopback
HTTP server. Playwright/Chromium are test tooling, not runtime dependencies.
"""
import argparse
import hashlib
import json
import shutil
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

FIXTURE = r"""
export default async function init() {}
export class FmnPlayer {
  static from_bundle_with_audio(bytes) { return new FmnPlayer(); }
  constructor() {
    this.fps=29; this.frame_count=800; this.duration_seconds=800/29;
    this.engine_version='explicit-native-fixture'; this.has_camera_track=true; this.has_soundtrack=true;
  }
  set_viewport(w,h) { this.width=w; this.height=h; }
  labels() { return ['begin']; }
  frame_of_label() { return 0; }
  seek_frame(frame) { this.frame=frame; }
  render_into(frame,out) {
    if(frame===799) throw new Error('fixture render failure');
    const end=performance.now()+(frame?120:0); while(performance.now()<end){}
    out.fill(frame%255);
  }
  take_soundtrack() {
    return {channels:2,sample_rate:48000,sample_frames:32000000,
      copy_channel_into(channel,start,out) {
        if(out.length>65536) throw new Error('whole-track transfer attempted');
        for(let i=0;i<out.length;i++) out[i]=(channel?-1:1)*((start+i)%1009)/1024;
      },free(){}};
  }
  free() {}
}
"""
OFFLINE = r"""async ({audioURL}) => {
  const {prepareSoundtrack}=await import(audioURL);
  const results=[];
  const flush=async audio=>{for(let i=0;i<8;i++){await Promise.resolve();audio.frameAt();}};
  for(const [rate,device,seek,fps] of [[48000,48000,0,8],[48000,48000,3,8],[48000,48000,31,29],
    [44100,48000,0,29],[44100,48000,31,29],[48000,44100,31,29],[192000,48000,0,29],
    [384000,48000,0,29],[8000,48000,31,29]]) {
    const frames=300123,offset=seek/fps,length=Math.ceil((frames/rate-offset+.2)*device);
    const actual=new OfflineAudioContext(2,length,device), reference=new OfflineAudioContext(2,length,device);
    let requests=0,maximum=0,freed=0;
    const sample=(i,c)=>Math.fround((((i*73)%251)-125)/128*(c?-.4:.5));
    const pcm={channels:2,sample_rate:rate,sample_frames:frames,free(){freed++;},
      async read_window(start,count,signal) {
        if(signal.aborted) throw new DOMException('aborted','AbortError');
        if(count>65536) throw new Error('unbounded PCM request');requests++;maximum=Math.max(maximum,count);
        return [0,1].map(c=>Float32Array.from({length:count},(_,i)=>sample(start+i,c)));
      }};
    const context={sampleRate:device,state:'running',get currentTime(){return actual.currentTime;},
      destination:actual.destination,resume:async()=>{},close:async()=>{},
      createBuffer:(...args)=>actual.createBuffer(...args),createBufferSource:()=>actual.createBufferSource()};
    const audio=await prepareSoundtrack({fps,frame_count:Math.ceil(frames/rate*fps),take_soundtrack:()=>pcm},()=>context);
    audio.seek(seek);await audio.play();await flush(audio);
    const full=reference.createBuffer(2,frames,rate);
    for(let c=0;c<2;c++)for(let i=0;i<frames;i++)full.getChannelData(c)[i]=sample(i,c);
    const source=reference.createBufferSource();source.buffer=full;source.connect(reference.destination);source.start(.06,offset);
    const pauses=[];
    for(let t=.125;t<length/device-.01;t+=.125) pauses.push(actual.suspend(t).then(async()=>{
      audio.frameAt();await flush(audio);return actual.resume();
    }));
    const [a,b]=await Promise.all([actual.startRendering(),reference.startRendering()]);await Promise.all(pauses);
    let max=0,bad=0;
    for(let c=0;c<2;c++)for(let i=0;i<length;i++){
      const difference=Math.abs(a.getChannelData(c)[i]-b.getChannelData(c)[i]);
      if(!Number.isFinite(difference))throw new Error('nonfinite audio');
      max=Math.max(max,difference);if(difference>1e-5)bad++;
    }
    audio.dispose();
    if(bad||freed!==1||maximum>65536)throw new Error(JSON.stringify({rate,device,seek,max,bad,freed,maximum}));
    results.push({rate,device,seek,fps,max,bad,samples:length*2,requests,maximum,freed});
  }
  return results;
}"""
HOSTED = r"""async ({sessionURL,audioURL,workerURL,moduleWorker}) => {
  const {WorkerTimelineSession}=await import(sessionURL);
  const {prepareSoundtrack}=await import(audioURL);
  const bytes=()=>new Uint8Array(57);
  const check=(value,text)=>{if(!value)throw new Error(text);};
  const wait=async condition=>{const start=performance.now();while(!condition()){
    if(performance.now()-start>5000)throw new Error('worker check timed out');
    await new Promise(resolve=>setTimeout(resolve,5));}};
  const requests=[],replies=[],devices=[],workers=[],frames=[],errors=[];
  let beats=0;const timer=setInterval(()=>beats++,5);
  const session=new WorkerTimelineSession({width:8,height:4,streamAudio:true,
    createWorker(){
      const worker=new Worker(workerURL,moduleWorker?{type:'module'}:{});workers.push(worker);
      const post=worker.postMessage.bind(worker);worker.postMessage=(message,transfer)=>{requests.push({kind:message.kind,count:message.count,frame:message.frame});post(message,transfer);};
      worker.addEventListener('message',event=>{const m=event.data;replies.push({kind:m.kind,bytes:m.channels?.reduce((n,b)=>n+b.byteLength,0)||0});});
      return worker;
    },
    prepareAudio:player=>prepareSoundtrack(player,()=>{const context=new AudioContext({sampleRate:48000});devices.push(context);return context;}),
    onFrame:frame=>frames.push(frame.index),onError:error=>errors.push(error.message)});
  try {
    check(await session.load(async()=>bytes()),'load failed');
    check(replies[0].bytes===0,'load transferred full PCM');
    check(requests.filter(r=>r.kind==='audio').length===1,'unbounded startup reads');
    const audio=session.current.audio;check(!audio.playing,'autoplay');
    await audio.play();const before=beats;
    session.render(1);for(let i=0;i<10000;i++)session.render(2+i%100);
    for(let i=0;i<60;i++){audio.frameAt();await new Promise(resolve=>setTimeout(resolve,10));}
    await wait(()=>frames.at(-1)===101);
    check(beats>before,'UI blocked by rendering');check(audio.playing,'stream did not remain live');
    audio.pause();const position=audio.positionSeconds,device=devices[0];
    session.render(799);await wait(()=>errors.length===1);
    check(await session.restart(),'recovery failed');
    check(session.current.audio===audio&&devices.length===1&&devices[0]===device,'recovery replaced the audio device');
    check(audio.positionSeconds===position&&!audio.playing,'recovery lost exact phase or autoplayed');
    await audio.play();for(let i=0;i<15;i++){audio.frameAt();await new Promise(resolve=>setTimeout(resolve,10));}
    check(audio.positionSeconds>=position,'recovered playback rewound');audio.pause();
    check(requests.filter(r=>r.kind==='audio').every(r=>r.count<=65536),'unbounded request');
    check(replies.every(r=>r.bytes<=512*1024),'unbounded transfer');
    session.dispose();await wait(()=>devices.every(d=>d.state==='closed'));
    return {normalModuleWorkers:moduleWorker,advertisedPcmBytes:32000000*8,requests:requests.length,
      audioRequests:requests.filter(r=>r.kind==='audio').length,maxTransferredBytes:Math.max(...replies.map(r=>r.bytes)),
      initialAudioRequests:1,heartbeatTicks:beats,frames,workers:workers.length,devices:devices.length,passed:true};
  } finally {clearInterval(timer);session.dispose();}
}"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--chromium', default=shutil.which('chromium'))
    parser.add_argument('--in-memory', action='store_true', help='Use Blob modules when browser policy refuses loopback HTTP; this does not test hosted module-worker startup.')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1] / 'demo/wasm'
    names = ['audio-playback.mjs', 'worker-session.mjs', 'worker-protocol.mjs', 'picture-worker-core.mjs', 'picture-worker.mjs']
    sources = {name: (root / name).read_bytes() for name in names}
    routes = {'/' + name: content for name, content in sources.items()}
    routes['/pkg/fmn_wasm.js'] = FIXTURE.encode()
    routes['/'] = b'<!doctype html><title>Streamed audio acceptance</title>'

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            content = routes.get(self.path)
            if content is None:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header('Content-Type', 'text/html' if self.path == '/' else 'text/javascript')
            self.send_header('Content-Length', str(len(content)))
            self.end_headers()
            self.wfile.write(content)

        def log_message(self, *args):
            pass

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    from playwright.sync_api import sync_playwright
    try:
        with sync_playwright() as p:
            options = {'headless': True, 'args': ['--autoplay-policy=no-user-gesture-required']}
            if args.chromium:
                options['executable_path'] = args.chromium
            browser = p.chromium.launch(**options)
            try:
                page = browser.new_page()
                if args.in_memory:
                    page.set_content('<!doctype html><title>In-memory streamed audio acceptance</title>')
                    loader = """async sources => {
                      const urls=[];
                      const mod=text=>{const url=URL.createObjectURL(new Blob([text],{type:'text/javascript'}));urls.push(url);return url;};
                      const protocol=mod(sources['worker-protocol.mjs']);
                      const audioURL=mod(sources['audio-playback.mjs']);
                      const decoder=mod(sources.fixture);
                      const core=mod(sources['picture-worker-core.mjs'].replace("'./worker-protocol.mjs'",JSON.stringify(protocol)));
                      const entry=mod(sources['picture-worker.mjs'].replace("'./picture-worker-core.mjs'",JSON.stringify(core)).replace("'./pkg/fmn_wasm.js'",JSON.stringify(decoder)));
                      const workerURL=mod(`const pending=[];self.onmessage=e=>pending.push(e);import(${JSON.stringify(entry)}).then(()=>{const receive=self.onmessage;for(const event of pending)receive(event);});`);
                      const sessionURL=mod(sources['worker-session.mjs'].replace("'./worker-protocol.mjs'",JSON.stringify(protocol)));
                      try {
                        const offline=await (""" + OFFLINE + """)({audioURL});
                        const hosted=await (""" + HOSTED + """)({audioURL,sessionURL,workerURL,moduleWorker:false});
                        return {offline,hosted};
                      } finally {for(const url of urls)URL.revokeObjectURL(url);}
                    }"""
                    result = page.evaluate(loader, {**{name: data.decode() for name, data in sources.items()}, 'fixture': FIXTURE})
                    offline, hosted = result['offline'], result['hosted']
                else:
                    page.goto(f'http://127.0.0.1:{server.server_port}/')
                    offline = page.evaluate(OFFLINE, {'audioURL': '/audio-playback.mjs'})
                    hosted = page.evaluate(HOSTED, {'audioURL': '/audio-playback.mjs',
                        'sessionURL': '/worker-session.mjs', 'workerURL': '/picture-worker.mjs', 'moduleWorker': True})
                if len(offline) != 9 or not hosted['passed']:
                    raise RuntimeError('missing browser acceptance cases')
                print(json.dumps({'schema': 'fmn.streamed-audio-browser', 'version': 1,
                    'browser': browser.version, 'offline': offline, 'hosted': hosted,
                    'nativeDecoder': 'explicit fixture', 'passed': True,
                    'source_sha256': {name: hashlib.sha256(data).hexdigest() for name, data in sources.items()}}, sort_keys=True))
            finally:
                browser.close()
    finally:
        server.shutdown()
        thread.join()
        server.server_close()


if __name__ == '__main__':
    main()
