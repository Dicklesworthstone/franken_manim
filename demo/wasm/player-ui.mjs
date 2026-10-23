import {FramePlaybackClock} from './playback.mjs';
import {WorkerTimelineSession} from './worker-session.mjs';
import {prepareSoundtrack} from './audio-playback.mjs';
import {readBundleFile, readBundleResponse} from './bundle-input.mjs';

// Automatic preview stays on the 16:9 pixel grid and caps at 1080p. This is
// presentation quality, not a change to recorded camera or animation data.
export function choosePlayerViewport(mode, cssWidth, pixelRatio = 1) {
  const presets = {'640x360': [640, 360], '1280x720': [1280, 720],
    '1920x1080': [1920, 1080], '1080x1920': [1080, 1920], '1080x1080': [1080, 1080]};
  if (Object.hasOwn(presets, mode)) return presets[mode];
  if (mode !== 'auto') throw new RangeError('Unknown preview resolution.');
  if (!Number.isFinite(cssWidth) || cssWidth < 0 || !Number.isFinite(pixelRatio) || pixelRatio <= 0) {
    throw new RangeError('Invalid display dimensions or pixel density.');
  }
  if (cssWidth === 0) return null; // Hidden containers do not allocate 0-sized frames.
  const width = Math.max(16, Math.min(1920, Math.round(Math.min(cssWidth, 1920) * Math.min(pixelRatio, 4) / 16) * 16));
  return [width, width * 9 / 16];
}

