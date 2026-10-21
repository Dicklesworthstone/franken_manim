import {FramePlaybackClock} from './playback.mjs';
import {WorkerTimelineSession} from './worker-session.mjs';
import {prepareSoundtrack} from './audio-playback.mjs';
import {readBundleFile, readBundleResponse} from './bundle-input.mjs';

export function mountTimelinePlayer({document = globalThis.document, window = globalThis.window,
  createSession = options => new WorkerTimelineSession(options),
  createContext = () => new AudioContext(),
  readDemo = async signal => readBundleResponse(await fetch('./bundle.fmtl', {signal}), signal)} = {}) {
  const $ = id => document.getElementById(id);
  const canvas = $('canvas'), ctx = canvas.getContext('2d');
  const status = $('status'), scrub = $('scrub'), play = $('play'), labels = $('labels');
  const lifetime = new AbortController();
  let session = null, clock = null, raf = 0, loading = false, disposed = false;
  let loadEpoch = 0, playEpoch = 0, seeking = null, lastRequested = -1;
  const on = (element, type, fn) => element.addEventListener(type, fn, {signal: lifetime.signal});
  function message(text, error = false) {
    status.classList.toggle('err', error); status.textContent = text;
  }
  function controls() {
    const available = Boolean(!disposed && !loading && session?.current && !session.current.renderer.error);
    play.disabled = !available || seeking !== null;
    scrub.disabled = !available;
    for (const button of labels.children) button.disabled = !available || button.dataset.unresolved === 'true';
  }
  function stop() {
    ++playEpoch;
    try { clock?.pause(); } catch (error) { message(`Playback stopped: ${error}`, true); }
    session?.invalidate(); seeking = null; lastRequested = -1;
    window.cancelAnimationFrame(raf); raf = 0; play.textContent = 'Play'; controls();
  }
  function draw({index, width, height, rgba}) {
    if (disposed || loading || !session.current) return;
    ctx.putImageData(new ImageData(new Uint8ClampedArray(rgba.buffer, rgba.byteOffset, rgba.byteLength), width, height), 0, 0);
    $('frameLabel').textContent = `${index} / ${session.current.metadata.frameCount - 1}`;
    scrub.value = String(index);
    if (seeking === index) seeking = null;
    controls();
  }
  function seek(index) {
    if (loading || !session?.current || session.current.renderer.error) return;
    stop();
    try {
      clock.seek(index); seeking = index; lastRequested = index; controls();
      session.render(index);
    } catch (error) { seeking = null; controls(); message(`Frame refused: ${error}`, true); }
  }
  function tick(now) {
    raf = 0;
    if (disposed || loading || !clock?.playing || !session?.current) return;
    try {
      const index = clock.frameAt(now);
      if (index !== lastRequested) { lastRequested = index; session.render(index); }
      if (clock.playing) raf = window.requestAnimationFrame(tick);
      else play.textContent = 'Play'; // Let the final requested picture arrive.
    } catch (error) { stop(); message(`Playback stopped: ${error}`, true); }
  }
  async function load(readBytes, name) {
    if (disposed) return false;
    const epoch = ++loadEpoch;
    stop(); loading = true; controls(); message(`Loading ${name}…`);
    try {
      if (!await session.load(readBytes) || epoch !== loadEpoch || disposed) return false;
      const {metadata, audio} = session.current;
      clock = audio ?? new FramePlaybackClock(metadata.fps, metadata.frameCount);
      scrub.max = String(metadata.frameCount - 1); labels.replaceChildren();
      for (const {name: label, index} of metadata.labels) {
        const button = document.createElement('button');
        button.textContent = label; button.dataset.unresolved = String(index === null);
        // These listeners leave with their button; no permanent signal retains
        // every label of every recording loaded in this page.
        button.addEventListener('click', () => seek(index)); labels.appendChild(button);
      }
      loading = false; session.present();
      message(`${name}: ${metadata.frameCount} frames @ ${metadata.fps} fps ` +
        `(${metadata.durationSeconds.toFixed(2)}s), ${metadata.hasCameraTrack ? 'recorded camera' : 'planar viewport'}; ` +
        (metadata.audio ? `${metadata.audio.channels}-channel sound @ ${metadata.audio.sampleRate} Hz; ` : '') +
        `engine ${metadata.engineVersion} · worker rendering`);
      return true;
    } catch (error) {
      if (epoch !== loadEpoch || disposed) return false;
      message(`${name}: ${error}. ` + (session.current ? 'The previous recording is still loaded.' :
        'Open a matching .fmtl recording, or build demo/wasm/pkg; see README.md.'), true);
      return false;
    } finally { if (epoch === loadEpoch) { loading = false; controls(); } }
  }
  function dispose() {
    if (disposed) return;
    stop(); disposed = true; ++loadEpoch; lifetime.abort(); session?.dispose(); controls();
  }
  try {
    if (!ctx) throw new Error('A Canvas 2D context is required.');
    session = createSession({width: canvas.width, height: canvas.height,
      prepareAudio: player => prepareSoundtrack(player, createContext), onFrame: draw,
      onError: error => { stop(); message(`Playback stopped: ${error}. Reload the recording to retry.`, true); }});
    on(scrub, 'input', () => seek(Number(scrub.value)));
    on(play, 'click', async () => {
      if (loading || seeking !== null || !clock || !session.current || session.current.renderer.error) return;
      if (clock.playing || clock.starting) { stop(); return; }
      const active = clock, current = session.current, epoch = ++playEpoch;
      try {
        if (!current.audio) active.seek(current.renderer.frame);
        play.textContent = 'Starting…';
        const started = await active.play(window.performance.now());
        if (disposed || clock !== active || current !== session.current || epoch !== playEpoch || started === false || !active.playing) return;
        play.textContent = 'Pause'; raf = window.requestAnimationFrame(tick);
      } catch (error) {
        if (disposed || clock !== active || epoch !== playEpoch) return;
        stop(); message(`Playback refused: ${error}`, true);
      }
    });
    on($('bundleFile'), 'change', () => {
      const file = $('bundleFile').files?.[0]; $('bundleFile').value = '';
      if (file) void load(signal => readBundleFile(file, signal), file.name);
    });
    on($('loadDemo'), 'click', () => { void load(readDemo, 'bundle.fmtl'); });
    on(document, 'visibilitychange', () => { if (document.hidden) stop(); });
    on(window, 'pagehide', event => { if (event.persisted) stop(); else dispose(); });
    $('bundleFile').disabled = false; $('loadDemo').disabled = false;
    void load(readDemo, 'bundle.fmtl');
  } catch (error) { message(`Player initialization failed: ${error}`, true); controls(); }
  return Object.freeze({load, seek, stop, dispose, get current() { return session?.current; }});
}
