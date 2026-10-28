//! Owned Lumen jobs between the serial scene owner and the existing runtime.

mod adaptive;
pub use adaptive::{NativeAdaptiveCamera, NativeAdaptiveRoutes, planar_stage_refusal};

use std::cell::RefCell;
use std::fmt;
use std::sync::{Arc, Mutex, Weak};

use fmn_anim::pure_frame::PureFrameJob;
use fmn_frame::convert::{
    rgba16f_to_bgra8, rgba16f_to_nv12_threaded, rgba16f_to_p010_threaded, rgba16f_to_rgba8,
};
use fmn_frame::{ChromaSiting, ColorRange, FrameBuffer, FrameError, PixelFormat};
use fmn_mobject::FrozenRenderState;
use fmn_output::{EmitterError, EmitterHandle, FrameReservation};
use fmn_render::{
    Camera, CameraConfig, EngineIdentity, FrameArena, OwnedVectorFrame, PixelTileCache,
    PreparedCameraFrame, RetainedFrameRenderer, RetainedFrameRendererConfig,
    RetainedFrameRendererError, VectorFrameCompiler, Viewport,
};
use fmn_runtime::{
    ExecutionEngine, ExecutionPlan, FrameStream, FrameStreamError, OutputPixelFormat,
    PipelineStages, PipelineStats, TeamPlan, TeamRole,
};

use fmn_scene::timeline_bundle::{BundleReadError, TimelineFrameCache, TimelineFrameJob};

use super::RenderError;

thread_local! {
    // Created and dropped on the runtime's scoped render worker. No live
    // arena, callable or RNG crosses a thread boundary. At most one decoded
    // pure endpoint pair is retained, and recorded frames release that pair.
    static COMPILED_ENDPOINTS: RefCell<TimelineFrameCache> = RefCell::default();
}

/// A failure in worker-owned preparation, rasterization, conversion, or publication.
#[derive(Debug)]
pub enum NativeFrameError {
    /// A validated compiled frame refused reconstruction on its worker.
    Bundle(BundleReadError),
    /// A prepared animation could not evaluate its exact native sample.
    Animation(fmn_anim::AnimError),
    /// Lumen refused the frozen input.
    Renderer(RetainedFrameRendererError),
    /// A frozen camera/light capture was invalid.
    Camera(fmn_render::CameraError),
    /// Frame layout or output conversion failed.
    Frame(FrameError),
    /// The ordered output stream failed or was cancelled.
    Emitter(EmitterError),
    /// A worker's retained scratch could not be used safely.
    WorkerState(&'static str),
}

impl fmt::Display for NativeFrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bundle(error) => error.fmt(f),
            Self::Animation(error) => error.fmt(f),
            Self::Renderer(error) => error.fmt(f),
            Self::Camera(error) => error.fmt(f),
            Self::Frame(error) => error.fmt(f),
            Self::Emitter(error) => error.fmt(f),
            Self::WorkerState(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for NativeFrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Bundle(error) => Some(error),
            Self::Animation(error) => Some(error),
            Self::Renderer(error) => Some(error),
            Self::Camera(error) => Some(error),
            Self::Frame(error) => Some(error),
            Self::Emitter(error) => Some(error),
            Self::WorkerState(_) => None,
        }
    }
}

pub(super) enum NativeFrame {
    Camera(Arc<PreparedCameraFrame>),
    Vector(OwnedVectorFrame),
    Compiled {
        job: TimelineFrameJob,
        config: RetainedFrameRendererConfig,
        camera: Option<Camera>,
    },
    Pure {
        job: PureFrameJob,
        config: RetainedFrameRendererConfig,
        camera: Option<Camera>,
    },
    AdaptivePure {
        job: PureFrameJob,
        config: RetainedFrameRendererConfig,
        camera: NativeAdaptiveCamera,
    },
}

/// Captured data is callback-free; compilation stays on the ordered prepare worker.
#[allow(
    clippy::large_enum_variant,
    reason = "bounded frame inputs move once; boxing would allocate per captured frame"
)]
pub(super) enum NativeFrameInput {
    Frozen {
        state: FrozenRenderState,
        camera: Option<Camera>,
    },
    Ready(NativeFrame),
}

pub(super) struct NativeFrameJob {
    pub frame: NativeFrameInput,
    // Reserve on the scene owner, in capture order, not on raster workers.
    pub output: FrameReservation,
}

pub(super) struct PreparedFrameJob {
    frame: NativeFrame,
    output: FrameReservation,
}

// Lumen already retains the latest complete immutable capture. Share one
// handle to that same capture so workers can hold weak pixel identities
// without extending a previous scene's lifetime beyond its in-flight permits.
struct CameraCompiler {
    renderer: RetainedFrameRenderer,
    latest: Option<Arc<PreparedCameraFrame>>,
}

impl CameraCompiler {
    fn new(config: RetainedFrameRendererConfig) -> Result<Self, RetainedFrameRendererError> {
        Ok(Self {
            renderer: RetainedFrameRenderer::new(config)?,
            latest: None,
        })
    }

    fn prepare_with_camera(
        &mut self,
        stage: &fmn_mobject::Stage,
        camera: &Camera,
    ) -> Result<Arc<PreparedCameraFrame>, RetainedFrameRendererError> {
        let prepared = match self.renderer.prepare_with_camera(stage, camera) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.latest = None;
                return Err(error);
            }
        };
        if let Some(latest) = &self.latest
            && latest.same_capture(&prepared)
        {
            return Ok(Arc::clone(latest));
        }
        let prepared = Arc::new(prepared);
        self.latest = Some(Arc::clone(&prepared));
        Ok(prepared)
    }
}

