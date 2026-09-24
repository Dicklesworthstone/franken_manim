//! Native scene-to-artifact composition over Proscenium, Lumen, and Reel.
//!
//! [`render`] runs an ordinary [`SceneConstruct`] and publishes PNG frames, a
//! GIF, or a Y4M stream without CPython or ffmpeg. The existing scene clock,
//! retained renderer, conversion kernels, and bounded ordered emitter remain
//! authoritative; this module does not implement a second animation loop.
//!
//! Output is published only after successful scene execution and sink
//! finalization. Errors (and unwinding scene panics) cancel and join the output
//! worker before returning. Existing destinations are never overwritten.
//!
//! `Certified` selects the certified CPU and canonical PNG encoding. A render
//! report is an artifact receipt, **not** a certified input-closure manifest;
//! caller-owned external assets and callbacks still need closure attestation.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use fmn_anim::FramePacket;
use fmn_codec::{CompressionLevel, Y4mColorspace};
use fmn_config::Config;
use fmn_config::config::{DeterminismMode, Engine, ThreadPolicy};
use fmn_core::color::{HexParseError, Srgb};
use fmn_frame::convert::{rgba_to_nv12, rgba16f_to_rgba8};
use fmn_frame::{ChromaSiting, ColorRange, FrameBuffer, FrameError, FrameLayout, PixelFormat};
use fmn_output::{
    EmitterConfig, EmitterError, EmitterFailure, GifSink, GifSinkConfig, OrderedEmitter,
    PngSink, PngSinkConfig, PngTarget, ReceiptError, SinkAdapterError, SinkLimits,
    SinkReceipt, Y4mSink, Y4mSinkConfig,
};
use fmn_platform::fs::{FileSystem, StdFs};
use fmn_platform::topology::HardwareTopology;
use fmn_render::{
    EngineIdentity, FrameConfig, RetainedFrameRenderer, RetainedFrameRendererConfig,
    RetainedFrameRendererError, ScreenMap, Tiling, Viewport,
};
use fmn_runtime::{ExecutionEngine, OutputPixelFormat, PlanError, PlanRequest, RenderIntent, SurfaceSpec};
use fmn_scene::{CaptureReason, IntegrationError, RuntimeConfig, SceneRunReport, SceneSink};

use crate::SceneConstruct;

pub use fmn_output::{EmitterReport, NativeArtifactReport};
pub use fmn_runtime::ExecutionPlan;

/// Native output formats. The output path is a directory for PNG sequences
/// and a file for GIF/Y4M. No format silently substitutes for another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderFormat {
    /// `frame_000000.png`, ... and a completion manifest, atomically published
    /// as one new directory generation.
    PngSequence,
    /// Native animated GIF, looping indefinitely.
    Gif,
    /// Native planar 4:2:0 Y4M. Width and height must both be even.
    Y4m,
}

impl RenderFormat {
    fn pixel_format(self) -> PixelFormat {
        match self {
            Self::PngSequence | Self::Gif => PixelFormat::Rgba8,
            Self::Y4m => PixelFormat::Nv12,
        }
    }
}

/// Configuration for the native rendering front door.
///
/// [`Self::new`] resolves the bundled configuration without reading ambient
/// files. Callers may replace `config` with their own resolved configuration.
/// Camera resolution/background/fps, frame height, scene timing, seed, AA and
/// CPU thread policy are honored. `file_writer` settings do not override the
/// explicit `format` or `output` below. Accelerator and video-encoder requests
/// are not silently accepted by this CPU/native-codec entry point.
#[derive(Clone, Debug)]
pub struct RenderOptions {
    /// The destination; publication is no-clobber.
    pub output: PathBuf,
    /// Explicit native output format.
    pub format: RenderFormat,
    /// Scene and renderer settings, using the existing configuration schema.
    pub config: Config,
    /// Maximum number of captured frames (checked before rasterization).
    pub max_frames: u64,
    /// Maximum input stream and encoded artifact size, independently enforced.
    pub max_output_bytes: u64,
    /// Budget for planned frame storage and sink resident buffers, not for the
    /// caller's scene graph or geometry. The sink also enforces its own bound.
    pub max_resident_bytes: u64,
    /// Upper bound on the preallocated output ring; the scheduler may lower it.
    pub frames_in_flight: usize,
}

