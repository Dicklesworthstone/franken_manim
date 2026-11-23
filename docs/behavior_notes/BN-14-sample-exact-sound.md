# BN-14 — Sound is placed on the sample grid, not a millisecond clock

**Status:** Draft. Landed in W8 (fm-0m7); becomes Final when G4's complete
native output pipeline and A/V corpus pass.

## What changed

Classic manim hands sound to pydub at `int(1000 * time)` milliseconds. That
truncates every placement to a millisecond, accumulates a separate audio clock
beside the frame clock, and raises when the resulting position is negative.
The selected pydub/ffmpeg resampler and quantizer are also host-installation
details rather than scene provenance.

FrankenManim keeps the `Scene.add_sound(sound_file, time_offset=0, gain=None,
gain_to_background=None)` surface, but Reel derives the placement directly from
Choreo's rational frame time:

```text
nearest_sample(frame * sample_rate / fps) + nearest_sample(time_offset)
```

Both roundings are exact over the received integers/f64, with ties away from
zero. There is no accumulated audio time. A pre-zero start clips the prefix at
sample zero; an overlay past the current end extends the output.

PCM conversion uses the named `blackman-windowed-sinc-64` resampler. It is a
64-tap Blackman-windowed sinc evaluated from the exact output-index/input-rate
ratio, with a measured contract of at most 0.1 dB passband loss through 80% of
the lower Nyquist frequency and at least 60 dB rejection from 150% onward.
Mono duplicates to stereo; stereo averages to mono. Explicit multichannel
WAV layouts use the fixed matrix below. Missing or unsupported speaker layouts
are named refusals rather than guessed.

## Explicit surround sources

Native `WAVE_FORMAT_EXTENSIBLE` PCM and floating-point WAV retain their speaker
mask in `WavAudio::channel_mask`. `ChannelLayout` admits mono, stereo, 2.1,
three-channel surround, quad, four-channel surround, 5.0 with back or side
speakers, 5.1 with back or side speakers, 6.1, and 7.1. The channel count must
match the mask. A missing or zero mask is accepted only for conventional mono
or stereo. Other masks remain readable metadata but are refused by native
sound admission and mixing with a typed unsupported-layout error.

Interleaved channels follow the ascending speaker-bit order of the WAV mask.
Each input frame is accumulated in that order using `f64` and this matrix:

| Speaker | Left | Right |
| --- | ---: | ---: |
| Front left | 1 | 0 |
| Front right | 0 | 1 |
| Front center | `1 / sqrt(2)` | `1 / sqrt(2)` |
| Back left or side left | `1 / sqrt(2)` | 0 |
| Back right or side right | 0 | `1 / sqrt(2)` |
| Back center | 0.5 | 0.5 |
| Low-frequency effects (LFE) | 0 | 0 |

Mono output averages the resulting left and right samples. LFE is omitted:
this matrix does not invent a crossover or bass-management system. Channel
conversion precedes resampling, cue gain, and background ducking. There is no
intermediate normalization or clipping; only the existing final output clamp
applies, and its clipping counter remains observable. Existing mono/stereo
conversion keeps its original arithmetic and bytes.

`encode_wav_with_layout` preserves explicit masks in a 68-byte extensible WAV
header. `SoundData` also preserves surround masks when canonicalizing sources
to F32 WAV, and its byte budget includes that header. Explicit conventional
mono/stereo masks canonicalize to the existing implicit representation.
Normalized multichannel PCM identities include the mask under a v2 hash
domain, so equal sample numbers with different speaker assignments do not
collide. Existing mono/stereo identities retain their v1 domain.

File sources, Rust memory sources, Python `SoundData.from_wav_bytes`, native
WAV rendering, soundtrack windows, and portable bundles share this path.
Python's raw-sample `SoundData(samples, ...)` constructor remains mono/stereo;
use an explicitly labelled WAV for surround input. Output remains mono/stereo.

## What stayed familiar