#[derive(Default)]
struct RenderWorker {
    arena: FrameArena,
    cache: PixelTileCache,
    compiled_vector: Option<VectorFrameCompiler>,
    compiled_camera: Option<CameraCompiler>,
    // One raw surface shared by camera and vector frames. Only a vector
    // raster carries the cache token proving its cached pixels remain intact.
    // Switching routes must clear that token, without retaining a second frame.
    spare: Option<(FrameBuffer, Option<u64>)>,
    // Identifies only the successful camera raster in `spare`. Weak ownership
    // keeps neither geometry nor expired frame permits alive between jobs.
    camera_pixels: Weak<PreparedCameraFrame>,
}

impl RenderWorker {
    // Render into the recycled raster, convert into the output slot on the
    // same team, then keep the raster for the next frame. A raster from a
    // failed render is dropped, so a stale surface can never be resumed.
    fn render_vector(
        &mut self,
        frame: &OwnedVectorFrame,
        threads: usize,
        output: &mut FrameBuffer,
    ) -> Result<(), NativeFrameError> {
        self.camera_pixels = Weak::new();
        let layout = frame.raster_layout().map_err(NativeFrameError::Frame)?;
        let (mut raster, resume) = match self.spare.take() {
            Some((raster, serial)) if *raster.layout() == layout => (raster, serial),
            _ => (FrameBuffer::new(layout), None),
        };
        frame
            .render_cached_into(
                threads,
                &mut self.arena,
                &mut self.cache,
                &mut raster,
                resume,
            )
            .map_err(NativeFrameError::Renderer)?;
        convert_into(&raster, output, threads)?;
        self.spare = Some((raster, Some(self.cache.serial())));
        Ok(())
    }

    // Return whether an intact raster was reused. Conversion and publication
    // still run for every frame; only the redundant Lumen raster is omitted.
    fn render_camera(
        &mut self,
        frame: &Arc<PreparedCameraFrame>,
        threads: usize,
        output: &mut FrameBuffer,
    ) -> Result<bool, NativeFrameError> {
        if threads == 0 {
            return Err(NativeFrameError::Renderer(
                RetainedFrameRendererError::InvalidThreads,
            ));
        }
        let layout = frame.layout().map_err(NativeFrameError::Frame)?;
        let same_capture = self
            .camera_pixels
            .upgrade()
            .is_some_and(|previous| Arc::ptr_eq(&previous, frame));
        // Clear before any fallible write, including output conversion on a
        // cache hit. Success alone publishes the new raster and its identity.
        self.camera_pixels = Weak::new();
        let (mut raster, reused) = match self.spare.take() {
            Some((raster, serial)) if *raster.layout() == layout => {
                (raster, same_capture && serial.is_none())
            }
            _ => (FrameBuffer::new(layout), false),
        };
        if !reused {
            frame
                .render_into(threads, &mut raster)
                .map_err(NativeFrameError::Renderer)?;
        }
        convert_into(&raster, output, threads)?;
        self.spare = Some((raster, None));
        self.camera_pixels = Arc::downgrade(frame);
        Ok(reused)
    }
}

pub(super) struct NativeFrameStages {
    // FrameStream invokes prepare in source order on its single preparation
    // worker. The mutex satisfies PipelineStages' shared-reference contract;
    // raster teams never acquire it. Only one retained compiler is allocated.
    compiler: Mutex<NativeCompiler>,
    // One independent arena/cache per real render team. No global raster lock:
    // nonconsecutive frame contents and cache identities decide reuse.
    workers: Vec<Mutex<RenderWorker>>,
}

impl NativeFrameStages {
    fn new(render_teams: usize, compiler: NativeCompiler) -> Self {
        Self {
            compiler: Mutex::new(compiler),
            workers: (0..render_teams)
                .map(|_| Mutex::new(RenderWorker::default()))
                .collect(),
        }
    }
}

impl NativeFrameStages {
    fn rasterize_reconstructed(
        &self,
        stage: &fmn_mobject::Stage,
        config: RetainedFrameRendererConfig,
        camera: Option<Camera>,
        team: &TeamPlan,
        output: &mut FrameReservation,
    ) -> Result<(), NativeFrameError> {
        let TeamRole::Render(index) = team.role else {
            return Err(NativeFrameError::WorkerState(
                "compiled frame requires a render team",
            ));
        };
        let mut worker = self
            .workers
            .get(index)
            .ok_or(NativeFrameError::WorkerState(
                "compiled frame has no worker state",
            ))?
            .lock()
            .map_err(|_| NativeFrameError::WorkerState("compiled worker state was poisoned"))?;
        if let Some(camera) = camera {
            if worker.compiled_camera.is_none() {
                worker.compiled_camera =
                    Some(CameraCompiler::new(config).map_err(NativeFrameError::Renderer)?);
            }
            let frame = worker
                .compiled_camera
                .as_mut()
                .ok_or(NativeFrameError::WorkerState(
                    "missing compiled camera renderer",
                ))?
                .prepare_with_camera(stage, &camera)
                .map_err(NativeFrameError::Renderer)?;
            worker.render_camera(&frame, team.threads(), output.frame_mut())?;
        } else {
            if worker.compiled_vector.is_none() {
                worker.compiled_vector =
                    Some(VectorFrameCompiler::new(config).map_err(NativeFrameError::Renderer)?);
            }
            let frame = worker
                .compiled_vector
                .as_mut()
                .ok_or(NativeFrameError::WorkerState(
                    "missing compiled vector compiler",
                ))?
                .capture(stage, 0)
                .map_err(NativeFrameError::Renderer)?;
            worker.render_vector(&frame, team.threads(), output.frame_mut())?;
        }
        Ok(())
    }
}

