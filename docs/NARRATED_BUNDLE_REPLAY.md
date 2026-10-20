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

Full replay preserves soundtrack tails and pads short soundtracks with silence
to the picture duration. `--subdivide` uses the recorded nonempty segment ranges.
Each clip starts its output sequence at zero and cuts PCM at independently
rounded absolute start/end boundaries. For example, at 10 audio samples/second
and 3 picture frames/second, three adjacent one-frame clips have 3, 4 and 3 audio
sample frames, not 3 each. Subdivision intentionally trims any tail beyond the
last picture boundary. Video subdivision currently decodes the already-read
recording bytes for each clip; it does not reopen the recording or its source
media, and it makes no decoder-speedup claim.

Every destination is preflighted before publication. Existing files, dangling
links and non-directory/link parents are refused. Native publication remains
atomic and create-only, so a competing writer cannot be overwritten after that
advisory preflight. Earlier published clips survive a later failure.

`--robot` emits `fmn.cli` records with kind `bundle-audio-render`, source/output
hashes, original frame range, and soundtrack rate/channel/sample counts. Video
records include actual ffmpeg invocation provenance, effective resource limits
and joined frame-pipeline counters. `--prerun` reports the stored schedule; it
never runs a Scene. `--quiet` suppresses human success decoration, not robot
records or errors.

This executable adapter is standard-mode only. It reports `certified: false`
and `manifest: null`; full FMNP input-closure manifests and `--reproducible` are
not implemented here. `fmn batch`, Studio audio transport and CLI `-n` range
selection remain separate work. The `fmn_cli::run_os` library entry itself is
unchanged; the shipping executable owns this adapter, as it owns its Studio
project adapter. Invalid files and picture-only recordings retain the existing
CLI route. No-cue Python exports remain picture-only and therefore use that route.

## Native frame-range API

`RenderOptions` exposes `render_bundle_range(bytes, start..end)` and
`render_bundle_range_with_fs(bytes, start..end, fs)`. Ranges are zero-based,
half-open, nonempty and checked before output creation. Only selected original
frame indices are reconstructed, without replaying the elapsed prefix. The
selected frame count is checked against `max_frames`. A PNG samples the selected
interval's last recorded frame; attached sound still requires video output.

A host that has already decoded and inspected a recording can transfer it once
through `render_loaded_bundle_with_fs(bundle, selection, fs)`. `None` selects
ordinary full replay including audio tails. `Some(0..frame_count)` is an explicit
cut and therefore trims the tail, unlike `None`. Video requires an explicit
`FfmpegCapability` on these native entry points.

The scene report retains the absolute ending clock frame; a range counts the
intersected nonempty recorded segments and marks an ending before the recording's
end as early. The artifact report counts only emitted frames. Loaded bundle and
worker-local geometry are additional to planned pixel/sink resident storage;
PCM windows retain the production bundle byte ceiling.

## Focused validation targets

```sh
cargo test --locked -p fmn --lib rendering::compiled::tests
cargo test --locked -p fmn-cli --features cli,batch --bin fmn bundle_audio::tests
```

These targets exercise exact PCM boundaries, tails, native WAV publication,
subdivision, no-clobber preflight and refusal/receipt behavior. Real video encoding
and the packaged executable require the normal native integration environment;
source review or compiling these tests is not an executed MP4 acceptance result.
