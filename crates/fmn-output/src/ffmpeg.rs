//! The one external-tool boundary (§14.3, D2, D-23): sandboxed,
//! fingerprinted, optional ffmpeg.
//!
//! Everything here rides fmn-platform's process capability — argv-only
//! (no shell exists in the API), cleared environment, wall-clock
//! timeout with kill, capped log capture. This module adds the
//! boundary-level protocol on top:
//!
//! - **Resolution + fingerprinting.** The tool is an absolute path,
//!   content-hashed (SHA-256) and version-probed before first use;
//!   path, hash, version, resolved encoder, and full argv land in the
//!   [`Provenance`] of every job.
//! - **Optionality as a capability error.** An absent ffmpeg yields
//!   [`BoundaryError::FfmpegUnavailable`] naming the native
//!   alternatives — never a silent format substitution.
//! - **Job-scoped working directories.** Each job selects its own
//!   directory (also the child's `TMPDIR`); the artifact is born
//!   there and only reaches its destination through atomic rename
//!   publication after size verification. Exclusive directory
//!   creation and stale-directory refusal belong to the separate
//!   `fm-yw7h` boundary-hardening contract.
//! - **Environment allowlist + locale pinning.** The child sees
//!   exactly `LANG=C`, `LC_ALL=C`, and `TMPDIR=<job dir>`.
//! - **Hardware encoders enter here and only here.** They are named,
//!   validated against the probed encoder list, and recorded in
//!   provenance; ffmpeg products are excluded from certification by
//!   construction, so none of this touches the determinism story.
//!
//! The filesystem side (job dirs, rename publication) uses
//! `std::fs` directly: the boundary is a host-only feature by
//! definition — a platform without subprocesses has no ffmpeg boundary
//! and uses the native outputs instead.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use fmn_platform::process::{
    ProcessCancellation, ProcessError, ProcessOutcome, ProcessRunner, ProcessSpec,
    ProcessStdinLimits, ProcessTermination, RunningProcess,
};

use crate::negotiate::{NegotiationError, VideoJob, encode_argv, mux_argv, transcode_audio_argv};

/// The message every "ffmpeg is missing" error carries: the named
/// native alternative (D2 — a capability error, never a substitution).
pub const NATIVE_ALTERNATIVE: &str = "native outputs need no ffmpeg: y4m, PNG sequences, and GIF \
     are built in; ffmpeg is only required for encoded video (mp4/mov), \
     audio mux, and media transcode";

/// Hardware encoders the boundary recognizes (§14.3): reported by
/// `fmn doctor`, selectable only by explicit name.
pub const HARDWARE_ENCODERS: [&str; 6] = [
    "h264_videotoolbox",
    "hevc_videotoolbox",
    "prores_videotoolbox",
    "h264_nvenc",
    "hevc_nvenc",
    "av1_nvenc",
];