- `sound_file` is found as the Reference's `get_full_sound_file_path` finds it:
  the name itself, else the configured `sounds` directory (custom_config.yml
  `directories`) with `.wav`, `.mp3` or no extension. A URL refuses at the
  call, toward the host AssetFetcher. One difference: the Reference raised
  "<name> not Found" at the call, while a name the lookup cannot find is
  recorded as authored here, and the mix refuses it by name. Cues are not read
  until they are mixed.
- `gain` is foreground gain in dB.
- `gain_to_background` is dB ducking applied to the already-mixed background
  during the new sound's active interval.
- Sounds mix in `add_sound` call order, so ducking and overlay order remain
  observable in the same direction as the Reference.
- The mix extends when a sound runs past its previous end.

## Defined output policy

Every sample sees the same cue order. Worker count only partitions disjoint
timeline ranges, so `{1,4,16}` workers produce identical PCM and WAV bytes.
Final over-range samples clamp to `[-1, 1]`; `MixReport.clipped_samples` is the
warning counter. Certified s16 WAV defaults to no dither. The optional TPDF
dither is seeded and counter-derived per sample, so it is reproducible and
schedule-independent. Native f32 WAV is not dithered.

Non-WAV assets decode through the fingerprinted ffmpeg capability and fail with
the named native-alternative capability error when ffmpeg is absent. That
decoded product, like every ffmpeg product, is outside certification; the
native mix and certified WAV artifact are inside it.

## Native in-memory sources

Rust callers can freeze synthesized or decoded PCM with
`fmn_scene::SoundData::from_audio`, or admit existing native WAV bytes with
`SoundData::from_wav_bytes`. Both constructors take `SoundDataLimits` and
validate channels, sample alignment, finite values, and byte/sample budgets
before retaining data. The result stores canonical F32 WAV bytes and their
SHA-256; cloning a sound shares immutable storage.

Queue the result with `Scene::add_sound_data`, using the same offset, gain,
and background-ducking arguments as `add_sound`. The typed
`Scene::add_sound_source` entry point also accepts
`SoundSource::File` or `SoundSource::Memory`. Native Studio builders accept
both through `add_sound_source`.

Python callers import `SoundData` from `fmn_python` and pass it directly to
`Scene.add_sound`. `SoundData(samples, sample_rate=48000, channels=1)` freezes a
finite, indexable sequence of interleaved floating-point PCM; flatten a stereo
NumPy array before passing it. `SoundData.from_wav_bytes(data)` admits native
PCM WAV bytes. Both accept smaller `max_samples` and `max_bytes` budgets, with
hard ceilings of 16,777,216 interleaved samples and 64 MiB. Source data and
metadata are immutable after construction. `wav_bytes()`, `sha256`, `channels`,
`sample_rate`, `sample_frames`, and `duration_seconds` expose the frozen asset.
This extension leaves the Reference's `manimlib` wildcard namespace unchanged.

The Python writer also accepts native sound: `SceneFileWriter.add_sound` accepts
a path or `SoundData`, and `add_audio_segment` accepts `SoundData`. An explicit
`time` is an absolute scene timestamp; omitting it appends at the current native
soundtrack's end. `audio_segment` lazily returns an immutable `SoundData` snapshot
of the shared scene cue queue, and `includes_sound` observes that queue directly.
Reading the snapshot can decode queued file assets, but never advances the scene
clock or opens a render generation. Writer snapshots have the same 64 MiB ceiling,
plus cumulative preparation limits and a 4096-cue limit.
Appending queries native cue placement and duration metadata without allocating
the mixed waveform, so a long soundtrack is not constrained by the snapshot's
64 MiB ceiling. Memory assets require no decode for this query; distinct file
assets are decoded once per query within the usual asset limits. Empty sources
do not extend the soundtrack even when scheduled in the future.