impl RenderOptions {
    /// PNG-sequence export using the bundled defaults and explicit budgets.
    ///
    /// # Errors
    /// Returns a configuration error if the bundled defaults cannot be resolved.
    pub fn new(output: impl Into<PathBuf>) -> Result<Self, fmn_config::ConfigError> {
        Ok(Self {
            output: output.into(),
            format: RenderFormat::PngSequence,
            config: Config::resolve(&[], None)?.config,
            max_frames: 1_000_000,
            max_output_bytes: 64 * 1024 * 1024 * 1024,
            max_resident_bytes: 512 * 1024 * 1024,
            frames_in_flight: 2,
        })
    }
}

/// Successful execution and publication, including the actual CPU plan.
#[derive(Debug, Clone)]
pub struct RenderReport {
    /// Exact scene time and play count from Proscenium.
    pub scene: SceneRunReport,
    /// Published destination, content digest, byte count and frame count.
    pub artifact: NativeArtifactReport,
    /// Final output queue and sink counters after the worker has joined.
    pub emission: EmitterReport,
    /// The scheduler decision used for this render.
    pub execution_plan: ExecutionPlan,
}

/// Typed failure of native scene rendering; underlying sources are retained.
#[derive(Debug)]
pub enum RenderError {
    /// Invalid bounds or a configuration combination this entry point cannot use.
    InvalidOptions(&'static str),
    /// A requested backend is not available through this entry point.
    Capability(&'static str),
    /// The configured background color is not valid hexadecimal RGB.
    Color(HexParseError),
    /// Invalid runtime configuration or a scene-authored error.
    Scene(crate::Error),
    /// Topology/scheduling request was refused.
    Plan(PlanError),
    /// Frame layout or conversion failed.
    Frame(FrameError),
    /// The retained renderer refused a frame.
    Renderer(RetainedFrameRendererError),
    /// Native sink creation failed.
    Sink(SinkAdapterError),
    /// A frame could not enter the bounded output queue.
    Emitter(EmitterError),
    /// Output drain/finalization failed; includes final delivery counters.
    Drain(EmitterFailure),
    /// A finalized sink did not supply its artifact receipt.
    Receipt(ReceiptError),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOptions(message) | Self::Capability(message) => f.write_str(message),
            Self::Color(error) => error.fmt(f),
            Self::Scene(error) => error.fmt(f),
            Self::Plan(error) => error.fmt(f),
            Self::Frame(error) => error.fmt(f),
            Self::Renderer(error) => error.fmt(f),
            Self::Sink(error) => error.fmt(f),
            Self::Emitter(error) => error.fmt(f),
            Self::Drain(error) => error.fmt(f),
            Self::Receipt(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RenderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidOptions(_) | Self::Capability(_) => None,
            Self::Color(error) => Some(error),
            Self::Scene(error) => Some(error),
            Self::Plan(error) => Some(error),
            Self::Frame(error) => Some(error),
            Self::Renderer(error) => Some(error),
            Self::Sink(error) => Some(error),
            Self::Emitter(error) => Some(error),
            Self::Drain(error) => Some(error),
            Self::Receipt(error) => Some(error),
        }
    }
}

/// Render a native Rust scene to a new artifact on the host filesystem.
///
/// A scene with no `play`/`wait` captures produces one image of its final state.
/// Animated scenes retain exactly the runtime's capture samples; no alpha-zero
/// or extra terminal frame is inserted. Audio cues are not muxed into native
/// image/Y4M outputs.
///
/// # Errors
/// Returns [`RenderError`] without publishing a successful partial artifact.
pub fn render<P: SceneConstruct + ?Sized>(
    program: &mut P,
    options: RenderOptions,
) -> Result<RenderReport, RenderError> {
    render_with_fs(program, options, Arc::new(StdFs))
}

/// The same rendering path with an explicit filesystem capability, useful for
/// sandboxed hosts and tests. No asset fetcher or process runner is implicit.
///
/// # Errors
/// Returns [`RenderError`] from scene execution, rasterization or publication.
pub fn render_with_fs<P: SceneConstruct + ?Sized>(
    program: &mut P,
    options: RenderOptions,
    fs: Arc<dyn FileSystem>,
) -> Result<RenderReport, RenderError> {
    let runtime = RuntimeConfig::from_config(&options.config);
    // Reject bad timing before constructing a sink or touching its destination.
    if runtime.fps == 0 {
        return Err(RenderError::InvalidOptions("fps must be nonzero"));
    }
    if !runtime.default_wait_time.is_finite() || runtime.default_wait_time < 0.0 {
        return Err(RenderError::InvalidOptions(
            "default_wait_time must be finite and non-negative",
        ));
    }
    let seed = options.config.determinism.seed;
    let mut sink = RenderSink::new(options, fs)?;
    let completed = crate::run_scene(program, runtime, seed, &mut sink);
    if let Some(error) = sink.failure.take() {
        return Err(error);
    }
    let completed = completed.map_err(RenderError::Scene)?;
    if sink.next_sequence == 0 {
        sink.render_stage(completed.scene().stage())?;
    }
    let emission = sink.emitter.take().ok_or(RenderError::InvalidOptions(
        "render emitter was already finalized",
    ))?.finish().map_err(RenderError::Drain)?;
    let artifact = sink.receipt.take().map_err(RenderError::Receipt)?;
    Ok(RenderReport {
        scene: *completed.report(),
        artifact,
        emission,
        execution_plan: sink.plan.clone(),
    })
}

struct RenderSink {
    renderer: RetainedFrameRenderer,
    scratch: Option<FrameBuffer>,
    emitter: Option<OrderedEmitter>,
    receipt: SinkReceipt<NativeArtifactReport>,
    plan: ExecutionPlan,
    next_sequence: u64,
    max_frames: u64,
    failure: Option<RenderError>,
}

impl RenderSink {
    fn new(options: RenderOptions, fs: Arc<dyn FileSystem>) -> Result<Self, RenderError> {
        let config = &options.config;
        if config.render.engine != Engine::Cpu {
            return Err(RenderError::Capability(
                "native render() supports render.engine=cpu; no accelerator is silently substituted",
            ));
        }
        if !config.sizes.frame_height.is_finite() || config.sizes.frame_height <= 0.0 {
            return Err(RenderError::InvalidOptions("frame height must be finite and positive"));
        }
        if !config.camera.background_opacity.is_finite()
            || !(0.0..=1.0).contains(&config.camera.background_opacity)
        {
            return Err(RenderError::InvalidOptions("background opacity must be in [0, 1]"));
        }
        if options.frames_in_flight == 0 {
            return Err(RenderError::InvalidOptions("frames_in_flight must be nonzero"));
        }
        let limits = SinkLimits::new(
            options.max_frames,
            options.max_resident_bytes,
            options.max_output_bytes,
            options.max_output_bytes,
        ).map_err(RenderError::Sink)?;
        let (width, height) = config.camera.resolution;
        let pixel_format = options.format.pixel_format();
        let layout = FrameLayout::tight(pixel_format, width, height).map_err(RenderError::Frame)?;
        let logical = std::thread::available_parallelism().ok()
            .and_then(|count| u32::try_from(count.get()).ok()).unwrap_or(1);
        let topology = if cfg!(target_os = "linux") {
            HardwareTopology::detect_linux(fs.as_ref())
                .unwrap_or_else(|_| HardwareTopology::fallback(logical))
        } else {
            HardwareTopology::fallback(logical)
        };
        let surface = SurfaceSpec::lumen(width, height);
        let output_format = if pixel_format == PixelFormat::Nv12 {
            OutputPixelFormat::Nv12
        } else {
            OutputPixelFormat::Rgba8
        };
        let certified = config.determinism.mode == DeterminismMode::Certified;
        let mut request = if certified {
            PlanRequest::certified(RenderIntent::Offline, surface, output_format)
        } else {
            PlanRequest::standard(RenderIntent::Offline, surface, output_format)
        }.with_max_frames_in_flight(options.frames_in_flight);
        if let ThreadPolicy::Fixed(threads) = config.render.threads {
            let threads = usize::try_from(threads)
                .map_err(|_| RenderError::InvalidOptions("thread count exceeds this target"))?;
            request = request.with_max_cpu_threads(threads);
        }
        let plan = ExecutionPlan::derive(request, &topology, None).map_err(RenderError::Plan)?;
        // Check before allocating the renderer, conversion scratch or ring.
        let planned = u64::try_from(plan.estimated_in_flight_bytes)
            .ok().and_then(|bytes| bytes.checked_add(64 * 1024 * 1024))
            .ok_or(RenderError::InvalidOptions("render memory budget overflowed"))?;
        if planned > options.max_resident_bytes {
            return Err(RenderError::InvalidOptions("render plan exceeds max_resident_bytes"));
        }
        let background = Srgb::from_hex(&config.camera.background_color)
            .map_err(RenderError::Color)?
            .to_linear(config.camera.background_opacity);
        let frame = FrameConfig::new(
            Viewport { width, height },
            ScreenMap {
                scale: f64::from(height) / config.sizes.frame_height,
                origin: [f64::from(width) / 2.0, f64::from(height) / 2.0],
                y_up: true,
            },
            background,
        ).with_aa_policy(config.render.aa);
        let renderer = RetainedFrameRenderer::new(RetainedFrameRendererConfig {
            frame,
            tiling: Tiling { macro_tile: plan.macro_tile, fine_tile: plan.fine_tile },
            engine: if plan.engine == ExecutionEngine::CertifiedCpu {
                EngineIdentity::certified()
            } else {
                EngineIdentity::fast()
            },
            threads: plan.render_teams.first().map_or(1, fmn_runtime::TeamPlan::threads),
        }).map_err(RenderError::Renderer)?;
        let scratch = if pixel_format == PixelFormat::Nv12 {
            Some(FrameBuffer::new(FrameLayout::tight(PixelFormat::Rgba8, width, height)
                .map_err(RenderError::Frame)?))
        } else {
            None
        };
        let (binding, receipt) = match options.format {
            RenderFormat::PngSequence => PngSink::new(fs, PngSinkConfig {
                target: PngTarget::Sequence {
                    directory: options.output,
                    stem: "frame".to_owned(),
                    digits: 6,
                },
                width, height, first_sequence: 0,
                compression: if certified { CompressionLevel::Best } else { CompressionLevel::Default },
                threads: plan.output_team.threads().max(1),
                limits, profile: None,
            }).map_err(RenderError::Sink)?.into_binding("png-sequence"),
            RenderFormat::Gif => GifSink::new(fs, GifSinkConfig {
                destination: options.output, width, height,
                fps: (config.camera.fps, 1), loop_forever: true,
                first_sequence: 0, limits, profile: None,
            }).map_err(RenderError::Sink)?.into_binding("gif"),
            RenderFormat::Y4m => Y4mSink::new(fs, Y4mSinkConfig {
                destination: options.output, width, height,
                fps: (config.camera.fps, 1), colorspace: Y4mColorspace::C420Mpeg2,
                first_sequence: 0, limits, profile: None,
            }).map_err(RenderError::Sink)?.into_binding("y4m"),
        };
        let emitter = OrderedEmitter::new(
            EmitterConfig::new(layout, plan.frames_in_flight, 0).map_err(RenderError::Emitter)?,
            vec![binding],
        ).map_err(RenderError::Emitter)?;
        Ok(Self {
            renderer, scratch, emitter: Some(emitter), receipt, plan,
            next_sequence: 0, max_frames: options.max_frames, failure: None,
        })
    }