/// Typed refusals of the boundary.
#[derive(Debug)]
pub enum BoundaryError {
    /// ffmpeg is not at the resolved path — the capability error that
    /// names the native alternative.
    FfmpegUnavailable {
        /// Where the boundary looked.
        attempted: PathBuf,
        /// The native alternative, spelled out.
        alternative: &'static str,
    },
    /// The process mechanism itself failed.
    Mechanism(ProcessError),
    /// A probe (`-version`, `-encoders`) ran but its output was not
    /// usable.
    ProbeFailed(&'static str),
    /// A named encoder the installed ffmpeg does not offer.
    UnknownEncoder {
        /// The encoder that was requested.
        requested: String,
        /// The recognized hardware encoders this ffmpeg does offer.
        hardware_available: Vec<String>,
    },
    /// The job negotiation was refused.
    Negotiation(NegotiationError),
    /// The frame payload is not a whole number of wire frames.
    PayloadGeometry {
        /// Bytes one frame occupies on the wire.
        frame_bytes: usize,
        /// The payload length that failed the divisibility check.
        got: usize,
    },
    /// The wall-clock timeout expired and the child was killed.
    JobTimedOut {
        /// The configured timeout.
        timeout: Duration,
    },
    /// Cooperative cancellation stopped the invocation.
    Cancelled,
    /// The child's log output exceeded its cap and the child was
    /// killed.
    LogOverflow,
    /// ffmpeg ran and failed.
    EncodeFailed {
        /// The exit code, if any.
        code: Option<i32>,
        /// The tail of stderr, lossily decoded.
        stderr: String,
    },
    /// The job succeeded but produced no artifact.
    ArtifactMissing,
    /// The artifact exceeds the declared size budget; it is not
    /// published.
    ArtifactOversized {
        /// The artifact's size.
        bytes: u64,
        /// The configured budget.
        max: u64,
    },
    /// Private-directory or publication filesystem failure.
    Workdir {
        /// What broke.
        detail: String,
    },
}

impl std::fmt::Display for BoundaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FfmpegUnavailable {
                attempted,
                alternative,
            } => write!(
                f,
                "ffmpeg not found at {}; {alternative}",
                attempted.display()
            ),
            Self::Mechanism(e) => write!(f, "process mechanism: {e}"),
            Self::ProbeFailed(what) => write!(f, "ffmpeg probe failed: {what}"),
            Self::UnknownEncoder {
                requested,
                hardware_available,
            } => write!(
                f,
                "encoder {requested:?} is not offered by the installed ffmpeg \
                 (recognized hardware encoders present: {hardware_available:?})"
            ),
            Self::Negotiation(e) => write!(f, "{e}"),
            Self::PayloadGeometry { frame_bytes, got } => write!(
                f,
                "payload of {got} bytes is not a whole number of {frame_bytes}-byte frames"
            ),
            Self::JobTimedOut { timeout } => {
                write!(f, "ffmpeg exceeded its {}s timeout", timeout.as_secs())
            }
            Self::Cancelled => f.write_str("ffmpeg job was cancelled"),
            Self::LogOverflow => write!(f, "ffmpeg log output exceeded its cap"),
            Self::EncodeFailed { code, stderr } => {
                write!(f, "ffmpeg failed (code {code:?}): {stderr}")
            }
            Self::ArtifactMissing => write!(f, "ffmpeg succeeded but produced no artifact"),
            Self::ArtifactOversized { bytes, max } => write!(
                f,
                "artifact of {bytes} bytes exceeds the {max}-byte budget; not published"
            ),
            Self::Workdir { detail } => write!(f, "boundary workdir: {detail}"),
        }
    }
}

impl std::error::Error for BoundaryError {}

impl From<ProcessError> for BoundaryError {
    fn from(e: ProcessError) -> Self {
        Self::Mechanism(e)
    }
}

impl From<NegotiationError> for BoundaryError {
    fn from(e: NegotiationError) -> Self {
        Self::Negotiation(e)
    }
}

/// The resolved, fingerprinted tool.
#[derive(Debug, Clone)]
pub struct FfmpegTool {
    /// The absolute executable path.
    pub path: PathBuf,
    /// SHA-256 of the executable bytes, hex.
    pub sha256_hex: String,
    /// The first line of `-version` output.
    pub version: String,
}

/// Probe timeouts are short: a probe is milliseconds of work.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_LOG_CAP: u64 = 1 << 20;

fn probe_spec(tool: &Path, argv: &[&str]) -> ProcessSpec {
    ProcessSpec {
        program: tool.to_path_buf(),
        argv: argv.iter().map(|s| (*s).to_string()).collect(),
        env: vec![("LANG".into(), "C".into()), ("LC_ALL".into(), "C".into())],
        cwd: None,
        stdin: None,
        timeout: PROBE_TIMEOUT,
        max_output_bytes: PROBE_LOG_CAP,
    }
}