The append end is computed on the canonical 48 kHz writer grid and retained as
an exact rational position. Changing the output rate converts that position
using integer arithmetic with ties away from zero. For example, an end at
400/48000 seconds becomes sample 368 at 44.1 kHz; converting through a float
would put this half-sample tie one sample early. This is a fixed writer-grid
policy, not deferred concatenation at each output rate. Source durations round
up when resampled: a one-sample 44.1 kHz source occupies two 48 kHz writer frames,
and an appended successor begins at sample two when exporting at 44.1 kHz.

Absolute timestamps are retained separately from frame-relative offsets, then
rounded once onto the receiving sample grid. Explicit caller-supplied seconds
retain their exact received f64 value; they are not replaced by an inferred
rational number. Native callers can preserve a known grid position through
`Scene::add_sound_source_at_rational`. A live insert subtracts its sample
origin; observed skipped intervals remove their independently rounded sample
windows. A cue overlapping the beginning of an insert retains its audible suffix.
This does not change the existing two-rounding rule for `Scene.add_sound`.

`init_audio` resets the queued cues; `create_audio_segment` resets them and makes
an empty native PCM snapshot observable. A new native soundtrack starts at zero,
without pydub's implicit one-second silent placeholder. Resetting a nonempty queue
during an active output generation is refused so recording boundaries cannot lose
accepted cues. Existing frozen snapshots remain valid after a reset. pydub objects
and pydub-specific processing methods remain outside the portal; convert their
PCM explicitly into `SoundData`. Video muxing remains owned by native publication.

Memory cues go through the existing native decoder and mixer without reading
a source file or invoking ffmpeg. They retain the rational call-site clock,
skip no-op, insertion order, negative-offset clipping, and resampling rules
above. Standalone WAV, native video, soundtrack windows, and portable native
bundle exports consume the same typed requests.

`SoundtrackInput::source` identifies the source explicitly. Use
`source.file_path()` when a real file is required; memory sources return
`None`. Their `memory:sha256:…` identity is a provenance label and is never
opened as a path. File paths and memory digests have distinct cache keys.
Repeated memory cues share retained source bytes and decode receipts, while
each cue still counts toward prepared-mix storage. The cumulative decoder
sample cap is enforced before the decoder allocates.

## Native video composition

The native Rust rendering API and standalone CLI use the same
`PreparedSceneSoundtrack` composer. It reads each authored file source once
under the asset byte cap, or retains the canonical bytes of a memory source,
and shares that immutable snapshot across all uses of the cue. Native WAV
decoding, channel conversion, resampling, gain, ducking and output quantization
remain Reel's operations. Other supported
containers use the injected video boundary for decoding, including its
ffmpeg fingerprint, timeout, log cap and artifact cap.

Ordinary live video runs the scene once. Lifecycle events record omitted
frame intervals; sound call-site frames are rebased onto the emitted video
timeline before the relative offset is applied. `add_sound` calls made while
skipping remain an early no-op, so their paths are not read. The mix spans at
least the emitted picture and retains a longer cue tail.

The CLI's `--subdivide` already performs a deterministic counting pass for
its compiled-in native programs. It also collects that pass's complete cue
list and decodes it once before starting the clips. Each clip receives the
corresponding half-open absolute sample window of that whole-scene mix:
background phase and ducking are preserved, cue tails stop at the clip end,
and a cue authored later with a negative offset can affect an earlier clip.
The start and end scene positions are rounded to samples independently, so
adjacent clips neither lose nor repeat a rounding sample. This is offline
whole-scene composition; the portal's live per-call clips see the cues
available when their own recording boundary closes.

The completed native PCM is supplied before the video sink prepares its
output. The existing second ffmpeg stage uses `-c:v copy -c:a aac`; audio
decode, mix, or mux failure prevents publication of the failing video
generation. Encoded video remains outside certification.

