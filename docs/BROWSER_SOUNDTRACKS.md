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
Invalid channels or ranges leave the destination unchanged. Call `free()` when
the host no longer needs to copy or seek the retained Wasm PCM. The bridge
marshals the supplied slice; this is bounded copying, not a zero-copy memory
view. Chunked transfers bound the bridge's scratch buffer.

Generate a moving-geometry recording with a stereo tone:

```sh
cargo run -p fmn-wasm --example export_audio_bundle -- /tmp/audible.fmtl
```

The example refuses an existing destination. Its 1.75-second audio tail extends
past the 1.5-second picture grid; an audio host should hold the final picture
instead of dropping the tail. No source-media dependency is stored in the file.

Python-authored recordings can also include sound with
`fmn_python.export_bundle(SceneClass, "scene.fmtl", audio=True, camera=True)`;
see [portable export](SOUNDTRACK_BUNDLES.md) for native WAV inputs and bounds.
The browser consumes the same FMTL bytes and never executes Python.

The CPU renderer and recorded frame indices are unchanged. Browser/device audio
resampling and scheduling are presentation behavior, not certified PCM or
encoded-output determinism. The Rust tests compare exact channel samples,
original camera/planar pixels and invalid-copy destination preservation.

## Play, pause and seek in the local browser player

Build `demo/wasm/pkg` using the existing WASM instructions, open `player.html`,
and choose the exported `.fmtl`. The page accepts camera-bearing recordings
and sound-bearing recordings through the same transactional session. Files stay
local. Silent bundles retain their existing looping presentation clock.

The 256 MiB decoded-PCM admission limit is independent of the 256 MiB bundle
input limit. Small soundtracks, up to 1 MiB of PCM, use the existing eager Web
Audio buffer and release the Wasm track after preparation. Larger tracks retain
one immutable Wasm PCM owner and copy bounded windows as playback advances.
They no longer allocate a second whole-soundtrack AudioBuffer.

The windowed path owns at most three device buffers of at most 65,536 sample
frames each, including interpolation padding: at most 1.5 MiB of application-
owned stereo device PCM. The first window is prepared before replacing the old
movie, without resuming the device. Each bridge copy is also bounded to 65,536
frames. Decoded pictures, retained Wasm PCM, input bytes, replacement candidates,
bridge scratch, garbage awaiting collection, and browser-internal device storage
are additional memory; this is not a process-RSS ceiling. Retained PCM is freed
once on replacement or disposal, not on pause or completion, so backward seeks
and restart remain available without decoding the recording again.

Windows target half a second, adjusted to whole cycles of both source and device
sample clocks and capped by the frame bound. Neighboring samples pad buffer
edges for browser resampling; device-clock stops prevent that padding from being
played twice. Fractional seek/resume phase is carried across windows rather than
reset at each node. A source/device rate ratio whose smallest common window
exceeds the bound is explicitly refused during preparation. Unsupported buffer
rates or device/allocation failures likewise reject the candidate; samples are
never silently reinterpreted at another rate.

Playback starts only from Play, after `AudioContext.resume()` succeeds. One audio
context clock selects the recorded picture indices, independent of monitor
refresh. The windowed path reserves 60 ms of scheduling headroom at start and
buffer recovery. Late presentation may skip display frames, not change the
recorded scene. When a render/UI stall exhausts lookahead, playback holds at the
first unplayed sample phase and resumes there when the next tick refills the
queue; elapsed device time is not treated as evidence that unqueued sound played.
No detached refill timer can restart a paused or replaced movie.

Pause keeps fractional audio time; resume does not jump back to the last
displayed picture. Scrubbing stops sound and selects an exact frame-grid offset.
A short track ends in silence while pictures finish. A long track holds the last
picture through its tail. Audible recordings stop at the longer duration;
pressing Play at the end restarts both from time zero.

Pause, seek, replacement and disposal invalidate pending resume operations.
A late resume cannot start old sound over the replacement. Preparation and
frame-zero rendering must both succeed before the previous player is released.
Failures preserve the previous movie; successful replacement stops and releases
its audio device. Page hiding pauses playback. A later refill failure stops all
queued sound and preserves the first unplayed position for an explicit retry.
The existing picture-only Rust constructor remains strict for consumers without
an audio adapter.

## Executable checks

```sh
node --test demo/wasm/*.test.mjs
python3 scripts/check_windowed_audio_browser.py --chromium /path/to/chromium
node scripts/check_browser_soundtrack.mjs /tmp/audible.fmtl
```

The windowed browser check requires Python Playwright and Chromium as test tools,
not runtime dependencies. It loads the production JavaScript module in memory,
without a local HTTP server or generated Wasm package. Nine real
OfflineAudioContext runs compare bounded-node output with a full-buffer browser
reference, including fractional seeks, stereo channel order, 8/44.1/48/192/384 kHz
source clocks, and different device rates. The deterministic PCM provider is a
fixture, not the native Wasm decoder. Missing browser tooling, an oversized
playback allocation, incorrect cleanup, or output differences above the declared
1e-5 sample tolerance fail the check. Browser resampling is not bit-certified.

The full bundle browser check requires the generated web-target bindings under
`demo/wasm/pkg`, Node Playwright and its Chromium installation, and the exact
native `export_audio_bundle` example above. `FMN_PLAYWRIGHT_MODULE` may name the
module file of an existing Playwright installation outside the repository.
These are test tools, not runtime or shipped Rust dependencies. The script
serves only an allowlist of demo assets on an ephemeral loopback port and closes
its browser and server on success or failure. Missing tooling or artifacts fail.

It compares all 168,000 transferred PCM samples with the fixture's canonical
bits, validates unchanged pictures, and uses a real OfflineAudioContext to
check 144,000 seeked output samples. It also drives the actual player controls:
user-gesture playback, fractional pause/resume, seek offsets, tail completion,
restart, failed replacement and device cleanup. This is functional browser
evidence, not the separate optimized npm size/release gate or hardware audio
latency certification.
