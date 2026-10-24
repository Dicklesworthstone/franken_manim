//! Compose the existing recorded camera worker and authenticated PCM host.
//! No scene code, external media or renderer runs in the stable HTTP process.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use fmn_cli::{Invocation, PreviewCodec, RunOutput, StudioCommand};
use fmn_config::config::{DeterminismMode, Engine, ThreadPolicy};
use fmn_hash::{Digest, sha256};
use fmn_platform::clock::{Clock, StdClock};
use fmn_platform::fs::{FileSystem, StdFs, VirtualFs};
use fmn_scene::{AssetRead, DEFAULT_MAX_BUNDLE_BYTES, Journal, TimelineBundle};
use fmn_studio::camera_bundle::{CameraBundleView, CameraBundleWorker};
use fmn_studio::{
    BuildError, CapabilityToken, FrameHub, RebuildDriver, StdWorkerLauncher, StudioHost,
    StudioHostConfig, StudioRecordedAudio, StudioWorkerSession, Supervisor, SupervisorConfig,
    SupervisorReply, SupervisorRequest, WorkerArtifact, WorkerResponse, WorkerServeOutcome,
};

mod launch;
use launch::Launch;

pub(crate) const WORKER_ARG: &str = "--fmn-internal-recorded-studio-worker-v1";
const SOURCE_ID: &str = "compiled/recording.fmtl";

#[derive(Debug)]
struct Failure {
    identity: &'static str,
    message: String,
}

impl Failure {
    fn new(identity: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            identity,
            message: error.to_string(),
        }
    }
}

fn status(identity: &str) -> u8 {
    fmn_cli::EXIT_CODE_SPECS
        .iter()
        .find(|entry| entry.name == identity)
        .map_or(fmn_cli::internal_exit_code(), |entry| entry.code)
}

fn quote(text: &str) -> String {
    let mut result = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            ch if ch <= '\u{1f}' => {
                use std::fmt::Write as _;
                let _ = write!(result, "\\u{:04x}", u32::from(ch));
            }
            ch => result.push(ch),
        }
    }
    result.push('"');
    result
}

fn output(result: Result<(), Failure>, robot: bool) -> RunOutput {
    match result {
        Ok(()) => RunOutput {
            code: status("success"),
            stdout: String::new(),
            stderr: String::new(),
        },
        Err(error) => {
            let code = status(error.identity);
            RunOutput {
                code,
                stdout: if robot {
                    format!(
                        "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"error\",\"exit\":{{\"code\":{code},\"identity\":{}}},\"message\":{}}}\n",
                        fmn_cli::ROBOT_SCHEMA_VERSION,
                        quote(error.identity),
                        quote(&error.message)
                    )
                } else {
                    String::new()
                },
                stderr: if robot {
                    String::new()
                } else {
                    format!("fmn: {}: {}\n", error.identity, error.message)
                },
            }
        }
    }
}

/// Picture-only, live and invalid CLI invocations keep the ordinary entry point.
pub(crate) fn try_run_os(
    arguments: &[OsString],
    ready: &mut dyn Write,
    shutdown: &AtomicBool,
) -> Option<RunOutput> {
    let arguments = arguments
        .iter()
        .map(|arg| arg.to_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()?;
    let Invocation::Studio(command) = fmn_cli::parse_args(arguments).ok()? else {
        return None;
    };
    let path = command.render.file.as_deref()?;
    if !path
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("fmtl"))
    {
        return None;
    }
    let bytes = match StdFs.read_bounded(path, DEFAULT_MAX_BUNDLE_BYTES) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Some(output(
                Err(Failure::new("scene", error)),
                command.render.common.robot,
            ));
        }
    };
    let (bundle, audio) = match StudioRecordedAudio::decode(SOURCE_ID, &bytes) {
        Ok(decoded) => decoded,
        Err(error) => {
            return Some(output(
                Err(Failure::new("scene", error)),
                command.render.common.robot,
            ));
        }
    };
    let audio = audio?;
    Some(output(
        run(&command, bundle, audio, ready, shutdown),
        command.render.common.robot,
    ))
}