impl PipelineStages for NativeFrameStages {
    type Frame = NativeFrameJob;
    type Prepared = PreparedFrameJob;
    // Converted on the render team that rasterized it (see `convert`).
    type Rasterized = FrameReservation;
    type Output = FrameReservation;
    type Error = NativeFrameError;

    fn prepare(&self, job: NativeFrameJob, _: &TeamPlan) -> Result<PreparedFrameJob, Self::Error> {
        let NativeFrameJob { frame, output } = job;
        let frame = match frame {
            NativeFrameInput::Ready(frame) => frame,
            NativeFrameInput::Frozen { state, camera } => {
                // Materialization moves the frozen buffers and graph vectors.
                // It installs no callbacks and drops no owner-thread updaters.
                let stage = state.materialize();
                let mut compiler = self.compiler.lock().map_err(|_| {
                    NativeFrameError::WorkerState("frame preparation compiler was poisoned")
                })?;
                match (&mut *compiler, camera.as_ref()) {
                    (NativeCompiler::Vector(compiler), None) => NativeFrame::Vector(
                        compiler
                            .capture(&stage, 0)
                            .map_err(NativeFrameError::Renderer)?,
                    ),
                    (NativeCompiler::Camera(renderer), Some(camera)) => NativeFrame::Camera(
                        renderer
                            .prepare_with_camera(&stage, camera)
                            .map_err(NativeFrameError::Renderer)?,
                    ),
                    _ => {
                        return Err(NativeFrameError::WorkerState(
                            "frozen camera policy does not match the retained compiler",
                        ));
                    }
                }
            }
        };
        Ok(PreparedFrameJob { frame, output })
    }

    fn rasterize(
        &self,
        job: PreparedFrameJob,
        team: &TeamPlan,
    ) -> Result<Self::Rasterized, Self::Error> {
        let PreparedFrameJob { frame, mut output } = job;
        match frame {
            NativeFrame::Camera(frame) => {
                let TeamRole::Render(index) = team.role else {
                    return Err(NativeFrameError::WorkerState(
                        "rasterization requires a render team",
                    ));
                };
                let worker = self
                    .workers
                    .get(index)
                    .ok_or(NativeFrameError::WorkerState(
                        "render team has no retained worker state",
                    ))?;
                let mut worker = worker.lock().map_err(|_| {
                    NativeFrameError::WorkerState(
                        "render worker scratch was poisoned by an earlier panic",
                    )
                })?;
                worker.render_camera(&frame, team.threads(), output.frame_mut())?;
            }
            NativeFrame::Compiled {
                job,
                config,
                camera,
            } => {
                // Reconstruction is inside the render-team worker, not the
                // serial source or prepare stage. It returns a local Stage
                // which is compiled here and dropped before conversion.
                let (stage, recorded_camera) = COMPILED_ENDPOINTS
                    .with(|cache| {
                        cache.borrow_mut().materialize_with_camera(
                            &job,
                            (config.frame.viewport.width, config.frame.viewport.height),
                        )
                    })
                    .map_err(NativeFrameError::Bundle)?;
                let camera = recorded_camera.or(camera);
                self.rasterize_reconstructed(&stage, config, camera, team, &mut output)?;
            }
            NativeFrame::Pure {
                job,
                config,
                camera,
            } => {
                // Only a closed prepared native program crosses here. Its
                // per-frame animation and arena are created on this render
                // team, not on the ordered preparation worker or scene owner.
                let stage = job.try_materialize().map_err(NativeFrameError::Animation)?;
                self.rasterize_reconstructed(&stage, config, camera, team, &mut output)?;
            }
            NativeFrame::AdaptivePure {
                job,
                mut config,
                camera,
            } => {
                let stage = job.try_materialize().map_err(NativeFrameError::Animation)?;
                let (pose, refusal) = camera.for_stage(&stage)?;
                config.frame.background = camera.background();
                self.rasterize_reconstructed(&stage, config, pose, team, &mut output)?;
                camera.record(output.sequence(), refusal)?;
            }
            NativeFrame::Vector(frame) => {
                let TeamRole::Render(index) = team.role else {
                    return Err(NativeFrameError::WorkerState(
                        "rasterization requires a render team",
                    ));
                };
                let worker = self
                    .workers
                    .get(index)
                    .ok_or(NativeFrameError::WorkerState(
                        "render team has no retained worker state",
                    ))?;
                let mut worker = worker.lock().map_err(|_| {
                    NativeFrameError::WorkerState(
                        "render worker scratch was poisoned by an earlier panic",
                    )
                })?;
                worker.render_vector(&frame, team.threads(), output.frame_mut())?;
            }
        }
        Ok(output)
    }

    // Output conversion runs at the end of `rasterize`, on the render team's
    // threads, which are otherwise idle until the next frame arrives. The
    // output team has at most two SMT siblings (one E-core on macOS); at 4K
    // the conversion there was the pipeline's serial bottleneck. Same bytes:
    // on a 128-thread Linux host, 600 4K frames took 13.3 s fused vs 13.5 s
    // on the output team.
    fn convert(
        &self,
        output: FrameReservation,
        _: &TeamPlan,
    ) -> Result<FrameReservation, Self::Error> {
        Ok(output)
    }
}

