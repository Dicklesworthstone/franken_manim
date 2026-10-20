"use strict";
// Recorded narration uses the device audio clock. The existing timeline owns
// picture requests; this controller never sends scene edits or runs live code.
(() => {
  const MAX_FRAMES = 65536, MAX_BYTES = MAX_FRAMES * 2 * 4, MAX_SLOTS = 3;
  const hex = bytes => [...new Uint8Array(bytes)].map(n => n.toString(16).padStart(2, "0")).join("");
  const uint = value => Number.isSafeInteger(value) && value >= 0;
  const headerInt = (response, name) => {
    const text = response.headers.get(name);
    if (text === null || !/^(0|[1-9][0-9]*)$/.test(text) || !uint(Number(text))) {
      throw new Error(`Invalid recorded audio header: ${name}.`);
    }
    return Number(text);
  };
  function metadata(value) {
    if (value?.schema !== "fmn.studio.audio" || value.version !== 1 ||
        !/^[0-9a-f]{64}$/.test(value.source) || !uint(value.worker_generation) || value.worker_generation < 1 ||
        !uint(value.sample_rate) || value.sample_rate < 1 || value.sample_rate > 384000 ||
        ![1, 2].includes(value.channels) || !uint(value.sample_frames) ||
        !uint(value.fps) || value.fps < 1 || !uint(value.picture_frames) || value.picture_frames < 1 ||
        !uint(value.max_chunk_frames) || value.max_chunk_frames < 1 || value.max_chunk_frames > MAX_FRAMES ||
        !uint(value.picture_frames * value.sample_rate) || !uint(value.sample_frames * value.fps)) {
      throw new Error("Unsupported or invalid recorded narration metadata.");
    }
    return Object.freeze({...value});
  }
  const identity = info => JSON.stringify([info.source, info.worker_generation, info.sample_rate,
    info.channels, info.sample_frames, info.fps, info.picture_frames, info.max_chunk_frames]);
  // Absolute rational boundaries, matching frames_to_samples; no sum of rounded durations.
  const sampleAtFrame = (info, frame) => Number((2n * BigInt(frame) * BigInt(info.sample_rate) + BigInt(info.fps)) / (2n * BigInt(info.fps)));
  async function bounded(response, limit) {
    if (!response.body) throw new Error("Missing recorded audio response body.");
    const declared = response.headers.get("Content-Length");
    const reader = response.body.getReader();
    try {
      if (declared !== null && (!/^[0-9]+$/.test(declared) || Number(declared) > limit)) {
        throw new Error("Recorded audio response exceeds its byte budget.");
      }
      const chunks = []; let size = 0;
      for (;;) {
        const {done, value} = await reader.read();
        if (done) break;
        size += value.byteLength;
        if (size > limit) throw new Error("Recorded audio response exceeds its byte budget.");
        chunks.push(value);
      }
      const bytes = new Uint8Array(size); let offset = 0;
      for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
      return bytes;
    } finally { await reader.cancel().catch(() => {}); }
  }
  class Transport {
    constructor(fetcher, capability, crypto) {
      this.fetcher = fetcher; this.capability = capability; this.crypto = crypto;
    }
    async get(path, signal, type, limit) {
      const timeout = AbortSignal.timeout(30000);
      const response = await this.fetcher(path, {headers: {"X-FMN-Capability": this.capability},
        cache: "no-store", redirect: "error", signal: AbortSignal.any([signal, timeout])});
      if (!response.ok) {
        await response.body?.cancel().catch(() => {});
        const error = new Error(`Recorded narration request refused (${response.status}). Refresh the Studio session before retrying.`);
        error.status = response.status; throw error;
      }
      if (response.headers.get("Content-Type")?.split(";")[0].trim() !== type) {
        await response.body?.cancel().catch(() => {});
        throw new Error("Unexpected recorded audio content type.");
      }
      return {response, bytes: await bounded(response, limit)};
    }
    async info(signal) {
      const {bytes} = await this.get("/api/audio", signal, "application/json", 8192);
      return metadata(JSON.parse(new TextDecoder("utf-8", {fatal: true}).decode(bytes)));
    }
    async chunk(info, start, count, signal) {
      if (!uint(start) || start > info.sample_frames || !uint(count) || count < 1 || count > info.max_chunk_frames) {
        throw new Error("Invalid local audio sample window.");
      }
      const query = new URLSearchParams({source: info.source, worker_generation: String(info.worker_generation),
        start: String(start), frames: String(count)});
      const expected = Math.min(count, info.sample_frames - start);
      const {response, bytes} = await this.get("/api/audio/chunk?" + query, signal,
        "application/vnd.frankenmanim.pcm-f32le", Math.min(MAX_BYTES, expected * info.channels * 4));
      if (response.headers.get("X-FMN-Audio-Source") !== info.source ||
          headerInt(response, "X-FMN-Worker-Generation") !== info.worker_generation ||
          headerInt(response, "X-FMN-Audio-Start") !== start ||
          headerInt(response, "X-FMN-Audio-Frames") !== expected ||
          headerInt(response, "X-FMN-Audio-Next") !== start + expected ||
          response.headers.get("X-FMN-Audio-End") !== String(start + expected === info.sample_frames) ||
          bytes.byteLength !== expected * info.channels * 4) {
        throw new Error("Stale, truncated or inconsistent recorded audio response.");
      }
      const digest = hex(await this.crypto.subtle.digest("SHA-256", bytes));
      if (digest !== response.headers.get("X-FMN-SHA256")) throw new Error("Recorded audio checksum mismatch.");
      const data = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
      const channels = Array.from({length: info.channels}, () => new Float32Array(expected));
      for (let frame = 0; frame < expected; frame++) {
        for (let channel = 0; channel < info.channels; channel++) {
          const value = data.getFloat32((frame * info.channels + channel) * 4, true);
          if (!Number.isFinite(value)) throw new Error("Non-finite recorded audio sample.");
          channels[channel][frame] = value;
        }
      }
      return {start, frames: expected, channels};
    }
  }
  class Player {
    constructor({transport, context, onState = () => {}, onFrame = () => {}, onReady = () => {}}) {
      this.transport = transport; this.contextFactory = context; this.onState = onState; this.onFrame = onFrame; this.onReady = onReady;
      this.context = null; this.info = null; this.position = 0; this.epoch = 0; this.controller = new AbortController();
      this.slots = new Set(); this.loading = false; this.checking = false; this.status = "idle";
      this.gain = null; this.volume = 1; this.lastPreview = -Infinity; this.lastFrame = -1; this.lastCheck = 0;
    }
    emit(message = "") { this.onState({status: this.status, message, sample: this.current(), info: this.info}); }
    current() {
      if (this.status !== "playing") return this.position;
      return Math.min(this.end, this.anchorSample + Math.max(0,
        Math.floor((this.context.currentTime - this.anchorTime) * this.info.sample_rate)));
    }
    clear() {
      this.epoch++; this.controller.abort(); this.controller = new AbortController();
      for (const slot of this.slots) { try { slot.node.stop(); } catch {} slot.node.disconnect(); }
      this.slots.clear(); this.loading = false; this.checking = false;
    }
    pause(message = "Paused", position = this.current()) {
      this.clear(); this.position = position; this.status = "paused"; this.emit(message);
    }
    seek(frame) {
      if (!this.info || !uint(frame) || frame >= this.info.picture_frames) throw new Error("Invalid narration frame.");
      this.pause("Paused at selected frame", sampleAtFrame(this.info, frame));
    }
    invalidate(message = "Narration stopped; refresh its recording before playback.") {
      this.pause(message); this.info = null; this.position = 0;
    }
    setVolume(value) {
      if (!Number.isFinite(value) || value < 0 || value > 1) throw new Error("Invalid narration volume.");
      this.volume = value;
      if (this.gain) this.gain.gain.value = value;
    }
    async probe() {
      const epoch = this.epoch;
      const info = await this.transport.info(this.controller.signal);
      if (epoch !== this.epoch) return false;
      this.info = info; this.emit("Recorded narration available"); return true;
    }
    async play(frame = null) {
      this.pause("Loading narration"); this.status = "loading"; this.emit("Loading narration");
      const epoch = this.epoch, signal = this.controller.signal;
      try {
        // Construct and resume synchronously in the user's click handler, before
        // fetching metadata. Autoplay is never attempted during page load.
        if (!this.context) {
          this.context = this.contextFactory(); this.gain = this.context.createGain();
          this.gain.gain.value = this.volume; this.gain.connect(this.context.destination);
        }
        const resumed = this.context.resume();
        const infoPromise = this.transport.info(signal);
        const [, info] = await Promise.all([resumed, infoPromise]);
        if (epoch !== this.epoch) return false;
        if (this.context.state !== "running") throw new Error("The audio device is suspended. Press Play narration to resume.");
        if (this.info && identity(this.info) !== identity(info)) {
          this.info = info; this.position = 0;
          throw new Error("The recording or worker changed. Select a frame and start narration again.");
        }
        this.onReady(info);
        this.info = info;
        if (frame !== null) {
          if (!uint(frame) || frame >= info.picture_frames) throw new Error("Invalid selected narration frame.");
          this.position = sampleAtFrame(info, frame);
        }
        this.end = Math.max(info.sample_frames, sampleAtFrame(info, info.picture_frames));
        if (this.position >= this.end) this.position = 0;
        const start = this.position;
        const count = Math.min(info.max_chunk_frames, Math.max(1, Math.ceil(info.sample_rate / 2)));
        const chunk = start < info.sample_frames ? await this.transport.chunk(info, start, count, signal) : null;
        if (epoch !== this.epoch) return false;
        this.anchorSample = start; this.anchorTime = this.context.currentTime + 0.06; this.queuedEnd = start;
        this.lastCheck = this.context.currentTime; this.lastPreview = -Infinity; this.lastFrame = -1;
        if (chunk) this.schedule(chunk);
        this.status = "playing"; this.emit("Playing recorded narration"); return true;
      } catch (error) {
        if (epoch === this.epoch) this.pause(error.message);
        return false;
      }
    }
    schedule(chunk) {
      const when = this.anchorTime + (chunk.start - this.anchorSample) / this.info.sample_rate;
      if (chunk.start !== this.queuedEnd || chunk.frames < 1 || when < this.context.currentTime) {
        throw new Error("Narration buffer underrun. Press Play narration to resume without skipping samples.");
      }
      const buffer = this.context.createBuffer(this.info.channels, chunk.frames, this.info.sample_rate);
      for (let channel = 0; channel < this.info.channels; channel++) buffer.copyToChannel(chunk.channels[channel], channel);
      const node = this.context.createBufferSource(); node.buffer = buffer; node.connect(this.gain);
      const slot = {node, endTime: when + chunk.frames / this.info.sample_rate};
      node.onended = () => { node.disconnect(); this.slots.delete(slot); };
      this.slots.add(slot);
      try { node.start(when); } catch (error) { node.disconnect(); this.slots.delete(slot); throw error; }
      this.queuedEnd = chunk.start + chunk.frames;
    }
    pump() {
      if (this.status !== "playing") return;
      const epoch = this.epoch, now = this.context.currentTime, current = this.current();
      if (this.context.state !== "running") { this.pause("Audio device suspended; press Play narration to resume."); return; }
      for (const slot of this.slots) if (slot.endTime <= now) { slot.node.disconnect(); this.slots.delete(slot); }
      if (current >= this.end) {
        const last = this.info.picture_frames - 1;
        if (last !== this.lastFrame) { this.lastFrame = last; this.onFrame(last); }
        if (epoch === this.epoch) this.pause("Narration finished", this.end);
        return;
      }
      if (current >= this.queuedEnd && now >= this.anchorTime && this.queuedEnd < this.info.sample_frames) {
        this.pause("Buffering stopped playback. Press Play narration to resume without skipping samples.", this.queuedEnd); return;
      }
      // At most ten picture requests/second. These select existing samples,
      // not a new render grid; late pictures catch up without slowing audio.
      if (now - this.lastPreview >= 0.1) {
        this.lastPreview = now;
        const frame = Math.min(this.info.picture_frames - 1, Math.floor(current * this.info.fps / this.info.sample_rate));
        if (frame !== this.lastFrame) { this.lastFrame = frame; this.onFrame(frame); }
      }
      if (epoch !== this.epoch || this.status !== "playing") return;
      if (!this.loading && this.slots.size < MAX_SLOTS && this.queuedEnd < this.info.sample_frames) {
        this.loading = true;
        const start = this.queuedEnd, info = this.info;
        const count = Math.min(info.max_chunk_frames, Math.max(1, Math.ceil(info.sample_rate / 2)));
        void this.transport.chunk(info, start, count, this.controller.signal).then(chunk => {
          if (epoch === this.epoch && this.status === "playing") this.schedule(chunk);
        }).catch(error => {
          if (epoch === this.epoch) this.pause(error.message, Math.min(this.current(), this.queuedEnd));
        }).finally(() => { if (epoch === this.epoch) this.loading = false; });
      }
      // Check generation even during an audio tail or silent picture suffix.
      if (!this.checking && now - this.lastCheck >= 1) {
        this.checking = true; this.lastCheck = now;
        void this.transport.info(this.controller.signal).then(info => {
          if (epoch === this.epoch && identity(info) !== identity(this.info)) {
            this.invalidate("The recording or worker changed. Refresh before playing narration.");
          }
        }).catch(error => { if (epoch === this.epoch) this.pause(error.message); })
          .finally(() => { if (epoch ===this.epoch) this.checking = false; });
      }
    }
    dispose() {
      this.pause("Narration stopped"); const context = this.context;
      this.context = null; this.gain = null; return context?.close();
    }
  }
  if (typeof module === "object" && module.exports) {
    module.exports = {Transport, Player, metadata, sampleAtFrame}; return;
  }
  const $ = id => document.getElementById(id), script = document.currentScript;
  const capability = script && new URL(script.src).searchParams.get("cap");
  if (!capability || !$("audio-play")) return;
  const Context = window.AudioContext || window.webkitAudioContext;
  let internalSeek = false, disposed = false, useSelectedFrame = true;
  const safe = value => String(value).split(capability).join("[session]").slice(0, 400);
  const player = new Player({
    transport: new Transport(window.fetch.bind(window), capability, window.crypto),
    context: () => { if (!Context) throw new Error("This browser has no Web Audio playback support."); return new Context(); },
    onReady: info => {
      const display = $("display"), timeline = $("timeline");
      if (timeline.disabled || display.dataset.connected !== "true" || display.dataset.synchronized !== "true" ||
          Number(timeline.max) + 1 !== info.picture_frames || Number(display.dataset.fps) !== info.fps ||
          Number(display.dataset.workerGeneration) !== info.worker_generation) {
        throw new Error("Narration and displayed picture belong to different clocks or workers. Refresh before playback.");
      }
    },
    onState: event => {
      $("audio-play").textContent = event.status === "playing" || event.status === "loading" ? "Pause narration" : "Play narration";
      $("audio-play").setAttribute("aria-pressed", String(event.status === "playing"));
      $("audio-status").textContent = safe(event.message);
      $("audio-status").dataset.state = event.status;
    },
    onFrame: frame => {
      const timeline = $("timeline");
      if (timeline.disabled || Number(timeline.max) + 1 !== player.info.picture_frames ||
          Number($("display").dataset.fps) !== player.info.fps ||
          Number($("display").dataset.workerGeneration) !== player.info.worker_generation) {
        player.pause("Picture timeline changed or disconnected. Refresh before playing narration."); return;
      }
      internalSeek = true;
      try { timeline.value = String(frame); timeline.dispatchEvent(new Event("input", {bubbles: true})); }
      finally { internalSeek = false; }
    },
  });
  $("audio-play").addEventListener("click", () => {
    if (["playing", "loading"].includes(player.status)) { player.pause(); return; }
    if ($("timeline").disabled || $("display").dataset.synchronized !== "true") {
      player.pause("Wait for a synchronized picture before starting narration."); return;
    }
    // Reuse the ordinary controls to release live input and stop frame-paced
    // playback. Narration never writes directly to the Studio command queue.
    if ($("play").textContent === "Pause") $("play").click();
    if ($("live-run").textContent === "Pause live") $("live-run").click();
    const frame = Number($("display").dataset.frame);
    const selected = useSelectedFrame ? frame : null;
    useSelectedFrame = false; void player.play(selected);
  });
  $("audio-volume").addEventListener("input", () => player.setVolume(Number($("audio-volume").value)));
  function stopForEdit() { if (!internalSeek) { player.pause(); useSelectedFrame = true; } }
  for (const id of ["first", "previous", "next", "last", "play", "live-step", "live-run", "restart", "save-session", "input-events"]) {
    $(id).addEventListener("click", stopForEdit, true);
  }
  for (const type of ["input", "change"]) $("timeline").addEventListener(type, stopForEdit, true);
  $("timeline-form").addEventListener("submit", stopForEdit, true);
  $("preview").addEventListener("pointerdown", stopForEdit, true);
  $("preview").addEventListener("keydown", stopForEdit, true);
  $("preview").addEventListener("wheel", stopForEdit, {capture: true, passive: true});
  window.addEventListener("blur", () => player.pause("Narration paused while the window is inactive."));
  window.addEventListener("offline", () => player.pause("Narration paused: connection unavailable."));
  document.addEventListener("visibilitychange", () => { if (document.hidden) player.pause("Narration paused while the page is hidden."); });
  window.addEventListener("pagehide", () => { disposed = true; void player.dispose(); });
  window.addEventListener("pageshow", event => {
    if (event.persisted && disposed) { disposed = false; useSelectedFrame = true; requestAnimationFrame(tick); }
  });
  new MutationObserver(() => {
    if (!["playing", "loading"].includes(player.status)) return;
    const display = $("display");
    if (display.dataset.connected !== "true" || !$("error").hidden ||
        (player.info && Number(display.dataset.workerGeneration) !== player.info.worker_generation)) {
      player.pause("Narration paused: the picture connection or worker changed."); useSelectedFrame = true;
    }
  }).observe(document.querySelector("main"), {attributes: true, subtree: true,
    attributeFilter: ["data-connected", "data-worker-generation"]});
  new MutationObserver(() => {
    if (!$("error").hidden && ["playing", "loading"].includes(player.status)) player.pause("Narration paused: the picture request failed.");
  }).observe($("error"), {attributes: true, attributeFilter: ["hidden"]});
  function tick() { if (!disposed) { player.pump(); requestAnimationFrame(tick); } }
  requestAnimationFrame(tick);
  // Probe is informational, not a permanent gate: a worker can still be starting,
  // or acquire narration later. An explicit click always retries its metadata.
  $("audio-play").disabled = !Context;
  void player.probe().catch(error => {
    $("audio-status").textContent = error.status === 404 ? "No recorded narration transport in this Studio session." : safe(error.message);
  });
})();