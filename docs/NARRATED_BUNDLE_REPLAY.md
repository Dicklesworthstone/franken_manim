# Narrated FMTL replay

A sound-bearing FMTL recording contains final normalized PCM as well as its
recorded pictures and optional camera track. Replaying it never executes scene
code, opens the original WAV assets, resamples the stored audio, or mixes cues
again. Video encoding and audio muxing use the existing governed ffmpeg boundary.

## Standalone executable

The shipping `fmn` binary recognizes sound-bearing recordings through the existing
generated argument parser and configuration resolver:

```sh
fmn lecture.fmtl --format video --video_dir media/replay
fmn lecture.fmtl --format wav --video_dir media/audio
fmn lecture.fmtl --format video --subdivide --video_dir media/clips
fmn lecture.fmtl --format wav --subdivide --video_dir media/audio-clips
fmn lecture.fmtl --format video -n 2,5 --video_dir media/excerpt
fmn lecture.fmtl --format wav -n 2 --video_dir media/audio-excerpt
fmn lecture.fmtl --format video -n 2,5 --subdivide --video_dir media/selected-clips
```

`-w` also selects video. Opaque video is MP4; `--transparent` selects the existing
alpha-preserving MOV route. Video accepts the existing ffmpeg, codec, quality,
AAC-bitrate, thread and resource-limit options. An incompatible explicit alpha
codec or wire format is refused, never silently replaced. WAV output requires no
ffmpeg capability and retains the recorded sample rate and channel count, using
the native canonical signed-16-bit WAV publisher without dither. This output
quantization is explicit; it is not a claim of Float32 WAV byte preservation.

The recording's FPS is authoritative. A conflicting `--fps` refuses. Recorded
camera poses, lighting and backgrounds remain attached to their global frames.
A picture-only output request refuses instead of silently dropping narration.

## Selecting an excerpt

`-n START,END` selects zero-based recorded play/wait segment ordinals, with END
exclusive, exactly as the generated CLI's `AnimationRange` specifies. `-n START`
selects from START through the final recorded segment. It does not count picture
frames or seconds. Empty, reversed and out-of-bounds selections refuse rather
than clamping; an interval containing no captured frames cannot publish an output.
The stored segment frame ranges determine both video and sound boundaries.
No skipped source code or elapsed picture prefix is executed.

Without `--file_name`, an excerpt gains the ordinary `_START_END` or `_START`
suffix. An explicit `--file_name` retains its requested stem. With `--subdivide`,
only selected segment outputs are preflighted and published, and their filenames
and robot `subdivision` values retain the original ordinals (for example,
`lecture_2_5/00002.mp4` through `00004.mp4`). An unselected zero-frame segment does
not block an excerpt. A selected zero-frame segment refuses subdivision because
it cannot form an output generation; it may lie inside a combined nonempty cut.
The frame and output-generation limits charge the selected workload, not its
unrendered prefix. Input decoding and immutable snapshot storage remain bounded
by the separate production bundle byte ceiling.

Full replay without `-n` preserves soundtrack tails and pads short soundtracks
with silence to the picture duration. Every explicit `-n` selection is a cut,
including `-n 0` and an explicit range covering all segments: audio beyond its
last picture boundary is trimmed. `--subdivide` always makes exact cuts.
Each clip starts its output sequence at zero and cuts PCM at independently
rounded absolute start/end boundaries. At 10 audio samples/second and 3 picture
frames/second, adjacent one-frame clips have 3, 4 and 3 sample frames, not 3 each.

Video subdivision now decodes and freezes the recording once for the entire
invocation. All outputs borrow the same immutable pictures, recorded camera
track and retained PCM; each owns only its render pipeline and selected sample
window. The source file is never reopened and the whole snapshot table is never
re-decoded per clip. This describes eliminated work, not a measured speedup.

Every destination is preflighted before publication. Existing files, dangling
links and non-directory/link parents are refused. Native publication remains
atomic and create-only, so a competing writer cannot be overwritten after that
advisory preflight. Earlier published clips survive a later failure.

`--robot` emits `fmn.cli` records with kind `bundle-audio-render`, source/output
hashes, original `source_frames` and `source_segments` half-open ranges, and
soundtrack rate/channel/sample counts. Video records include actual ffmpeg
invocation provenance, effective resource limits and joined frame-pipeline
counters. `--prerun` reports the selected stored schedule and its original ranges;
it never runs a Scene. `--quiet` suppresses human success decoration, not robot
records or errors.

This executable adapter is standard-mode only. It reports `certified: false`
and `manifest: null`; full FMNP input-closure manifests and `--reproducible` are
not implemented here. `fmn batch` and Studio audio transport remain separate
work. The `fmn_cli::run_os` library entry itself is unchanged; the shipping
executable owns this adapter, as it owns its Studio project adapter. Invalid
files and picture-only recordings retain the existing CLI route. No-cue Python
exports remain picture-only and therefore use that route, including its existing
range restrictions.

## Native frame-range and reusable-input APIs

`RenderOptions` exposes `render_bundle_range(bytes, start..end)` and
`render_bundle_range_with_fs(bytes, start..end, fs)`. These ranges count picture
frames, unlike CLI `-n` segment ordinals. They are zero-based, half-open, nonempty
and checked before output creation. Only selected original frames reconstruct.
A PNG samples the selected interval's last recorded frame; attached sound still
requires video output.

A host that has decoded and inspected a recording can transfer it once through
`render_loaded_bundle_with_fs(bundle, selection, fs)`. `None` selects ordinary
full replay including audio tails. `Some(0..frame_count)` explicitly trims them.
Video requires an explicit `FfmpegCapability` on these native entry points.

For many outputs, take `bundle.take_soundtrack()` and consume its pictures with
`bundle.into_shared()` once. Call
`options.render_shared_bundle_with_fs(&pictures, soundtrack.as_ref(), selection, fs)`
for each destination. Both inputs remain usable after success or failure and can
be borrowed by concurrent independent renders. No source-file access or complete
recording decode occurs during those renders. Supplying audio to a picture-only
format still refuses; explicitly omitting the extracted soundtrack is the host's
choice, not a certification that independently supplied inputs belong together.

The scene report retains the absolute ending clock frame; a range counts the
intersected nonempty recorded segments and marks an ending before the recording's
end as early. The artifact counts only emitted frames. Loaded bundle and
worker-local geometry are additional to planned pixel/sink resident storage;
PCM windows retain the production bundle byte ceiling.

## Focused validation targets

```sh
cargo test --locked -p fmn --lib rendering::compiled::tests
cargo test --locked -p fmn --test compiled_ranges
cargo test --locked -p fmn-cli --features cli,batch --bin fmn bundle_audio::tests
FMN_REQUIRE_FULL_INPUTS=1 cargo test --locked -p fmn-cli --features cli,batch --test narrated_bundle_cli
```

The final command requires real ffmpeg. It exercises the shipping executable,
WAV excerpts with an empty PATH, decoded video frames and audio, and exact
lossless-alpha frame slicing. Source review or compiling tests alone does not
establish an executed media acceptance result.
