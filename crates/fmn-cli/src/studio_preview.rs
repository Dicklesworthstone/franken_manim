//! Optional browser MJPEG composition. PNG remains the default and the TUI
//! keeps canonical PNGs. Missing ffmpeg is reported as an explicit PNG fallback;
//! malformed capabilities and encoding failures are never silently substituted.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fmn_output::{Boundary, FfmpegTool, InvocationReport, JobLimits};
use fmn_platform::process::{
    FfmpegLocator, FfmpegLocatorError, ProcessRunner, StdFfmpegLocator, StdProcessRunner,
};
use fmn_studio::FrameHub;
use fmn_studio::host::{JpegPreviewEncoder, PngFrame};

use crate::{CliError, PreviewCodec, json_string};

const ENCODE_TIMEOUT: Duration = Duration::from_secs(2);

struct FfmpegPreviewEncoder {
    boundary: Boundary,
    last_invocation: Mutex<Option<InvocationReport>>,
}

impl std::fmt::Debug for FfmpegPreviewEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfmpegPreviewEncoder")
            .field("tool_sha256", &self.boundary.tool().sha256_hex())
            .finish_non_exhaustive()
    }
}

impl JpegPreviewEncoder for FfmpegPreviewEncoder {
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
        *self
            .last_invocation
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(encoded.invocation);
        Ok(encoded.jpeg)
    }
}

/// Resolved preview capability shared by native and recorded Studio front doors.
#[derive(Debug)]
pub struct StudioPreview {
    requested: PreviewCodec,
    encoder: Option<Arc<FfmpegPreviewEncoder>>,
    fallback: Option<String>,
}

impl StudioPreview {
    /// Resolve the requested optional codec before creating a listener or worker.
    /// A missing executable keeps PNG with an explicit readiness diagnostic.
    /// Every other resolution or executable-integrity refusal remains an error.
    pub fn prepare(
        requested: PreviewCodec,
        ffmpeg_path: &Path,
        max_frame_bytes: usize,
    ) -> Result<Self, CliError> {
        let mut preview = Self {
            requested,
            encoder: None,
            fallback: None,
        };
        if requested == PreviewCodec::Png {
            return Ok(preview);
        }
        let executable = match StdFfmpegLocator::from_host_path().locate_ffmpeg(ffmpeg_path) {
            Ok(executable) => executable,
            Err(error) if missing_executable(&error) => {
                preview.fallback = Some(format!(
                    "MJPEG capability unavailable: {error}; using native PNG; select --preview-codec png to request it directly"
                ));
                return Ok(preview);
            }
            Err(error) => return Err(capability(error)),
        };
        let cap = u64::try_from(max_frame_bytes)
            .ok()
            .and_then(|bytes| bytes.checked_add(1024))
            .filter(|&bytes| bytes > 1024)
            .ok_or_else(|| capability("invalid preview byte ceiling"))?;
        let root = std::env::temp_dir();
        let runner: Arc<dyn ProcessRunner> = Arc::new(StdProcessRunner);
        let tool = FfmpegTool::resolve(executable, runner.as_ref(), &root).map_err(capability)?;
        let boundary = Boundary::new(
            tool,
            runner,
            JobLimits {
                timeout: ENCODE_TIMEOUT,
                max_log_bytes: 8192,
                max_artifact_bytes: cap,
                keep_workdir: false,
            },
            root,
        )
        .map_err(capability)?;
        preview.encoder = Some(Arc::new(FfmpegPreviewEncoder {
            boundary,
            last_invocation: Mutex::new(None),
        }));
        Ok(preview)
    }

    /// Construct a bounded hub with this explicit codec policy.
    pub fn frame_hub(
        &self,
        max_history: usize,
        max_frame_bytes: usize,
    ) -> Result<FrameHub, CliError> {
        let frames = FrameHub::new(max_history, max_frame_bytes).map_err(capability)?;
        match &self.encoder {
            Some(encoder) => frames
                .with_jpeg_encoder(encoder.clone())
                .map_err(capability),
            None => Ok(frames),
        }
    }

    /// The latest successfully completed encode, including exact tool and argv.
    #[must_use]
    pub fn latest_invocation(&self) -> Option<InvocationReport> {
        self.encoder.as_ref().and_then(|encoder| {
            encoder
                .last_invocation
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        })
    }

