//! Standalone sound-bearing FMTL output, composed from native replay and Reel.
//!
//! The generated CLI parser and config resolver remain authoritative. This
//! executable adapter does not construct a Scene, remap its clock, decode source
//! media, or mix again. Picture-only recordings keep the existing CLI route.
//! Successful output includes a sealed FMNP closure in a create-only sidecar.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fmn::rendering::{FfmpegCapability, RenderError, RenderFormat, RenderOptions, RenderReport};
use fmn_cli::{AnimationRange, Invocation, OutputFormat, RenderCommand, RunOutput};
use fmn_codec::{SampleFormat, WavAudio};
use fmn_config::config::{DeterminismMode, Engine};
use fmn_output::{
    DitherPolicy, MixKernel, MixReport, WavPublicationConfig, frames_to_samples, publish_wav_new,
};
use fmn_platform::fs::{FileSystem, FsNodeKind, StdFs};
use fmn_scene::timeline_bundle::BundleSoundtrack;
use fmn_scene::{DEFAULT_MAX_BUNDLE_BYTES, OutputNaming, TimelineBundle};

mod provenance;
#[cfg(test)]
mod tests;

const MAX_OUTPUTS: usize = 4096;

#[derive(Debug)]
struct Failure {
    identity: &'static str,
    message: String,
}

impl Failure {
    fn new(identity: &'static str, message: impl Into<String>) -> Self {
        Self {
            identity,
            message: message.into(),
        }
    }
}

impl From<fmn_cli::CliError> for Failure {
    fn from(error: fmn_cli::CliError) -> Self {
        Self::new(error.exit_name(), error.to_string())
    }
}

impl From<RenderError> for Failure {
    fn from(error: RenderError) -> Self {
        let identity = match &error {
            RenderError::Capability(_)
            | RenderError::FfmpegUnavailable(_)
            | RenderError::EncoderUnavailable(_) => "capability",
            RenderError::InvalidOptions(_)
            | RenderError::Negotiation(_)
            | RenderError::Color(_)
            | RenderError::Camera(_) => "config",
            _ => "render",
        };
        Self::new(identity, error.to_string())
    }
}

fn code(identity: &str) -> u8 {
    fmn_cli::EXIT_CODE_SPECS
        .iter()
        .find(|entry| entry.name == identity)
        .map_or(fmn_cli::internal_exit_code(), |entry| entry.code)
}

/// Only take responsibility for a successfully decoded sound-bearing recording.
/// Parse/read/decode failures and picture-only input stay with the ordinary CLI.
pub(crate) fn try_run_os(arguments: &[OsString]) -> Option<RunOutput> {
    let arguments = arguments
        .iter()
        .map(|arg| arg.to_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()?;
    let Invocation::Render(command) = fmn_cli::parse_args(arguments).ok()? else {
        return None;
    };
    let source = command.file.as_deref()?;
    if !source
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("fmtl"))
    {
        return None;
    }
    let fs: Arc<dyn FileSystem> = Arc::new(StdFs);
    let bytes = fs.read_bounded(source, DEFAULT_MAX_BUNDLE_BYTES).ok()?;
    let bundle = TimelineBundle::from_bytes(&bytes).ok()?;
    if !bundle.has_soundtrack() {
        return None;
    }
    Some(run(command, &bytes, bundle, fs, &|| {
        Some(FfmpegCapability {
            runner: Arc::new(fmn_platform::process::StdProcessRunner),
            locator: Arc::new(fmn_platform::process::StdFfmpegLocator::from_host_path()),
            workdir_root: std::env::temp_dir(),
        })
    }))
}

#[derive(Clone, Copy)]
enum Target {
    Wav,
    Video(RenderFormat),
}

impl Target {
    fn extension(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Video(RenderFormat::Mov) => "mov",
            Self::Video(_) => "mp4",
        }
    }
}

struct Request {
    config: fmn_config::Config,
    target: Target,
    scene: String,
    outputs: Vec<(PathBuf, Option<Range<u32>>)>,
    segments: Range<usize>,
    frames: Range<u32>,
}

