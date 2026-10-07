# Explicit video encoding quality

`RenderOptions::video_quality` now controls compressed video independently of
scene resolution, frame sampling, and Lumen's raw-frame renderer. It reaches
the existing negotiated `VideoJob` and `encode_argv` through the ordinary
streaming ffmpeg boundary. It is not an argv-rewriting process wrapper.

## Native Rust usage

```rust
use fmn::rendering::{EncoderPreset, EncoderTune, RenderFormat, RenderOptions, VideoQuality};

let mut options = RenderOptions::with_format("lecture.mp4", RenderFormat::Mp4)?;
options.video_quality = VideoQuality {
    crf: Some(16),
    preset: Some(EncoderPreset::Slow),
    tune: Some(EncoderTune::Animation),
    bitrate: None,
};
// Pass options to render(), render_with_fs(), render_camera(), or render_bundle().
// Those entry points share the native render sink and one encoder negotiation.
```

This is an explicit settings example, not a new default or a visual-quality
certificate. `VideoQuality::default()` sets every field to `None`: no `-crf`,
`-preset`, `-tune`, or `-b:v` is added and the existing encoder behavior remains.
The new controls do not fix a color-transfer/tag mismatch or establish HDR,
codec availability, cross-platform encoding identity, or a renderer speedup.

The configured codec remains `options.config.file_writer.video_codec`. CRF
accepts integers from 0 through 51 for `libx264`/`libx265`. Presets are the closed
catalog `ultrafast`, `superfast`, `veryfast`, `faster`, `fast`, `medium`, `slow`,
`slower`, `veryslow`, `placebo`. Tunes accept one catalog value; comma-separated
combinations and arbitrary encoder parameter strings are refused.

| Encoder | Admitted controls |
| --- | --- |
| `libx264` | CRF or target bitrate, preset, film/animation/grain/stillimage/psnr/ssim/fastdecode/zerolatency tune |
| `libx265` | CRF or target bitrate, preset, animation/grain/psnr/ssim/fastdecode/zerolatency tune |
| `h264_videotoolbox`, `hevc_videotoolbox`, `h264_nvenc`, `hevc_nvenc`, `av1_nvenc` | Target bitrate only; software CRF/preset/tune requests fail explicitly |

`bitrate: Some(12_000_000)` means twelve million **bits per second**, not bytes
and not a strict output-size ceiling. It is an alternative to CRF; supplying
both is an error. Artifact/input ceilings are still independently enforced by
[the native job-limit policy](NATIVE_VIDEO_LIMITS.md). Unsupported encoders,
zero bitrate, alpha-preserving MOV, and native PNG/GIF/Y4M reject explicit video
quality settings rather than quietly dropping them. Catalog string parsers
accept only exact canonical spellings, not raw argv or shell fragments.

## Shared negotiation and provenance

Lower-level callers use `VideoJob::with_quality`. Existing `VideoJob` struct
literals and its legacy `crf` field remain supported. A configured CRF and a
legacy CRF must agree; conflicting values fail rather than follow accidental
precedence. `EncoderChoice::Configured` binds a resolved encoder name to the
policy. Exhaustive matches over that public enum must handle the new variant;
consumers should use `resolved_encoder()` when they only need its name.

Validation is repeated at encoder resolution and argument generation, including
for callers who construct or mutate a configured enum directly. Encoder options
are emitted once, after the raw input and output codec and before the output
path. Frame dimensions/rate, pixel conversion, color description, mux stream
copy, private executable binding, and atomic no-clobber publication are unchanged.

A successful native video receipt includes `RenderArtifact::video_quality` and
the existing ffmpeg invocation provenance records the exact encoder argv. An
empty policy in a receipt means that the encoder's defaults were requested;
it does not infer a particular default CRF from the installed tool.

This tranche does **not** add CLI/config-file flags, Python keywords, or AAC
audio bitrate control. Those adapters must consume this same typed policy in
their own entry points; no second negotiation implementation is needed.

## Regression evidence

```sh
cargo test --locked -p fmn-output --lib negotiate::quality
cargo test --locked -p fmn-output --features ffmpeg-test-fixture --test video_quality_boundary
FMN_REQUIRE_FULL_INPUTS=1 cargo test --locked -p fmn --test native_video_quality -- --nocapture
```

The pure tests cover the complete CRF/preset/tune combinations, out-of-range
values, hardware refusals, conflicting rate modes, mutation revalidation and
unchanged default argv. The native fake executable records what the actual
streaming child received. The supported-host test renders MP4/MOV through the
native API, with and without a camera, then checks x264's user-data SEI inside
`mdat`: requested CRF, the slow preset's `subme=8`, and animation's `psy_rd=0.40`.
The bounded test reader exercises AVC emulation-prevention bytes and refuses
truncated NAL/box lengths; it does not mistake unrelated metadata text for SEI.
Only an executed passing test run is evidence for the Rust integration.
