// Presentation only: Web Audio owns elapsed time; recorded picture indices and
// native PCM are never resampled here. Device resampling is the browser's job.
const MAX_U32 = 0xffff_ffff;
export const MAX_AUDIO_BYTES = 256 * 1024 * 1024;
const COPY_FRAMES = 65_536;

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
  let context = null;
  try {
    const channels = integer(pcm.channels, 'audio channels', 1, 2);
    const frames = integer(pcm.sample_frames, 'audio sample frames', 0);
    const rate = integer(pcm.sample_rate, 'audio sample rate', 1);
    if (frames * channels * 4 > maxBytes) throw new RangeError('soundtrack exceeds browser audio byte budget');
    context = createContext();
    if (!context || context.state === 'closed') throw new Error('Web Audio context is unavailable');
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
    pcm.free();
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