/// Resolve the schema's exclusive play range against stored segment ordinals.
/// Duration arithmetic never participates: the recording owns every boundary.
fn selected_ranges(
    bundle: &TimelineBundle,
    selection: Option<AnimationRange>,
    max_frames: u64,
) -> Result<(Range<usize>, Range<u32>), Failure> {
    let total = bundle.segment_count();
    let (start, end) = match selection {
        Some(range) => {
            let start = usize::try_from(range.start)
                .map_err(|_| Failure::new("usage", "play-range start exceeds this recording"))?;
            let end = range
                .end
                .map_or(Ok(total), usize::try_from)
                .map_err(|_| Failure::new("usage", "play-range end exceeds this recording"))?;
            (start, end)
        }
        None => (0, total),
    };
    if start >= end || end > total {
        return Err(Failure::new(
            "usage",
            format!(
                "compiled play range must be nonempty and lie inside 0..{total}; END is exclusive"
            ),
        ));
    }
    let first = bundle
        .segment_frame_range(start)
        .ok_or_else(|| Failure::new("scene", "missing compiled start segment"))?;
    let last = bundle
        .segment_frame_range(end - 1)
        .ok_or_else(|| Failure::new("scene", "missing compiled end segment"))?;
    let frames = first.start..last.end;
    if frames.is_empty() {
        return Err(Failure::new(
            "scene",
            "selected compiled segments contain no captured frames",
        ));
    }
    if u64::from(frames.end - frames.start) > max_frames {
        return Err(Failure::new(
            "budget",
            "selected compiled schedule exceeds the output frame budget",
        ));
    }
    Ok((start..end, frames))
}