impl FfmpegTool {
    /// Resolve and fingerprint the tool at `path`: read + hash the
    /// executable bytes, then probe `-version`.
    ///
    /// # Errors
    /// [`BoundaryError::FfmpegUnavailable`] when nothing is there (the
    /// capability error naming the native alternative);
    /// [`BoundaryError::ProbeFailed`] when something is there that does
    /// not behave like ffmpeg.
    pub fn resolve(path: &Path, runner: &dyn ProcessRunner) -> Result<Self, BoundaryError> {
        let bytes = std::fs::read(path).map_err(|_| BoundaryError::FfmpegUnavailable {
            attempted: path.to_path_buf(),
            alternative: NATIVE_ALTERNATIVE,
        })?;
        let sha256_hex = fmn_hash::sha256::sha256(&bytes).to_hex();
        let outcome = runner.run(&probe_spec(path, &["-version"]))?;
        if !outcome.success() {
            return Err(BoundaryError::ProbeFailed("-version exited nonzero"));
        }
        let version = String::from_utf8_lossy(&outcome.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        if version.is_empty() {
            return Err(BoundaryError::ProbeFailed("-version produced no output"));
        }
        Ok(Self {
            path: path.to_path_buf(),
            sha256_hex,
            version,
        })
    }
}

/// The installed ffmpeg's encoder inventory.
#[derive(Debug, Clone, Default)]
pub struct EncoderCapabilities {
    names: std::collections::BTreeSet<String>,
}

impl EncoderCapabilities {
    /// Probe `-encoders` and parse the inventory.
    ///
    /// # Errors
    /// [`BoundaryError`] when the probe cannot run or parse.
    pub fn probe(tool: &FfmpegTool, runner: &dyn ProcessRunner) -> Result<Self, BoundaryError> {
        let outcome = runner.run(&probe_spec(&tool.path, &["-hide_banner", "-encoders"]))?;
        if !outcome.success() {
            return Err(BoundaryError::ProbeFailed("-encoders exited nonzero"));
        }
        Ok(Self::parse(&String::from_utf8_lossy(&outcome.stdout)))
    }

    /// Parse `-encoders` output: after the `------` separator, each
    /// line is ` FLAGS name description`.
    #[must_use]
    pub fn parse(listing: &str) -> Self {
        let mut names = std::collections::BTreeSet::new();
        let mut seen_separator = false;
        for line in listing.lines() {
            if !seen_separator {
                seen_separator = line.trim_start().starts_with("---");
                continue;
            }
            let mut fields = line.split_whitespace();
            let (Some(_flags), Some(name)) = (fields.next(), fields.next()) else {
                continue;
            };
            names.insert(name.to_string());
        }
        Self { names }
    }

    /// Whether `encoder` is offered.
    #[must_use]
    pub fn offers(&self, encoder: &str) -> bool {
        self.names.contains(encoder)
    }

    /// The recognized hardware encoders present in this inventory
    /// (`fmn doctor`'s report).
    #[must_use]
    pub fn hardware(&self) -> Vec<String> {
        HARDWARE_ENCODERS
            .iter()
            .filter(|name| self.offers(name))
            .map(|s| (*s).to_string())
            .collect()
    }
}

/// Per-job resource bounds.
#[derive(Debug, Clone)]
pub struct JobLimits {
    /// Wall-clock bound on each ffmpeg invocation.
    pub timeout: Duration,
    /// Cap on captured stdout/stderr per stream.
    pub max_log_bytes: u64,
    /// Cap on the produced artifact's size; larger is refused, not
    /// published.
    pub max_artifact_bytes: u64,
    /// Keep the job-scoped working directory after the job (the repro-
    /// bundle hook; default false).
    pub keep_workdir: bool,
}

impl Default for JobLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(600),
            max_log_bytes: 1 << 20,
            max_artifact_bytes: 8 << 30,
            keep_workdir: false,
        }
    }
}

/// The provenance record of one ffmpeg invocation.
///
/// This names the resolution-time tool identity and exact argv. Spawn-time
/// executable revalidation is the separate `fm-yw7h` boundary-hardening
/// contract and is not implied by this record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// The tool's absolute path.
    pub tool_path: PathBuf,
    /// SHA-256 of the tool's bytes, hex.
    pub tool_sha256_hex: String,
    /// The tool's `-version` first line.
    pub tool_version: String,
    /// The resolved encoder (`None` for muxer-level modes like GIF and
    /// stream-copy jobs).
    pub encoder: Option<String>,
    /// The complete argv, verbatim.
    pub argv: Vec<String>,
}

/// One completed invocation within a boundary job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvocationReport {
    /// The provenance record.
    pub provenance: Provenance,
    /// The captured stderr log (up to the cap).
    pub stderr: Vec<u8>,
    /// Private artifact path named by this invocation's argv.
    pub artifact: PathBuf,
}

