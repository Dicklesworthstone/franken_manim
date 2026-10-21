import {WORKER_PROTOCOL, MAX_BUNDLE_BYTES, MAX_PCM_BYTES, integer, viewport,
  metadata, pixels, channelBuffers, audioInfo} from './worker-protocol.mjs';

export {MAX_BUNDLE_BYTES};
const cancelled = () => new DOMException('Recording load cancelled.', 'AbortError');
const defaultWorker = () => new Worker(new URL('./picture-worker.mjs', import.meta.url), {type: 'module'});

// An ownership adapter for the existing eager/windowed audio scheduler. The
// worker has already validated PCM through the native decoder and relinquished
// these channel buffers. free() releases them once, not once per picture.
export function transferredSoundtrack(info, buffers) {
  channelBuffers(buffers, info);
  let channels = buffers.map(buffer => new Float32Array(buffer));
  return {
    sample_rate: info.sampleRate, channels: info.channels, sample_frames: info.sampleFrames,
    copy_channel_into(channel, start, destination) {
      if (!channels) throw new Error('Transferred soundtrack is freed.');
      integer(channel, 'audio channel', 0, info.channels - 1);
      integer(start, 'audio offset', 0, info.sampleFrames);
      if (!(destination instanceof Float32Array) || destination.length > info.sampleFrames - start) {
        throw new RangeError('Invalid audio destination.');
      }
      destination.set(channels[channel].subarray(start, start + destination.length));
    },
    free() { channels = null; },
  };
}

// Exactly one in-flight raster and one latest pending frame number. Pixel
// ownership moves back and forth; observers borrow a view only during onFrame.
class PictureConnection {
  #worker;
  #timeout;
  #timer = null;
  #active = null;
  #pending = null;
  #pixels = null;
  #bufferFrame = 0;
  #presenting = false;
  #id = 0;
  #epoch = 0;
  #closed = false;
  #resolve = null;
  #reject = null;
  #onFrame;
  #onError;
  #width;
  #height;
  metadata = null;
  frame = 0;
  error = null;