fn prepare(
    command: &RenderCommand,
    bundle: &TimelineBundle,
    fs: &dyn FileSystem,
) -> Result<Request, Failure> {
    if command.write_all || command.scene_names.len() > 1 {
        return Err(Failure::new(
            "usage",
            "an FMTL artifact contains one compiled scene; --write_all and multiple names do not apply",
        ));
    }
    if command.skip_animations
        || command.presenter_mode
        || command.full_screen
        || command.autoreload
        || command.embed_line.is_some()
        || command.open
        || command.finder
        || command.show_animation_progress
        || command.leave_progress_bars
    {
        return Err(Failure::new(
            "capability",
            "narrated FMTL file replay does not support skip, live-window, opener or progress-bar controls; -n selects recorded segments and --subdivide emits them separately",
        ));
    }
    let video_format = if command.transparent {
        RenderFormat::Mov
    } else {
        RenderFormat::Mp4
    };
    let target = match command.format {
        OutputFormat::Wav => Target::Wav,
        OutputFormat::Video => Target::Video(video_format),
        OutputFormat::Auto if command.write_file => Target::Video(video_format),
        _ => {
            return Err(Failure::new(
                "capability",
                "sound-bearing FMTL requires --format video (or -w) or --format wav; picture-only output cannot discard its soundtrack",
            ));
        }
    };
    if matches!(target, Target::Wav)
        && (command.transparent
            || command.vcodec.is_some()
            || command.pix_fmt.is_some()
            || command.video_quality != fmn::rendering::VideoQuality::default()
            || command.audio_bitrate.is_some()
            || command.common.ffmpeg.is_some()
            || command.ffmpeg_limits != fmn_cli::FfmpegLimitOptions::default())
    {
        return Err(Failure::new(
            "usage",
            "video/ffmpeg controls do not apply to native FMTL-to-WAV extraction",
        ));
    }
    if command.transparent {
        if command.pix_fmt.as_deref().is_some_and(|format| {
            !matches!(
                format.to_ascii_lowercase().as_str(),
                "rgba" | "rgba8" | "bgra" | "bgra8"
            )
        }) {
            return Err(Failure::new(
                "config",
                "transparent video requires an rgba or bgra wire format",
            ));
        }
        if command
            .vcodec
            .as_deref()
            .is_some_and(|codec| !codec.eq_ignore_ascii_case("auto") && codec != "qtrle")
        {
            return Err(Failure::new(
                "config",
                "transparent video requires qtrle or auto; an explicit encoder cannot be silently replaced",
            ));
        }
    }
    if bundle.has_camera_track() && command.background.is_some() {
        return Err(Failure::new(
            "capability",
            "a recorded FMTL camera/background track cannot be overridden by --color",
        ));
    }
    if command.fps.is_some_and(|fps| fps != bundle.fps()) {
        return Err(Failure::new(
            "config",
            format!(
                "--fps disagrees with the recording's fixed {} fps schedule",
                bundle.fps()
            ),
        ));
    }
    let mut effective = command.clone();
    effective.fps = Some(bundle.fps());
    let config = fmn_cli::resolve_render_config(fs, &effective)?;
    if command.common.reproducible || config.determinism.mode == DeterminismMode::Certified {
        return Err(Failure::new(
            "capability",
            "narrated standalone replay currently admits standard-mode manifests only; --reproducible is not supported by this adapter",
        ));
    }
    if config.render.engine != Engine::Cpu {
        return Err(Failure::new(
            "capability",
            "narrated FMTL replay requires render.engine=cpu; an accelerator request is never silently substituted",
        ));
    }
    if matches!(target, Target::Video(_))
        && (config.file_writer.gamma != 1.0 || config.file_writer.saturation != 1.0)
    {
        return Err(Failure::new(
            "capability",
            "non-default file_writer gamma/saturation have no native color-transform adapter",
        ));
    }
    if !bundle.has_soundtrack() {
        return Err(Failure::new(
            "scene",
            "recording has no embedded soundtrack",
        ));
    }
    let (segments, frames) = selected_ranges(
        bundle,
        command.animation_range,
        fmn_scene::DEFAULT_MAX_BUNDLE_EXPORT_FRAMES,
    )?;
    let source = command
        .file
        .as_deref()
        .ok_or_else(|| Failure::new("usage", "missing FMTL source"))?;
    let scene = command.scene_names.first().cloned().unwrap_or_else(|| {
        source
            .file_stem()
            .and_then(OsStr::to_str)
            .unwrap_or("scene")
            .to_owned()
    });
    if scene.is_empty()
        || scene == "."
        || scene == ".."
        || scene.contains('/')
        || scene.contains('\\')
        || scene.chars().any(char::is_control)
    {
        return Err(Failure::new(
            "usage",
            "compiled scene name must be one nonempty path component",
        ));
    }
    let mut directory = PathBuf::from(&config.directories.base);
    if let Some((_, output)) = config
        .directories
        .subdirs
        .iter()
        .find(|(name, _)| name == "output")
    {
        directory.push(output);
    }
    if directory.as_os_str().is_empty() {
        directory = PathBuf::from(".");
    }
    let naming = OutputNaming {
        output_directory: command.video_dir.clone().unwrap_or(directory),
        file_name: command.file_name.as_deref().map(PathBuf::from),
        start_at_play: command.animation_range.map(|range| range.start),
        end_at_play: command.animation_range.and_then(|range| range.end),
        open_on_completion: false,
    };
    let mut outputs = Vec::new();
    if command.subdivide {
        if segments.len() > MAX_OUTPUTS {
            return Err(Failure::new(
                "budget",
                "selected compiled audio subdivision exceeds 4096 output generations",
            ));
        }
        for index in segments.clone() {
            let range = bundle
                .segment_frame_range(index)
                .ok_or_else(|| Failure::new("scene", "missing compiled segment range"))?;
            if range.is_empty() {
                return Err(Failure::new(
                    "scene",
                    "a selected zero-frame segment cannot produce an output generation",
                ));
            }
            outputs.push((
                naming.partial_artifact(&scene, index as u64, target.extension()),
                Some(range),
            ));
        }
    } else {
        outputs.push((
            naming.artifact(&scene, target.extension()),
            command.animation_range.map(|_| frames.clone()),
        ));
    }
    for (path, _) in &outputs {
        preflight_destination(fs, path)?;
        provenance::preflight(fs, path)?;
    }
    Ok(Request {
        config,
        target,
        scene,
        outputs,
        segments,
        frames,
    })
}

