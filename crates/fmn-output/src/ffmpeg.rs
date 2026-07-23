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
//! - **Private working directories.** Each job runs in its own fresh
//!   directory (also the child's `TMPDIR`); the artifact is born
//!   there and only reaches its destination through atomic rename
//!   publication after size verification. A failed job never touches
//!   the destination.
//! - **Environment allowlist + locale pinning.** The child sees
//!   exactly `LANG=C`, `LC_ALL=C`, and `TMPDIR=<private dir>`.
//! - **Hardware encoders enter here and only here.** They are named,
//!   validated against the probed encoder list, and recorded in
//!   provenance; ffmpeg products are excluded from certification by
//!   construction, so none of this touches the determinism story.
//!
//! The filesystem side (private dirs, rename publication) uses
//! `std::fs` directly: the boundary is a host-only feature by
//! definition — a platform without subprocesses has no ffmpeg boundary
//! and uses the native outputs instead.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use fmn_platform::process::{ProcessError, ProcessOutcome, ProcessRunner, ProcessSpec};

use crate::negotiate::{NegotiationError, VideoJob, encode_argv, mux_argv};

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
    /// Keep the private working directory after the job (the repro-
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

/// The provenance record of one boundary job — everything a repro
/// bundle needs to name the exact tool and invocation.
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
    /// The published destination.
    pub destination: PathBuf,
}

/// A completed boundary job.
#[derive(Debug)]
pub struct BoundaryReport {
    /// The provenance record.
    pub provenance: Provenance,
    /// The captured stderr log (up to the cap).
    pub stderr: Vec<u8>,
}

/// The boundary: one resolved tool + one process runner + bounds.
pub struct Boundary<'r> {
    tool: FfmpegTool,
    runner: &'r dyn ProcessRunner,
    limits: JobLimits,
    /// Private job directories are created under here.
    workdir_root: PathBuf,
}

/// Distinguishes concurrent jobs within one process.
static JOB_COUNTER: AtomicU64 = AtomicU64::new(0);

impl<'r> Boundary<'r> {
    /// A boundary over a resolved tool.
    #[must_use]
    pub fn new(
        tool: FfmpegTool,
        runner: &'r dyn ProcessRunner,
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
    /// the private directory, nothing else.
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
        if outcome.timed_out {
            return Err(BoundaryError::JobTimedOut {
                timeout: self.limits.timeout,
            });
        }
        if outcome.truncated {
            return Err(BoundaryError::LogOverflow);
        }
        if outcome.code != Some(0) {
            let stderr = String::from_utf8_lossy(&outcome.stderr);
            let tail: String = stderr
                .chars()
                .rev()
                .take(2048)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            return Err(BoundaryError::EncodeFailed {
                code: outcome.code,
                stderr: tail.trim().to_string(),
            });
        }
        Ok(())
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

    /// Run one invocation inside a private dir, then verify + publish
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
            provenance: Provenance {
                tool_path: self.tool.path.clone(),
                tool_sha256_hex: self.tool.sha256_hex.clone(),
                tool_version: self.tool.version.clone(),
                encoder,
                argv,
                destination: destination.to_path_buf(),
            },
            stderr: outcome.stderr,
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
        let encoder = job.resolved_encoder()?;
        if let Some(name) = &encoder
            && !caps.offers(name)
        {
            return Err(BoundaryError::UnknownEncoder {
                requested: name.clone(),
                hardware_available: caps.hardware(),
            });
        }
        let frame_bytes = job.wire.frame_bytes(job.width, job.height);
        if frame_bytes == 0 || !frames.len().is_multiple_of(frame_bytes) {
            return Err(BoundaryError::PayloadGeometry {
                frame_bytes,
                got: frames.len(),
            });
        }
        let workdir = self.make_workdir()?;
        let artifact = workdir.join(format!("out.{}", job.container.extension()));
        let result = encode_argv(job, &artifact)
            .map_err(BoundaryError::from)
            .and_then(|argv| {
                self.run_publishing(
                    argv,
                    &workdir,
                    Some(frames),
                    &artifact,
                    destination,
                    encoder,
                )
            });
        self.cleanup(&workdir);
        result
    }

    /// The two-stage audio mux: stage 1 encodes video into the private
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
        let encoder = job.resolved_encoder()?;
        if let Some(name) = &encoder
            && !caps.offers(name)
        {
            return Err(BoundaryError::UnknownEncoder {
                requested: name.clone(),
                hardware_available: caps.hardware(),
            });
        }
        let frame_bytes = job.wire.frame_bytes(job.width, job.height);
        if frame_bytes == 0 || !frames.len().is_multiple_of(frame_bytes) {
            return Err(BoundaryError::PayloadGeometry {
                frame_bytes,
                got: frames.len(),
            });
        }
        let workdir = self.make_workdir()?;
        let result = (|| {
            // Stage 1: video only, artifact stays private.
            let video = workdir.join(format!("video.{}", job.container.extension()));
            let argv = encode_argv(job, &video)?;
            let spec = self.spec(argv, &workdir, Some(frames));
            let outcome = self.runner.run(&spec)?;
            self.check_outcome(&outcome)?;
            if !std::fs::exists(&video).unwrap_or(false) {
                return Err(BoundaryError::ArtifactMissing);
            }
            // Stage 2: mux, verify, publish.
            let muxed = workdir.join(format!("muxed.{}", job.container.extension()));
            let argv = mux_argv(&video, audio, &muxed);
            self.run_publishing(argv, &workdir, None, &muxed, destination, encoder.clone())
        })();
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
