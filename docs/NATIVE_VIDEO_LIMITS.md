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

The process/negotiation rules are specified in [FFMPEG_PROTOCOL.md](FFMPEG_PROTOCOL.md).
This native Rust option does not by itself add a CLI flag or Python keyword.

Regression entry points:

```sh
cargo test --locked -p fmn --lib rendering::ffmpeg_limits
cargo test --locked -p fmn --test native_video_limits
```

The real encode/mux checks require a supported host ffmpeg and honor
`FMN_REQUIRE_FULL_INPUTS=1`; admission and native-codec checks do not need it.