fn preflight_destination(fs: &dyn FileSystem, path: &Path) -> Result<(), Failure> {
    // Advisory admission only; Reel still performs the atomic create-new.
    // Classifying the leaf catches dangling links that exists() misses.
    let parents: Vec<_> = path.ancestors().skip(1).collect();
    for parent in parents.into_iter().rev() {
        if parent.as_os_str().is_empty() {
            continue;
        }
        match fs
            .node_kind_no_follow(parent)
            .map_err(|error| Failure::new("render", error.to_string()))?
        {
            None | Some(FsNodeKind::Directory) => {}
            Some(_) => {
                return Err(Failure::new(
                    "render",
                    format!(
                        "output parent is not a real directory: {}",
                        parent.display()
                    ),
                ));
            }
        }
    }
    if fs
        .node_kind_no_follow(path)
        .map_err(|error| Failure::new("render", error.to_string()))?
        .is_some()
    {
        return Err(Failure::new(
            "render",
            format!("output already exists: {}", path.display()),
        ));
    }
    Ok(())
}

fn run(
    command: RenderCommand,
    bytes: &[u8],
    bundle: TimelineBundle,
    fs: Arc<dyn FileSystem>,
    capability: &dyn Fn() -> Option<FfmpegCapability>,
) -> RunOutput {
    let mut stdout = String::new();
    let result = execute(&command, bytes, bundle, fs, capability, &mut stdout);
    match result {
        Ok(()) => RunOutput {
            code: code("success"),
            stdout,
            stderr: String::new(),
        },
        Err(error) => {
            let status = code(error.identity);
            if command.common.robot {
                let _ = writeln!(
                    stdout,
                    "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"error\",\"exit\":{{\"code\":{status},\"identity\":{}}},\"message\":{}}}",
                    fmn_cli::ROBOT_SCHEMA_VERSION,
                    quote(error.identity),
                    quote(&error.message),
                );
            }
            RunOutput {
                code: status,
                stdout,
                stderr: if command.common.robot {
                    String::new()
                } else {
                    format!("fmn: {}: {}\n", error.identity, error.message)
                },
            }
        }
    }
}

