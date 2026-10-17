# Long-form native video: explicit ffmpeg budgets

`RenderOptions::ffmpeg_limits` controls the existing D2 boundary for native
MP4/MOV export. It is a `fmn::rendering::JobLimits`, not a second process
mechanism. Ordinary scenes, live/fixed camera rendering, and compiled FMTL
replay all enter the same `RenderSink` and use this policy.

The encoder's wall clock includes time spent constructing and rendering the
scene while its stdin remains open. It is **not** the movie's duration, the
encoder's CPU time, or an idle timeout. Native render requests therefore default
to a finite 24-hour allowance per invocation instead of the low-level boundary's
600-second short-job default. The low-level `JobLimits::default()` and tool
probe deadlines remain unchanged. A service should explicitly select its own
smaller per-request allowance; an overnight render can select a larger one.

```rust,no_run
use std::time::Duration;
use fmn::rendering::{RenderFormat, RenderOptions};

let mut options = RenderOptions::with_format("lecture.mp4", RenderFormat::Mp4)?;
options.ffmpeg_limits.timeout = Duration::from_secs(6 * 60 * 60);
options.ffmpeg_limits.max_log_bytes = 2 * 1024 * 1024;
options.ffmpeg_limits.max_artifact_bytes = 16 * 1024 * 1024 * 1024;
options.max_output_bytes = 256 * 1024 * 1024 * 1024;
// Supply options.ffmpeg explicitly, or use render() with the ffmpeg feature.
# Ok::<(), fmn_config::ConfigError>(())
```

The admitted artifact cap is the **minimum** of
`ffmpeg_limits.max_artifact_bytes` and `max_output_bytes`. The latter also
independently caps cumulative raw-frame input, which may be much larger than
the compressed movie. Neither cap silently widens the other. Log capture stays
bounded separately per stdout/stderr stream. Defaults remain 8 GiB per artifact,
1 MiB per log stream, and 64 GiB of raw input. Set both byte budgets deliberately
for large or long renders; increasing the timeout alone does not disable them.

One admitted policy reaches video encoding, the second-stage audio mux, and
non-WAV sound decoding. The audio decoder retains its tighter format-specific
limits. Process-tree cancellation, private executable/workdir binding,
no-clobber publication, and refusal of certified video remain unchanged. Zero
bounds and unrepresentable monotonic deadlines fail before tool discovery or
scene execution. PNG/GIF/Y4M never consult the irrelevant ffmpeg settings.

A successful `RenderArtifact` exposes `ffmpeg_limits: Option<FfmpegLimitsReport>`.
Its `to_json()` returns a deterministic object suitable for embedding in the
host's artifact/provenance manifest, with duration represented exactly as
seconds plus subsecond nanoseconds. This is an admitted-policy receipt, not an
input-closure certificate or an automatically published sidecar. The ordinary
ffmpeg invocation records still carry the tool hash and exact argv.

## Standalone CLI

The standalone `fmn` command uses the same 24-hour default and admission policy
for its streaming video sink. The encoder watchdog includes frame construction
and rendering time. Set explicit bounds for long jobs:

```sh
fmn --format video --ffmpeg-timeout 21600 \
  --ffmpeg-max-artifact-bytes 17179869184 \
  --ffmpeg-max-input-bytes 274877906944 \
  --crf 16 --preset slow --video_dir videos @builtin circle_shift.v1
```

| Flag | Unit | Default |
| --- | --- | --- |
| `--ffmpeg-timeout` | Whole wall-clock seconds per encode | 86,400 (24 hours) |
| `--ffmpeg-max-log-bytes` | Captured bytes per stdout/stderr stream | 1,048,576 (1 MiB) |
| `--ffmpeg-max-artifact-bytes` | Encoded artifact bytes | 8,589,934,592 (8 GiB) |
| `--ffmpeg-max-input-bytes` | Cumulative raw-frame bytes | 68,719,476,736 (64 GiB) |

All values are positive integers. Any of these flags selects video output and
implies durable output; explicit native formats and `--gif` reject them. A
timeout outside the host monotonic clock's range fails before tool discovery.
The artifact ceiling is the smaller of the requested artifact and input caps.
Frame-count and resident-memory guards remain separate, so raising a byte cap
permits a longer stream without retaining its frames in memory.

