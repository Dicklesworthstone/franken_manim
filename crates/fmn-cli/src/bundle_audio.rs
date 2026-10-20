//! Standalone sound-bearing FMTL output, composed from native replay and Reel.
//!
//! The generated CLI parser and config resolver remain authoritative. This
//! executable adapter does not construct a Scene, remap its clock, decode source
//! media, or mix again. Picture-only recordings keep the existing CLI route.
//! Receipts explicitly do not claim an input-closure manifest/certification.

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
            "narrated standalone FMTL replay currently publishes artifact receipts, not full input-closure manifests; --reproducible is not supported by this adapter",
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
    let source_digest = fmn_hash::sha256(bytes).to_hex();
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
    let mut soundtrack = bundle.take_soundtrack();
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
        let (artifact_bytes, digest, frames, rate, channels, samples, video) = match request.target
        {
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
                (
                    report.bytes,
                    report.digest.to_hex(),
                    0,
                    rate,
                    channels,
                    samples,
                    String::new(),
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
                (
                    report.artifact.bytes,
                    report.artifact.digest.to_hex(),
                    report.artifact.frame_count,
                    sound.sample_rate,
                    sound.channels,
                    sound.sample_frames,
                    video_facts(&report),
                )
            }
        };
        // Emit only after native publication. Earlier clip receipts survive a
        // later failure; no successful output is rolled back or overwritten.
        if command.common.robot {
            let segments = if command.subdivide {
                let ordinal = request.segments.start + index;
                ordinal..ordinal + 1
            } else {
                request.segments.clone()
            };
            let subdivision = if command.subdivide {
                format!(",\"subdivision\":{}", segments.start)
            } else {
                String::new()
            };
            let _ = writeln!(
                stdout,
                "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"bundle-audio-render\",\"source\":\"compiled\",\"source_artifact\":{},\"source_sha256\":{},\"scene\":{},\"artifact\":{},\"format\":{},\"bytes\":{artifact_bytes},\"sha256\":{},\"frames\":{frames},\"fps\":{fps},\"source_frames\":[{},{}],\"source_segments\":[{},{}],\"certified\":false,\"manifest\":null,\"soundtrack\":{{\"sample_rate\":{rate},\"channels\":{channels},\"sample_frames\":{samples}}}{subdivision}{video}}}",
                fmn_cli::ROBOT_SCHEMA_VERSION,
                quote(&source.to_string_lossy()),
                quote(&source_digest),
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
                "wrote {}: {artifact_bytes} bytes; {samples} audio frames at {rate} Hz / {channels} channels",
                destination.display(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_platform::fs::VirtualFs;

    fn command(extra: &[&str]) -> RenderCommand {
        let mut args = vec![
            "/scene.fmtl",
            "--format",
            "wav",
            "--video_dir",
            "/out",
            "--robot",
        ];
        args.extend_from_slice(extra);
        let Invocation::Render(command) = fmn_cli::parse_args(args).unwrap() else {
            panic!("render invocation");
        };
        command
    }

    fn recording() -> Vec<u8> {
        let mut stage = fmn::mobject::Stage::new();
        let mut timeline = fmn::animation::Timeline::new(3).unwrap();
        timeline.wait(1.0 / 3.0).unwrap();
        timeline.wait(1.0 / 3.0).unwrap();
        timeline.wait(1.0 / 3.0).unwrap();
        let pictures = fmn_scene::export_timeline_bundle(
            timeline,
            &mut stage,
            &fmn_core::rng::RngRoot::from_seed(0),
        )
        .unwrap();
        let pcm = BundleSoundtrack::new(10, 2, [0.25, -0.5].repeat(14)).unwrap();
        fmn_scene::timeline_bundle::attach_bundle_soundtrack(
            &pictures,
            &pcm,
            DEFAULT_MAX_BUNDLE_BYTES,
        )
        .unwrap()
    }

    #[test]
    fn full_wav_uses_embedded_pcm_and_preserves_tail_without_process_capability() {
        let fs = Arc::new(VirtualFs::new());
        let bytes = recording();
        let output = run(
            command(&[]),
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            fs.clone(),
            &|| panic!("WAV acquired ffmpeg"),
        );
        assert_eq!(output.code, 0, "{}", output.stderr);
        let wav = fs.read(Path::new("/out/scene.wav")).unwrap();
        let decoded = fmn_codec::decode_wav(&wav, &fmn_codec::WavLimits::default()).unwrap();
        assert_eq!(decoded.sample_rate, 10);
        assert_eq!(decoded.channels, 2);
        assert_eq!(decoded.samples, [0.25, -0.5].repeat(14));
        assert!(output.stdout.contains("\"sample_frames\":14"));
        assert!(output.stdout.contains("\"certified\":false"));
        assert!(output.stdout.contains("\"manifest\":null"));
    }

    #[test]
    fn subdivision_windows_join_without_a_sample_gap_or_duplicate() {
        let fs = Arc::new(VirtualFs::new());
        let bytes = recording();
        let bundle = TimelineBundle::from_bytes(&bytes).unwrap();
        let command = command(&["--subdivide"]);
        let planned = prepare(&command, &bundle, fs.as_ref()).unwrap();
        let paths: Vec<_> = planned
            .outputs
            .iter()
            .map(|(path, _)| path.clone())
            .collect();
        let output = run(command, &bytes, bundle, fs.clone(), &|| {
            panic!("WAV acquired ffmpeg")
        });
        assert_eq!(output.code, 0, "{}", output.stderr);
        let mut joined = Vec::new();
        let mut counts = Vec::new();
        for path in paths {
            let wav = fs.read(&path).unwrap();
            let audio = fmn_codec::decode_wav(&wav, &fmn_codec::WavLimits::default()).unwrap();
            counts.push(audio.samples.len() / 2);
            joined.extend(audio.samples);
        }
        assert_eq!(counts, [3, 4, 3]);
        assert_eq!(joined, [0.25, -0.5].repeat(10));
        assert_eq!(output.stdout.lines().count(), 3);
    }

    #[test]
    fn all_subdivision_destinations_are_preflighted_before_publication() {
        let fs = Arc::new(VirtualFs::new());
        let bytes = recording();
        let bundle = TimelineBundle::from_bytes(&bytes).unwrap();
        let command = command(&["--subdivide"]);
        let planned = prepare(&command, &bundle, fs.as_ref()).unwrap();
        fs.insert(planned.outputs[1].0.clone(), b"foreign generation".to_vec());
        let output = run(command, &bytes, bundle, fs.clone(), &|| {
            panic!("WAV acquired ffmpeg")
        });
        assert_ne!(output.code, 0);
        assert!(!fs.exists(&planned.outputs[0].0));
        assert_eq!(
            fs.read(&planned.outputs[1].0).unwrap(),
            b"foreign generation"
        );
    }

    #[test]
    fn file_parent_refuses_before_native_publication() {
        let fs = Arc::new(VirtualFs::new());
        fs.insert("/out", b"not a directory".to_vec());
        let bytes = recording();
        let output = run(
            command(&[]),
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            fs.clone(),
            &|| panic!("refusal acquired ffmpeg"),
        );
        assert_ne!(output.code, 0);
        assert_eq!(fs.read(Path::new("/out")).unwrap(), b"not a directory");
        assert!(!fs.exists(Path::new("/out/scene.wav")));
    }

    #[test]
    fn unsupported_modes_and_clock_overrides_refuse_before_output() {
        for extra in [
            vec!["--reproducible"],
            vec!["--fps", "4"],
            vec!["--write_all"],
        ] {
            let fs = Arc::new(VirtualFs::new());
            let bytes = recording();
            let output = run(
                command(&extra),
                &bytes,
                TimelineBundle::from_bytes(&bytes).unwrap(),
                fs.clone(),
                &|| panic!("refusal acquired ffmpeg"),
            );
            assert_ne!(output.code, 0, "{extra:?}");
            assert!(!fs.exists(Path::new("/out/scene.wav")));
        }
        // The generated parser owns this CLI refusal. It must not be passed
        // through the helper that deliberately requires a valid invocation.
        let error = fmn_cli::parse_args(["/scene.fmtl", "--format", "wav", "--vcodec", "libx264"])
            .unwrap_err();
        assert_eq!(error.exit_name(), "usage");
        assert_eq!(error.rule(), Some("native-format-vs-codec"));
        let mut incompatible = command(&[]);
        incompatible.vcodec = Some("libx264".to_owned());
        let fs = Arc::new(VirtualFs::new());
        let bytes = recording();
        let output = run(
            incompatible,
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            fs.clone(),
            &|| panic!("codec refusal acquired ffmpeg"),
        );
        assert_eq!(output.code, code("usage"));
        assert!(!fs.exists(Path::new("/out/scene.wav")));
    }

    #[test]
    fn quiet_success_suppresses_human_decoration_not_robot_receipts() {
        let bytes = recording();
        for robot in [false, true] {
            let mut command = command(&["--quiet"]);
            command.common.robot = robot;
            let output = run(
                command,
                &bytes,
                TimelineBundle::from_bytes(&bytes).unwrap(),
                Arc::new(VirtualFs::new()),
                &|| None,
            );
            assert_eq!(output.code, 0);
            assert_eq!(output.stdout.is_empty(), !robot);
        }
    }

    #[test]
    fn alpha_video_does_not_discard_explicit_encoder_or_wire_requests() {
        let fs = VirtualFs::new();
        let bundle = TimelineBundle::from_bytes(&recording()).unwrap();
        let mut command = command(&[]);
        command.format = OutputFormat::Video;
        command.transparent = true;
        command.vcodec = Some("libx264".to_owned());
        assert!(prepare(&command, &bundle, &fs).is_err());
        command.vcodec = Some("qtrle".to_owned());
        command.pix_fmt = Some("nv12".to_owned());
        assert!(prepare(&command, &bundle, &fs).is_err());
        command.pix_fmt = Some("rgba".to_owned());
        assert!(prepare(&command, &bundle, &fs).is_ok());
    }

    #[test]
    fn json_strings_escape_controls_quotes_and_backslashes() {
        assert_eq!(quote("a\"\\\n\u{0000}\t界"), "\"a\\\"\\\\\\n\\u0000\\t界\"");
    }

    fn varying_recording(empty_segments: bool) -> (Vec<u8>, Vec<f32>) {
        let mut stage = fmn::mobject::Stage::new();
        let mut timeline = fmn::animation::Timeline::new(3).unwrap();
        let durations = if empty_segments {
            vec![0.0, 1.0 / 3.0, 0.0, 1.0 / 3.0, 1.0 / 3.0, 0.0]
        } else {
            vec![1.0 / 3.0; 3]
        };
        for duration in durations {
            timeline.wait(duration).unwrap();
        }
        let pictures = fmn_scene::export_timeline_bundle(
            timeline,
            &mut stage,
            &fmn_core::rng::RngRoot::from_seed(0),
        )
        .unwrap();
        let samples: Vec<_> = (0_u8..14)
            .flat_map(|index| [f32::from(index) / 32.0, -f32::from(index) / 32.0])
            .collect();
        let pcm = BundleSoundtrack::new(10, 2, samples.clone()).unwrap();
        let bytes = fmn_scene::timeline_bundle::attach_bundle_soundtrack(
            &pictures,
            &pcm,
            DEFAULT_MAX_BUNDLE_BYTES,
        )
        .unwrap();
        (bytes, samples)
    }

    fn wav_samples(fs: &VirtualFs, path: &str) -> Vec<f32> {
        let wav = fs.read(Path::new(path)).unwrap();
        fmn_codec::decode_wav(&wav, &fmn_codec::WavLimits::default())
            .unwrap()
            .samples
    }

    #[test]
    fn ranged_wav_honors_exclusive_segments_absolute_offsets_and_tail_trimming() {
        let (bytes, samples) = varying_recording(false);
        for (selection, path, expected, frames) in [
            ("1,2", "/out/scene_1_2.wav", 6..14, "[1,2]"),
            ("1", "/out/scene_1.wav", 6..20, "[1,3]"),
            ("0", "/out/scene_0.wav", 0..20, "[0,3]"),
            ("0,3", "/out/scene_0_3.wav", 0..20, "[0,3]"),
            ("2,3", "/out/scene_2_3.wav", 14..20, "[2,3]"),
        ] {
            let fs = Arc::new(VirtualFs::new());
            let output = run(
                command(&["-n", selection]),
                &bytes,
                TimelineBundle::from_bytes(&bytes).unwrap(),
                fs.clone(),
                &|| panic!("range WAV acquired ffmpeg"),
            );
            assert_eq!(output.code, 0, "{}", output.stdout);
            assert_eq!(wav_samples(&fs, path), samples[expected]);
            assert!(
                output
                    .stdout
                    .contains(&format!("\"source_frames\":{frames}"))
            );
            assert!(!fs.exists(Path::new("/out/scene.wav")));
        }
    }

    #[test]
    fn ranged_subdivision_keeps_original_ordinals_and_exact_sample_partition() {
        let (bytes, samples) = varying_recording(false);
        let fs = Arc::new(VirtualFs::new());
        let output = run(
            command(&["-n", "1,3", "--subdivide", "--prerun"]),
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            fs.clone(),
            &|| panic!("range WAV acquired ffmpeg"),
        );
        assert_eq!(output.code, 0, "{}", output.stdout);
        let mut joined = wav_samples(&fs, "/out/scene_1_3/00001.wav");
        joined.extend(wav_samples(&fs, "/out/scene_1_3/00002.wav"));
        assert_eq!(joined, samples[6..20]);
        assert!(!fs.exists(Path::new("/out/scene_1_3/00000.wav")));
        assert!(output.stdout.contains("\"subdivision\":1"));
        assert!(output.stdout.contains("\"subdivision\":2"));
        assert!(!output.stdout.contains("\"subdivision\":0"));
        assert!(output.stdout.contains("\"frames\":2,\"segments\":2"));
        assert!(output.stdout.contains("\"source_segments\":[1,3]"));
    }

    #[test]
    fn unselected_empty_segments_do_not_block_a_valid_excerpt() {
        let (bytes, samples) = varying_recording(true);
        let fs = Arc::new(VirtualFs::new());
        let output = run(
            command(&["-n", "3,5", "--subdivide"]),
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            fs.clone(),
            &|| panic!("range WAV acquired ffmpeg"),
        );
        assert_eq!(output.code, 0, "{}", output.stdout);
        assert_eq!(wav_samples(&fs, "/out/scene_3_5/00003.wav"), samples[6..14]);
        assert_eq!(
            wav_samples(&fs, "/out/scene_3_5/00004.wav"),
            samples[14..20]
        );
        for extra in [vec!["-n", "0,1"], vec!["-n", "1,4", "--subdivide"]] {
            let fs = Arc::new(VirtualFs::new());
            let output = run(
                command(&extra),
                &bytes,
                TimelineBundle::from_bytes(&bytes).unwrap(),
                fs.clone(),
                &|| panic!("empty refusal acquired ffmpeg"),
            );
            assert_ne!(output.code, 0);
            assert!(!fs.exists(Path::new("/out")));
        }
        let fs = Arc::new(VirtualFs::new());
        let output = run(
            command(&["-n", "1,4"]),
            &bytes,
            TimelineBundle::from_bytes(&bytes).unwrap(),
            fs.clone(),
            &|| None,
        );
        assert_eq!(output.code, 0, "{}", output.stdout);
        assert_eq!(wav_samples(&fs, "/out/scene_1_4.wav"), samples[..14]);
    }

    #[test]
    fn invalid_compiled_ranges_refuse_without_output_or_process_access() {
        let bytes = recording();
        for (start, end) in [
            (3, None),
            (1, Some(4)),
            (2, Some(1)),
            (0, Some(0)),
            (u64::MAX, None),
        ] {
            let fs = Arc::new(VirtualFs::new());
            let mut command = command(&[]);
            // A host may construct requests without going through parse_args.
            command.animation_range = Some(AnimationRange { start, end });
            let output = run(
                command,
                &bytes,
                TimelineBundle::from_bytes(&bytes).unwrap(),
                fs.clone(),
                &|| panic!("invalid range acquired ffmpeg"),
            );
            assert_eq!(output.code, code("usage"));
            assert!(!fs.exists(Path::new("/out")));
        }
    }

    #[test]
    fn selected_preflight_preserves_unselected_outputs_and_refuses_late_collisions() {
        let bytes = recording();
        for collide in [false, true] {
            let fs = Arc::new(VirtualFs::new());
            fs.insert("/out/scene_1_3/00000.wav", b"unselected".to_vec());
            if collide {
                fs.insert("/out/scene_1_3/00002.wav", b"existing winner".to_vec());
            }
            let output = run(
                command(&["-n", "1,3", "--subdivide"]),
                &bytes,
                TimelineBundle::from_bytes(&bytes).unwrap(),
                fs.clone(),
                &|| None,
            );
            assert_eq!(output.code == 0, !collide, "{}", output.stdout);
            assert_eq!(
                fs.read(Path::new("/out/scene_1_3/00000.wav")).unwrap(),
                b"unselected"
            );
            if collide {
                assert!(!fs.exists(Path::new("/out/scene_1_3/00001.wav")));
                assert_eq!(
                    fs.read(Path::new("/out/scene_1_3/00002.wav")).unwrap(),
                    b"existing winner"
                );
            }
        }
    }

    #[test]
    fn selected_work_budget_and_explicit_names_do_not_charge_the_prefix() {
        let bytes = recording();
        let bundle = TimelineBundle::from_bytes(&bytes).unwrap();
        assert!(selected_ranges(&bundle, None, 1).is_err());
        let selection = Some(AnimationRange {
            start: 1,
            end: Some(2),
        });
        assert_eq!(
            selected_ranges(&bundle, selection, 1).unwrap(),
            (1..2, 1..2)
        );
        let fs = Arc::new(VirtualFs::new());
        let output = run(
            command(&["-n", "1,2", "--file_name", "excerpt", "--prerun"]),
            &bytes,
            bundle,
            fs.clone(),
            &|| None,
        );
        assert_eq!(output.code, 0, "{}", output.stdout);
        assert!(fs.exists(Path::new("/out/excerpt.wav")));
        assert!(output.stdout.contains("\"source_segments\":[1,2]"));
        assert!(output.stdout.contains("\"frames\":1,\"segments\":1"));
    }
}