// Direct binary16 kernels: no frame-sized RGBA8 intermediate per frame (PG-6).
// BGRA/NV12 are byte-identical to the legacy two-step route; P010 keeps its
// precision beyond eight bits. The video formats fan out over `threads`: at
// 4K the serial NV12 pass was the largest single cost.
fn convert_into(
    frame: &FrameBuffer,
    destination: &mut FrameBuffer,
    threads: usize,
) -> Result<(), NativeFrameError> {
    match destination.layout().format() {
        PixelFormat::Rgba8 => rgba16f_to_rgba8(frame, destination),
        PixelFormat::Bgra8 => rgba16f_to_bgra8(frame, destination),
        PixelFormat::Nv12 => rgba16f_to_nv12_threaded(
            frame,
            destination,
            ColorRange::Limited,
            ChromaSiting::Left,
            threads,
        ),
        PixelFormat::P010 => rgba16f_to_p010_threaded(
            frame,
            destination,
            ColorRange::Limited,
            ChromaSiting::Left,
            threads,
        ),
        PixelFormat::Rgba16F => {
            return Err(NativeFrameError::WorkerState(
                "RGBA16F is a renderer intermediate, not a native output format",
            ));
        }
    }
    .map_err(NativeFrameError::Frame)
}

/// Shared CPU capture-to-output pipeline for native front doors.
///
/// The caller owns the `OrderedEmitter` and its final artifact publication.
/// The scene owner freezes callback-free data after admission. The existing
/// in-order preparation worker materializes and compiles it while the owner
/// advances the next frame. Raster/conversion work uses every planned render
/// team and their worker-local retained tile caches. Unchanged camera captures
/// also reuse the team's complete raw raster; conversion and output sequence
/// publication still occur for every admitted frame.
/// The output ring capacity must equal `plan.frames_in_flight`.
/// Call `finish` successfully BEFORE finishing the emitter. Dropping an
/// unfinished adapter cancels the emitter before joining all pipeline workers.
///
/// Pixel admission must include one retained RGBA16F cache per affine render
/// team (or a retained camera frame on the preparation worker and each
/// compiled-camera render team), in addition to the plan's in-flight surfaces.
/// Output conversion runs directly from the RGBA16F raster into the reserved output
/// slot; no per-frame intermediate surface is allocated. Frozen graph/record
/// storage is additional to that pixel budget, with at most one input per
/// admitted pipeline slot. Worker camera-pixel identities are weak and retain
/// no additional scene resources. Caller-retained replay packets have their own lifetime.
pub struct NativeFramePipeline {
    camera: Option<Camera>,
    stream: Option<FrameStream<NativeFrameJob, NativeFrameError>>,
    output: EmitterHandle,
    output_format: PixelFormat,
    viewport: Viewport,
    renderer_config: RetainedFrameRendererConfig,
}

enum NativeCompiler {
    Vector(Box<VectorFrameCompiler>),
    Camera(Box<CameraCompiler>),
}

impl NativeFramePipeline {
    /// Compose the existing runtime, Lumen renderer and ordered output ring.
    ///
    /// # Errors
    /// Refuses invalid renderer configuration or failure to start workers.
    pub fn new(
        plan: ExecutionPlan,
        config: RetainedFrameRendererConfig,
        camera: Option<Camera>,
        output: EmitterHandle,
    ) -> Result<Self, RenderError> {
        // Source demand is synchronous. A smaller output ring could block
        // its producer while the coordinator awaits that same next source
        // value instead of processing the completion that frees the ring.
        if output.stats().capacity != plan.frames_in_flight {
            return Err(RenderError::InvalidOptions(
                "output ring capacity must match the pipeline frame limit",
            ));
        }
        let expected_engine = match plan.engine {
            ExecutionEngine::CertifiedCpu => EngineIdentity::certified(),
            ExecutionEngine::FastCpu => EngineIdentity::fast(),
            ExecutionEngine::Metal | ExecutionEngine::Cuda => {
                return Err(RenderError::Capability(
                    "native frame pipeline requires a CPU execution plan",
                ));
            }
        };
        if config.engine != expected_engine
            || config.tiling.fine_tile != plan.fine_tile
            || config.tiling.macro_tile != plan.macro_tile
        {
            return Err(RenderError::InvalidOptions(
                "renderer identity and tiles must match the execution plan",
            ));
        }
        if plan.frames_in_flight == 0
            || plan.render_teams.is_empty()
            || plan
                .render_teams
                .iter()
                .enumerate()
                .any(|(index, team)| team.role != TeamRole::Render(index) || team.threads() == 0)
        {
            return Err(RenderError::InvalidOptions(
                "frame pipeline requires nonempty indexed render teams",
            ));
        }
        let output_format = match plan.output_format {
            OutputPixelFormat::Rgba8 => PixelFormat::Rgba8,
            OutputPixelFormat::Bgra8 => PixelFormat::Bgra8,
            OutputPixelFormat::Nv12 => PixelFormat::Nv12,
            OutputPixelFormat::P010 => PixelFormat::P010,
            OutputPixelFormat::Rgba16F => {
                return Err(RenderError::Capability(
                    "RGBA16F is a renderer intermediate, not a native output format",
                ));
            }
        };
        let viewport = config.frame.viewport;
        if camera.as_ref().is_some_and(|camera| {
            camera.pixel_shape() != (viewport.width, viewport.height)
                || plan.engine != ExecutionEngine::CertifiedCpu
        }) {
            return Err(RenderError::InvalidOptions(
                "camera dimensions and certified CPU identity must match the plan",
            ));
        }
        let compiler = match &camera {
            Some(_) => NativeCompiler::Camera(Box::new(
                CameraCompiler::new(config).map_err(RenderError::Renderer)?,
            )),
            None => NativeCompiler::Vector(Box::new(
                VectorFrameCompiler::new(config).map_err(RenderError::Renderer)?,
            )),
        };
        let stages = NativeFrameStages::new(plan.render_teams.len(), compiler);
        let stream = FrameStream::new(plan, stages, |_, output| {
            output.publish().map_err(NativeFrameError::Emitter)
        })
        .map_err(RenderError::Pipeline)?;
        Ok(Self {
            camera,
            stream: Some(stream),
            output,
            output_format,
            viewport,
            renderer_config: config,
        })
    }

