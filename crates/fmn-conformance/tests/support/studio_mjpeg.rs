//! One real capture through the governed MJPEG boundary and shared preview hub.
//! The host-native ffmpeg fixture supplies a known JPEG; this is a contract and
//! failure-atomicity scenario, not encoder quality or performance evidence.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fmn_output::{Boundary, FfmpegTool, InvocationReport, JobLimits};
use fmn_platform::process::{FfmpegLocator, ProcessRunner, StdFfmpegLocator, StdProcessRunner};
use fmn_studio::host::JpegPreviewEncoder;
use fmn_studio::{FrameHub, ProtocolLimits};

use super::{LogEvent, RunCtx, RunOutcome, ScenarioError, fail, scenario_dir};

struct Encoder {
    boundary: Boundary,
    receipt: Mutex<Option<InvocationReport>>,
}

impl std::fmt::Debug for Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("E2eMjpegEncoder").finish_non_exhaustive()
    }
}

impl JpegPreviewEncoder for Encoder {
    fn encode(
        &self,
        width: u32,
        height: u32,
        rgba: &[u8],
        max_bytes: usize,
    ) -> Result<Vec<u8>, String> {
        let encoded = self
            .boundary
            .encode_mjpeg_preview(width, height, rgba, max_bytes)
            .map_err(|error| error.to_string())?;
        *self.receipt.lock().map_err(|error| error.to_string())? = Some(encoded.invocation);
        Ok(encoded.jpeg)
    }
}

