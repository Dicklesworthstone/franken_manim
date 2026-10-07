//! Explicit ffmpeg budgets for an offline native render.
//!
//! A streaming encoder lives while frames are being constructed, not just
//! while ffmpeg is doing work. Its wall-clock allowance therefore belongs to
//! the render request. The low-level boundary's short-job defaults and probe
//! bounds are deliberately unchanged.

use std::time::{Duration, Instant};

use super::{JobLimits, RenderError, RenderOptions};

/// Default allowance for each offline render-time ffmpeg invocation.
///
/// Twenty-four hours accommodates long-form/overnight rendering without
/// disabling the watchdog. Services should choose a smaller tenant-specific
/// allowance explicitly. This is wall time, not movie duration or idle time.
pub const DEFAULT_RENDER_FFMPEG_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

pub(super) fn default_job_limits() -> JobLimits {
    JobLimits {
        timeout: DEFAULT_RENDER_FFMPEG_TIMEOUT,
        ..JobLimits::default()
    }
}

impl RenderOptions {
    /// Resolve the ffmpeg resource policy without locating or launching a tool.
    ///
    /// The encoded-artifact cap is the smaller of the job-specific cap and
    /// `max_output_bytes`. The latter independently caps cumulative raw frame
    /// input, so raising one bound cannot silently disable the other. The same
    /// admitted job policy reaches video encode, audio mux, and non-WAV decode;
    /// the audio decoder retains its additional decode-specific ceilings.
    ///
    /// # Errors
    /// Rejects zero timeout/log/artifact/input bounds and a timeout that cannot
    /// be represented by this host's monotonic clock. There is no unlimited
    /// sentinel. Native PNG/GIF/Y4M rendering does not consult this policy.
    pub fn effective_ffmpeg_limits(&self) -> Result<JobLimits, RenderError> {
        let mut limits = self.ffmpeg_limits.clone();
        if limits.timeout.is_zero() {
            return Err(RenderError::InvalidOptions("ffmpeg timeout must be nonzero"));
        }
        if Instant::now().checked_add(limits.timeout).is_none() {
            return Err(RenderError::InvalidOptions(
                "ffmpeg timeout exceeds the monotonic clock range",
            ));
        }
        if limits.max_log_bytes == 0 {
            return Err(RenderError::InvalidOptions("ffmpeg max_log_bytes must be nonzero"));
        }
        if limits.max_artifact_bytes == 0 || self.max_output_bytes == 0 {
            return Err(RenderError::InvalidOptions(
                "ffmpeg artifact and input byte limits must be nonzero",
            ));
        }
        limits.max_artifact_bytes = limits.max_artifact_bytes.min(self.max_output_bytes);
        Ok(limits)
    }
}

/// Effective resource policy attached to a successfully published video receipt.
///
/// This records the admitted upper bounds, not elapsed time or bytes consumed.
/// Tool probes retain their separate short deadlines. Audio decoding may apply
/// tighter format-specific limits. This is not an input-closure certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FfmpegLimitsReport {
    /// Wall-clock allowance for each encode, mux, or media-decode invocation.
    pub timeout: Duration,
    /// Captured stdout/stderr ceiling, independently for each stream.
    pub max_log_bytes: u64,
    /// Encoded/intermediate artifact ceiling after intersecting both budgets.
    pub max_artifact_bytes: u64,
    /// Independent ceiling on cumulative raw-frame bytes sent to the encoder.
    pub max_input_bytes: u64,
    /// Whether job work directories are retained for diagnosis.
    pub keep_workdir: bool,
}

impl FfmpegLimitsReport {
    pub(super) fn new(limits: &JobLimits, max_input_bytes: u64) -> Self {
        Self {
            timeout: limits.timeout,
            max_log_bytes: limits.max_log_bytes,
            max_artifact_bytes: limits.max_artifact_bytes,
            max_input_bytes,
            keep_workdir: limits.keep_workdir,
        }
    }

    /// Stable JSON object for embedding in an artifact/provenance manifest.
    ///
    /// Duration is represented as whole seconds plus subsecond nanoseconds,
    /// preserving the exact configured allowance without floating-point loss.
    /// No timestamps, paths, or ambient configuration enter this object.
    #[must_use]
    pub fn to_json(&self) -> String {
        format!(
            "{{\"timeout_seconds\":{},\"timeout_nanoseconds\":{},\"max_log_bytes\":{},\"max_artifact_bytes\":{},\"max_input_bytes\":{},\"keep_workdir\":{}}}",
            self.timeout.as_secs(),
            self.timeout.subsec_nanos(),
            self.max_log_bytes,
            self.max_artifact_bytes,
            self.max_input_bytes,
            self.keep_workdir,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_default_keeps_a_watchdog_without_the_ten_minute_ceiling() {
        let options = RenderOptions::new("movie.mp4").unwrap();
        let limits = options.effective_ffmpeg_limits().unwrap();
        assert_eq!(limits.timeout, Duration::from_secs(86_400));
        assert_eq!(JobLimits::default().timeout, Duration::from_secs(600));
        assert_eq!(limits.max_artifact_bytes, 8 << 30);
        assert_eq!(limits.max_log_bytes, 1 << 20);
        assert!(!limits.keep_workdir);
    }

    #[test]
    fn resource_intersection_never_raises_either_artifact_ceiling() {
        let mut options = RenderOptions::new("movie.mp4").unwrap();
        options.ffmpeg_limits.timeout = Duration::new(1_801, 123_456_789);
        options.ffmpeg_limits.max_log_bytes = 17;
        options.ffmpeg_limits.keep_workdir = true;
        for (job_cap, sink_cap, expected) in [(91, 37, 37), (19, 83, 19), (41, 41, 41)] {
            options.ffmpeg_limits.max_artifact_bytes = job_cap;
            options.max_output_bytes = sink_cap;
            let effective = options.effective_ffmpeg_limits().unwrap();
            assert_eq!(effective.max_artifact_bytes, expected);
            assert_eq!(effective.timeout, Duration::new(1_801, 123_456_789));
            assert_eq!(effective.max_log_bytes, 17);
            assert!(effective.keep_workdir);
            assert_eq!(options.ffmpeg_limits.max_artifact_bytes, job_cap);
            assert_eq!(options.max_output_bytes, sink_cap);
        }
    }

    #[test]
    fn manifest_policy_preserves_subseconds_and_both_independent_byte_caps() {
        let mut options = RenderOptions::new("movie.mp4").unwrap();
        options.ffmpeg_limits = JobLimits {
            timeout: Duration::new(1_801, 123_456_789),
            max_log_bytes: 17,
            max_artifact_bytes: 37,
            keep_workdir: true,
        };
        options.max_output_bytes = 91;
        let limits = options.effective_ffmpeg_limits().unwrap();
        let report = FfmpegLimitsReport::new(&limits, options.max_output_bytes);
        assert_eq!(
            report.to_json(),
            "{\"timeout_seconds\":1801,\"timeout_nanoseconds\":123456789,\"max_log_bytes\":17,\"max_artifact_bytes\":37,\"max_input_bytes\":91,\"keep_workdir\":true}"
        );
    }
}