The `--robot` render record contains `ffmpeg_limits` with the effective policy.
The same stable policy enters the video manifest's C9 capability record. Native
artifact manifests retain their existing contract. Flags travel with native
scene, compiled FMTL, batch, and subdivision video requests.

## Python portal

`fmn-python` uses the same finite 24-hour default and shared native admission
function. It accepts the same four flag spellings for MP4/MOV, including
transparent MOV, and for compressed sound decoding during WAV output:

```sh
fmn-python lecture.py Lecture --format mp4 --ffmpeg-timeout 21600 \
  --ffmpeg-max-artifact-bytes 17179869184 \
  --ffmpeg-max-input-bytes 274877906944 --video_dir lecture.mp4
```

The corresponding optional attributes on `scene.file_writer` are
`ffmpeg_timeout` (whole seconds), `ffmpeg_max_log_bytes`,
`ffmpeg_max_artifact_bytes`, and `ffmpeg_max_input_bytes`. Set them before
starting `render_scene`, a `render_session`, or a live recording. Missing or
`None` attributes select defaults. Positive integers are required; booleans,
fractional seconds, zero, and deadlines beyond the monotonic clock range fail
before tool discovery or output reservation. Authored descriptor exceptions
propagate, and configuration getters cannot steal another output generation.

The portal retains its existing maximum of 1,000,000 frames and derives its
default raw-input ceiling from that count and the actual negotiated frame
layout. An explicit input cap intersects that bound; it does not widen the
frame-count guard. This preserves long-form and high-resolution input capacity.
The artifact cap defaults to 8 GiB and is intersected with the admitted input
ceiling. WAV/compressed-audio decoding additionally retains its native 64 MiB
source and decoded-sample budgets. For WAV output, compressed-source bytes
and decoded PCM artifact bytes remain independent: a small compressed source
is allowed to expand up to the decoded-sample and artifact ceilings.

One admitted policy reaches the live encoder, lazy compressed sound decodes,
and the final stream-copy audio mux. The same options travel through named
batches, `--write_all`, subdivision, paired video/PNG export, and checkpoint
receipts. Every successful video/WAV `RenderResult` exposes `ffmpeg_limits`;
`as_dict()` and robot output include the same exact policy. During a generation,
`scene._render_ffmpeg_limits` also makes the admitted bounds available for
diagnosing a timeout. PNG, GIF, Y4M, SVG, and FMTL requests reject explicit
ffmpeg resource flags. Tool probes keep their separate short deadlines.

The shared Rust functions `fmn_output::default_render_ffmpeg_limits()` and
`fmn_output::admit_render_ffmpeg_limits()` let every front door use the same
defaults, validation, and cap intersection. Existing facade paths and the
`RenderOptions` method remain available and delegate to the same policy.

The process/negotiation rules are specified in [FFMPEG_PROTOCOL.md](FFMPEG_PROTOCOL.md).

Regression entry points:

```sh
cargo test --locked -p fmn --lib rendering::ffmpeg_limits
cargo test --locked -p fmn --test native_video_limits
cargo test --locked -p fmn-cli --lib ffmpeg_limit
cargo test --locked -p fmn-conformance --test e2e_scenarios cli_ffmpeg_limit_admission_scenario_passes
python crates/fmn-python/tests/video_cli_unit.py
FMN_REQUIRE_FFMPEG=1 cargo test --locked -p fmn-python --lib portal_video_quality -- --test-threads=1
```

The real encode/mux checks require a supported host ffmpeg and honor
`FMN_REQUIRE_FULL_INPUTS=1`; admission and native-codec checks do not need it.
The CLI process-boundary regression records the actual streaming `ProcessSpec`
and stdin budget through a scripted process capability, then verifies that its
intentional start failure publishes neither video nor a sidecar. It does not
claim to measure a real encoder's elapsed timeout.
The Python extension suite exercises the real encoder: a one-second allowance
fails during a delayed scene with no publication, and the same scene succeeds
with a sixty-second allowance. It also checks cumulative-input and artifact
refusals, compressed-audio expansion caps, and resource receipts across console
output owners.