fn validate(command: &StudioCommand) -> Result<(), Failure> {
    let render = &command.render;
    if render.write_all || render.scene_names.len() > 1 {
        return Err(Failure::new(
            "usage",
            "an FMTL recording contains exactly one compiled scene",
        ));
    }
    if !command.bind.is_loopback() {
        return Err(Failure::new(
            "capability",
            "Studio must bind to a loopback address",
        ));
    }
    if command.tui
        || command.preview_codec != PreviewCodec::Png
        || command.restore_session.is_some()
        || render.animation_range.is_some()
        || render.skip_animations
        || render.autoreload
        || render.embed_line.is_some()
        || render.presenter_mode
        || render.full_screen
        || render.write_file
        || render.open
        || render.finder
    {
        return Err(Failure::new(
            "capability",
            "narrated Studio is immutable browser preview with PNG frames; live editing, saved live sessions, ranges, file output, TUI and MJPEG are not supported by this adapter",
        ));
    }
    Ok(())
}

fn scene_name(command: &StudioCommand) -> Result<String, Failure> {
    let scene = command
        .render
        .scene_names
        .first()
        .cloned()
        .unwrap_or_else(|| {
            command
                .render
                .file
                .as_deref()
                .and_then(Path::file_stem)
                .and_then(OsStr::to_str)
                .unwrap_or("Recording")
                .to_owned()
        });
    if scene.is_empty() || scene.len() > 4096 || scene.chars().any(char::is_control) {
        return Err(Failure::new("usage", "invalid recorded Studio scene name"));
    }
    Ok(scene)
}

fn prepare_launch(
    command: &StudioCommand,
    bundle: &TimelineBundle,
    audio: &StudioRecordedAudio,
    fs: &dyn FileSystem,
    source_path: PathBuf,
) -> Result<Launch, Failure> {
    validate(command)?;
    if bundle.frame_count() == 0
        || bundle.fps() != audio.fps()
        || bundle.frame_count() != audio.picture_frames()
    {
        return Err(Failure::new(
            "scene",
            "narrated Studio requires one nonempty matching picture/audio clock",
        ));
    }
    if command.render.fps.is_some_and(|fps| fps != bundle.fps()) {
        return Err(Failure::new(
            "config",
            "--fps cannot resample a recorded Studio timeline",
        ));
    }
    if bundle.has_camera_track()
        && (command.render.background.is_some() || command.render.transparent)
    {
        return Err(Failure::new(
            "capability",
            "a captured camera/background track cannot be overridden",
        ));
    }
    let mut effective = command.render.clone();
    effective.fps = Some(bundle.fps());
    let config = fmn_cli::resolve_render_config(fs, &effective)
        .map_err(|error| Failure::new(error.exit_name(), error))?;
    if config.render.engine != Engine::Cpu || config.render.aa != fmn_core::AaPolicy::Adaptive {
        return Err(Failure::new(
            "capability",
            "recorded camera preview requires CPU rendering with adaptive coverage; accelerator and forced-SSAA requests are not substituted",
        ));
    }
    if config.determinism.mode == DeterminismMode::Certified {
        return Err(Failure::new(
            "capability",
            "audible Studio playback is not a certified artifact; use --format wav --reproducible for certified extraction",
        ));
    }
    let threads = match config.render.threads {
        ThreadPolicy::Auto => {
            std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
        }
        ThreadPolicy::Fixed(count) => {
            usize::try_from(count).map_err(|error| Failure::new("config", error))?
        }
    };
    let seed = config.determinism.seed;
    let config_digest = sha256(
        &config
            .canonical_bytes()
            .map_err(|error| Failure::new("config", error))?,
    );
    let mut options = fmn::rendering::RenderOptions::with_format(
        "studio-preview.png",
        fmn::rendering::RenderFormat::Png,
    )
    .map_err(|error| Failure::new("config", error))?;
    options.config = config;
    let camera = fmn_render::Camera::new(
        options
            .camera_config()
            .map_err(|error| Failure::new("config", error))?,
    )
    .map_err(|error| Failure::new("config", error))?;
    let launch = Launch {
        source_path,
        source_digest: audio.info().source.to_hex(),
        scene: scene_name(command)?,
        width: camera.pixel_shape().0,
        height: camera.pixel_shape().1,
        fps: bundle.fps(),
        frame_height: camera.frame().shape()[1],
        background: camera.background(),
        threads,
        seed,
        config_digest: config_digest.to_hex(),
    };
    launch.view(bundle)?;
    Ok(launch)
}

