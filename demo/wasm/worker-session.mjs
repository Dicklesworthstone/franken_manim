import {WORKER_PROTOCOL, MAX_BUNDLE_BYTES, MAX_PCM_BYTES, integer, viewport,
  metadata, pixels, channelBuffers, audioInfo, recordingSource, sourceIdentity, sameSource, audioWindow, windowChannels} from './worker-protocol.mjs';

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


// Stable PCM identity across a transactional picture-worker restart. The audio
// scheduler keeps its device and fractional phase; only this transport changes.
class WorkerSoundtrack {
  #connection;
  #pending = null;
  #freed = false;
  constructor(info, connection) {
    this.sample_rate = info.sampleRate; this.channels = info.channels;
    this.sample_frames = info.sampleFrames; this.#connection = connection;
  }
  async read_window(start, count, signal) {
    if (this.#freed) throw new Error('Worker soundtrack is freed.');
    if (this.#pending) throw new Error('A soundtrack window is already pending.');
    const controller = new AbortController();
    const abort = () => controller.abort();
    this.#pending = controller;
    signal?.addEventListener('abort', abort, {once: true});
    if (signal?.aborted) controller.abort();
    try { return await this.#connection.readAudio(start, count, controller.signal); }
    finally {
      signal?.removeEventListener('abort', abort);
      if (this.#pending === controller) this.#pending = null;
    }
  }
  rebind(connection) {
    if (this.#freed) throw new Error('Worker soundtrack is freed.');
    this.#pending?.abort(); this.#pending = null;
    this.#connection = connection;
  }
  free() {
    if (this.#freed) return;
    this.#freed = true; this.#pending?.abort(); this.#pending = null; this.#connection = null;
  }
}

// Exactly one in-flight raster and one latest pending frame number. Pixel
// ownership moves back and forth; observers borrow a view only during onFrame.
class PictureConnection {
  #worker;
  #timeout;
  #timer = null;
  #active = null;
  #pending = null;
  #pendingAudio = null;
  #audioMode = 'eager';
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
  #source = null;
  metadata = null;
  get source() { return this.#source; }
  frame = 0;
  error = null;

  constructor(worker, width, height, timeout, onFrame, onError) {
    this.#worker = worker; this.#width = width; this.#height = height;
    this.#timeout = timeout; this.#onFrame = onFrame; this.#onError = onError;
    worker.onmessage = event => this.#receive(event.data);
    worker.onerror = event => { event.preventDefault?.(); this.#fail(new Error(`Picture worker failed: ${String(event.message || 'execution error').slice(0, 600)}`)); };
    worker.onmessageerror = () => this.#fail(new Error('Picture worker response could not be decoded.'));
  }
  load(bytes, maxBundleBytes, maxAudioBytes, restore = null, streamAudio = false) {
    return new Promise((resolve, reject) => {
      this.#resolve = resolve; this.#reject = reject;
      this.#audioMode = streamAudio ? 'windowed' : 'eager';
      // Never transfer unrelated bytes outside a caller's subarray.
      const owned = bytes.byteOffset === 0 && bytes.byteLength === bytes.buffer.byteLength ? bytes : bytes.slice();
      this.#source = recordingSource(owned);
      if (restore && !sameSource(this.source, restore.source)) throw new Error('Recording changed; reopen it instead of restarting with old narration.');
      this.#send({kind: restore ? 'restore' : 'load', bytes: owned.buffer, width: this.#width, height: this.#height,
        maxBundleBytes, maxAudioBytes, audioMode: this.#audioMode, ...(restore ? {frame: restore.frame, source: restore.source} : {})}, [owned.buffer]);
    });
  }
  #send(fields, transfer, audio = null) {
    const id = ++this.#id;
    try {
      integer(id, 'request identity', 1, Number.MAX_SAFE_INTEGER);
      this.#active = {id, kind: fields.kind, frame: fields.frame, epoch: this.#epoch, audio};
      this.#timer = setTimeout(() => this.#fail(new Error('Picture worker request timed out; restart the picture worker to retry.')), this.#timeout);
      this.#worker.postMessage({protocol: WORKER_PROTOCOL, id, ...fields}, transfer);
    } catch (error) { this.#fail(error); }
  }
  #receive(value) {
    if (this.#closed) return;
    const active = this.#active;
    try {
      if (!active || value?.protocol !== WORKER_PROTOCOL || value.id !== active.id) throw new Error('Uncorrelated picture worker response.');
      if (value.kind === 'error') throw new Error(String(value.message || 'Native picture rendering failed.').slice(0, 600));
      if (active.kind === 'audio') {
        const request = active.audio;
        if (value.kind !== 'audio' || value.start !== request.start || value.count !== request.count) {
          throw new Error('Worker returned a different audio window.');
        }
        windowChannels(value.channels, request.start, request.count, this.metadata.audio);
        clearTimeout(this.#timer); this.#timer = null; this.#active = null;
        this.#settleAudio(request, null, value.channels.map(buffer => new Float32Array(buffer)));
        this.#drain();
        return;
      }
      if (value.width !== this.#width || value.height !== this.#height) throw new Error('Picture worker changed the viewport.');
      pixels(value.pixels, this.#width, this.#height);
      if (active.kind === 'load' || active.kind === 'restore') {
        const restoring = active.kind === 'restore';
        if (value.kind !== 'ready' || value.frame !== (restoring ? active.frame : 0) || value.audioReused !== restoring) {
          throw new Error('Invalid first picture response.');
        }
        if (!sameSource(sourceIdentity(value.source), this.source)) throw new Error('Picture worker returned a different recording.');
        this.metadata = metadata(value.metadata);
        if (this.#audioMode === 'windowed' && value.audioMode !== 'windowed') throw new Error('Worker does not support bounded audio windows.');
        if (this.metadata.audio && !restoring && this.#audioMode === 'eager') channelBuffers(value.channels, this.metadata.audio);
        else if (!Array.isArray(value.channels) || value.channels.length) throw new Error('Unexpected transferred audio.');
      } else if (value.kind !== 'frame' || value.frame !== active.frame) {
        throw new Error('Picture worker returned a different frame.');
      }
      clearTimeout(this.#timer); this.#timer = null;
      this.#active = null; this.#pixels = value.pixels; this.#bufferFrame = value.frame;
      if (active.kind === 'load' || active.kind === 'restore') {
        this.frame = value.frame;
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
  readAudio(start, count, signal) {
    return new Promise((resolve, reject) => {
      if (this.#closed) throw this.error || new Error('Picture worker is closed.');
      if (this.#audioMode !== 'windowed' || !this.metadata?.audio) throw new Error('Worker has no retained soundtrack.');
      audioWindow(start, count, this.metadata.audio);
      if (signal?.aborted) throw cancelled();
      if (this.#pendingAudio || this.#active?.audio?.resolve) throw new Error('An audio window is already pending.');
      const request = {start, count, resolve, reject, signal, abort: null};
      request.abort = () => {
        if (this.#pendingAudio === request) this.#pendingAudio = null;
        this.#settleAudio(request, cancelled());
      };
      signal?.addEventListener('abort', request.abort, {once: true});
      this.#pendingAudio = request;
      this.#drain();
    });
  }
  #settleAudio(request, error, channels) {
    if (!request) return;
    request.signal?.removeEventListener('abort', request.abort);
    const resolve = request.resolve, reject = request.reject;
    request.resolve = null; request.reject = null;
    if (error) reject?.(error); else resolve?.(channels);
  }
  #drain() {
    if (this.#closed || this.#active || this.#presenting) return;
    // Audio gets the next slot ahead of a pending preview. An executing raster
    // is never duplicated or preempted; its completion/timeout is still bounded.
    if (this.#pendingAudio) {
      const request = this.#pendingAudio; this.#pendingAudio = null;
      this.#send({kind: 'audio', start: request.start, count: request.count}, [], request);
      return;
    }
    if (this.#pending === null) return;
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
    this.#settleAudio(this.#pendingAudio, error);
    this.#settleAudio(this.#active?.audio, error);
    this.#pendingAudio = null;
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
  #streamAudio;
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
  #readBytes = null;
  #restarting = null;

  constructor({width, height, createWorker = defaultWorker, prepareAudio = null,
    onFrame = () => {}, onError = () => {}, maxBytes = MAX_BUNDLE_BYTES,
    maxAudioBytes = MAX_PCM_BYTES, timeoutMs = 30_000, streamAudio = false}) {
    if (typeof streamAudio !== 'boolean') throw new TypeError('streamAudio must be boolean.');
    this.#streamAudio = streamAudio;
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
  get restarting() { return this.#restarting !== null; }
  load(readBytes) { return this.#replace(readBytes, null); }
  async restart(frame = null) {
    if (this.#disposed) throw new Error('Worker timeline session is disposed.');
    if (!this.#current || !this.#readBytes) throw new Error('No recording is loaded.');
    if (this.#restarting) return false;
    const previous = this.#current;
    if (frame !== null) integer(frame, 'restart frame', 0, previous.metadata.frameCount - 1);
    previous.audio?.pause();
    const index = frame ?? previous.audio?.frame ?? previous.renderer.frame;
    integer(index, 'restart frame', 0, previous.metadata.frameCount - 1);
    previous.renderer.invalidate();
    return this.#replace(this.#readBytes, {previous, frame: index, source: previous.renderer.source});
  }
  cancelRestart() {
    if (!this.#restarting) return false;
    ++this.#generation; this.#restarting = null;
    this.#controller?.abort(); this.#candidate?.dispose(); this.#candidate = null;
    return true;
  }
  async #replace(readBytes, restore) {
    if (this.#disposed) throw new Error('Worker timeline session is disposed.');
    if (typeof readBytes !== 'function') throw new TypeError('A repeatable recording reader is required.');
    const generation = ++this.#generation;
    this.#restarting = restore ? generation : null;
    this.#controller?.abort(); this.#candidate?.dispose();
    const controller = new AbortController(); this.#controller = controller;
    let renderer = null, audio = null, pcm = null, soundtrack = null;
    try {
      const bytes = await readBytes(controller.signal);
      if (this.#disposed || generation !== this.#generation) return false;
      if (!(bytes instanceof Uint8Array) || bytes.byteLength > this.#maxBytes) throw new RangeError('Invalid or oversized bundle bytes.');
      renderer = new PictureConnection(this.#workerFactory(), this.#width, this.#height, this.#timeout,
        frame => { if (this.#current?.renderer === renderer) this.#onFrame(frame); },
        error => {
          if (this.#current?.renderer === renderer) {
            try { this.#current.audio?.pause(); } finally { if (!this.#restarting) this.#onError(error); }
          }
        });
      this.#candidate = renderer;
      let channels = await renderer.load(bytes, this.#maxBytes, this.#maxAudioBytes, restore, this.#streamAudio);
      if (this.#disposed || generation !== this.#generation) return false;
      const info = renderer.metadata;
      if (restore) {
        if (JSON.stringify(info) !== JSON.stringify(restore.previous.metadata)) throw new Error('Restarted recording metadata changed.');
        audio = restore.previous.audio;
        soundtrack = restore.previous.soundtrack;
      } else if (info.audio) {
        audioInfo(info.audio, this.#maxAudioBytes);
        pcm = this.#streamAudio ? new WorkerSoundtrack(info.audio, renderer) : transferredSoundtrack(info.audio, channels);
        if (this.#streamAudio && info.audio.sampleFrames) soundtrack = pcm;
        // Drop the transfer envelope: a small eager soundtrack may free its
        // backing buffers during preparation; the session must not retain them.
        channels = null;
        if (!this.#prepareAudio) throw new Error('Sound-bearing FMTL requires an audio adapter.');
        let taken = false;
        audio = await this.#prepareAudio({fps: info.fps, frame_count: info.frameCount,
          take_soundtrack: () => { if (taken) return null; taken = true; return pcm; }});
        if (!taken || !audio || typeof audio.dispose !== 'function' || typeof audio.pause !== 'function' ||
            typeof audio.play !== 'function' || typeof audio.seek !== 'function' || typeof audio.frameAt !== 'function') {
          throw new Error('Audio adapter did not accept the transferred soundtrack.');
        }
        pcm = null;
      }
      if (this.#disposed || generation !== this.#generation) return false;
      const previous = this.#current;
      if (restore) soundtrack?.rebind(renderer);
      this.#current = Object.freeze({renderer, metadata: info, audio, soundtrack});
      this.#candidate = null; this.#readBytes = readBytes;
      // Closures above keep the renderer identity. Ownership is marked by the
      // current object rather than nulling that captured variable.
      if (restore) previous.renderer.dispose();
      else release(previous);
      return true;
    } catch (error) {
      if (this.#disposed || generation !== this.#generation) return false;
      throw error;
    } finally {
      pcm?.free();
      if (renderer && this.#current?.renderer !== renderer) {
        try { if (!restore) audio?.dispose?.(); } finally { renderer.dispose(); }
      }
      if (this.#candidate === renderer) this.#candidate = null;
      if (this.#restarting === generation) this.#restarting = null;
    }
  }
  present() { this.#current?.renderer.present(); }
  render(frame) {
    if (this.#restarting) throw new Error('Picture worker restart is in progress.');
    this.#current?.renderer.request(frame);
  }
  invalidate() { this.#current?.renderer.invalidate(); }
  dispose() {
    this.#disposed = true; ++this.#generation; this.#restarting = null; this.#readBytes = null;
    this.#controller?.abort(); this.#candidate?.dispose(); this.#candidate = null;
    const previous = this.#current; this.#current = null;
    release(previous);
  }
}