pub(super) fn run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let fmn_cli::Invocation::Studio(command) = fmn_cli::parse_args([
        "studio",
        "--no-browser",
        "--preview-codec",
        "mjpeg",
        "--resolution",
        "64x48",
        "--fps",
        "8",
        "--threads",
        "1",
        fmn_cli::BUILTIN_SCENE_SOURCE,
        "circle_shift.v1",
    ])
    .map_err(|error| fail(error.to_string()))?
    else {
        return Err(fail("MJPEG scenario did not parse as Studio"));
    };
    let input = fmn_cli::compose_studio_preview_frame(&fmn_platform::fs::StdFs, &command, 1)
        .map_err(|error| fail(error.to_string()))?;
    let directory = scenario_dir("studio_mjpeg")?;
    let mode_path = directory.join(".fmn-native-ffmpeg-mode");
    std::fs::write(&mode_path, "mjpeg-preview").map_err(|error| fail(error.to_string()))?;
    let executable = StdFfmpegLocator::default()
        .locate_ffmpeg(Path::new(env!(
            "CARGO_BIN_FILE_FMN_OUTPUT_fmn-ffmpeg-test-fixture"
        )))
        .map_err(|error| fail(error.to_string()))?;
    let runner: Arc<dyn ProcessRunner> = Arc::new(StdProcessRunner);
    let tool = FfmpegTool::resolve(executable, runner.as_ref(), &directory)
        .map_err(|error| fail(error.to_string()))?;
    let boundary = Boundary::new(
        tool,
        runner,
        JobLimits {
            timeout: Duration::from_secs(2),
            max_log_bytes: 8192,
            max_artifact_bytes: 65_536 + 1024,
            keep_workdir: false,
        },
        directory.clone(),
    )
    .map_err(|error| fail(error.to_string()))?;
    let encoder = Arc::new(Encoder {
        boundary,
        receipt: Mutex::new(None),
    });
    let hub = FrameHub::new(2, 65_536)
        .and_then(|hub| hub.with_jpeg_encoder(encoder.clone()))
        .map_err(|error| fail(error.to_string()))?;
    let first = hub
        .publish(&input, ProtocolLimits::default())
        .map_err(|error| fail(error.to_string()))?;
    let mut multipart = Vec::new();
    FrameHub::write_multipart_part(&mut multipart, &first)
        .map_err(|error| fail(error.to_string()))?;
    let header_end = multipart
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .ok_or_else(|| fail("MJPEG part has no header boundary"))?
        + 4;
    let headers =
        std::str::from_utf8(&multipart[..header_end]).map_err(|error| fail(error.to_string()))?;
    let jpeg = first.stream_bytes();
    let decoded = fmn_codec::decode_jpeg(jpeg, &fmn_codec::JpegLimits { max_pixels: 3072 })
        .map_err(|error| fail(error.to_string()))?;
    if first.stream_content_type() != "image/jpeg"
        || !headers.contains("Content-Type: image/jpeg\r\n")
        || !headers.contains(&format!("Content-Length: {}\r\n", jpeg.len()))
        || !headers.contains(&format!(
            "X-FMN-SHA256: {}\r\n",
            first.stream_digest().to_hex()
        ))
        || !headers.contains(&format!("X-FMN-PNG-Bytes: {}\r\n", first.png.len()))
        || &multipart[header_end..multipart.len() - 2] != jpeg
        || (decoded.width, decoded.height, decoded.orientation) != (64, 48, 1)
    {
        return Err(fail(
            "MJPEG multipart payload, identity or geometry drifted",
        ));
    }
    let receipt = encoder
        .receipt
        .lock()
        .map_err(|error| fail(error.to_string()))?
        .clone()
        .ok_or_else(|| fail("MJPEG encode omitted its receipt"))?;
    if receipt.provenance.encoder.as_deref() != Some("mjpeg")
        || receipt.provenance.tool_sha256_hex.len() != 64
        || !receipt
            .provenance
            .argv
            .windows(2)
            .any(|pair| pair == ["-f", "mpjpeg"])
        || receipt.artifact.exists()
        || receipt.provenance.bound_tool_path.exists()
    {
        return Err(fail("MJPEG provenance or private-job cleanup drifted"));
    }

    let fallback = fmn_cli::studio_preview::StudioPreview::prepare(
        command.preview_codec,
        &directory.join("absent-ffmpeg"),
        65_536,
    )
    .map_err(|error| fail(error.to_string()))?;
    let fallback_hub = fallback
        .frame_hub(1, 65_536)
        .map_err(|error| fail(error.to_string()))?;
    let png = fallback_hub
        .publish(&input, ProtocolLimits::default())
        .map_err(|error| fail(error.to_string()))?;
    let fallback_fields = fallback
        .ready_fields(&png)
        .map_err(|error| fail(error.to_string()))?;
    if png.stream_content_type() != "image/png"
        || png.png != first.png
        || !fallback_fields.contains("\"preview_codec\":\"png\"")
        || !fallback_fields.contains("\"requested_preview_codec\":\"mjpeg\"")
        || !fallback_fields.contains("using native PNG")
        || fallback.latest_invocation().is_some()
    {
        return Err(fail(
            "missing MJPEG capability did not report its exact PNG fallback",
        ));
    }

    std::fs::write(&mode_path, "mjpeg-preview-truncated")
        .map_err(|error| fail(error.to_string()))?;
    let refusal = hub
        .publish(&input, ProtocolLimits::default())
        .err()
        .ok_or_else(|| fail("truncated MJPEG output reached publication"))?;
    if !refusal.to_string().contains("--preview-codec png")
        || !hub
            .latest()
            .is_some_and(|latest| Arc::ptr_eq(&latest, &first))
    {
        return Err(fail(
            "failed MJPEG capture lost the latest valid publication",
        ));
    }
    ctx.event(
        LogEvent::new("e2e.studio.mjpeg")
            .field("codec", "mjpeg")
            .field("png_bytes", first.png.len() as u64)
            .field("jpeg_bytes", jpeg.len() as u64)
            .field("fallback", "png")
            .field("failure_preserved", "true"),
    );
    for counter in [
        "studio_mjpeg_frames",
        "studio_mjpeg_receipts",
        "studio_mjpeg_fallback",
        "studio_mjpeg_failure_preserved",
    ] {
        ctx.counter(counter, 1);
    }
    Ok(RunOutcome::ok()
        .with_artifact("preview.multipart", multipart)
        .with_artifact("preview.jpg", jpeg.to_vec())
        .with_artifact("preview.png", first.png.clone())
        .with_artifact(
            "preview_fallback.json",
            format!("{{{fallback_fields}}}\n").into_bytes(),
        )
        .with_counter("studio_mjpeg_frames", 1)
        .with_counter("studio_mjpeg_receipts", 1)
        .with_counter("studio_mjpeg_fallback", 1)
        .with_counter("studio_mjpeg_failure_preserved", 1))
}