/// A completed and published boundary job.
#[derive(Debug)]
pub struct BoundaryReport {
    /// Every invocation in execution order (video encode, then optional mux).
    pub invocations: Vec<InvocationReport>,
    /// The atomically published destination.
    pub destination: PathBuf,
}

/// The boundary: one resolved tool + one process runner + bounds.
pub struct Boundary {
    tool: FfmpegTool,
    runner: Arc<dyn ProcessRunner>,
    limits: JobLimits,
    /// Job-scoped directories are created under here. Exclusive creation is
    /// the separate `fm-yw7h` hardening contract.
    workdir_root: PathBuf,
}

/// Distinguishes concurrent jobs within one process.
static JOB_COUNTER: AtomicU64 = AtomicU64::new(0);

impl Boundary {
    /// A boundary over a resolved tool.
    #[must_use]
    pub fn new(
        tool: FfmpegTool,
        runner: Arc<dyn ProcessRunner>,
        limits: JobLimits,
        workdir_root: PathBuf,
    ) -> Self {
        Self {
            tool,
            runner,
            limits,
            workdir_root,
        }
    }

    /// The resolved tool.
    #[must_use]
    pub const fn tool(&self) -> &FfmpegTool {
        &self.tool
    }

    /// Start a negotiated long-lived encode whose stdin is supplied in
    /// ordered chunks.
    ///
    /// The returned job writes only inside its job-scoped work directory.
    /// [`StreamingEncode::prepare`] closes stdin, waits, verifies, and runs an
    /// optional audio mux without publishing. Only
    /// [`PreparedFfmpegArtifact::commit`] can mutate `destination`.
    ///
    /// # Errors
    /// Negotiation, capability, work-directory, argv, or process-start
    /// refusals.
    pub fn start_encode(
        &self,
        job: &VideoJob,
        caps: &EncoderCapabilities,
        destination: &Path,
        audio: Option<&Path>,
        cancellation: ProcessCancellation,
        stdin_limits: ProcessStdinLimits,
    ) -> Result<StreamingEncode, BoundaryError> {
        let encoder = job.resolved_encoder()?;
        if let Some(name) = &encoder
            && !caps.offers(name)
        {
            return Err(BoundaryError::UnknownEncoder {
                requested: name.clone(),
                hardware_available: caps.hardware(),
            });
        }
        let workdir = self.make_workdir()?;
        let video_artifact = if audio.is_some() {
            workdir.join(format!("video.{}", job.container.extension()))
        } else {
            workdir.join(format!("out.{}", job.container.extension()))
        };
        let argv = match encode_argv(job, &video_artifact) {
            Ok(argv) => argv,
            Err(error) => {
                self.cleanup(&workdir);
                return Err(error.into());
            }
        };
        let spec = self.spec(argv.clone(), &workdir, None);
        let process = match self.runner.start(&spec, cancellation.clone(), stdin_limits) {
            Ok(process) => process,
            Err(error) => {
                self.cleanup(&workdir);
                return Err(error.into());
            }
        };
        Ok(StreamingEncode {
            tool: self.tool.clone(),
            runner: Arc::clone(&self.runner),
            limits: self.limits.clone(),
            workdir,
            destination: destination.to_path_buf(),
            video_artifact,
            audio: audio.map(Path::to_path_buf),
            encoder,
            video_argv: argv,
            process: Some(process),
            cancellation,
            cleanup: true,
        })
    }