    /// Additive fields for the Studio readiness record, including observed byte
    /// counts and the first capture's exact tool identity, argv and process policy.
    pub fn ready_fields(&self, first: &PngFrame) -> Result<String, CliError> {
        let effective = if self.encoder.is_some() {
            "mjpeg"
        } else {
            "png"
        };
        let requested = if self.requested == PreviewCodec::Mjpeg {
            "mjpeg"
        } else {
            "png"
        };
        let fallback = self
            .fallback
            .as_ref()
            .map_or_else(|| "null".into(), |value| json_string(value));
        let invocation = if let Some(report) = self.latest_invocation() {
            let provenance = report.provenance;
            let tool_path = provenance
                .tool_path
                .to_str()
                .ok_or_else(|| capability("tool path is not UTF-8"))?;
            let bound_path = provenance
                .bound_tool_path
                .to_str()
                .ok_or_else(|| capability("bound tool path is not UTF-8"))?;
            let argv = provenance
                .argv
                .iter()
                .map(|arg| json_string(arg))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "{{\"tool_path\":{},\"tool_sha256\":{},\"tool_version\":{},\"bound_tool_path\":{},\"process_mechanism\":{},\"process_policy_version\":{},\"encoder\":\"mjpeg\",\"argv\":[{}],\"timeout_ms\":{}}}",
                json_string(tool_path),
                json_string(&provenance.tool_sha256_hex),
                json_string(&provenance.tool_version),
                json_string(bound_path),
                json_string(&provenance.process_mechanism),
                provenance.process_policy_version,
                argv,
                ENCODE_TIMEOUT.as_millis()
            )
        } else {
            "null".into()
        };
        Ok(format!(
            "\"preview_codec\":{},\"requested_preview_codec\":{},\"preview_fallback\":{},\"preview_png_bytes\":{},\"preview_bytes\":{},\"preview_process_per_capture\":{},\"preview_encoder\":{}",
            json_string(effective),
            json_string(requested),
            fallback,
            first.png.len(),
            first.stream_bytes().len(),
            self.encoder.is_some(),
            invocation
        ))
    }

    /// Human-readable effective codec and any capability fallback.
    #[must_use]
    pub fn description(&self) -> String {
        match &self.fallback {
            Some(reason) => reason.clone(),
            None if self.encoder.is_some() => {
                "MJPEG preview; one bounded ffmpeg process per capture".into()
            }
            None => "native PNG preview".into(),
        }
    }
}

fn missing_executable(error: &FfmpegLocatorError) -> bool {
    matches!(error, FfmpegLocatorError::NotFound)
        || matches!(error, FfmpegLocatorError::CandidateIo { err, .. } if err.kind() == std::io::ErrorKind::NotFound)
}

fn capability(error: impl std::fmt::Display) -> CliError {
    CliError::new(
        "capability",
        format!("Studio MJPEG preview: {error}; use --preview-codec png"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_never_probes_the_optional_tool_and_missing_mjpeg_is_a_named_fallback() {
        let absent =
            std::env::temp_dir().join(format!("fmn-absent-preview-{}/ffmpeg", std::process::id()));
        let png = StudioPreview::prepare(PreviewCodec::Png, &absent, 1024).unwrap();
        assert!(png.encoder.is_none());
        assert!(png.fallback.is_none());
        let mjpeg = StudioPreview::prepare(PreviewCodec::Mjpeg, &absent, 1024).unwrap();
        assert!(mjpeg.encoder.is_none());
        assert!(
            mjpeg
                .fallback
                .as_deref()
                .unwrap()
                .contains("using native PNG")
        );
        assert!(mjpeg.description().contains("--preview-codec png"));
        assert!(mjpeg.latest_invocation().is_none());
    }

    #[test]
    fn malformed_ffmpeg_policy_is_not_a_png_fallback() {
        let error = StudioPreview::prepare(PreviewCodec::Mjpeg, Path::new("relative/custom"), 1024)
            .unwrap_err();
        assert_eq!(error.exit_name(), "capability");
        assert!(error.to_string().contains("--preview-codec png"));
    }
}