    /// Apply a camera pose and light update to subsequent captures only.
    ///
    /// Earlier jobs keep their frozen camera data. The retained camera's
    /// revision stays monotone across updates, so a new pose cannot reuse an
    /// earlier pose's projection cache. Camera policy (pixel shape, frame
    /// rate, background, samples and point-norm bound) stays fixed.
    ///
    /// Validation precedes mutation or frame admission. A rejected update
    /// leaves the current camera usable and consumes no sequence number.
    ///
    /// # Errors
    /// Refuses non-camera pipelines, cancelled streams, invalid camera values,
    /// or changes to immutable capture policy.
    pub fn update_camera(&mut self, config: CameraConfig) -> Result<(), RenderError> {
        let stream = self
            .stream
            .as_ref()
            .ok_or(RenderError::Pipeline(FrameStreamError::Closed))?;
        if stream.cancellation_token().is_cancelled() {
            return Err(RenderError::Pipeline(FrameStreamError::Closed));
        }
        let Some(camera) = &mut self.camera else {
            return Err(RenderError::InvalidOptions(
                "camera updates require a camera pipeline",
            ));
        };
        let candidate = Camera::new(config).map_err(RenderError::Camera)?;
        if candidate.pixel_shape() != camera.pixel_shape()
            || candidate.fps() != camera.fps()
            || candidate.background() != camera.background()
            || candidate.samples() != camera.samples()
            || candidate.max_allowable_norm() != camera.max_allowable_norm()
        {
            return Err(RenderError::InvalidOptions(
                "camera updates must preserve capture policy",
            ));
        }
        let mut next = camera.clone();
        let from = camera.frame();
        let to = candidate.frame();
        if from.center() != to.center()
            || from.shape() != to.shape()
            || from.orientation() != to.orientation()
            || from.field_of_view() != to.field_of_view()
            || from.euler_axes() != to.euler_axes()
        {
            *next.frame_mut() = to.clone();
        }
        if next.light_source_position() != candidate.light_source_position() {
            next.set_light_source_position(candidate.light_source_position())
                .map_err(RenderError::Camera)?;
        }
        *camera = next;
        Ok(())
    }

    /// Admit and freeze one captured scene without transferring callbacks.
    ///
    /// Sequences must be reserved in increasing output order. Both pipeline
    /// and output capacity are acquired BEFORE freezing, including with one
    /// slot. The in-order preparation worker materializes and compiles the
    /// frozen state; the scene owner may advance while this work runs.
    ///
    /// # Errors
    /// Refuses closed/cancelled work or output admission immediately. Invalid
    /// scene data and compilation failures are asynchronous stage errors,
    /// recovered by `finish`; a later admission can first report a closed stream.
    pub fn capture(
        &mut self,
        stage: &fmn_mobject::Stage,
        sequence: u64,
    ) -> Result<(), RenderError> {
        self.capture_with(sequence, || stage.freeze_render_state())
    }

    /// Consume an already observed frame without materializing another arena
    /// or cloning its snapshot metadata on the scene owner.
    ///
    /// Admission precedes conversion into the callback-free render projection.
    /// Updater references from the packet are released only on this calling
    /// thread. A packet retained by a journal keeps its full replay state.
    /// The packet's exact capture time, records and handle domain survive;
    /// `sequence` is the independent ordered output index.
    ///
    /// # Errors
    /// As [`Self::capture`]. Compilation errors are recovered by `finish`.
    pub fn capture_packet(
        &mut self,
        packet: fmn_anim::FramePacket,
        sequence: u64,
    ) -> Result<(), RenderError> {
        self.capture_with(sequence, || packet.into_render_state())
    }

    fn capture_with(
        &mut self,
        sequence: u64,
        freeze: impl FnOnce() -> FrozenRenderState,
    ) -> Result<(), RenderError> {
        let stream = self
            .stream
            .as_mut()
            .ok_or(RenderError::Pipeline(FrameStreamError::Closed))?;
        let permit = stream.reserve().map_err(RenderError::Pipeline)?;
        let output = self
            .output
            .reserve(sequence)
            .map_err(RenderError::Emitter)?;
        let layout = output.frame().layout();
        if layout.format() != self.output_format
            || layout.width() != self.viewport.width
            || layout.height() != self.viewport.height
        {
            return Err(RenderError::InvalidOptions(
                "output ring layout must match the frame execution plan",
            ));
        }
        let frame = NativeFrameInput::Frozen {
            state: freeze(),
            camera: self.camera.clone(),
        };
        permit
            .submit(sequence, NativeFrameJob { frame, output })
            .map_err(|_| RenderError::Pipeline(FrameStreamError::Closed))
    }