    fn make_workdir(&self) -> Result<PathBuf, BoundaryError> {
        let dir = self.workdir_root.join(format!(
            "fmn-ffmpeg-{}-{}",
            std::process::id(),
            JOB_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).map_err(|e| BoundaryError::Workdir {
            detail: format!("create {}: {e}", dir.display()),
        })?;
        Ok(dir)
    }

    /// The environment allowlist (D2): locale pinned, `TMPDIR` inside
    /// the job-scoped directory, nothing else.
    fn env_allowlist(workdir: &Path) -> Vec<(String, String)> {
        vec![
            ("LANG".into(), "C".into()),
            ("LC_ALL".into(), "C".into()),
            ("TMPDIR".into(), workdir.display().to_string()),
        ]
    }

    fn spec(&self, argv: Vec<String>, workdir: &Path, stdin: Option<Vec<u8>>) -> ProcessSpec {
        ProcessSpec {
            program: self.tool.path.clone(),
            argv,
            env: Self::env_allowlist(workdir),
            cwd: Some(workdir.to_path_buf()),
            stdin,
            timeout: self.limits.timeout,
            max_output_bytes: self.limits.max_log_bytes,
        }
    }

    /// Map a finished outcome to success or a typed refusal.
    fn check_outcome(&self, outcome: &ProcessOutcome) -> Result<(), BoundaryError> {
        check_process_outcome(&self.limits, outcome)
    }

    /// Verify the artifact and publish it to `destination` by atomic
    /// rename. A failure at any point leaves the destination untouched.
    fn publish(&self, artifact: &Path, destination: &Path) -> Result<(), BoundaryError> {
        let meta = std::fs::metadata(artifact).map_err(|_| BoundaryError::ArtifactMissing)?;
        if meta.len() > self.limits.max_artifact_bytes {
            return Err(BoundaryError::ArtifactOversized {
                bytes: meta.len(),
                max: self.limits.max_artifact_bytes,
            });
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|e| BoundaryError::Workdir {
                detail: format!("create {}: {e}", parent.display()),
            })?;
        }
        std::fs::rename(artifact, destination).map_err(|e| BoundaryError::Workdir {
            detail: format!(
                "publish {} -> {}: {e}",
                artifact.display(),
                destination.display()
            ),
        })
    }

    fn cleanup(&self, workdir: &Path) {
        if !self.limits.keep_workdir {
            let _ = std::fs::remove_dir_all(workdir);
        }
    }

    /// Run one invocation inside a job-scoped dir, then verify + publish
    /// the artifact it should have produced.
    fn run_publishing(
        &self,
        argv: Vec<String>,
        workdir: &Path,
        stdin: Option<Vec<u8>>,
        artifact: &Path,
        destination: &Path,
        encoder: Option<String>,
    ) -> Result<BoundaryReport, BoundaryError> {
        let spec = self.spec(argv.clone(), workdir, stdin);
        let outcome = self.runner.run(&spec)?;
        self.check_outcome(&outcome)?;
        self.publish(artifact, destination)?;
        Ok(BoundaryReport {
            invocations: vec![InvocationReport {
                provenance: Provenance {
                    tool_path: self.tool.path.clone(),
                    tool_sha256_hex: self.tool.sha256_hex.clone(),
                    tool_version: self.tool.version.clone(),
                    encoder,
                    argv,
                },
                stderr: outcome.stderr,
                artifact: artifact.to_path_buf(),
            }],
            destination: destination.to_path_buf(),
        })
    }

    /// Encode `frames` (concatenated tightly-packed wire frames, output
    /// orientation) to `destination`.
    ///
    /// # Errors
    /// Every refusal in [`BoundaryError`]; the destination is written
    /// only on success, atomically.
    pub fn encode(
        &self,
        job: &VideoJob,
        frames: Vec<u8>,
        caps: &EncoderCapabilities,
        destination: &Path,
    ) -> Result<BoundaryReport, BoundaryError> {
        let frame_bytes = job.wire.frame_bytes(job.width, job.height);
        if frame_bytes == 0 || frames.is_empty() || !frames.len().is_multiple_of(frame_bytes) {
            return Err(BoundaryError::PayloadGeometry {
                frame_bytes,
                got: frames.len(),
            });
        }
        let bytes = u64::try_from(frames.len()).unwrap_or(u64::MAX);
        let mut encode = self.start_encode(
            job,
            caps,
            destination,
            None,
            ProcessCancellation::new(),
            ProcessStdinLimits::new(bytes, bytes),
        )?;
        encode.write_stdin(&frames)?;
        encode.prepare()?.commit()
    }