CLI robot records include `soundtrack` (cue count, sample frames, rate,
channels, clipping), `audio_inputs` (source and normalized PCM SHA-256),
`ffmpeg_invocations` (encode, then optional mux), and `audio_decodes`.
Decoder receipts are separate because decoding can overlap a streaming
video encoder. C1 binds each source path once even when several cues reuse
it; C9 binds the audio identities and all media invocation receipts. Native
Rust `RenderArtifact` likewise separates `audio_decodes` from its existing
encode/mux `ffmpeg` list.

## Migration guidance

- Code that expected integer-millisecond placement should remove that
  assumption. At 48 kHz, FrankenManim addresses all 48 samples within each
  millisecond.
- A negative effective start no longer raises. The inaudible pre-zero prefix is
  discarded and the remainder starts at sample zero.
- Do not rely on a host pydub/ffmpeg resampler, quantizer, or channel guess.
  Compare native WAV semantics, not those installation-dependent bytes.
- If clipping is intentional, inspect and acknowledge the clipping counter;
  otherwise reduce cue gains. Wraparound is never available.

## Evidence

- `crates/fmn-scene/src/sound.rs`: immutable PCM admission, canonical identity,
  and distinct file/memory cache keys.
- `crates/fmn-scene/src/runtime.rs`: the four-argument request surface, exact
  `RationalTime`, separately retained absolute timestamps, skip no-op, and
  insertion-ordered request queue.
- `crates/fmn-python/src/portal_audio.rs`, `tests/audio_inputs.py`: immutable
  Python PCM, bounded admission, native writer snapshots, absolute/append timing,
  live overlapping inserts, skipped windows, and file/memory bundle parity.
- `crates/fmn-output/src/sound.rs`: placement arithmetic, resampler, channel
  matrix, gain/duck order, clipping, dithering, WAV golden, quality bounds, and
  thread-count equivalence.
- `crates/fmn-codec/tests/codec2.rs`: independent extensible WAV fixtures,
  complete subtype validation, speaker-mask preservation, and legacy encoding.
- `crates/fmn-output/tests/audio.rs`, `tests/sound_source_edits.rs`: all
  admitted layouts, per-speaker matrix oracles, source edits, layout identity,
  native-only decoding, and scalar/compiled mixer equivalence.
- `crates/fmn/tests/native_soundtrack.rs`, `tests/soundtrack_bundle.rs`,
  and `tests/support/surround_audio.rs`: independent 5.1/7.1 fixtures through
  public file/memory rendering and bundles, exact source budgets, shared
  decode receipts, and output bytes at `{1,4,16}` workers.
- `crates/fmn-conformance/tests/e2e_scenarios.rs`,
  `render_matrix.native_surround_soundtrack.v1`: public surround-source
  rendering, bundle parity, and malformed-layout refusal.
- `crates/fmn-output/src/ffmpeg.rs` and `tests/boundary.rs`: sandboxed non-WAV
  decode and fake-ffmpeg capability proof.
- `crates/fmn-conformance/tests/sound_pipeline.rs`: frame-N click placement and
  identical certified WAV bytes at `{1,4,16}` workers.
- `crates/fmn/src/rendering/soundtrack.rs`: shared source snapshots, skipped
  time, offsets, and rational-window parity with resampling and ducking.
- `crates/fmn-cli/src/lib.rs`, `tests::video_soundtrack`: the live CLI render,
  deferred mux, subdivision windows, compressed input receipts, and failure
  publication contract through a scripted process capability.
- `crates/fmn-conformance/tests/e2e_scenarios.rs`,
  `lifecycle.scene_soundtrack_windows.v1`: real scene clock and skip events
  through the shared audio composer and adjacent sample windows.
- `crates/fmn-conformance/tests/e2e_scenarios.rs`,
  `lifecycle.portal_asset_root.v1` (`crates/fmn-python/tests/asset_root.py`):
  a named cue found in the configured `sounds` directory reaches the mix, and
  a missing or network name refuses by name.
- Pinned Reference
  `manimlib/scene/scene.py::add_sound` and
  `manimlib/scene/scene_file_writer.py::add_sound`.
