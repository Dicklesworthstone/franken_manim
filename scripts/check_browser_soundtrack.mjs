#!/usr/bin/env node
// Run against real web bindings and the native export_audio_bundle example.
// Usage: node scripts/check_browser_soundtrack.mjs /path/to/audible.fmtl
// Optional FMN_PLAYWRIGHT_MODULE points to an installed playwright module file.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import http from 'node:http';
import { fileURLToPath, pathToFileURL } from 'node:url';
import path from 'node:path';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const audioPath = process.argv[2];
if (!audioPath || process.argv.length !== 3) throw new Error('usage: check_browser_soundtrack.mjs AUDIO.fmtl');
const audioBytes = await readFile(audioPath);
const { chromium } = await import(process.env.FMN_PLAYWRIGHT_MODULE
  ? pathToFileURL(path.resolve(process.env.FMN_PLAYWRIGHT_MODULE)).href : 'playwright');
const allowed = new Set([
  '/demo/wasm/player.html', '/demo/wasm/playback.mjs', '/demo/wasm/player-session.mjs',
  '/demo/wasm/audio-playback.mjs', '/demo/wasm/bundle.fmtl',
  '/demo/wasm/player-ui.mjs', '/demo/wasm/bundle-input.mjs',
  '/demo/wasm/worker-session.mjs', '/demo/wasm/worker-protocol.mjs',
  '/demo/wasm/picture-worker.mjs', '/demo/wasm/picture-worker-core.mjs',
  '/demo/wasm/pkg/fmn_wasm.js', '/demo/wasm/pkg/fmn_wasm_bg.wasm',
]);
const server = http.createServer(async (request, response) => {
  const pathname = new URL(request.url, 'http://127.0.0.1').pathname;
  try {
    if (pathname !== '/audible.fmtl' && !allowed.has(pathname)) {
      response.writeHead(404); response.end(); return;
    }
    const bytes = pathname === '/audible.fmtl' ? audioBytes : await readFile(path.join(root, pathname));
    const mime = pathname.endsWith('.html') ? 'text/html' : pathname.endsWith('.wasm')
      ? 'application/wasm' : /\.(mjs|js)$/.test(pathname) ? 'text/javascript' : 'application/octet-stream';
    response.writeHead(200, { 'Content-Type': mime, 'Cache-Control': 'no-store' }); response.end(bytes);
  } catch (error) { response.writeHead(500); response.end(String(error)); }
});
await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
let browser;
try {
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  const failures = [];
  page.on('pageerror', error => failures.push(String(error)));
  // Observe real nodes without replacing their timing, PCM or device behavior.
  await page.addInitScript(() => {
    const NativeContext = window.AudioContext;
    window.audioEvidence = { contexts: [], starts: [], stops: [] };
    window.AudioContext = class extends NativeContext {
      constructor(...args) { super(...args); window.audioEvidence.contexts.push(this); }
      createBufferSource() {
        const node = super.createBufferSource();
        const start = node.start.bind(node), stop = node.stop.bind(node);
        node.start = (when, offset, ...rest) => {
          window.audioEvidence.starts.push({ when, offset, rate: node.buffer.sampleRate,
            frames: node.buffer.length, channels: node.buffer.numberOfChannels });
          return start(when, offset, ...rest);
        };
        node.stop = (...args) => { window.audioEvidence.stops.push(this.currentTime); return stop(...args); };
        return node;
      }
    };
  });
  const origin = `http://127.0.0.1:${server.address().port}`;
  await page.goto(`${origin}/demo/wasm/player.html`);
  await page.waitForFunction(() => !document.getElementById('scrub').disabled, null, { timeout: 120000 });
  assert.equal(await page.evaluate(() => window.audioEvidence.contexts.length), 0);

  const waveform = await page.evaluate(async () => {
    const { default: init, FmnPlayer } = await import('./pkg/fmn_wasm.js');
    await init(); // The page now initializes its own Wasm instance in the worker.
    const bytes = new Uint8Array(await (await fetch('/audible.fmtl')).arrayBuffer());
    const visualBytes = new Uint8Array(await (await fetch('./bundle.fmtl')).arrayBuffer());
    function check(value, message) { if (!value) throw new Error(message); }
    let refused = false;
    try { const bad = FmnPlayer.from_bundle(bytes); bad.free(); } catch { refused = true; }
    check(refused, 'picture-only constructor accepted sound');
    const player = FmnPlayer.from_bundle_with_audio(bytes);
    const plain = FmnPlayer.from_bundle(visualBytes);
    let sound;
    try {
      player.set_viewport(64, 36); plain.set_viewport(64, 36);
      const pixels = new Uint8Array(64 * 36 * 4).fill(91);
      refused = false; try { player.render_into(0, pixels); } catch { refused = true; }
      check(refused && pixels.every(v => v === 91), 'attached audio was silently dropped');
      sound = player.take_soundtrack();
      check(sound.sample_rate === 48000 && sound.channels === 2 && sound.sample_frames === 84000, 'fixture identity');
      check(!player.has_soundtrack && !player.take_soundtrack(), 'PCM transferred more than once');
      const left = new Float32Array(84000), right = new Float32Array(84000);
      sound.copy_channel_into(0, 0, left); sound.copy_channel_into(1, 0, right);
      // Inspect the example's known-size PCM tail, not a replacement FMTL parser.
      const tail = new DataView(bytes.buffer, bytes.byteOffset + bytes.length - 32 - 84000 * 8, 84000 * 8);
      const lbits = new Uint32Array(left.buffer), rbits = new Uint32Array(right.buffer);
      for (let i = 0; i < 84000; i++) {
        check(lbits[i] === tail.getUint32(i * 8, true), `left PCM bit drift at ${i}`);
        check(rbits[i] === tail.getUint32(i * 8 + 4, true), `right PCM bit drift at ${i}`);
      }
      const sentinel = new Float32Array([42, 42]); refused = false;
      try { sound.copy_channel_into(2, 0, sentinel); } catch { refused = true; }
      check(refused && sentinel.every(v => v === 42), 'invalid channel modified JS destination');
      refused = false; try { sound.copy_channel_into(0, 83999, sentinel); } catch { refused = true; }
      check(refused && sentinel.every(v => v === 42), 'invalid window modified JS destination');
      for (const index of [44, 0, 20, 1]) {
        player.render_into(index, pixels); const expected = plain.render_frame(index);
        check(pixels.every((v, i) => v === expected[i]), `picture drift at ${index}`);
      }
      // The real browser audio renderer must preserve channels and seek offset.
      const offline = new OfflineAudioContext(2, 72000, 48000);
      const buffer = offline.createBuffer(2, 84000, 48000);
      buffer.copyToChannel(left, 0); buffer.copyToChannel(right, 1);
      const source = offline.createBufferSource(); source.buffer = buffer;
      source.connect(offline.destination); source.start(0, 0.25);
      const rendered = await offline.startRendering();
      for (const [channel, expected] of [[0, left], [1, right]]) {
        const actual = rendered.getChannelData(channel);
        for (let i = 0; i < actual.length; ++i) check(actual[i] === expected[i + 12000], `offline seek/channel drift ${channel}:${i}`);
      }
      return { copiedSamples: 168000, offlineSamples: 144000, pictureComparisons: 4 };
    } finally { sound?.free(); player.free(); plain.free(); }
  });

  await page.locator('#bundleFile').setInputFiles(path.resolve(audioPath));
  await page.waitForFunction(() => document.getElementById('status').textContent.includes('2-channel sound @ 48000'));
  assert.equal(await page.evaluate(() => window.audioEvidence.starts.length), 0);
  await page.locator('#play').click();
  await page.waitForFunction(() => Number(document.getElementById('scrub').value) >= 3);
  await page.locator('#play').click();
  const pausedFrame = await page.locator('#scrub').inputValue();
  await page.waitForTimeout(120);
  assert.equal(await page.locator('#scrub').inputValue(), pausedFrame);
  const first = await page.evaluate(() => ({ start: window.audioEvidence.starts[0], stop: window.audioEvidence.stops[0] }));
  assert.equal(first.start.offset, 0); assert.equal(first.start.frames, 84000);
  await page.locator('#play').click();
  await page.waitForFunction(() => window.audioEvidence.starts.length === 2);
  const resumed = await page.evaluate(() => window.audioEvidence.starts[1]);
  assert.ok(Math.abs(resumed.offset - (first.stop - first.start.when)) <= 0.03, 'resume lost audio phase');

  await page.locator('#scrub').evaluate(input => { input.value = '15'; input.dispatchEvent(new Event('input')); });
  assert.equal(await page.locator('#play').textContent(), 'Play');
  await page.locator('#play').click();
  await page.waitForFunction(() => window.audioEvidence.starts.length === 3);
  assert.equal(await page.evaluate(() => window.audioEvidence.starts[2].offset), 0.5);
  await page.waitForFunction(() => document.getElementById('play').textContent === 'Play'
    && document.getElementById('scrub').value === '44', null, { timeout: 30000 });
  await page.locator('#play').click();
  await page.waitForFunction(() => window.audioEvidence.starts.length === 4);
  assert.equal(await page.evaluate(() => window.audioEvidence.starts[3].offset), 0);

  await page.locator('#bundleFile').setInputFiles({ name: 'broken.fmtl', mimeType: 'application/octet-stream', buffer: Buffer.from([1,2,3]) });
  await page.waitForFunction(() => document.getElementById('status').textContent.includes('previous recording is still loaded'));
  assert.equal(await page.locator('#play').isDisabled(), false);
  assert.equal(await page.evaluate(() => window.audioEvidence.contexts.length), 1);
  assert.notEqual(await page.evaluate(() => window.audioEvidence.contexts[0].state), 'closed');
  await page.locator('#loadDemo').click();
  await page.waitForFunction(() => document.getElementById('status').textContent.startsWith('bundle.fmtl: 45'));
  await page.waitForFunction(() => window.audioEvidence.contexts[0].state === 'closed');
  assert.deepEqual(failures, []);
  console.log(JSON.stringify({ result: 'PASS', browser: browser.version(), ...waveform,
    checks: ['audio loader admission', 'exact PCM', 'invalid-copy preservation', 'unchanged pixels',
      'real offline seek waveform', 'gesture playback', 'fractional pause/resume', 'seek offset',
      'tail completion/restart', 'failed replacement preservation', 'device cleanup'] }, null, 2));
} finally {
  await browser?.close();
  await new Promise(resolve => server.close(resolve));
}