    /// The two-stage audio mux: stage 1 encodes video into the job-scoped
    /// dir; stage 2 muxes with `-c:v copy` (never re-encoding video)
    /// and publishes.
    ///
    /// # Errors
    /// Every refusal in [`BoundaryError`].
    pub fn encode_with_audio(
        &self,
        job: &VideoJob,
        frames: Vec<u8>,
        audio: &Path,
        caps: &EncoderCapabilities,
        destination: &Path,
    ) -> Result<BoundaryReport, BoundaryError> {
        let frame_bytes = job.wire.frame_bytes(job.width, job.height);
        if frame_bytes == 0 || frames.is_empty() || !frames.len().is_multiple_of(frame_bytes) {
            return Err(BoundaryError::PayloadGeometry {
                frame_bytes,
                got: frames.len(),
            });
        }
        let bytes = u64::try_from(frames.len()).unwrap_or(u64::MAX);
        let mut encode = self.start_encode(
            job,
            caps,
            destination,
            Some(audio),
            ProcessCancellation::new(),
            ProcessStdinLimits::new(bytes, bytes),
        )?;
        encode.write_stdin(&frames)?;
        encode.prepare()?.commit()
    }

    /// Decode any ffmpeg-readable audio source to native s16 PCM WAV.
    ///
    /// The source is resolved before entering the job-scoped working directory so
    /// relative caller paths retain their meaning. The decoded WAV is born in
    /// that directory and reaches `destination` only through the same bounded,
    /// atomic publication path as encoded video. Certified runs consume the
    /// resulting PCM through the native decoder; the ffmpeg product itself is
    /// outside certification by construction.
    ///
    /// # Errors
    /// Every refusal in [`BoundaryError`].
    pub fn transcode_audio(
        &self,
        input: &Path,
        destination: &Path,
    ) -> Result<BoundaryReport, BoundaryError> {
        let input = std::fs::canonicalize(input).map_err(|error| BoundaryError::Workdir {
            detail: format!("resolve audio input {}: {error}", input.display()),
        })?;
        let workdir = self.make_workdir()?;
        let artifact = workdir.join("decoded.wav");
        let argv = transcode_audio_argv(&input, &artifact);
        let result = self.run_publishing(argv, &workdir, None, &artifact, destination, None);
        self.cleanup(&workdir);
        result
    }

    /// Concatenate already-encoded partial files with stream copy (the
    /// insert-file mechanism).
    ///
    /// # Errors
    /// Every refusal in [`BoundaryError`]; single quotes in input paths
    /// are refused rather than escaped.
    pub fn concat(
        &self,
        inputs: &[PathBuf],
        destination: &Path,
    ) -> Result<BoundaryReport, BoundaryError> {
        if inputs.is_empty() {
            return Err(BoundaryError::Negotiation(NegotiationError(
                "concat of zero inputs",
            )));
        }
        let workdir = self.make_workdir()?;
        let result = (|| {
            let mut listing = String::new();
            for input in inputs {
                let text = input.display().to_string();
                if text.contains('\'') || text.contains('\n') {
                    return Err(BoundaryError::Negotiation(NegotiationError(
                        "concat input path contains a quote or newline",
                    )));
                }
                listing.push_str(&format!("file '{text}'\n"));
            }
            let list_file = workdir.join("concat.txt");
            std::fs::write(&list_file, listing).map_err(|e| BoundaryError::Workdir {
                detail: format!("write {}: {e}", list_file.display()),
            })?;
            let ext = destination
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("mp4");
            let artifact = workdir.join(format!("joined.{ext}"));
            let argv = crate::negotiate::concat_argv(&list_file, &artifact);
            self.run_publishing(argv, &workdir, None, &artifact, destination, None)
        })();
        self.cleanup(&workdir);
        result
    }
}

/// A live, job-scoped ffmpeg encode with backpressured stdin.
pub struct StreamingEncode {
    tool: FfmpegTool,
    runner: Arc<dyn ProcessRunner>,
    limits: JobLimits,
    workdir: PathBuf,
    destination: PathBuf,
    video_artifact: PathBuf,
    audio: Option<PathBuf>,
    encoder: Option<String>,
    video_argv: Vec<String>,
    process: Option<Box<dyn RunningProcess>>,
    cancellation: ProcessCancellation,
    cleanup: bool,
}

