//! Assembly of previously encoded clips through the governed ffmpeg boundary.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use fmn_config::config::DeterminismMode;
use fmn_hash::sha256;
use fmn_output::{
    ArtifactDigest, Boundary, BoundaryError, BoundaryReport, FfmpegLimitsReport, FfmpegTool,
    JobLimits, admit_render_ffmpeg_limits,
};
use fmn_platform::fs::{FileSystem, FsError, StdFs};
use fmn_platform::process::FfmpegLocatorError;

use super::FfmpegCapability;

/// Policy for assembling encoded clips without rendering their frames again.
///
/// All clips must be self-contained MP4/MOV files with compatible codecs,
/// stream layouts and time bases. This API copies their encoded streams in
/// the supplied order; it does not resize, resample, trim, or reconcile
/// incompatible streams. MP4 and MOV
/// destinations are supported, including the per-play clips from CLI
/// `--subdivide` runs with the same settings.
///
/// Input files are frozen in memory before ffmpeg starts. Their combined byte
/// length is bounded by `max_total_input_bytes` (256 MiB by default), in addition
/// to the per-clip and clip-count bounds. This is not a streaming source reader.
/// The private ffmpeg work directory also contains those frozen input bytes;
/// the process's additional scratch and output use `ffmpeg_limits`.
#[derive(Clone, Debug)]
pub struct VideoConcatOptions {
    /// Host destination with an MP4 or MOV extension. Never overwritten.
    pub output: PathBuf,
    /// Encoded video is outside the certified artifact set.
    pub determinism: DeterminismMode,
    /// Explicit authority for the governed ffmpeg process boundary.
    ///
    /// With the `ffmpeg` feature, [`concatenate_video`] supplies the host
    /// capability when this is absent. [`concatenate_video_with_fs`] always
    /// requires an explicit capability.
    pub ffmpeg: Option<FfmpegCapability>,
    /// Executable request passed to the capability's locator.
    pub ffmpeg_bin: PathBuf,
    /// Timeout, log, output and scratch bounds for the concatenation job.
    /// Tool identity probes retain their own short bounds.
    pub ffmpeg_limits: JobLimits,
    /// Maximum number of input positions, including repeated clips. Must not
    /// exceed the boundary's hard ceiling of 4093
    /// ([`fmn_output::ffmpeg::MAX_CONCAT_INPUTS`]).
    pub max_inputs: usize,
    /// Maximum bytes in any one input clip.
    pub max_input_bytes: u64,
    /// Maximum combined bytes retained for all input positions.
    /// A repeated clip consumes this budget again.
    pub max_total_input_bytes: u64,
}

impl VideoConcatOptions {
    /// Standard-mode, create-only MP4/MOV assembly with explicit byte bounds.
    #[must_use]
    pub fn new(output: impl Into<PathBuf>) -> Self {
        Self {
            output: output.into(),
            determinism: DeterminismMode::Standard,
            ffmpeg: None,
            ffmpeg_bin: PathBuf::from("ffmpeg"),
            ffmpeg_limits: JobLimits::default(),
            max_inputs: 1024,
            max_input_bytes: 64 * 1024 * 1024,
            max_total_input_bytes: 256 * 1024 * 1024,
        }
    }
}

/// One input position, identifying the exact frozen bytes copied by ffmpeg.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoClipReport {
    /// Caller-supplied descriptive path. The digest identifies its content.
    pub source_path: PathBuf,
    /// Exact bytes read for this input position.
    pub bytes: u64,
    /// SHA-256 of those exact bytes, before tool discovery or execution.
    pub digest: ArtifactDigest,
}

/// A successfully published concatenation and its ordered input evidence.
///
/// This receipt makes no claim about frame count or duration: stream copying
/// does not decode the clips or measure their timelines. The boundary receipt
/// identifies the output bytes and the exact tool invocation.
#[derive(Debug)]
pub struct VideoConcatReport {
    /// Inputs in movie order. Repeated paths retain separate positions.
    pub inputs: Vec<VideoClipReport>,
    /// Sum of the exact byte lengths in `inputs`.
    pub total_input_bytes: u64,
    /// Actual host destination, output length/digest and process provenance.
    pub boundary: BoundaryReport,
    /// Effective process limits and the aggregate source-byte ceiling.
    pub limits: FfmpegLimitsReport,
}

