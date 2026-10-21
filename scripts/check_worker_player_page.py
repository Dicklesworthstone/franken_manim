#!/usr/bin/env python3
"""Drive the production FMTL page controls with real Chromium workers and audio.

The decoder is the explicit fixture from check_picture_worker_browser, not a
native Wasm build. Blob-module bindings and a classic-worker import bootstrap
allow an entirely offline, opaque-origin harness. No runtime dependency is added.
"""
import argparse
import hashlib
import json
import re
import shutil
from pathlib import Path
from check_picture_worker_browser import FIXTURE, fixture_recording

BOOTSTRAP = r"""async sources => {
  const urls=[];
  const module=text=>{const u=URL.createObjectURL(new Blob([text],{type:'text/javascript'}));urls.push(u);return u;};
  const protocol=module(sources['worker-protocol.mjs']);
  const audio=module(sources['audio-playback.mjs']), clock=module(sources['playback.mjs']);
  const input=module(sources['bundle-input.mjs'].replace("'./worker-protocol.mjs'",JSON.stringify(protocol)));
  const decoder=module(sources.fixture);
  const core=module(sources['picture-worker-core.mjs'].replace("'./worker-protocol.mjs'",JSON.stringify(protocol)));
  const entry=module(sources['picture-worker.mjs'].replace("'./picture-worker-core.mjs'",JSON.stringify(core)).replace("'./pkg/fmn_wasm.js'",JSON.stringify(decoder)));
  const worker=module(`const pending=[];self.onmessage=e=>pending.push(e);import(${JSON.stringify(entry)}).then(()=>{const receive=self.onmessage;for(const e of pending)receive(e);});`);
  const session=module(sources['worker-session.mjs'].replace("'./worker-protocol.mjs'",JSON.stringify(protocol)));
  const ui=module(sources['player-ui.mjs'].replace("'./worker-session.mjs'",JSON.stringify(session))
    .replace("'./audio-playback.mjs'",JSON.stringify(audio)).replace("'./playback.mjs'",JSON.stringify(clock))
    .replace("'./bundle-input.mjs'",JSON.stringify(input)));
  const {WorkerTimelineSession}=await import(session),{mountTimelinePlayer}=await import(ui);
  const {readBundleResponse}=await import(input);
  const workers=[],contexts=[],starts=[],errors=[];
  const createWorker=()=>{const w=new Worker(worker),record={terminated:0};workers.push(record);
    const terminate=w.terminate.bind(w);w.terminate=()=>{record.terminated++;terminate();};return w;};
  const owner=mountTimelinePlayer({
    createSession:options=>new WorkerTimelineSession({...options,createWorker}),
    createContext(){const c=new AudioContext({sampleRate:48000});contexts.push(c);
      const create=c.createBufferSource.bind(c);c.createBufferSource=()=>{const n=create(),start=n.start.bind(n);
        n.start=(when,offset,...rest)=>{starts.push({when,offset,frames:n.buffer.length});return start(when,offset,...rest);};return n;};return c;},
    readDemo:signal=>readBundleResponse(new Response(new Uint8Array(sources.recordings[1])),signal),
  });
  window.workerPageEvidence={owner,workers,contexts,starts,errors,urls};
  return true;
}"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--chromium', default=shutil.which('chromium'))
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    directory = root / 'demo/wasm'
    names = ['worker-protocol.mjs', 'worker-session.mjs', 'picture-worker-core.mjs', 'picture-worker.mjs',
        'audio-playback.mjs', 'playback.mjs', 'bundle-input.mjs', 'player-ui.mjs']
    sources = {name: (directory / name).read_text() for name in names}
    html = (directory / 'player.html').read_text()
    if 'mountTimelinePlayer();' not in html:
        raise RuntimeError('The shipping page is not wired to the worker player UI.')
    markup = re.sub(r'<script type="module">.*?</script>', '', html, flags=re.S)
    from playwright.sync_api import sync_playwright
    checks = []
    with sync_playwright() as p:
        options = {'headless': True}
        if args.chromium:
            options['executable_path'] = args.chromium
        browser = p.chromium.launch(**options)
        try:
            page = browser.new_page()
            failures = []
            page.on('pageerror', lambda error: failures.append(str(error)))
            page.set_content(markup)
            page.evaluate(BOOTSTRAP, {**sources, 'fixture': FIXTURE, 'recordings': {str(n): list(fixture_recording(n)) for n in [1, 2, 3, 255]}})
            page.wait_for_function("!document.getElementById('play').disabled")
            assert page.locator('#status').inner_text().endswith('worker rendering')
            assert page.evaluate('workerPageEvidence.contexts.length') == 0
            checks.append('silent admission without audio device')
            page.locator('#play').click()
            page.wait_for_function("Number(document.getElementById('scrub').value)>0")
            page.locator('#play').click()
            paused = page.locator('#scrub').input_value()
            page.wait_for_timeout(200)
            assert page.locator('#scrub').input_value() == paused
            checks.append('pause suppresses in-flight picture')
            page.locator('#scrub').evaluate("s=>{for(const n of [15,20,8]){s.value=String(n);s.dispatchEvent(new Event('input'));}}")
            page.wait_for_function("document.getElementById('frameLabel').textContent==='8 / 31' && !document.getElementById('play').disabled")
            page.locator('#labels button', has_text='middle').click()
            page.wait_for_function("document.getElementById('frameLabel').textContent==='16 / 31'")
            checks.append('rapid seek and authored label navigation')
            page.locator('#bundleFile').set_input_files({'name':'narrated.fmtl','mimeType':'application/octet-stream','buffer':fixture_recording(2)})
            page.wait_for_function("document.getElementById('status').textContent.includes('2-channel sound @ 48000')")
            assert page.evaluate('workerPageEvidence.starts.length') == 0
            page.locator('#play').click()
            page.wait_for_function('workerPageEvidence.starts.length>0')
            page.wait_for_function("Number(document.getElementById('scrub').value)>=2")
            page.locator('#play').click()
            phase = page.evaluate('workerPageEvidence.owner.current.audio.positionSeconds')
            page.wait_for_timeout(150)
            assert page.evaluate('workerPageEvidence.owner.current.audio.positionSeconds') == phase
            page.locator('#play').click()
            page.wait_for_function('workerPageEvidence.owner.current.audio.playing')
            assert page.evaluate('workerPageEvidence.owner.current.audio.positionSeconds') >= phase
            checks.append('gesture audio and fractional pause/resume')
            page.locator('#scrub').evaluate("s=>{s.value='4';s.dispatchEvent(new Event('input'));}")
            page.wait_for_function("document.getElementById('frameLabel').textContent==='4 / 31' && !document.getElementById('play').disabled")
            assert page.evaluate('workerPageEvidence.owner.current.audio.positionSeconds') == .5
            page.locator('#play').click()
            page.wait_for_function("document.getElementById('play').textContent==='Play' && document.getElementById('scrub').value==='31'", timeout=15000)
            assert page.evaluate('Math.max(...workerPageEvidence.starts.map(s=>s.frames))') <= 65536
            checks.append('exact seek and final picture after silent suffix')
            page.locator('#bundleFile').set_input_files({'name':'broken.fmtl','mimeType':'application/octet-stream','buffer':fixture_recording(255)})
            page.wait_for_function("document.getElementById('status').textContent.includes('previous recording is still loaded')")
            assert not page.locator('#play').is_disabled()
            assert page.evaluate('workerPageEvidence.contexts[0].state') != 'closed'
            checks.append('failed replacement preserves audible movie')
            page.locator('#loadDemo').click()
            page.wait_for_function("document.getElementById('status').textContent.startsWith('bundle.fmtl:')")
            page.wait_for_function("workerPageEvidence.contexts[0].state==='closed'")
            checks.append('successful replacement releases prior audio and worker')
            page.evaluate("workerPageEvidence.owner.dispose()")
            assert page.evaluate('workerPageEvidence.workers.every(w=>w.terminated===1)')
            assert page.locator('#play').is_disabled()
            assert not failures, failures
            checks.append('page owner disposal leaves no workers')
            print(json.dumps({'schema':'fmn.worker-player-page','version':1,'passed':True,
                'browser':browser.version,'nativeDecoder':'explicit fixture','checks':checks,
                'source_sha256':{name:hashlib.sha256(text.encode()).hexdigest() for name,text in {**sources,'player.html':html}.items()}},sort_keys=True))
        finally:
            browser.close()


if __name__ == '__main__':
    main()