impl StreamingEncode {
    /// Feed the next tightly packed ordered byte chunk.
    ///
    /// The OS pipe supplies bounded backpressure; the process supervisor can
    /// cancel and close its read end while this call is blocked.
    ///
    /// # Errors
    /// [`BoundaryError`] when process stdin refuses the chunk.
    pub fn write_stdin(&mut self, bytes: &[u8]) -> Result<(), BoundaryError> {
        let write = self
            .process
            .as_mut()
            .ok_or_else(|| BoundaryError::Workdir {
                detail: "streaming ffmpeg stdin is already closed".to_string(),
            })?
            .write_stdin(bytes);
        let Err(write_error) = write else {
            return Ok(());
        };

        // A killed/exited child commonly surfaces first as BrokenPipe on the
        // bounded writer. Reap it here and prefer the supervisor's typed
        // terminal reason (timeout/log cap/cancellation/nonzero exit) over the
        // incidental pipe diagnostic. Input-limit errors retain their precise
        // mechanism category if the child itself had not failed.
        let process = self.process.take().ok_or_else(|| BoundaryError::Workdir {
            detail: "streaming ffmpeg process disappeared after stdin failure".to_string(),
        })?;
        if matches!(
            &write_error,
            ProcessError::StdinChunkLimit { .. } | ProcessError::StdinTotalLimit { .. }
        ) {
            let _ = process.cancel();
            return Err(BoundaryError::Mechanism(write_error));
        }
        match process.finish() {
            Ok(outcome) => match check_process_outcome(&self.limits, &outcome) {
                Ok(()) => Err(BoundaryError::Mechanism(write_error)),
                Err(terminal) => Err(terminal),
            },
            Err(_) => Err(BoundaryError::Mechanism(write_error)),
        }
    }

    /// Finish every job-scoped invocation and verify the final artifact without
    /// publishing it.
    ///
    /// # Errors
    /// Process, timeout, log, codec, artifact, or cancellation refusals.
    pub fn prepare(mut self) -> Result<PreparedFfmpegArtifact, BoundaryError> {
        let process = self.process.take().ok_or_else(|| BoundaryError::Workdir {
            detail: "streaming ffmpeg process was already finalized".to_string(),
        })?;
        let outcome = process.finish()?;
        check_process_outcome(&self.limits, &outcome)?;
        verify_private_artifact(&self.video_artifact, self.limits.max_artifact_bytes)?;
        let mut invocations = vec![invocation_report(
            &self.tool,
            self.encoder.clone(),
            self.video_argv.clone(),
            self.video_artifact.clone(),
            outcome.stderr,
        )];

        let artifact = if let Some(audio) = &self.audio {
            let muxed = self.workdir.join(format!(
                "muxed.{}",
                extension_or_default(&self.video_artifact)
            ));
            let argv = mux_argv(&self.video_artifact, audio, &muxed);
            let spec = ProcessSpec {
                program: self.tool.path.clone(),
                argv: argv.clone(),
                env: Boundary::env_allowlist(&self.workdir),
                cwd: Some(self.workdir.clone()),
                stdin: None,
                timeout: self.limits.timeout,
                max_output_bytes: self.limits.max_log_bytes,
            };
            let process = self.runner.start(
                &spec,
                self.cancellation.clone(),
                ProcessStdinLimits::new(1, 0),
            )?;
            let outcome = process.finish()?;
            check_process_outcome(&self.limits, &outcome)?;
            verify_private_artifact(&muxed, self.limits.max_artifact_bytes)?;
            invocations.push(invocation_report(
                &self.tool,
                None,
                argv,
                muxed.clone(),
                outcome.stderr,
            ));
            muxed
        } else {
            self.video_artifact.clone()
        };

        self.cleanup = false;
        Ok(PreparedFfmpegArtifact {
            limits: self.limits.clone(),
            workdir: self.workdir.clone(),
            artifact,
            destination: self.destination.clone(),
            invocations,
            cleanup: true,
        })
    }

    /// Cancel and reap a live encode.
    ///
    /// # Errors
    /// [`BoundaryError`] when supervision fails while reaping.
    pub fn cancel(mut self) -> Result<(), BoundaryError> {
        self.cancellation.cancel();
        if let Some(process) = self.process.take() {
            process.cancel()?;
        }
        Ok(())
    }
}

