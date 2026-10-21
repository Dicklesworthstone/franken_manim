import {WORKER_PROTOCOL, MAX_BUNDLE_BYTES, MAX_PCM_BYTES, COPY_FRAMES,
  integer, viewport, audioInfo, metadata, pixels, recordingSource, sourceIdentity, sameSource} from './worker-protocol.mjs';

// Factory injection is for the host-side test seam. The shipping entry point
// always instantiates the real FmnPlayer from the fixed local Wasm module.
export class PictureWorkerRuntime {
  #createPlayer;
  #post;
  #player = null;
  #busy = false;
  #disposed = false;
  #lastId = 0;
  #width = 0;
  #height = 0;

  constructor(createPlayer, post) { this.#createPlayer = createPlayer; this.#post = post; }

  async receive(message) {
    let candidate = null, pcm = null;
    try {
      if (this.#disposed) return;
      if (this.#busy || message?.protocol !== WORKER_PROTOCOL ||
          message.id !== this.#lastId + 1) throw new Error('Invalid or concurrent picture worker request.');
      integer(message.id, 'request identity', 1, Number.MAX_SAFE_INTEGER);
      this.#lastId = message.id;
      this.#busy = true;
      if (message.kind === 'load' || message.kind === 'restore') {
        if (this.#player) throw new Error('A picture worker owns exactly one recording.');
        const length = viewport(message.width, message.height);
        integer(message.maxBundleBytes, 'bundle byte limit', 1, MAX_BUNDLE_BYTES);
        integer(message.maxAudioBytes, 'PCM byte limit', 1, MAX_PCM_BYTES);
        if (!(message.bytes instanceof ArrayBuffer) || message.bytes.byteLength > message.maxBundleBytes) {
          throw new RangeError('Recording exceeds the worker input byte budget.');
        }
        const restoring = message.kind === 'restore';
        const expected = restoring ? sourceIdentity(message.source) : null;
        const firstFrame = restoring ? integer(message.frame, 'restore frame') : 0;
        const source = recordingSource(new Uint8Array(message.bytes));
        candidate = await this.#createPlayer(new Uint8Array(message.bytes));
        if (this.#disposed) return;
        // Only the native decoder can verify the claimed checksum. Even a
        // matching trailer must pass that admission before old PCM is reused.
        if (expected && !sameSource(source, expected)) throw new Error('Recording changed; reopen it instead of restarting with old narration.');
        candidate.set_viewport(message.width, message.height);
        const hasSoundtrack = Boolean(candidate.has_soundtrack);
        let audio = null;
        if (hasSoundtrack) {
          pcm = candidate.take_soundtrack();
          if (!pcm) throw new Error('Native player did not supply its soundtrack.');
          audio = audioInfo({sampleRate: pcm.sample_rate, channels: pcm.channels,
            sampleFrames: pcm.sample_frames}, message.maxAudioBytes);
        }
        const names = candidate.labels();
        if (!Array.isArray(names) || names.length > 4096) throw new RangeError('Too many recording labels.');
        const info = metadata({frameCount: candidate.frame_count, fps: candidate.fps,
          durationSeconds: candidate.duration_seconds, engineVersion: candidate.engine_version,
          hasCameraTrack: Boolean(candidate.has_camera_track), hasSoundtrack, audio,
          labels: names.map(name => ({name, index: candidate.frame_of_label(name) ?? null}))});
        const rgba = new Uint8Array(length);
        integer(firstFrame, 'restore frame', 0, info.frameCount - 1);
        candidate.render_into(firstFrame, rgba);
        candidate.seek_frame(firstFrame);
        // Move the soundtrack to the browser audio owner once. No second Wasm
        // player is decoded on the UI thread. Every bridge copy stays bounded.
        const channels = [];
        if (pcm && !restoring) {
          for (let channel = 0; channel < audio.channels; channel++) {
            const values = new Float32Array(audio.sampleFrames);
            for (let start = 0; start < values.length; start += COPY_FRAMES) {
              pcm.copy_channel_into(channel, start, values.subarray(start, Math.min(start + COPY_FRAMES, values.length)));
            }
            channels.push(values.buffer);
          }
        }
        if (pcm) { const released = pcm; pcm = null; released.free(); }
        this.#width = message.width; this.#height = message.height;
        this.#player = candidate; candidate = null;
        this.#post({protocol: WORKER_PROTOCOL, id: message.id, kind: 'ready',
          metadata: info, source, audioReused: restoring, frame: firstFrame, width: this.#width, height: this.#height,
          pixels: rgba.buffer, channels}, [rgba.buffer, ...channels]);
      } else if (message.kind === 'render' && this.#player) {
        const frame = integer(message.frame, 'requested frame', 0, this.#player.frame_count - 1);
        const buffer = pixels(message.pixels, this.#width, this.#height);
        this.#player.render_into(frame, new Uint8Array(buffer));
        this.#player.seek_frame(frame);
        this.#post({protocol: WORKER_PROTOCOL, id: message.id, kind: 'frame',
          frame, width: this.#width, height: this.#height, pixels: buffer}, [buffer]);
      } else {
        throw new Error('Unsupported picture worker request.');
      }
    } catch (error) {
      if (!this.#disposed) {
        this.dispose();
        this.#post({protocol: WORKER_PROTOCOL, id: message?.id, kind: 'error',
          message: String(error?.message || error).slice(0, 600)}, []);
      }
    } finally {
      try { pcm?.free(); } finally { candidate?.free(); this.#busy = false; }
    }
  }

  dispose() {
    this.#disposed = true;
    const player = this.#player; this.#player = null;
    player?.free();
  }
}
