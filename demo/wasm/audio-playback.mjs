// Presentation only: Web Audio owns elapsed time; recorded picture indices and
// native PCM are never resampled here. Device resampling is the browser's job.
const MAX_U32 = 0xffff_ffff;
export const MAX_AUDIO_BYTES = 256 * 1024 * 1024;
const COPY_FRAMES = 65_536;
// Small clips keep the existing eager path. Larger clips retain one Wasm PCM
// owner and copy only a bounded device lookahead, not another whole soundtrack.
export const MAX_EAGER_AUDIO_BYTES = 1024 * 1024;
export const MAX_AUDIO_WINDOWS = 3;
const AUDIO_LEAD_SECONDS = 0.06;
// Neighboring samples keep device-rate interpolation away from artificial
// buffer edges. Explicit device-clock stops prevent the halo from playing twice.
const AUDIO_HALO_FRAMES = 32;

function integer(value, name, min, max = MAX_U32) {
  if (!Number.isInteger(value) || value < min || value > max) {
    throw new RangeError(`${name} must be an integer in ${min}..=${max}`);
  }
  return value;
}
function time(value) {
  if (!Number.isFinite(value) || value < 0) throw new RangeError('invalid audio context time');
  return value;
}
function close(context) {
  if (context?.state !== 'closed') {
    try { Promise.resolve(context?.close()).catch(() => {}); } catch {}
  }
}

// Ownership of both the extracted Wasm PCM and the context is explicit. On any
// preparation error the old session remains usable; the candidate is disposable.
export function prepareSoundtrack(player, createContext, maxBytes = MAX_AUDIO_BYTES) {
  integer(maxBytes, 'audio byte budget', 1, Number.MAX_SAFE_INTEGER);
  if (typeof createContext !== 'function') throw new TypeError('audio context factory required');
  const pcm = player.take_soundtrack();
  if (!pcm) throw new Error('sound-bearing player did not supply its soundtrack');
  let context = null, retained = false;
  try {
    const channels = integer(pcm.channels, 'audio channels', 1, 2);
    const frames = integer(pcm.sample_frames, 'audio sample frames', 0);
    const rate = integer(pcm.sample_rate, 'audio sample rate', 1);
    if (frames * channels * 4 > maxBytes) throw new RangeError('soundtrack exceeds browser audio byte budget');
    context = createContext();
    if (!context || context.state === 'closed') throw new Error('Web Audio context is unavailable');
    const asynchronous = typeof pcm.read_window === 'function';
    if (frames && (asynchronous || frames * channels * 4 > MAX_EAGER_AUDIO_BYTES)) {
      const playback = new WindowedSoundtrackPlayback(context, pcm, player.fps, player.frame_count, {
        sampleRate: rate, channels, sampleFrames: frames,
      });
      retained = true;
      if (asynchronous) return playback.prepare().catch(error => {
        playback.dispose();
        throw error;
      });
      return playback;
    }
    // AudioBuffer rejects unsupported rates; never silently reinterpret PCM.
    const buffer = frames === 0 ? null : context.createBuffer(channels, frames, rate);
    if (buffer) {
      for (let channel = 0; channel < channels; ++channel) {
        const destination = buffer.getChannelData(channel);
        for (let start = 0; start < frames; start += COPY_FRAMES) {
          pcm.copy_channel_into(channel, start, destination.subarray(start, Math.min(frames, start + COPY_FRAMES)));
        }
      }
    }
    return new SoundtrackPlayback(context, buffer, player.fps, player.frame_count, {
      sampleRate: rate, channels, sampleFrames: frames,
    });
  } catch (error) {
    close(context);
    throw error;
  } finally {
    if (!retained) pcm.free();
  }
}

export class SoundtrackPlayback {
  #context;
  #buffer;
  #fps;
  #count;
  #duration;
  #position = 0;
  #frame = 0;
  #anchor = 0;
  #anchorPosition = 0;
  #lastTime = 0;
  #source = null;
  #playing = false;
  #starting = false;
  #generation = 0;
  #disposed = false;