    /// Queue a compiled frame without reconstructing its scene on the caller.
    ///
    /// The bundle's proven pure law or verbatim recorded state runs on the
    /// assigned render team. Only immutable canonical input crosses threads;
    /// the reconstructed arena and its compiler remain worker-local. A worker
    /// retains at most one decoded pure endpoint pair between frames. The job
    /// keeps its original clock index even when output sequences are rebased
    /// for a range or subdivision. Current camera pose is frozen on admission.
    ///
    /// This replays compiled FMTL inputs. It does not declare arbitrary native
    /// callbacks pure or rerun updater closures on workers. Decoded geometry
    /// and endpoint storage are additional to the pixel-only execution budget.
    /// Camera jobs require one retained raw frame per active render team.
    ///
    /// # Errors
    /// Refuses closed output, incompatible layouts or invalid admission order.
    /// Worker reconstruction/render errors are retained by `finish`.
    pub fn capture_compiled(
        &mut self,
        job: TimelineFrameJob,
        sequence: u64,
    ) -> Result<(), RenderError> {
        if job.has_camera_track() && self.camera.is_none() {
            return Err(RenderError::Capability(
                "camera-bearing FMTL requires a camera-aware CPU execution plan",
            ));
        }
        let frame = NativeFrame::Compiled {
            job,
            config: self.renderer_config,
            camera: self.camera.clone(),
        };
        self.capture_ready(frame, sequence)
    }

    /// Queue one native pure-play sample for interpolation on its render team.
    ///
    /// The job shares a callback-free begin program, never a live scene or
    /// trait object. Sampling, interpolation, compilation and rasterization
    /// stay under the same in-flight permit and ordered output reservation.
    /// Camera pose is frozen at admission; the worker uses the same Lumen
    /// compiler and conversion kernels as compiled replay. Program/record
    /// storage is additional to the existing pixel-only memory budget.
    ///
    /// # Errors
    /// As `capture_compiled`; worker failures are recovered by `finish`.
    pub fn capture_pure(&mut self, job: PureFrameJob, sequence: u64) -> Result<(), RenderError> {
        let frame = NativeFrame::Pure {
            job,
            config: self.renderer_config,
            camera: self.camera.clone(),
        };
        self.capture_ready(frame, sequence)
    }

    /// Queue a pure sample whose planar/camera route is selected after native
    /// interpolation on the render worker. The supplied camera is immutable
    /// capture data, never a host callback. Its native light handle is sampled
    /// from that frame's reconstructed Stage, including animated lights.
    ///
    /// This is the portal's existing adaptive policy: planar geometry under
    /// the default camera uses the selected CPU engine; camera-bound geometry
    /// uses Lumen's ordinary camera renderer. A Transform may enter and leave
    /// the camera route within one segment without changing either behavior.
    /// The fixed-route `capture_pure` contract is unchanged.
    ///
    /// # Errors
    /// Rejects incompatible camera dimensions before reserving output. Native
    /// geometry/light/worker failures are joined by `finish` as usual.
    pub fn capture_pure_adaptive(
        &mut self,
        job: PureFrameJob,
        sequence: u64,
        camera: NativeAdaptiveCamera,
    ) -> Result<(), RenderError> {
        if camera.pixel_shape() != (self.viewport.width, self.viewport.height) {
            return Err(RenderError::InvalidOptions(
                "adaptive camera dimensions must match the frame execution plan",
            ));
        }
        let frame = NativeFrame::AdaptivePure {
            job,
            config: self.renderer_config,
            camera,
        };
        self.capture_ready(frame, sequence)
    }

    fn capture_ready(&mut self, frame: NativeFrame, sequence: u64) -> Result<(), RenderError> {
        #[cfg(feature = "planted-frame-allocation")]
        for _ in 0..super::PLANTED_FRAME_ALLOCATIONS.load(std::sync::atomic::Ordering::Relaxed) {
            std::hint::black_box(Vec::<u8>::with_capacity(1));
        }
        let stream = self
            .stream
            .as_mut()
            .ok_or(RenderError::Pipeline(FrameStreamError::Closed))?;
        let permit = stream.reserve().map_err(RenderError::Pipeline)?;
        let output = self
            .output
            .reserve(sequence)
            .map_err(RenderError::Emitter)?;
        let layout = output.frame().layout();
        if layout.format() != self.output_format
            || layout.width() != self.viewport.width
            || layout.height() != self.viewport.height
        {
            return Err(RenderError::InvalidOptions(
                "output ring layout must match the frame execution plan",
            ));
        }
        let frame = NativeFrameInput::Ready(frame);
        permit
            .submit(sequence, NativeFrameJob { frame, output })
            .map_err(|_| RenderError::Pipeline(FrameStreamError::Closed))
    }

    /// Drain prior raster/conversion jobs without closing the capture stream.
    ///
    /// Earlier output reservations have been published in sequence when this
    /// returns. The emitter still owns their asynchronous delivery and final
    /// artifact commit; this barrier does not finalize an output sink. No
    /// extra frame or sequence number is generated, and captures may resume.
    ///
    /// # Errors
    /// Refuses cancellation or a failed stage. Call `finish` to recover the
    /// original stage failure and its joined resource counters.
    pub fn flush(&mut self) -> Result<fmn_runtime::BarrierContext, RenderError> {
        let result = self
            .stream
            .as_mut()
            .ok_or(RenderError::Pipeline(FrameStreamError::Closed))?
            .flush()
            .map_err(RenderError::Pipeline);
        if result.is_err() {
            self.output.cancel();
        }
        result
    }