export function mountTimelinePlayer({document = globalThis.document, window = globalThis.window,
  createSession = options => new WorkerTimelineSession(options),
  createContext = () => new AudioContext(),
  readDemo = async signal => readBundleResponse(await fetch('./bundle.fmtl', {signal}), signal)} = {}) {
  const $ = id => document.getElementById(id);
  const canvas = $('canvas'), ctx = canvas.getContext('2d');
  const status = $('status'), scrub = $('scrub'), play = $('play'), labels = $('labels'), restartButton = $('restartWorker');
  const resolution = $('resolution'), viewportLabel = $('viewportLabel');
  const lifetime = new AbortController();
  let resizeObserver = null, resizeRaf = 0, densityQuery = null;
  let session = null, clock = null, raf = 0, loading = false, disposed = false;
  let loadEpoch = 0, playEpoch = 0, restartEpoch = 0, recovering = false, seeking = null, lastRequested = -1;
  const on = (element, type, fn) => element.addEventListener(type, fn, {signal: lifetime.signal});
  function message(text, error = false) {
    status.classList.toggle('err', error); status.textContent = text;
  }
  function controls() {
    const available = Boolean(!disposed && !loading && !recovering && session?.current && !session.current.renderer.error);
    play.disabled = !available || seeking !== null;
    if (resolution) resolution.disabled = disposed;
    scrub.disabled = !available;
    restartButton.disabled = Boolean(disposed || loading || !session?.current);
    restartButton.textContent = recovering ? 'Cancel restart' : 'Restart picture worker';
    for (const button of labels.children) button.disabled = !available || button.dataset.unresolved === 'true';
  }
  function stop() {
    ++playEpoch;
    if (recovering) {
      ++restartEpoch; recovering = false; session?.cancelRestart();
      message('Picture worker restart cancelled; playback remains paused.');
    }
    try { clock?.pause(); } catch (error) { message(`Playback stopped: ${error}`, true); }
    session?.invalidate(); seeking = null; lastRequested = -1;
    window.cancelAnimationFrame(raf); raf = 0; play.textContent = 'Play'; controls();
    queueViewport();
  }
  function draw({index, width, height, rgba}) {
    if (disposed || loading || !session.current) return;
    const image = new ImageData(new Uint8ClampedArray(rgba.buffer, rgba.byteOffset, rgba.byteLength), width, height);
    // Keep the previous picture until a complete validated replacement arrives.
    if (canvas.width !== width) canvas.width = width;
    if (canvas.height !== height) canvas.height = height;
    if (canvas.style) canvas.style.aspectRatio = `${width} / ${height}`;
    ctx.putImageData(image, 0, 0);
    if (viewportLabel) viewportLabel.textContent = `${width} × ${height}`;
    $('frameLabel').textContent = `${index} / ${session.current.metadata.frameCount - 1}`;
    scrub.value = String(index);
    if (seeking === index) seeking = null;
    controls();
  }
  function seek(index) {
    if (loading || recovering || !session?.current || session.current.renderer.error) return;
    stop();
    try {
      clock.seek(index); seeking = index; lastRequested = index; controls();
      session.render(index);
    } catch (error) { seeking = null; controls(); message(`Frame refused: ${error}`, true); }
  }
  function tick(now) {
    raf = 0;
    if (disposed || loading || recovering || !clock?.playing || !session?.current) return;
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
  async function restart() {
    if (disposed || loading || !session?.current) return false;
    if (recovering) { stop(); return false; }
    stop();
    const epoch = ++restartEpoch;
    recovering = true; controls(); message('Restarting picture worker; narration remains paused…');
    try {
      // Audio keeps its fractional position; a silent clock keeps the selected
      // grid index, including a picture that was still rendering at failure.
      const frame = session.current.audio ? null : clock?.frame ?? session.current.renderer.frame;
      if (!await session.restart(frame) || epoch !== restartEpoch || disposed) return false;
      recovering = false; lastRequested = session.current.renderer.frame;
      session.present();
      message(`Picture worker restarted at frame ${lastRequested}; playback is paused.`);
      return true;
    } catch (error) {
      if (epoch !== restartEpoch || disposed) return false;
      message(`Picture worker restart failed: ${error}. The recording and paused narration were retained.`, true);
      return false;
    } finally { if (epoch === restartEpoch) { recovering = false; controls(); } }
  }
  function updateViewport() {
    if (disposed || !resolution || !session) return;
    try {
      const size = choosePlayerViewport(resolution.value, canvas.clientWidth, window.devicePixelRatio ?? 1);
      if (!size) return;
      const [width, height] = size;
      session.resize(width, height);
      if (canvas.width !== width || canvas.height !== height) {
        if (viewportLabel) viewportLabel.textContent = `${width} × ${height} pending`;
        // Pause invalidates old display work. Reissue the selected picture,
        // without seeking the audio clock or creating a new device.
        if (!loading && !recovering && session.current && !session.current.renderer.error) {
          const frame = seeking ?? clock?.frame ?? session.current.renderer.frame;
          lastRequested = frame; session.render(frame);
        }
      }
    } catch (error) { message(`Preview resolution refused: ${error}`, true); }
  }
  function queueViewport() {
    if (disposed || !resolution || resizeRaf) return;
    resizeRaf = window.requestAnimationFrame(() => { resizeRaf = 0; updateViewport(); });
  }
  function watchDensity() {
    densityQuery?.removeEventListener('change', densityChanged);
    densityQuery = typeof window.matchMedia === 'function' ?
      window.matchMedia(`(resolution: ${window.devicePixelRatio ?? 1}dppx)`) : null;
    densityQuery?.addEventListener('change', densityChanged);
  }
  function densityChanged() { watchDensity(); queueViewport(); }
  function dispose() {
    if (disposed) return;
    stop(); disposed = true; ++loadEpoch; lifetime.abort();
    window.cancelAnimationFrame(resizeRaf); resizeRaf = 0; resizeObserver?.disconnect();
    densityQuery?.removeEventListener('change', densityChanged); densityQuery = null;
    session?.dispose(); controls();
  }
  try {
    if (!ctx) throw new Error('A Canvas 2D context is required.');
    session = createSession({streamAudio: true, width: canvas.width, height: canvas.height,
      prepareAudio: player => prepareSoundtrack(player, createContext), onFrame: draw,
      onError: error => { stop(); message(`Playback stopped: ${error}. Restart the picture worker to retry.`, true); }});
    if (resolution) {
      on(resolution, 'change', updateViewport);
      on(window, 'resize', queueViewport);
      on(window, 'pageshow', queueViewport);
      if (typeof window.ResizeObserver === 'function') {
        resizeObserver = new window.ResizeObserver(queueViewport); resizeObserver.observe(canvas);
      }
      watchDensity(); updateViewport();
    }
    on(scrub, 'input', () => seek(Number(scrub.value)));
    on(restartButton, 'click', () => { void restart(); });
    on(play, 'click', async () => {
      if (loading || recovering || seeking !== null || !clock || !session.current || session.current.renderer.error) return;
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
    on(document, 'visibilitychange', () => { if (document.hidden) stop(); else queueViewport(); });
    on(window, 'pagehide', event => { if (event.persisted) stop(); else dispose(); });
    $('bundleFile').disabled = false; $('loadDemo').disabled = false;
    void load(readDemo, 'bundle.fmtl');
  } catch (error) { message(`Player initialization failed: ${error}`, true); controls(); }
  return Object.freeze({load, seek, stop, restart, dispose, get restarting() { return recovering; }, get current() { return session?.current; }});
}