    fn render_stage(&mut self, stage: &fmn_mobject::Stage) -> Result<(), RenderError> {
        if self.next_sequence >= self.max_frames {
            return Err(RenderError::InvalidOptions("scene exceeded max_frames"));
        }
        self.renderer.render(stage, 0).map_err(RenderError::Renderer)?;
        let emitter = self.emitter.as_ref().ok_or(RenderError::InvalidOptions(
            "render emitter was already finalized",
        ))?;
        let mut reservation = emitter.reserve(self.next_sequence).map_err(RenderError::Emitter)?;
        if let Some(scratch) = &mut self.scratch {
            rgba16f_to_rgba8(self.renderer.frame(), scratch).map_err(RenderError::Frame)?;
            rgba_to_nv12(scratch, reservation.frame_mut(), ColorRange::Limited, ChromaSiting::Left)
                .map_err(RenderError::Frame)?;
        } else {
            rgba16f_to_rgba8(self.renderer.frame(), reservation.frame_mut())
                .map_err(RenderError::Frame)?;
        }
        reservation.publish().map_err(RenderError::Emitter)?;
        self.next_sequence += 1;
        Ok(())
    }
}

impl SceneSink for RenderSink {
    fn capture(&mut self, _reason: CaptureReason, packet: FramePacket) -> Result<(), IntegrationError> {
        if let Some(error) = &self.failure {
            return Err(IntegrationError::new("native-render", error.to_string()));
        }
        let stage = packet.materialize_stage();
        self.render_stage(&stage).map_err(|error| {
            let message = error.to_string();
            self.failure = Some(error);
            IntegrationError::new("native-render", message)
        })
    }
}

impl Drop for RenderSink {
    fn drop(&mut self) {
        // OrderedEmitter's bare Drop cancels but detaches its worker. At this
        // host boundary, returning while abort cleanup is still running would
        // race an immediate retry and leave observable temporary artifacts.
        if let Some(emitter) = self.emitter.take() {
            emitter.cancel();
            let _ = emitter.finish();
        }
    }
}