    /// Seal capture input while already-admitted frames continue draining.
    ///
    /// This does not join workers or finalize the output generation. A caller
    /// can use another planned render team while this prefix drains, then
    /// call [`Self::finish`] to recover the complete result and release it.
    ///
    /// # Errors
    /// Refuses an already-finished pipeline. Stage failures remain for `finish`.
    pub fn close_input(&mut self) -> Result<(), RenderError> {
        self.stream
            .as_mut()
            .ok_or(RenderError::Pipeline(FrameStreamError::Closed))?
            .close_input();
        Ok(())
    }

    /// Drain and join all frame stages. This does NOT publish the artifact.
    ///
    /// # Errors
    /// Preserves the original stage failure and its final resource counters.
    pub fn finish(mut self) -> Result<PipelineStats, RenderError> {
        let stream = self
            .stream
            .take()
            .ok_or(RenderError::Pipeline(FrameStreamError::Closed))?;
        let result = stream.finish().map_err(RenderError::Pipeline);
        if result.is_err() {
            self.output.cancel();
        }
        result
    }
}

impl Drop for NativeFramePipeline {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            self.output.cancel();
            let _ = stream.cancel();
        }
    }
}

#[cfg(test)]
mod handoff_tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn camera_fixture() -> (fmn_mobject::Stage, RetainedFrameRendererConfig, Camera) {
        let background = fmn_core::color::LinearRgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };
        let config = RetainedFrameRendererConfig {
            frame: fmn_render::FrameConfig::new(
                Viewport {
                    width: 16,
                    height: 16,
                },
                fmn_render::ScreenMap {
                    scale: 2.0,
                    origin: [8.0, 8.0],
                    y_up: true,
                },
                background,
            ),
            tiling: fmn_render::Tiling {
                macro_tile: 16,
                fine_tile: 8,
            },
            engine: EngineIdentity::certified(),
            threads: 1,
        };
        let camera = Camera::new(CameraConfig {
            resolution: (16, 16),
            background,
            ..CameraConfig::default()
        })
        .expect("valid camera");
        let mut buffer = fmn_mobject::RecordBuffer::new(fmn_mobject::RecordSchema::vmobject(), 3)
            .expect("small triangle");
        buffer.write_range(
            "point",
            0,
            &[-2.0, -1.5, 0.0, 0.0, 1.5, 0.0, 2.0, -1.5, 0.0],
        );
        buffer.write_range("fill_rgba", 0, &[0.2, 0.6, 1.0, 1.0].repeat(3));
        let mut stage = fmn_mobject::Stage::new();
        let mob = stage.add(fmn_mobject::Mobject::from_buffer(buffer));
        stage.add_to_scene(mob).expect("live triangle root");
        (stage, config, camera)
    }

    #[test]
    fn camera_team_recycles_raw_pixels_and_clears_vector_resume_state() {
        let (stage, config, mut camera) = camera_fixture();
        let mut renderer = CameraCompiler::new(config).expect("retained renderer");
        let mut worker = RenderWorker::default();
        let layout =
            fmn_frame::FrameLayout::tight(PixelFormat::Rgba8, 16, 16).expect("output layout");
        let mut output = FrameBuffer::new(layout.clone());
        let initial = renderer
            .prepare_with_camera(&stage, &camera)
            .expect("first camera job");
        worker
            .render_camera(&initial, 1, &mut output)
            .expect("first camera raster");
        let first_bits = output.as_bytes().to_vec();
        let allocation = worker
            .spare
            .as_ref()
            .expect("retained raster")
            .0
            .as_bytes()
            .as_ptr();
        for (center, threads) in [0.5, 1.5, 2.5].into_iter().zip([1, 4, 16]) {
            camera
                .frame_mut()
                .set_center([center, 0.0, 0.0])
                .expect("camera motion");
            let frame = renderer
                .prepare_with_camera(&stage, &camera)
                .expect("camera capture");
            let fresh = frame.render(1).expect("fresh reference raster");
            let mut expected = FrameBuffer::new(layout.clone());
            convert_into(&fresh, &mut expected, 1).expect("reference conversion");
            assert!(
                !worker
                    .render_camera(&frame, threads, &mut output)
                    .expect("recycled raster")
            );
            let (raster, token) = worker.spare.as_ref().expect("retained raster");
            assert_eq!(raster.as_bytes().as_ptr(), allocation);
            assert_eq!(*token, None);
            assert_eq!(output.as_bytes(), expected.as_bytes());
        }
        assert_ne!(output.as_bytes(), first_bits);
        let mut compiler = VectorFrameCompiler::new(config).expect("vector compiler");
        let vector = compiler.capture(&stage, 0).expect("vector frame");
        worker
            .render_vector(&vector, 1, &mut output)
            .expect("vector raster");
        let vector_bits = output.as_bytes().to_vec();
        let (raster, token) = worker.spare.as_ref().expect("retained vector raster");
        assert_eq!(raster.as_bytes().as_ptr(), allocation);
        assert!(token.is_some());
        assert!(worker.camera_pixels.upgrade().is_none());
        assert!(
            !worker
                .render_camera(&initial, 4, &mut output)
                .expect("camera after vector")
        );
        assert_eq!(output.as_bytes(), first_bits);
        assert_eq!(worker.spare.as_ref().expect("camera raster").1, None);
        worker
            .render_vector(&vector, 4, &mut output)
            .expect("vector after camera");
        assert_eq!(output.as_bytes(), vector_bits);
        assert_eq!(
            worker
                .spare
                .as_ref()
                .expect("reused vector raster")
                .0
                .as_bytes()
                .as_ptr(),
            allocation
        );
    }

    #[test]
    fn failed_camera_conversion_discards_the_recycled_surface() {
        let (stage, config, camera) = camera_fixture();
        let mut renderer = CameraCompiler::new(config).expect("retained renderer");
        let frame = renderer
            .prepare_with_camera(&stage, &camera)
            .expect("camera job");
        let mut worker = RenderWorker::default();
        let mut output = FrameBuffer::new(
            fmn_frame::FrameLayout::tight(PixelFormat::Rgba8, 16, 16).expect("valid output"),
        );
        worker
            .render_camera(&frame, 1, &mut output)
            .expect("initial render");
        let expected = output.as_bytes().to_vec();
        let mut wrong_size = FrameBuffer::new(
            fmn_frame::FrameLayout::tight(PixelFormat::Rgba8, 8, 8).expect("small output"),
        );
        assert!(matches!(
            worker.render_camera(&frame, 1, &mut wrong_size),
            Err(NativeFrameError::Frame(FrameError::DimensionMismatch))
        ));
        assert!(worker.spare.is_none());
        assert!(worker.camera_pixels.upgrade().is_none());
        assert!(
            !worker
                .render_camera(&frame, 4, &mut output)
                .expect("fresh raster after failure")
        );
        assert_eq!(output.as_bytes(), expected);
        assert!(worker.spare.is_some());
    }

    #[test]
    fn repeated_camera_jobs_reuse_pixels_but_convert_each_output_slot() {
        let (stage, config, camera) = camera_fixture();
        let mut compiler = CameraCompiler::new(config).expect("camera compiler");
        let initial = compiler
            .prepare_with_camera(&stage, &camera)
            .expect("initial capture");
        let fresh = initial.render(1).expect("independent serial raster");
        let mut worker = RenderWorker::default();
        let mut calls = 0;
        for format in [
            PixelFormat::Rgba8,
            PixelFormat::Bgra8,
            PixelFormat::Nv12,
            PixelFormat::P010,
        ] {
            let layout = fmn_frame::FrameLayout::tight(format, 16, 16).expect("output layout");
            let mut expected = FrameBuffer::new(layout.clone());
            convert_into(&fresh, &mut expected, 1).expect("fresh output conversion");
            for threads in [1, 4, 16] {
                let repeated = compiler
                    .prepare_with_camera(&stage, &camera)
                    .expect("unchanged capture");
                assert!(Arc::ptr_eq(&initial, &repeated));
                let mut output = FrameBuffer::new(layout.clone());
                output.plane_mut(0).fill(0xA5);
                let reused = worker
                    .render_camera(&repeated, threads, &mut output)
                    .expect("worker capture");
                assert_eq!(reused, calls != 0);
                assert_eq!(output.as_bytes(), expected.as_bytes());
                calls += 1;
            }
        }
        let layout = fmn_frame::FrameLayout::tight(PixelFormat::Rgba8, 16, 16).unwrap();
        let mut output = FrameBuffer::new(layout);
        output.plane_mut(0).fill(0x3C);
        let before = output.as_bytes().to_vec();
        assert!(matches!(
            worker.render_camera(&initial, 0, &mut output),
            Err(NativeFrameError::Renderer(
                RetainedFrameRendererError::InvalidThreads
            ))
        ));
        assert_eq!(output.as_bytes(), before);
        assert!(worker.render_camera(&initial, 1, &mut output).unwrap());
    }

    #[test]
    fn worker_pixel_keys_do_not_keep_expired_captures_alive() {
        let (stage, config, camera) = camera_fixture();
        let mut compiler = CameraCompiler::new(config).expect("camera compiler");
        let first = compiler.prepare_with_camera(&stage, &camera).unwrap();
        let weak = Arc::downgrade(&first);
        let mut worker = RenderWorker::default();
        let layout = fmn_frame::FrameLayout::tight(PixelFormat::Rgba8, 16, 16).unwrap();
        let mut output = FrameBuffer::new(layout.clone());
        assert!(!worker.render_camera(&first, 1, &mut output).unwrap());
        let original = output.as_bytes().to_vec();
        let changed = Camera::new(CameraConfig {
            resolution: (16, 16),
            background: fmn_core::color::LinearRgba {
                r: 0.3,
                g: 0.1,
                b: 0.2,
                a: 1.0,
            },
            ..CameraConfig::default()
        })
        .unwrap();
        assert_eq!(camera.revision(), changed.revision());
        let next = compiler.prepare_with_camera(&stage, &changed).unwrap();
        assert!(!first.same_capture(&next));
        drop(first);
        assert!(weak.upgrade().is_none());
        assert!(worker.camera_pixels.upgrade().is_none());
        assert!(!worker.render_camera(&next, 4, &mut output).unwrap());
        assert_ne!(original, output.as_bytes());
        let raw = next.render(1).unwrap();
        let mut expected = FrameBuffer::new(layout);
        convert_into(&raw, &mut expected, 1).unwrap();
        assert_eq!(output.as_bytes(), expected.as_bytes());
        let repeated = compiler.prepare_with_camera(&stage, &changed).unwrap();
        assert!(worker.render_camera(&repeated, 16, &mut output).unwrap());
        drop(repeated);
        drop(next);
        drop(compiler);
        assert!(worker.camera_pixels.upgrade().is_none());
        assert!(
            worker.spare.is_some(),
            "only reusable pixel storage remains"
        );
    }

    #[test]
    fn preparation_inputs_never_carry_owner_thread_callbacks() {
        fn send<T: Send + 'static>() {}
        fn sync<T: Sync>() {}
        send::<NativeFrameJob>();
        send::<PreparedFrameJob>();
        send::<NativeCompiler>();
        sync::<NativeFrameStages>();
    }
}