fn execute(
    command: &RenderCommand,
    bytes: &[u8],
    mut bundle: TimelineBundle,
    fs: Arc<dyn FileSystem>,
    capability: &dyn Fn() -> Option<FfmpegCapability>,
    stdout: &mut String,
) -> Result<(), Failure> {
    let request = prepare(command, &bundle, fs.as_ref())?;
    let fps = bundle.fps();
    let total_frames = bundle.frame_count();
    let source_digest = fmn_hash::sha256(bytes);
    let mut soundtrack = bundle.take_soundtrack();
    let provenance = provenance::Session::new(
        &request.config,
        source_digest,
        &bundle,
        soundtrack.as_ref().ok_or_else(|| {
            Failure::new("scene", "recording has no embedded soundtrack")
        })?,
        fs.as_ref(),
    )?;
    let source = command
        .file
        .as_deref()
        .ok_or_else(|| Failure::new("usage", "missing FMTL source"))?;
    if command.prerun {
        let selected_frames = request.frames.end - request.frames.start;
        if command.common.robot {
            let _ = writeln!(
                stdout,
                "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"prerun\",\"source\":\"compiled\",\"frames\":{selected_frames},\"segments\":{},\"source_frames\":[{},{}],\"source_segments\":[{},{}]}}",
                fmn_cli::ROBOT_SCHEMA_VERSION,
                request.segments.len(),
                request.frames.start,
                request.frames.end,
                request.segments.start,
                request.segments.end,
            );
        } else if !command.common.quiet {
            let _ = writeln!(
                stdout,
                "compiled selection: {selected_frames} frames, {} segments; no scene executed",
                request.segments.len(),
            );
        }
    }
    let video = matches!(request.target, Target::Video(_));
    let needs_camera = video && !bundle.has_camera_track() && bundle.requires_camera();
    // Convert the complete validated recording once, not once per output.
    // WAV releases pictures without freezing them or obtaining process access.
    let shared = if video {
        Some(
            bundle
                .into_shared()
                .map_err(|error| Failure::new("scene", error.to_string()))?,
        )
    } else {
        drop(bundle);
        None
    };
    let ffmpeg = if video { capability() } else { None };
    for (index, (destination, selection)) in request.outputs.iter().enumerate() {
        let range = selection.clone().unwrap_or(0..total_frames);
        let segments = if command.subdivide {
            let ordinal = request.segments.start + index;
            ordinal..ordinal + 1
        } else {
            request.segments.clone()
        };
        let (artifact_bytes, digest, frames, rate, channels, samples, video, publication) =
            match request.target {
                Target::Wav => {
                    let pcm = wav_window(&mut soundtrack, fps, &range, selection.is_none())?;
                    let rate = pcm.sample_rate();
                    let channels = pcm.channels();
                    let samples = pcm.sample_frames();
                    let mix = MixReport {
                        audio: WavAudio {
                            channels,
                            sample_rate: rate,
                            format: SampleFormat::F32,
                            samples: pcm.into_samples(),
                        },
                        clipped_samples: 0,
                        cues_mixed: 1,
                        workers_used: 0,
                        kernel: MixKernel::Scalar,
                    };
                    let report = publish_wav_new(
                        fs.as_ref(),
                        &WavPublicationConfig {
                            destination: destination.clone(),
                            format: SampleFormat::S16,
                            dither: DitherPolicy::None,
                            max_artifact_bytes: DEFAULT_MAX_BUNDLE_BYTES as u64,
                            profile: None,
                        },
                        &mix,
                    )
                    .map_err(|error| Failure::new("render", error.to_string()))?;
                    let publication = provenance.publish(
                        Arc::clone(&fs),
                        provenance::Artifact {
                            destination,
                            target: request.target,
                            digest: report.digest,
                            frames: range.clone(),
                            segments: segments.clone(),
                            keep_tail: selection.is_none(),
                            sample_rate: rate,
                            channels,
                            sample_frames: report.sample_frames,
                            video: None,
                        },
                    )?;
                    (
                        report.bytes,
                        report.digest.to_hex(),
                        0,
                        rate,
                        channels,
                        samples,
                        String::new(),
                        publication,
                    )
                }
                Target::Video(format) => {
                    let pictures = shared.as_ref().ok_or_else(|| {
                        Failure::new("internal", "missing prepared compiled pictures")
                    })?;
                    let mut options = RenderOptions::with_format(destination.clone(), format)
                        .map_err(|error| Failure::new("config", error.to_string()))?;
                    options.config = request.config.clone();
                    options.typeset_cache = false;
                    options.max_frames = fmn_scene::DEFAULT_MAX_BUNDLE_EXPORT_FRAMES;
                    let (limits, input_bytes) = command.ffmpeg_limits.effective_limits()?;
                    options.ffmpeg_limits = limits;
                    options.max_output_bytes = input_bytes;
                    options.ffmpeg = ffmpeg.clone();
                    if needs_camera {
                        options.camera = Some(options.camera_config()?);
                    }
                    let report = options.render_shared_bundle_with_fs(
                        pictures,
                        soundtrack.as_ref(),
                        selection.clone(),
                        Arc::clone(&fs),
                    )?;
                    let sound = report.artifact.soundtrack.as_ref().ok_or_else(|| {
                        Failure::new(
                            "internal",
                            format!(
                                "video {} was published but omitted its soundtrack receipt",
                                destination.display()
                            ),
                        )
                    })?;
                    let publication = provenance.publish(
                        Arc::clone(&fs),
                        provenance::Artifact {
                            destination,
                            target: request.target,
                            digest: report.artifact.digest,
                            frames: range.clone(),
                            segments: segments.clone(),
                            keep_tail: selection.is_none(),
                            sample_rate: sound.sample_rate,
                            channels: sound.channels,
                            sample_frames: sound.sample_frames,
                            video: Some(&report),
                        },
                    )?;
                    (
                        report.artifact.bytes,
                        report.artifact.digest.to_hex(),
                        report.artifact.frame_count,
                        sound.sample_rate,
                        sound.channels,
                        sound.sample_frames,
                        video_facts(&report),
                        publication,
                    )
                }
            };
        // Success requires both media and its complete manifest generation.
        // A later failure never rolls back or overwrites earlier outputs.
        if command.common.robot {
            let subdivision = if command.subdivide {
                format!(",\"subdivision\":{}", segments.start)
            } else {
                String::new()
            };
            let manifest = publication.json();
            let certified = publication.certified;
            let _ = writeln!(
                stdout,
                "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"bundle-audio-render\",\"source\":\"compiled\",\"source_artifact\":{},\"source_sha256\":{},\"scene\":{},\"artifact\":{},\"format\":{},\"bytes\":{artifact_bytes},\"sha256\":{},\"frames\":{frames},\"fps\":{fps},\"source_frames\":[{},{}],\"source_segments\":[{},{}],\"certified\":{certified},\"manifest\":{manifest},\"soundtrack\":{{\"sample_rate\":{rate},\"channels\":{channels},\"sample_frames\":{samples}}}{subdivision}{video}}}",
                fmn_cli::ROBOT_SCHEMA_VERSION,
                quote(&source.to_string_lossy()),
                quote(&source_digest.to_hex()),
                quote(&request.scene),
                quote(&destination.to_string_lossy()),
                quote(request.target.extension()),
                quote(&digest),
                range.start,
                range.end,
                segments.start,
                segments.end,
            );
        } else if !command.common.quiet {
            let _ = writeln!(
                stdout,
                "wrote {}: {artifact_bytes} bytes; {samples} audio frames at {rate} Hz / {channels} channels; manifest {}",
                destination.display(),
                publication.path.display(),
            );
        }
    }
    Ok(())
}