struct RecordingBuilder {
    launch: Launch,
    artifact: WorkerArtifact,
}

impl RebuildDriver for RecordingBuilder {
    fn rebuild(&mut self) -> Result<WorkerArtifact, BuildError> {
        self.launch
            .read(&StdFs)
            .map_err(|error| BuildError::new(error.message))?;
        Ok(self.artifact.clone())
    }
}

struct SessionGuard(Arc<StudioWorkerSession>);
impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.0.shutdown_worker();
    }
}

fn run(
    command: &StudioCommand,
    bundle: TimelineBundle,
    audio: StudioRecordedAudio,
    ready: &mut dyn Write,
    shutdown: &AtomicBool,
) -> Result<(), Failure> {
    validate(command)?;
    let source_path = command
        .render
        .file
        .as_ref()
        .ok_or_else(|| Failure::new("usage", "missing recording"))?
        .canonicalize()
        .map_err(|error| Failure::new("scene", error))?;
    let launch = prepare_launch(command, &bundle, &audio, &StdFs, source_path)?;
    let view = launch.view(&bundle)?;
    let build_id = launch.build_id();
    let reads = CameraBundleWorker::input_reads(build_id, audio.source(), &bundle, &view)
        .map_err(|error| Failure::new("scene", error))?;
    // Retain PCM in the host, not a second decoded picture arena.
    drop(bundle);
    let asset_ok: Arc<dyn Fn(&AssetRead) -> bool + Send + Sync> =
        Arc::new(move |read| reads.contains(read));
    let mut entropy = [0_u8; 32];
    fmn_platform::entropy::HostEntropy::fill(&fmn_platform::entropy::StdHostEntropy, &mut entropy)
        .map_err(|error| Failure::new("capability", error))?;
    let token = CapabilityToken::new(entropy).map_err(|error| Failure::new("capability", error))?;
    let executable = std::env::current_exe().map_err(|error| Failure::new("internal", error))?;
    let artifact = WorkerArtifact {
        executable,
        argv: launch.argv()?,
        env: Vec::new(),
        cwd: None,
        build_id,
    };
    let mut builder = Box::new(RecordingBuilder { launch, artifact });
    let clock: Arc<dyn Clock> = Arc::new(StdClock::new());
    let cache = fmn_cache::Store::open(
        Arc::new(VirtualFs::new()),
        Arc::clone(&clock),
        std::env::temp_dir().join("fmn-narrated-studio-cache"),
        fmn_cache::StoreConfig::default(),
    )
    .and_then(|store| {
        store.namespace(
            "narrated-preview",
            1,
            fmn_cache::NamespacePolicy {
                ceiling_bytes: Some(8 * 1024 * 1024),
            },
        )
    })
    .map_err(|error| Failure::new("internal", error))?;
    let mut supervisor = Supervisor::new(
        Box::new(StdWorkerLauncher::default()),
        Arc::clone(&clock),
        cache,
        SupervisorConfig {
            request_timeout: Duration::from_secs(30),
            ..SupervisorConfig::default()
        },
    );
    let scene = builder.launch.scene.clone();
    supervisor
        .install_session(&scene, Journal::new())
        .map_err(|error| Failure::new("scene", error))?;
    supervisor
        .build_and_start(&mut *builder)
        .map_err(|error| Failure::new("scene", error))?;
    let reply = supervisor
        .request(
            SupervisorRequest::Scrub {
                scene: scene.clone(),
                frame: 0,
            },
            &*asset_ok,
        )
        .map_err(|error| Failure::new("scene", error))?;
    let SupervisorReply::Worker(WorkerResponse::Frame(first)) = reply else {
        return Err(Failure::new(
            "scene",
            "recorded worker did not return its first picture",
        ));
    };
    let generation = supervisor.generation();
    let config = StudioHostConfig {
        bind_addr: SocketAddr::new(command.bind, command.port),
        ..StudioHostConfig::default()
    };
    let frames = FrameHub::new(config.max_frame_history, config.max_png_bytes)
        .map_err(|error| Failure::new("budget", error))?;
    frames
        .publish(&first, supervisor.protocol_limits())
        .map_err(|error| Failure::new("render", error))?;
    let session = Arc::new(
        StudioWorkerSession::new(&scene, supervisor, builder, asset_ok)
            .map_err(|error| Failure::new("scene", error))?
            .require_guarded_input(),
    );
    let _guard = SessionGuard(Arc::clone(&session));
    let host = StudioHost::bind(session, frames, token, clock, config)
        .map_err(|error| Failure::new("capability", error))?;
    let url = host
        .launch_url()
        .map_err(|error| Failure::new("internal", error))?;
    if command.render.common.robot {
        writeln!(ready, "{{\"schema\":\"fmn.cli\",\"version\":{},\"kind\":\"studio_ready\",\"scene\":{},\"url\":{},\"preview_codec\":\"png\",\"browser_launch\":false,\"worker_generation\":{generation},\"recorded_audio\":true,\"source_sha256\":{},\"fps\":{},\"picture_frames\":{}}}",
            fmn_cli::ROBOT_SCHEMA_VERSION, quote(&scene), quote(&url), quote(&audio.info().source.to_hex()), audio.fps(), audio.picture_frames())
            .map_err(|error| Failure::new("internal", error))?;
    } else {
        writeln!(
            ready,
            "Studio: {url}\nRecorded narration is available; close stdin to stop the session."
        )
        .map_err(|error| Failure::new("internal", error))?;
    }
    ready
        .flush()
        .map_err(|error| Failure::new("internal", error))?;
    host.serve_recorded_until(audio, shutdown)
        .map_err(|error| Failure::new("scene", error))
}

pub(crate) fn is_worker(arguments: &[OsString]) -> bool {
    arguments.first().is_some_and(|arg| arg == WORKER_ARG)
}

pub(crate) fn run_worker_os(arguments: &[OsString]) -> RunOutput {
    let result = (|| {
        let launch = Launch::parse(arguments.get(1..).unwrap_or_default())?;
        let (bundle, audio) = launch.read(&StdFs)?;
        let view = launch.view(&bundle)?;
        let mut worker = CameraBundleWorker::new(
            launch.scene.clone(),
            launch.build_id(),
            audio.source().clone(),
            bundle,
            view,
        )
        .map_err(|error| Failure::new("scene", error))?;
        drop(audio);
        let result = fmn_studio::serve_worker(
            &mut worker,
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
            fmn_studio::ProtocolLimits::default(),
        )
        .map_err(|error| Failure::new("internal", error))?;
        match result {
            WorkerServeOutcome::Shutdown | WorkerServeOutcome::PeerClosed => Ok(()),
            _ => Err(Failure::new(
                "scene",
                "recorded Studio worker stopped after a protocol refusal or crash",
            )),
        }
    })();
    // stdout belongs exclusively to the canonical worker protocol.
    output(result, false)
}

#[cfg(test)]
mod tests;
