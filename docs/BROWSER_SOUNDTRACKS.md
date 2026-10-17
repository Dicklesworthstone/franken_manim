# Self-contained browser soundtracks

`FmnPlayer.from_bundle_with_audio(bytes)` accepts sound-bearing FMTL/1 minor-2
artifacts through the production decoder: engine identity, nested pictures,
camera captures, normalized PCM and input limits remain authoritative. It does
not execute scene code or reopen source audio.

The ordinary `FmnPlayer.from_bundle(bytes)` remains picture-only and refuses
sound at load. The audio-aware constructor still blocks rendering while sound
is attached. `player.take_soundtrack()` transfers it once and authorizes picture
reconstruction. Reading `has_soundtrack` alone does not authorize silent playback.

The returned `FmnSoundtrack` exposes `sample_rate`, `channels`, `sample_frames`
and `duration_seconds`. `copy_channel_into(channel, start_frame, destination)`
copies a sample-frame window into a Float32Array without mixing, gain,
resampling or quantization. Mono is channel zero; stereo is left then right.
Invalid channels or ranges leave the destination unchanged. Call `free()` after
copying to release Wasm PCM. The host then owns scheduling and storage. The
bridge marshals the supplied slice; this is bounded copying, not a zero-copy
memory view. Chunked transfers bound the bridge's scratch buffer.

Generate a moving-geometry recording with a stereo tone:

```sh
cargo run -p fmn-wasm --example export_audio_bundle -- /tmp/audible.fmtl
```

The example refuses an existing destination. Its 1.75-second audio tail extends
past the 1.5-second picture grid; an audio host should hold the final picture
instead of dropping the tail. No source-media dependency is stored in the file.

The CPU renderer and recorded frame indices are unchanged. Browser/device audio
resampling and scheduling are presentation behavior, not certified PCM or
encoded-output determinism. The Rust tests compare exact channel samples,
original camera/planar pixels and invalid-copy destination preservation.

## Play, pause and seek in the local browser player

Build `demo/wasm/pkg` using the existing WASM instructions, open `player.html`,
and choose the exported `.fmtl`. The page accepts camera-bearing recordings
and sound-bearing recordings through the same transactional session. Files stay
local. Silent bundles retain their existing looping presentation clock.

Sound is copied into a Web Audio buffer in channel windows of at most 65,536
frames. The transfer frees the original Wasm PCM after preparation. The 256 MiB
audio-buffer admission limit is independent of the 256 MiB bundle input limit;
decoded pictures, the input bytes, bridge scratch and replacement candidates
are additional storage, not a claimed process-memory ceiling. Browser-supported
sample rates and device capabilities are checked by Web Audio; unsupported
rates or missing devices reject the candidate instead of reinterpreting samples.

Playback starts only from Play, after `AudioContext.resume()` succeeds. One audio
context clock selects the recorded picture indices, independent of monitor
refresh. Late presentation may skip display frames, not change the recorded
scene. Pause keeps fractional audio time; resume does not jump back to the last
displayed picture. Scrubbing stops sound and selects an exact frame-grid offset.
A short track ends in silence while pictures finish. A long track holds the last
picture through its tail. Audible recordings stop at the longer duration;
pressing Play at the end restarts both from time zero.

Pause, seek, replacement and disposal invalidate pending resume operations.
A late resume cannot start old sound over the replacement. Preparation and
frame-zero rendering must both succeed before the previous player is released.
Failures preserve the previous movie; successful replacement stops and releases
its audio device. Page hiding pauses playback. The existing picture-only Rust
constructor remains intentionally strict for consumers without an audio adapter.

## Executable checks

```sh
node --test demo/wasm/*.test.mjs
node scripts/check_browser_soundtrack.mjs /tmp/audible.fmtl
```

The browser check requires the generated web-target bindings under
`demo/wasm/pkg`, Playwright and its Chromium installation, and the exact native
`export_audio_bundle` example above. `FMN_PLAYWRIGHT_MODULE` may name the module
file of an existing Playwright installation outside the repository. These are
test tools, not runtime or shipped Rust dependencies. The script serves only an
allowlist of demo assets on an ephemeral loopback port and closes its browser
and server on success or failure. Missing tooling or artifacts fail the check.

It compares all 168,000 transferred PCM samples with the fixture's canonical
bits, validates unchanged pictures, and uses a real OfflineAudioContext to
check 144,000 seeked output samples. It also drives the actual player controls:
user-gesture playback, fractional pause/resume, seek offsets, tail completion,
restart, failed replacement and device cleanup. This is functional browser
evidence, not the separate optimized npm size/release gate or hardware audio
latency certification.