  constructor(context, buffer, fps, frameCount, audio) {
    this.#fps = integer(fps, 'picture fps', 1);
    this.#count = integer(frameCount, 'picture frame count', 1);
    this.#context = context;
    this.#buffer = buffer;
    this.#duration = Math.max(frameCount / fps, audio.sampleFrames / audio.sampleRate);
    if (!Number.isFinite(this.#duration) || this.#duration <= 0) throw new RangeError('invalid audio presentation duration');
    this.audio = Object.freeze({ ...audio });
  }
  get playing() { return this.#playing; }
  get starting() { return this.#starting; }
  get durationSeconds() { return this.#duration; }
  get positionSeconds() { return this.#position; }
  get frame() { return this.#frame; }

  #check() {
    if (this.#disposed) throw new Error('audio playback is disposed');
    if (this.#context.state === 'closed') throw new Error('audio context is closed');
  }
  #sample() {
    if (!this.#playing) return;
    this.#lastTime = Math.max(this.#lastTime, time(this.#context.currentTime));
    this.#position = Math.min(this.#duration, this.#anchorPosition + Math.max(0, this.#lastTime - this.#anchor));
    // A seek owns its exact integer index. Converting index/fps back to an
    // index can round down (31/29*29 < 31); forward playback cannot rewind it.
    this.#frame = Math.max(this.#frame, Math.min(this.#count - 1, Math.floor(this.#position * this.#fps)));
  }
  #stopSource() {
    const source = this.#source;
    this.#source = null;
    if (source) {
      // stop is allowed after natural completion. Disconnect also handles a
      // device teardown that makes stop refuse; the node cannot keep sounding.
      try { source.stop(); } catch {} // A torn-down device can already be stopped.
      try { source.disconnect(); } catch {} // dispose also closes the context.
    }
  }
  async play() {
    this.#check();
    if (this.#playing) return true;
    if (this.#starting) return false;
    const generation = ++this.#generation;
    this.#starting = true;
    try {
      // Must be reached from the user's gesture. Never claim playback before
      // resume completes; pause/seek/replacement invalidates this continuation.
      await this.#context.resume();
      if (this.#disposed || generation !== this.#generation) return false;
      if (this.#context.state !== 'running') throw new Error('audio context did not resume; press Play to retry');
      if (this.#position >= this.#duration) { this.#position = 0; this.#frame = 0; }
      const now = time(this.#context.currentTime);
      if (this.#buffer && this.#position < this.#buffer.duration) {
        const source = this.#context.createBufferSource();
        try {
          source.buffer = this.#buffer;
          source.connect(this.#context.destination);
          source.start(now, this.#position);
          this.#source = source;
        } catch (error) {
          source.disconnect();
          throw error;
        }
      }
      this.#anchor = now;
      this.#lastTime = now;
      this.#anchorPosition = this.#position;
      this.#playing = true;
      return true;
    } catch (error) {
      if (this.#disposed || generation !== this.#generation) return false;
      throw error;
    } finally {
      if (generation === this.#generation) this.#starting = false;
    }
  }
  pause() {
    ++this.#generation;
    this.#starting = false;
    try { this.#sample(); } finally {
      this.#playing = false;
      this.#stopSource();
    }
  }
  seek(index) {
    this.#check();
    integer(index, 'frame index', 0, this.#count - 1);
    this.pause();
    this.#position = index / this.#fps;
    this.#frame = index;
  }
  frameAt() {
    this.#check();
    this.#sample();
    if (this.#position >= this.#duration) this.pause();
    return this.frame;
  }
  dispose() {
    if (this.#disposed) return;
    this.#disposed = true;
    ++this.#generation;
    this.#starting = false;
    this.#playing = false;
    try { this.#stopSource(); } finally {
      this.#buffer = null;
      close(this.#context);
    }
  }
}


// The immutable Wasm track stays alive for random seeks. Only three bounded
// windows can be owned by Web Audio. frameAt() alone refills the device queue.
// Async sources retain at most one pending transfer; completion can only prime
// a window, never start sound after pause, seek, replacement or disposal.
class WindowedSoundtrackPlayback {
  #context;
  #pcm;
  #primed;
  #asynchronous;
  #pending = null;
  #readError = null;
  #fps;
  #count;
  #windowFrames;
  #deviceRate;
  #duration;
  #slots = new Set();
  #queuedFrame = 0;
  #position = 0;
  #frame = 0;
  #anchor = 0;
  #anchorPosition = 0;
  #anchorFrame = 0;
  #phaseFrames = 0;
  #lastTime = 0;
  #playing = false;
  #starting = false;
  #generation = 0;
  #disposed = false;

  constructor(context, pcm, fps, frameCount, audio) {
    this.#fps = integer(fps, 'picture fps', 1);
    this.#count = integer(frameCount, 'picture frame count', 1);
    const sampleRate = integer(audio.sampleRate, 'audio sample rate', 1);
    const channels = integer(audio.channels, 'audio channels', 1, 2);
    const sampleFrames = integer(audio.sampleFrames, 'audio sample frames', 1);
    this.audio = Object.freeze({ sampleRate, channels, sampleFrames });
    this.#context = context;
    this.#pcm = pcm;
    this.#asynchronous = typeof pcm.read_window === 'function';
    this.#deviceRate = integer(context.sampleRate ?? sampleRate, 'audio device sample rate', 1);
    let a = sampleRate, b = this.#deviceRate;
    while (b) [a, b] = [b, a % b];
    const quantum = sampleRate / a;
    const ceiling = COPY_FRAMES - 2 * AUDIO_HALO_FRAMES;
    if (quantum > ceiling) throw new RangeError('source/device sample-rate ratio exceeds bounded playback window');
    // Whole source/device clock cycles keep every node on the same resampler
    // phase, including a fractional seek or resume. A node never starts midway
    // through a device sample and rounds its source offset differently.
    this.#windowFrames = Math.max(quantum,
      Math.floor(Math.min(ceiling, Math.max(1, Math.ceil(sampleRate / 2))) / quantum) * quantum);
    this.#duration = Math.max(frameCount / fps, sampleFrames / sampleRate);
    // Admission and the first native copy precede session replacement. A bad
    // device rate/allocation/bridge never destroys the previous movie.
    this.#primed = this.#asynchronous ? null : this.#copy(0);
  }
  get playing() { return this.#playing; }
  get starting() { return this.#starting; }
  get durationSeconds() { return this.#duration; }
  get positionSeconds() { return this.#position; }
  get frame() { return this.#frame; }

  #check() {
    if (this.#disposed) throw new Error('audio playback is disposed');
    if (this.#context.state === 'closed') throw new Error('audio context is closed');
  }
  async prepare() {
    await this.#request(0);
    this.#check();
    if (this.#readError) throw this.#readError;
    if (!this.#primed) throw new Error('audio preparation was cancelled');
    return this;
  }
  #request(start) {
    if (this.#pending) return this.#pending.promise;
    const generation = this.#generation;
    const pending = {controller: new AbortController(), promise: null};
    this.#pending = pending;
    const readStart = Math.max(0, start - AUDIO_HALO_FRAMES);
    const readEnd = Math.min(this.audio.sampleFrames, start + this.#windowFrames + AUDIO_HALO_FRAMES);
    const valid = () => !this.#disposed && this.#generation === generation && this.#pending === pending;
    pending.promise = Promise.resolve().then(() => {
      if (!valid()) return null;
      return this.#pcm.read_window(readStart, readEnd - readStart, pending.controller.signal);
    }).then(channels => {
      if (!valid()) return;
      const window = this.#copy(start, channels);
      if (valid()) this.#primed = window;
    }).catch(error => { if (valid()) this.#readError = error; })
      .finally(() => { if (this.#pending === pending) this.#pending = null; });
    return pending.promise;
  }
  #cancelRead() {
    const pending = this.#pending; this.#pending = null;
    pending?.controller.abort();
    this.#readError = null;
    if (this.#asynchronous) this.#primed = null;
  }
  #copy(start, channels = null) {
    const end = Math.min(start + this.#windowFrames, this.audio.sampleFrames);
    const readStart = Math.max(0, start - AUDIO_HALO_FRAMES);
    const readEnd = Math.min(this.audio.sampleFrames, end + AUDIO_HALO_FRAMES);
    if (this.#asynchronous && (!Array.isArray(channels) || channels.length !== this.audio.channels ||
        channels.some(channel => !(channel instanceof Float32Array) || channel.length !== readEnd - readStart))) {
      throw new Error('invalid asynchronous PCM window');
    }
    const buffer = this.#context.createBuffer(this.audio.channels, readEnd - readStart, this.audio.sampleRate);
    for (let channel = 0; channel < this.audio.channels; ++channel) {
      if (this.#asynchronous) buffer.getChannelData(channel).set(channels[channel]);
      else this.#pcm.copy_channel_into(channel, readStart, buffer.getChannelData(channel));
    }
    return { start, end, readStart, buffer };
  }
  #updateFrame() {
    this.#frame = Math.max(this.#frame, Math.min(this.#count - 1, Math.floor(this.#position * this.#fps)));
  }
  #sample() {
    if (!this.#playing) return;
    this.#lastTime = Math.max(this.#lastTime, time(this.#context.currentTime));
    // A delayed render/UI tick cannot turn unqueued samples into elapsed audio.
    // Silence after EOF is allowed only after the whole track was queued, or
    // after an explicit seek beyond it.
    const available = this.#queuedFrame < this.audio.sampleFrames
      ? this.#anchorPosition + (this.#queuedFrame - this.#anchorFrame) / this.audio.sampleRate : this.#duration;
    this.#position = Math.min(this.#duration, available,
      this.#anchorPosition + Math.max(0, this.#lastTime - this.#anchor));
    this.#updateFrame();
  }
  #release(slot, stop) {
    if (!this.#slots.delete(slot)) return;
    slot.node.onended = null;
    if (stop) { try { slot.node.stop(); } catch {} }
    try { slot.node.disconnect(); } catch {}
    try { slot.node.buffer = null; } catch {}
  }
  #stop() {
    for (const slot of this.#slots) this.#release(slot, true);
  }
  #fill(reset = false) {
    if (this.#readError) { const error = this.#readError; this.#readError = null; throw error; }
    const generation = this.#generation;
    const before = time(this.#context.currentTime);
    for (const slot of this.#slots) if (slot.endTime <= before) this.#release(slot, false);
    if (this.#primed && this.#primed.start !== this.#queuedFrame) this.#primed = null;
    const prepared = [];
    let cursor = this.#queuedFrame;
    while (this.#slots.size + prepared.length < MAX_AUDIO_WINDOWS && cursor < this.audio.sampleFrames) {
      if (this.#asynchronous && !this.#primed) { this.#request(cursor); break; }
      const window = this.#primed ?? this.#copy(cursor);
      this.#primed = null;
      if (generation !== this.#generation || this.#disposed) return;
      prepared.push(window);
      cursor = window.end;
    }
    const now = time(this.#context.currentTime);
    if (reset || (prepared.length && now > this.#anchor +
        (this.#queuedFrame - this.#anchorFrame) / this.audio.sampleRate)) {
      // Resume from the first unheard sample phase, not the advanced device
      // clock. Retain the phase explicitly instead of accumulating rounded cuts.
      if (reset) this.#phaseFrames = this.#position * this.audio.sampleRate - this.#queuedFrame;
      else {
        this.#stop();
        this.#position = this.#anchorPosition + (this.#queuedFrame - this.#anchorFrame) / this.audio.sampleRate;
      }
      this.#anchorPosition = this.#position;
      this.#anchorFrame = this.#queuedFrame;
      this.#anchor = Math.ceil((now + AUDIO_LEAD_SECONDS) * this.#deviceRate - 1e-7) / this.#deviceRate;
      this.#lastTime = now;
    }
    for (const window of prepared) {
      const nominal = this.#anchor + (window.start - this.#anchorFrame) / this.audio.sampleRate;
      const when = Math.round(nominal * this.#deviceRate) / this.#deviceRate;
      const offset = (window.start - window.readStart + this.#phaseFrames) / this.audio.sampleRate;
      const duration = (window.end - window.start -
        (window.end === this.audio.sampleFrames ? this.#phaseFrames : 0)) / this.audio.sampleRate;
      const node = this.#context.createBufferSource();
      const slot = { node, endTime: when + duration };
      this.#slots.add(slot);
      node.onended = () => this.#release(slot, false);
      try {
        node.buffer = window.buffer;
        node.connect(this.#context.destination);
        if (when < time(this.#context.currentTime)) throw new Error('audio device missed its scheduling window; press Play to retry');
        node.start(when, offset);
        // Stop on the shared device clock, not start()'s source-duration grain:
        // browsers can round that grain one output sample short when resampling.
        if (window.end < this.audio.sampleFrames) node.stop(slot.endTime);
        this.#queuedFrame = window.end;
      } catch (error) {
        this.#release(slot, true);
        throw error;
      }
    }
  }
  async play() {
    this.#check();
    if (this.#playing) return true;
    if (this.#starting) return false;
    const generation = ++this.#generation;
    this.#starting = true;
    try {
      await this.#context.resume();
      if (this.#disposed || generation !== this.#generation) return false;
      if (this.#context.state !== 'running') throw new Error('audio context did not resume; press Play to retry');
      if (this.#position >= this.#duration) { this.#position = 0; this.#frame = 0; }
      this.#queuedFrame = Math.min(this.audio.sampleFrames, Math.floor(this.#position * this.audio.sampleRate));
      if (this.#primed && this.#primed.start !== this.#queuedFrame) this.#primed = null;
      if (this.#asynchronous && !this.#primed && this.#queuedFrame < this.audio.sampleFrames) {
        await this.#request(this.#queuedFrame);
        if (this.#disposed || generation !== this.#generation) return false;
        if (this.#context.state !== 'running') throw new Error('audio device suspended during buffering; press Play to retry');
      }
      this.#fill(true);
      if (this.#disposed || generation !== this.#generation) return false;
      this.#playing = true;
      return true;
    } catch (error) {
      if (this.#disposed || generation !== this.#generation) return false;
      this.#cancelRead();
      this.#stop();
      throw error;
    } finally {
      if (generation === this.#generation) this.#starting = false;
    }
  }
  pause() {
    ++this.#generation;
    this.#starting = false;
    this.#cancelRead();
    try { this.#sample(); } finally {
      this.#playing = false;
      this.#stop();
    }
  }
  seek(index) {
    this.#check();
    integer(index, 'frame index', 0, this.#count - 1);
    this.pause();
    this.#position = index / this.#fps;
    this.#frame = index;
  }
  frameAt() {
    this.#check();
    try {
      this.#sample();
      if (this.#playing) {
        if (this.#position >= this.#duration || this.#context.state !== 'running') this.pause();
        else this.#fill();
      }
    } catch (error) {
      // Copy/allocation/device failures stop every queued node. A later explicit
      // Play may retry; it must not skip the samples that failed to load.
      this.pause();
      throw error;
    }
    return this.frame;
  }
  dispose() {
    if (this.#disposed) return;
    this.#disposed = true;
    ++this.#generation;
    this.#starting = false;
    this.#playing = false;
    this.#cancelRead();
    const pcm = this.#pcm;
    this.#pcm = null;
    this.#primed = null;
    try { this.#stop(); } finally {
      try { pcm.free(); } finally { close(this.#context); }
    }
  }
}