fn wav_window(
    soundtrack: &mut Option<BundleSoundtrack>,
    fps: u32,
    range: &Range<u32>,
    full: bool,
) -> Result<BundleSoundtrack, Failure> {
    let rate = soundtrack
        .as_ref()
        .ok_or_else(|| Failure::new("scene", "recording has no embedded soundtrack"))?
        .sample_rate();
    let boundary = |frame| {
        frames_to_samples(i64::from(frame), fps, rate)
            .map_err(|error| Failure::new("render", error.to_string()))
            .and_then(|count| {
                u64::try_from(count).map_err(|_| Failure::new("scene", "negative audio boundary"))
            })
    };
    let end = boundary(range.end)?;
    if full {
        return soundtrack
            .take()
            .ok_or_else(|| Failure::new("scene", "recording has no embedded soundtrack"))?
            .pad_to(end, DEFAULT_MAX_BUNDLE_BYTES)
            .map_err(|error| Failure::new("budget", error.to_string()));
    }
    let start = boundary(range.start)?;
    let count = end
        .checked_sub(start)
        .ok_or_else(|| Failure::new("scene", "reversed audio interval"))?;
    soundtrack
        .as_ref()
        .ok_or_else(|| Failure::new("scene", "recording has no embedded soundtrack"))?
        .sample_window(start, count, DEFAULT_MAX_BUNDLE_BYTES)
        .map_err(|error| Failure::new("budget", error.to_string()))
}

fn video_facts(report: &RenderReport) -> String {
    let invocations = report.artifact.ffmpeg.iter().map(|invocation| {
        let p = &invocation.provenance;
        let argv = p.argv.iter().map(|arg| quote(arg)).collect::<Vec<_>>().join(",");
        format!(
            "{{\"path\":{},\"sha256\":{},\"version\":{},\"bound_path\":{},\"process_mechanism\":{},\"process_policy_version\":{},\"argv\":[{argv}]}}",
            quote(&p.tool_path.to_string_lossy()), quote(&p.tool_sha256_hex), quote(&p.tool_version),
            quote(&p.bound_tool_path.to_string_lossy()), quote(&p.process_mechanism), p.process_policy_version,
        )
    }).collect::<Vec<_>>().join(",");
    let limits = report
        .artifact
        .ffmpeg_limits
        .as_ref()
        .map_or_else(|| "null".to_owned(), |limits| limits.to_json());
    let pipeline = report.frame_pipeline.as_ref().map_or_else(|| "null".to_owned(), |p| {
        format!(
            "{{\"submitted\":{},\"emitted\":{},\"outstanding_slots\":{},\"max_in_flight\":{},\"render_team_frames\":{:?}}}",
            p.submitted, p.emitted, p.outstanding_slots, p.max_in_flight, p.render_team_frames,
        )
    });
    format!(
        ",\"ffmpeg_invocations\":[{invocations}],\"ffmpeg_limits\":{limits},\"frame_pipeline\":{pipeline}"
    )
}

fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch <= '\u{1f}' => {
                let _ = write!(out, "\\u{:04x}", u32::from(ch));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}