impl Drop for StreamingEncode {
    fn drop(&mut self) {
        if self.cleanup {
            self.cancellation.cancel();
            if let Some(process) = self.process.take() {
                let _ = process.cancel();
            }
            cleanup_workdir(&self.limits, &self.workdir);
        }
    }
}

/// A verified unpublished ffmpeg artifact. Dropping it never publishes.
pub struct PreparedFfmpegArtifact {
    limits: JobLimits,
    workdir: PathBuf,
    artifact: PathBuf,
    destination: PathBuf,
    invocations: Vec<InvocationReport>,
    cleanup: bool,
}

impl PreparedFfmpegArtifact {
    /// Atomically publish the verified artifact.
    ///
    /// # Errors
    /// Artifact revalidation or rename failure.
    pub fn commit(mut self) -> Result<BoundaryReport, BoundaryError> {
        verify_private_artifact(&self.artifact, self.limits.max_artifact_bytes)?;
        if let Some(parent) = self.destination.parent() {
            std::fs::create_dir_all(parent).map_err(|error| BoundaryError::Workdir {
                detail: format!("create {}: {error}", parent.display()),
            })?;
        }
        std::fs::rename(&self.artifact, &self.destination).map_err(|error| {
            BoundaryError::Workdir {
                detail: format!(
                    "publish {} -> {}: {error}",
                    self.artifact.display(),
                    self.destination.display()
                ),
            }
        })?;
        self.cleanup = false;
        cleanup_workdir(&self.limits, &self.workdir);
        Ok(BoundaryReport {
            invocations: std::mem::take(&mut self.invocations),
            destination: self.destination.clone(),
        })
    }
}

impl Drop for PreparedFfmpegArtifact {
    fn drop(&mut self) {
        if self.cleanup {
            cleanup_workdir(&self.limits, &self.workdir);
        }
    }
}

fn extension_or_default(path: &Path) -> String {
    path.extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("mp4")
        .to_string()
}

fn verify_private_artifact(path: &Path, max_bytes: u64) -> Result<(), BoundaryError> {
    let metadata = std::fs::metadata(path).map_err(|_| BoundaryError::ArtifactMissing)?;
    if !metadata.is_file() {
        return Err(BoundaryError::ArtifactMissing);
    }
    if metadata.len() > max_bytes {
        return Err(BoundaryError::ArtifactOversized {
            bytes: metadata.len(),
            max: max_bytes,
        });
    }
    Ok(())
}

fn invocation_report(
    tool: &FfmpegTool,
    encoder: Option<String>,
    argv: Vec<String>,
    artifact: PathBuf,
    stderr: Vec<u8>,
) -> InvocationReport {
    InvocationReport {
        provenance: Provenance {
            tool_path: tool.path.clone(),
            tool_sha256_hex: tool.sha256_hex.clone(),
            tool_version: tool.version.clone(),
            encoder,
            argv,
        },
        stderr,
        artifact,
    }
}

fn check_process_outcome(
    limits: &JobLimits,
    outcome: &ProcessOutcome,
) -> Result<(), BoundaryError> {
    match outcome.termination {
        ProcessTermination::Exited(Some(0)) => Ok(()),
        ProcessTermination::TimedOut => Err(BoundaryError::JobTimedOut {
            timeout: limits.timeout,
        }),
        ProcessTermination::OutputLimitExceeded => Err(BoundaryError::LogOverflow),
        ProcessTermination::Cancelled => Err(BoundaryError::Cancelled),
        ProcessTermination::Exited(code) => {
            let stderr = String::from_utf8_lossy(&outcome.stderr);
            let tail: String = stderr
                .chars()
                .rev()
                .take(2048)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            Err(BoundaryError::EncodeFailed {
                code,
                stderr: tail.trim().to_string(),
            })
        }
    }
}

fn cleanup_workdir(limits: &JobLimits, workdir: &Path) {
    if !limits.keep_workdir {
        let _ = std::fs::remove_dir_all(workdir);
    }
}