  constructor(worker, width, height, timeout, onFrame, onError) {
    this.#worker = worker; this.#width = width; this.#height = height;
    this.#timeout = timeout; this.#onFrame = onFrame; this.#onError = onError;
    worker.onmessage = event => this.#receive(event.data);
    worker.onerror = event => { event.preventDefault?.(); this.#fail(new Error(`Picture worker failed: ${String(event.message || 'execution error').slice(0, 600)}`)); };
    worker.onmessageerror = () => this.#fail(new Error('Picture worker response could not be decoded.'));
  }
  load(bytes, maxBundleBytes, maxAudioBytes) {
    return new Promise((resolve, reject) => {
      this.#resolve = resolve; this.#reject = reject;
      // Never transfer unrelated bytes outside a caller's subarray.
      const owned = bytes.byteOffset === 0 && bytes.byteLength === bytes.buffer.byteLength ? bytes : bytes.slice();
      this.#send({kind: 'load', bytes: owned.buffer, width: this.#width, height: this.#height,
        maxBundleBytes, maxAudioBytes}, [owned.buffer]);
    });
  }
  #send(fields, transfer) {
    const id = ++this.#id;
    try {
      integer(id, 'request identity', 1, Number.MAX_SAFE_INTEGER);
      this.#active = {id, kind: fields.kind, frame: fields.frame, epoch: this.#epoch};
      this.#timer = setTimeout(() => this.#fail(new Error('Picture worker request timed out; reload the recording.')), this.#timeout);
      this.#worker.postMessage({protocol: WORKER_PROTOCOL, id, ...fields}, transfer);
    } catch (error) { this.#fail(error); }
  }
  #receive(value) {
    if (this.#closed) return;
    const active = this.#active;
    try {
      if (!active || value?.protocol !== WORKER_PROTOCOL || value.id !== active.id) throw new Error('Uncorrelated picture worker response.');
      if (value.kind === 'error') throw new Error(String(value.message || 'Native picture rendering failed.').slice(0, 600));
      if (value.width !== this.#width || value.height !== this.#height) throw new Error('Picture worker changed the viewport.');
      pixels(value.pixels, this.#width, this.#height);
      if (active.kind === 'load') {
        if (value.kind !== 'ready' || value.frame !== 0) throw new Error('Invalid first picture response.');
        this.metadata = metadata(value.metadata);
        if (this.metadata.audio) channelBuffers(value.channels, this.metadata.audio);
        else if (!Array.isArray(value.channels) || value.channels.length) throw new Error('Unexpected transferred audio.');
      } else if (value.kind !== 'frame' || value.frame !== active.frame) {
        throw new Error('Picture worker returned a different frame.');
      }
      clearTimeout(this.#timer); this.#timer = null;
      this.#active = null; this.#pixels = value.pixels; this.#bufferFrame = value.frame;
      if (active.kind === 'load') {
        const resolve = this.#resolve; this.#resolve = null; this.#reject = null;
        resolve(value.channels);
      } else if (active.epoch === this.#epoch) {
        this.frame = value.frame;
        this.present();
      }
      this.#drain();
    } catch (error) { this.#fail(error); }
  }
  present() {
    if (this.#closed) throw this.error || new Error('Picture worker is closed.');
    if (!this.#pixels || this.#presenting || this.#bufferFrame !== this.frame) return;
    this.#presenting = true;
    try {
      this.#onFrame({index: this.frame, width: this.#width,
        height: this.#height, rgba: new Uint8Array(this.#pixels)});
    } finally { this.#presenting = false; }
    this.#drain();
  }
  request(frame) {
    if (this.#closed) throw this.error || new Error('Picture worker is closed.');
    integer(frame, 'frame index', 0, this.metadata.frameCount - 1);
    if (this.#active?.frame === frame && this.#active.epoch === this.#epoch) {
      this.#pending = null; return;
    }
    if (this.#presenting && frame === this.frame) { this.#pending = null; return; }
    this.#pending = frame;
    this.#drain();
  }
  #drain() {
    if (this.#closed || this.#active || this.#presenting || this.#pending === null) return;
    const frame = this.#pending; this.#pending = null;
    if (frame === this.#bufferFrame && this.#pixels) {
      this.frame = frame; this.present();
      return;
    }
    const buffer = this.#pixels; this.#pixels = null;
    this.#send({kind: 'render', frame, pixels: buffer}, [buffer]);
  }
  invalidate() { ++this.#epoch; this.#pending = null; }
  #fail(error) {
    if (this.#closed) return;
    this.error = error;
    this.#close(error);
    this.#onError(error);
  }
  #close(error) {
    this.#closed = true;
    clearTimeout(this.#timer); this.#timer = null;
    this.#pending = null; this.#active = null; this.#pixels = null;
    this.#worker.onmessage = null; this.#worker.onerror = null; this.#worker.onmessageerror = null;
    this.#worker.terminate();
    const reject = this.#reject; this.#reject = null; this.#resolve = null;
    reject?.(error);
  }
  dispose() { if (!this.#closed) this.#close(cancelled()); }
}

function release(current) {
  if (!current) return;
  try { current.audio?.dispose(); } finally { current.renderer.dispose(); }
}

export class WorkerTimelineSession {
  #workerFactory;
  #prepareAudio;
  #onFrame;
  #onError;
  #width;
  #height;
  #maxBytes;
  #maxAudioBytes;
  #timeout;
  #generation = 0;
  #disposed = false;
  #current = null;
  #candidate = null;
  #controller = null;

  constructor({width, height, createWorker = defaultWorker, prepareAudio = null,
    onFrame = () => {}, onError = () => {}, maxBytes = MAX_BUNDLE_BYTES,
    maxAudioBytes = MAX_PCM_BYTES, timeoutMs = 30_000}) {
    viewport(width, height);
    integer(maxBytes, 'bundle byte limit', 1, MAX_BUNDLE_BYTES);
    integer(maxAudioBytes, 'PCM byte limit', 1, MAX_PCM_BYTES);
    integer(timeoutMs, 'worker timeout', 1, 2_147_483_647);
    if (typeof createWorker !== 'function' || (prepareAudio !== null && typeof prepareAudio !== 'function') ||
        typeof onFrame !== 'function' || typeof onError !== 'function') throw new TypeError('Invalid worker session callbacks.');
    this.#workerFactory = createWorker; this.#prepareAudio = prepareAudio;
    this.#onFrame = onFrame; this.#onError = onError;
    this.#width = width; this.#height = height; this.#maxBytes = maxBytes;
    this.#maxAudioBytes = maxAudioBytes; this.#timeout = timeoutMs;
  }
  get current() { return this.#current; }
  async load(readBytes) {
    if (this.#disposed) throw new Error('Worker timeline session is disposed.');
    const generation = ++this.#generation;
    this.#controller?.abort(); this.#candidate?.dispose();
    const controller = new AbortController(); this.#controller = controller;
    let renderer = null, audio = null, pcm = null;
    try {
      const bytes = await readBytes(controller.signal);
      if (this.#disposed || generation !== this.#generation) return false;
      if (!(bytes instanceof Uint8Array) || bytes.byteLength > this.#maxBytes) throw new RangeError('Invalid or oversized bundle bytes.');
      renderer = new PictureConnection(this.#workerFactory(), this.#width, this.#height, this.#timeout,
        frame => { if (this.#current?.renderer === renderer) this.#onFrame(frame); },
        error => {
          if (this.#current?.renderer === renderer) {
            try { this.#current.audio?.pause(); } finally { this.#onError(error); }
          }
        });
      this.#candidate = renderer;
      let channels = await renderer.load(bytes, this.#maxBytes, this.#maxAudioBytes);
      if (this.#disposed || generation !== this.#generation) return false;
      const info = renderer.metadata;
      if (info.audio) {
        audioInfo(info.audio, this.#maxAudioBytes);
        pcm = transferredSoundtrack(info.audio, channels);
        // Drop the transfer envelope: a small eager soundtrack may free its
        // backing buffers during preparation; the session must not retain them.
        channels = null;
        if (!this.#prepareAudio) throw new Error('Sound-bearing FMTL requires an audio adapter.');
        let taken = false;
        audio = this.#prepareAudio({fps: info.fps, frame_count: info.frameCount,
          take_soundtrack: () => { if (taken) return null; taken = true; return pcm; }});
        if (!taken || !audio || typeof audio.dispose !== 'function' || typeof audio.pause !== 'function' ||
            typeof audio.play !== 'function' || typeof audio.seek !== 'function' || typeof audio.frameAt !== 'function') {
          throw new Error('Audio adapter did not accept the transferred soundtrack.');
        }
        pcm = null;
      }
      if (this.#disposed || generation !== this.#generation) return false;
      const previous = this.#current;
      this.#current = Object.freeze({renderer, metadata: info, audio});
      this.#candidate = null;
      // Closures above keep the renderer identity. Ownership is marked by the
      // current object rather than nulling that captured variable.
      release(previous);
      return true;
    } catch (error) {
      if (this.#disposed || generation !== this.#generation) return false;
      throw error;
    } finally {
      pcm?.free();
      if (renderer && this.#current?.renderer !== renderer) {
        try { audio?.dispose?.(); } finally { renderer.dispose(); }
      }
      if (this.#candidate === renderer) this.#candidate = null;
    }
  }
  present() { this.#current?.renderer.present(); }
  render(frame) { this.#current?.renderer.request(frame); }
  invalidate() { this.#current?.renderer.invalidate(); }
  dispose() {
    this.#disposed = true; ++this.#generation;
    this.#controller?.abort(); this.#candidate?.dispose(); this.#candidate = null;
    const previous = this.#current; this.#current = null;
    release(previous);
  }
}