/// A refusal or failure before a complete movie can be published.
#[derive(Debug)]
pub enum VideoConcatError {
    /// Invalid destination format, empty list or unusable resource bounds.
    InvalidOptions(&'static str),
    /// Missing process authority or a request for certified encoded video.
    Capability(&'static str),
    /// The destination already names a node, including a dangling link.
    DestinationExists(PathBuf),
    /// Inspecting or resolving the host output path failed.
    Output(FsError),
    /// A bounded source read failed. The index is zero-based.
    Input {
        /// Position in the supplied clip sequence.
        index: usize,
        /// Source read diagnostic, including its path and bound.
        error: FsError,
    },
    /// A source clip has no bytes and cannot contribute an encoded stream.
    EmptyInput {
        /// Position in the supplied clip sequence.
        index: usize,
        /// Caller-supplied path.
        path: PathBuf,
    },
    /// Bounded input or receipt storage could not be reserved.
    Allocation(&'static str),
    /// The explicit locator could not admit the configured ffmpeg.
    FfmpegUnavailable(FfmpegLocatorError),
    /// Tool binding, bounded process execution or atomic publication failed.
    Boundary(BoundaryError),
}

impl fmt::Display for VideoConcatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOptions(detail) => write!(f, "video concatenation options: {detail}"),
            Self::Capability(detail) => write!(
                f,
                "video concatenation capability: {detail}; {}",
                fmn_output::NATIVE_ALTERNATIVE,
            ),
            Self::DestinationExists(path) => write!(
                f,
                "video concatenation destination already exists: {}",
                path.display(),
            ),
            Self::Output(error) => write!(f, "video concatenation output: {error}"),
            Self::Input { index, error } => {
                write!(f, "video concatenation input {index}: {error}")
            }
            Self::EmptyInput { index, path } => write!(
                f,
                "video concatenation input {index} is empty: {}",
                path.display(),
            ),
            Self::Allocation(detail) => write!(f, "video concatenation allocation: {detail}"),
            Self::FfmpegUnavailable(error) => {
                write!(
                    f,
                    "video concatenation ffmpeg capability: {error}; {}",
                    fmn_output::NATIVE_ALTERNATIVE,
                )
            }
            Self::Boundary(error) => write!(f, "video concatenation: {error}"),
        }
    }
}

impl std::error::Error for VideoConcatError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Output(error) | Self::Input { error, .. } => Some(error),
            Self::FfmpegUnavailable(error) => Some(error),
            Self::Boundary(error) => Some(error),
            Self::InvalidOptions(_)
            | Self::Capability(_)
            | Self::DestinationExists(_)
            | Self::EmptyInput { .. }
            | Self::Allocation(_) => None,
        }
    }
}

/// Assemble encoded clips in order and publish a new MP4/MOV atomically.
///
/// Enables host tool discovery only when the `ffmpeg` Cargo feature is enabled
/// or `options.ffmpeg` supplies explicit authority. Existing destinations are
/// never overwritten, including when another writer wins during encoding.
///
/// ```no_run
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::path::PathBuf;
/// use fmn::prelude::*;
///
/// let clips = ["intro.mp4", "scene.mp4", "outro.mp4"].map(PathBuf::from);
/// // Enable the ffmpeg Cargo feature, or set options.ffmpeg explicitly.
/// let options = VideoConcatOptions::new("complete.mp4");
/// let report = concatenate_video(&clips, options)?;
/// assert_eq!(report.inputs.len(), 3);
/// # Ok(())
/// # }
/// ```
///
/// # Errors
/// Refuses invalid limits, certified mode, an existing destination, missing or
/// oversized source clips, missing process authority and governed tool failures.
/// Compatible encoded streams remain the caller's responsibility.
pub fn concatenate_video(
    inputs: &[PathBuf],
    #[cfg_attr(not(feature = "ffmpeg"), allow(unused_mut))] mut options: VideoConcatOptions,
) -> Result<VideoConcatReport, VideoConcatError> {
    #[cfg(feature = "ffmpeg")]
    if options.ffmpeg.is_none() {
        options.ffmpeg = Some(FfmpegCapability::host());
    }
    concatenate_video_with_fs(inputs, options, Arc::new(StdFs))
}

