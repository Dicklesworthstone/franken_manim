#!/usr/bin/env python3
"""Run the production windowed scheduler through real Chromium Web Audio.

Requires Python Playwright and Chromium, only as test tooling. PCM decoding is
an explicit deterministic fixture; this does not claim native Wasm acceptance
or physical-device playback. All module inputs stay in memory; no HTTP server,
remote site, source recording, npm install, or generated Wasm package is needed.
"""
import argparse
import hashlib
import json
import shutil
import sys
from pathlib import Path

BROWSER_CASES = r"""async (source) => {
    const moduleURL=URL.createObjectURL(new Blob([source], {type:'text/javascript'}));
    const {prepareSoundtrack} = await import(moduleURL);URL.revokeObjectURL(moduleURL);
    const results=[];
    for (const [rate, device, seek, fps] of [[48000,48000,0,8],[48000,48000,3,8],[48000,48000,31,29],[44100,48000,0,29],[44100,48000,31,29],[48000,44100,31,29],[192000,48000,0,29],[384000,48000,0,29],[8000,48000,31,29]]) {
      const frames=300123, channels=2, offset=seek/fps;
      const length=Math.ceil((frames/rate - offset + .2)*device);
      const actual=new OfflineAudioContext(channels,length,device);
      const reference=new OfflineAudioContext(channels,length,device);
      let freed=0, maxBuffer=0, nodes=0;
      const sample=(i,c)=>Math.fround((((i*73)%251)-125)/128*(c?-0.4:0.5));
      const pcm={channels,sample_rate:rate,sample_frames:frames,free(){freed++;},copy_channel_into(c,start,dst){
        for(let i=0;i<dst.length;i++) dst[i]=sample(start+i,c);
      }};
      const context={sampleRate:device,get state(){return 'running';},get currentTime(){return actual.currentTime;},
        destination:actual.destination,resume:async()=>{},close:async()=>{},
        createBuffer(c,n,r){if(n>65536)throw new Error('unbounded playback allocation');maxBuffer=Math.max(maxBuffer,n);return actual.createBuffer(c,n,r);},
        createBufferSource(){nodes++;return actual.createBufferSource();}};
      const a=prepareSoundtrack({fps,frame_count:Math.ceil(frames/rate*fps),take_soundtrack:()=>pcm},()=>context);
      a.seek(seek);await a.play();
      const full=reference.createBuffer(channels,frames,rate);
      for(let c=0;c<channels;c++)for(let i=0;i<frames;i++)full.getChannelData(c)[i]=sample(i,c);
      const src=reference.createBufferSource();src.buffer=full;src.connect(reference.destination);src.start(.06,offset);
      const suspensions=[];
      for(let t=.125;t<length/device-.01;t+=.125){
        suspensions.push(actual.suspend(t).then(()=>{a.frameAt();return actual.resume();}));
      }
      const [rendered,expected]=await Promise.all([actual.startRendering(),reference.startRendering()]);
      await Promise.all(suspensions);let max=0,n=0,sum=0,at=0;
      for(let c=0;c<channels;c++)for(let i=0;i<length;i++){
        const diff=Math.abs(rendered.getChannelData(c)[i]-expected.getChannelData(c)[i]);
        if(!Number.isFinite(diff))throw new Error('nonfinite browser audio output');
        if(diff>max){max=diff;at=i;}if(diff>1e-5)n++;sum+=diff*diff;
      }
      a.dispose();results.push({rate,device,seek,fps,max,at,n,rms:Math.sqrt(sum/(length*channels)),freed,maxBuffer,nodes,samples:length*channels});
    } return results;
  }"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--chromium", default=shutil.which("chromium"))
    parser.add_argument("--source", type=Path, default=Path(__file__).resolve().parents[1] / "demo/wasm/audio-playback.mjs")
    args = parser.parse_args()
    try:
        from playwright.sync_api import sync_playwright
        source = args.source.read_bytes()
        with sync_playwright() as playwright:
            options = {"headless": True}
            if args.chromium:
                options["executable_path"] = args.chromium
            browser = playwright.chromium.launch(**options)
            try:
                page = browser.new_page()
                page.set_content("<!doctype html><title>Offline windowed audio acceptance</title>")
                # This timeout fails the check; it does not turn missing work into
                # a skipped case. Browser teardown also cancels pending rendering.
                expression = "async source => Promise.race([(" + BROWSER_CASES + ")(source), new Promise((_, reject) => setTimeout(() => reject(new Error('audio acceptance timed out')), 30000))])"
                results = page.evaluate(expression, source.decode("utf-8"))
                browser_version = browser.version
            finally:
                browser.close()
        if len(results) != 9:
            raise RuntimeError("missing browser test cases")
        failed = False
        digest = hashlib.sha256(source).hexdigest()
        for result in results:
            passed = result["n"] == 0 and result["max"] <= 1e-5 and result["freed"] == 1 and result["maxBuffer"] <= 65536 and result["nodes"] > 1
            failed |= not passed
            print(json.dumps({"schema": "fmn.windowed-audio-browser", "version": 1,
                "source_sha256": digest, "browser": browser_version,
                "passed": passed, **result}, sort_keys=True))
        return int(failed)
    except Exception as error:
        print(f"windowed audio acceptance failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
