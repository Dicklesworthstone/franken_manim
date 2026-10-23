#!/usr/bin/env python3
"""Live viewport controls in the shipping page with real Chromium workers.

The FmnPlayer decoder/raster is an explicit fixture. Actual page markup,
controller, session, worker runtime, transfers, Canvas, ResizeObserver and Web
Audio execute. This is not native Wasm or physical-device rendering evidence.
"""
import argparse
import hashlib
import json
import re
import shutil
from pathlib import Path
from check_worker_player_page import BOOTSTRAP
from check_picture_worker_browser import FIXTURE, fixture_recording

VIEWPORT_FIXTURE = FIXTURE + r"""
const originalRender = FmnPlayer.prototype.render_into;
FmnPlayer.prototype.render_into = function(frame, out) {
  originalRender.call(this, frame, out);
  if(out.length !== this.width*this.height*4) throw new Error('fixture viewport mismatch');
  for(let n=0;n<out.length;n+=4) {
    out[n]=frame;out[n+1]=this.width%251;out[n+2]=this.height%251;out[n+3]=255;
  }
};
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--chromium', default=shutil.which('chromium'))
    parser.add_argument('--screenshot')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    names = ['worker-protocol.mjs', 'worker-session.mjs', 'picture-worker-core.mjs',
             'picture-worker.mjs', 'audio-playback.mjs', 'playback.mjs', 'bundle-input.mjs', 'player-ui.mjs']
    sources = {name: (root / 'demo/wasm' / name).read_text() for name in names}
    html = (root / 'demo/wasm/player.html').read_text()
    assert 'mountTimelinePlayer();' in html and 'id="resolution"' in html
    from playwright.sync_api import sync_playwright
    checks = []
    with sync_playwright() as p:
        options = {'headless': True}
        if args.chromium:
            options['executable_path'] = args.chromium
        browser = p.chromium.launch(**options)
        try:
            page = browser.new_page(viewport={'width': 1000, 'height': 900}, device_scale_factor=2)
            page.set_default_timeout(10000)
            errors = []
            page.on('pageerror', lambda error: errors.append(str(error)))
            page.set_content(re.sub(r'<script type="module">.*?</script>', '', html, flags=re.S))
            page.evaluate(BOOTSTRAP, {**sources, 'fixture': VIEWPORT_FIXTURE,
                'recordings': {str(n): list(fixture_recording(n)) for n in [1, 2]}})
            page.wait_for_function("!document.getElementById('play').disabled")
            page.wait_for_function("!document.getElementById('viewportLabel').textContent.includes('pending')")
            assert page.evaluate("document.getElementById('canvas').width") > 640
            assert page.evaluate("document.getElementById('canvas').width") <= 1920
            checks.append('automatic initial high-DPI viewport is bounded')

            def verify(width, height, index=None):
                page.wait_for_function("([w,h])=>{const c=document.getElementById('canvas');return c.width===w&&c.height===h&&!document.getElementById('viewportLabel').textContent.includes('pending');}", arg=[width, height])
                data = page.evaluate("() => {const c=document.getElementById('canvas');return {frame:Number(document.getElementById('scrub').value),pixels:[...c.getContext('2d').getImageData(c.width-1,c.height-1,1,1).data]};}")
                if index is not None:
                    assert data['frame'] == index, data
                assert data['pixels'] == [data['frame'], width % 251, height % 251, 255], data

            page.locator('#scrub').evaluate("s=>{s.value='7';s.dispatchEvent(new Event('input'));}")
            page.wait_for_function("document.getElementById('frameLabel').textContent==='7 / 31'")
            page.locator('#resolution').select_option('1280x720')
            verify(1280, 720, 7)
            page.locator('#resolution').select_option('1080x1920')
            verify(1080, 1920, 7)
            assert page.evaluate('workerPageEvidence.workers.length') == 1
            checks.append('landscape and portrait rasterize the same selected frame without another worker')

            page.locator('#resolution').select_option('auto')
            page.set_viewport_size({'width': 360, 'height': 800})
            page.wait_for_function("()=>{const c=document.getElementById('canvas');const w=Math.max(16,Math.min(1920,Math.round(c.clientWidth*devicePixelRatio/16)*16));return c.width===w&&c.height===w*9/16&&!document.getElementById('viewportLabel').textContent.includes('pending');}")
            assert page.evaluate("document.getElementById('canvas').width") < 800
            assert page.evaluate('workerPageEvidence.workers.length') == 1
            checks.append('automatic preview follows mobile container size at high pixel density')

            page.locator('#bundleFile').set_input_files({'name': 'narrated.fmtl', 'mimeType': 'application/octet-stream', 'buffer': fixture_recording(2)})
            page.wait_for_function("document.getElementById('status').textContent.includes('2-channel sound')")
            page.locator('#scrub').evaluate("s=>{s.value='3';s.dispatchEvent(new Event('input'));}")
            page.wait_for_function("document.getElementById('frameLabel').textContent==='3 / 31'&&!document.getElementById('play').disabled")
            page.evaluate('workerPageEvidence.audioBefore=workerPageEvidence.owner.current.audio')
            phase = page.evaluate('workerPageEvidence.audioBefore.positionSeconds')
            page.locator('#resolution').select_option('1080x1080')
            verify(1080, 1080, 3)
            assert page.evaluate('workerPageEvidence.owner.current.audio===workerPageEvidence.audioBefore')
            assert page.evaluate('workerPageEvidence.audioBefore.positionSeconds') == phase
            assert page.evaluate('workerPageEvidence.starts.length') == 0
            assert page.evaluate('workerPageEvidence.contexts.length') == 1
            checks.append('paused resize preserves exact soundtrack phase and does not start audio')

            page.locator('#play').click()
            page.wait_for_function('workerPageEvidence.owner.current.audio.playing')
            phase = page.evaluate('workerPageEvidence.audioBefore.positionSeconds')
            page.locator('#resolution').select_option('640x360')
            verify(640, 360)
            assert page.evaluate('workerPageEvidence.owner.current.audio===workerPageEvidence.audioBefore')
            assert page.evaluate('workerPageEvidence.audioBefore.positionSeconds') >= phase
            assert page.evaluate('workerPageEvidence.audioBefore.playing')
            assert page.evaluate('workerPageEvidence.contexts.length') == 1
            page.locator('#play').click()
            checks.append('playing resize retains the running audio device and forward clock')

            page.locator('#resolution').evaluate("s=>{for(const value of ['1080x1920','1920x1080','1280x720']){s.value=value;s.dispatchEvent(new Event('change'));}}")
            verify(1280, 720)
            phase = page.evaluate('workerPageEvidence.audioBefore.positionSeconds')
            page.locator('#restartWorker').click()
            page.wait_for_function("document.getElementById('status').textContent.startsWith('Picture worker restarted')")
            verify(1280, 720)
            assert page.evaluate('workerPageEvidence.owner.current.audio===workerPageEvidence.audioBefore')
            assert page.evaluate('workerPageEvidence.audioBefore.positionSeconds') == phase
            assert page.evaluate('workerPageEvidence.contexts.length') == 1
            checks.append('resize storm and worker restart retain the selected resolution and audio owner')

            if args.screenshot:
                page.screenshot(path=args.screenshot)
            count = page.evaluate('workerPageEvidence.workers.length')
            page.evaluate('workerPageEvidence.owner.dispose()')
            page.set_viewport_size({'width': 1400, 'height': 900})
            page.wait_for_timeout(150)
            assert page.evaluate('workerPageEvidence.workers.length') == count
            assert page.evaluate('workerPageEvidence.workers.every(w=>w.terminated===1)')
            assert page.locator('#resolution').is_disabled()
            assert not errors, errors
            checks.append('disposed page cannot recreate workers through resize observers')
            print(json.dumps({'schema': 'fmn.player-viewport-browser', 'version': 1,
                'passed': True, 'browser': browser.version, 'nativeDecoder': 'explicit fixture',
                'checks': checks, 'source_sha256': {name: hashlib.sha256(text.encode()).hexdigest()
                    for name, text in {**sources, 'player.html': html}.items()}}, sort_keys=True))
        finally:
            browser.close()


if __name__ == '__main__':
    main()