/// Assemble clips read through an explicit source filesystem capability.
///
/// The filesystem controls only source reads. The governed ffmpeg work
/// directory and published destination are on the host filesystem, as for
/// native video rendering; a virtual source filesystem does not redirect the
/// output. This entry point never discovers host process authority implicitly.
///
/// Each input position is read once with a byte bound and frozen before tool
/// resolution. The hashes in the receipt therefore identify precisely the
/// bytes used even if an original source changes during tool discovery or
/// concatenation. Source names never enter the ffmpeg concat-list grammar.
///
/// # Errors
/// The same refusals as [`concatenate_video`]. All option checks and bounded
/// source reads complete before ffmpeg discovery or a process invocation.
pub fn concatenate_video_with_fs(
    inputs: &[PathBuf],
    options: VideoConcatOptions,
    fs: Arc<dyn FileSystem>,
) -> Result<VideoConcatReport, VideoConcatError> {
    let limits = validate_options(inputs, &options)?;
    let capability = options.ffmpeg.as_ref().ok_or(VideoConcatError::Capability(
        "ffmpeg is required to concatenate encoded clips; supply FfmpegCapability or enable the ffmpeg feature for concatenate_video",
    ))?;
    let destination = std::path::absolute(&options.output).map_err(|err| {
        VideoConcatError::Output(FsError::Io {
            path: options.output.clone(),
            err,
        })
    })?;
    if StdFs
        .node_kind_no_follow(&destination)
        .map_err(VideoConcatError::Output)?
        .is_some()
    {
        return Err(VideoConcatError::DestinationExists(destination));
    }

    let mut frozen = Vec::new();
    frozen
        .try_reserve_exact(inputs.len())
        .map_err(|_| VideoConcatError::Allocation("input snapshots"))?;
    let mut reports = Vec::new();
    reports
        .try_reserve_exact(inputs.len())
        .map_err(|_| VideoConcatError::Allocation("input receipts"))?;
    let mut total_input_bytes = 0_u64;
    for (index, path) in inputs.iter().enumerate() {
        let remaining = options
            .max_total_input_bytes
            .checked_sub(total_input_bytes)
            .ok_or(VideoConcatError::InvalidOptions(
                "combined source bytes exceed max_total_input_bytes",
            ))?;
        let cap = usize::try_from(options.max_input_bytes.min(remaining)).map_err(|_| {
            VideoConcatError::InvalidOptions("source byte limit exceeds the addressable range")
        })?;
        let bytes = fs
            .read_bounded(path, cap)
            .map_err(|error| VideoConcatError::Input { index, error })?;
        // Keep this admission explicit even for caller-defined FS capabilities.
        if bytes.len() > cap {
            return Err(VideoConcatError::Input {
                index,
                error: FsError::TooLarge {
                    path: path.clone(),
                    limit: cap,
                },
            });
        }
        if bytes.is_empty() {
            return Err(VideoConcatError::EmptyInput {
                index,
                path: path.clone(),
            });
        }
        let source_bytes = u64::try_from(bytes.len()).map_err(|_| {
            VideoConcatError::InvalidOptions("source length exceeds the receipt range")
        })?;
        total_input_bytes =
            total_input_bytes
                .checked_add(source_bytes)
                .ok_or(VideoConcatError::InvalidOptions(
                    "combined source byte count overflow",
                ))?;
        reports.push(VideoClipReport {
            source_path: path.clone(),
            bytes: source_bytes,
            digest: sha256(&bytes),
        });
        frozen.push(bytes);
    }

    let mut slices = Vec::new();
    slices
        .try_reserve_exact(frozen.len())
        .map_err(|_| VideoConcatError::Allocation("input references"))?;
    slices.extend(frozen.iter().map(Vec::as_slice));

    let executable = capability
        .locator
        .locate_ffmpeg(&options.ffmpeg_bin)
        .map_err(VideoConcatError::FfmpegUnavailable)?;
    let tool = FfmpegTool::resolve(
        executable,
        capability.runner.as_ref(),
        &capability.workdir_root,
    )
    .map_err(VideoConcatError::Boundary)?;
    let limit_report = FfmpegLimitsReport::new(&limits, options.max_total_input_bytes);
    let boundary = Boundary::new(
        tool,
        Arc::clone(&capability.runner),
        limits,
        capability.workdir_root.clone(),
    )
    .map_err(VideoConcatError::Boundary)?;
    let prepared = boundary
        .prepare_concat_bytes(&slices, &destination, options.max_total_input_bytes)
        .map_err(VideoConcatError::Boundary)?;
    drop(slices);
    drop(frozen);
    let boundary = prepared.commit_new().map_err(VideoConcatError::Boundary)?;
    Ok(VideoConcatReport {
        inputs: reports,
        total_input_bytes,
        boundary,
        limits: limit_report,
    })
}

fn validate_options(
    inputs: &[PathBuf],
    options: &VideoConcatOptions,
) -> Result<JobLimits, VideoConcatError> {
    if options.determinism == DeterminismMode::Certified {
        return Err(VideoConcatError::Capability(
            "encoded video is outside the certified artifact set",
        ));
    }
    if inputs.is_empty() {
        return Err(VideoConcatError::InvalidOptions(
            "at least one input clip is required",
        ));
    }
    if options.max_inputs == 0
        || options.max_inputs > fmn_output::ffmpeg::MAX_CONCAT_INPUTS
        || inputs.len() > options.max_inputs
    {
        return Err(VideoConcatError::InvalidOptions(
            "input count exceeds max_inputs, or max_inputs is outside 1..=4093",
        ));
    }
    for limit in [options.max_input_bytes, options.max_total_input_bytes] {
        if limit == 0 {
            return Err(VideoConcatError::InvalidOptions(
                "source byte limits must be nonzero",
            ));
        }
        if usize::try_from(limit)
            .ok()
            .is_none_or(|limit| limit > isize::MAX as usize)
        {
            return Err(VideoConcatError::InvalidOptions(
                "source byte limit exceeds the addressable range",
            ));
        }
    }
    let extension = options.output.extension().and_then(|value| value.to_str());
    if !extension
        .is_some_and(|value| value.eq_ignore_ascii_case("mp4") || value.eq_ignore_ascii_case("mov"))
    {
        return Err(VideoConcatError::InvalidOptions(
            "destination must have an MP4 or MOV extension",
        ));
    }
    // Source-memory and encoded-output ceilings are independent. The shared
    // helper validates process bounds without capping output at source length.
    admit_render_ffmpeg_limits(&options.ffmpeg_limits, u64::MAX)
        .map_err(VideoConcatError::InvalidOptions)
}
