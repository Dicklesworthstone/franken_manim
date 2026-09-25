//! Owned Lumen jobs between the serial scene owner and the existing runtime.

use std::cell::RefCell;
use std::fmt;
use std::sync::Mutex;

use fmn_frame::convert::{rgba_to_nv12, rgba_to_p010, rgba16f_to_rgba8, swap_rb8};
use fmn_frame::{ChromaSiting, ColorRange, FrameBuffer, FrameError, FrameLayout, PixelFormat};
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

/// A failure in worker-owned rasterization, conversion, or publication.
#[derive(Debug)]
pub enum NativeFrameError {
    /// A validated compiled frame refused reconstruction on its worker.
    Bundle(BundleReadError),
    /// Lumen refused the frozen input.
    Renderer(RetainedFrameRendererError),
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
            Self::Renderer(error) => error.fmt(f),
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
            Self::Renderer(error) => Some(error),
            Self::Frame(error) => Some(error),
            Self::Emitter(error) => Some(error),
            Self::WorkerState(_) => None,
        }
    }
}

#[expect(
    clippy::large_enum_variant,
    reason = "each frame moves once through a bounded queue holding about one job per worker; \
              boxing the camera frame would add a heap allocation per frame (PG-6)"
)]
pub(super) enum NativeFrame {
    Camera(PreparedCameraFrame),
    Vector(OwnedVectorFrame),
    Compiled {
        job: TimelineFrameJob,
        config: RetainedFrameRendererConfig,
        camera: Option<Camera>,
    },
}

pub(super) struct NativeFrameJob {
    pub frame: NativeFrame,
    // Reserve on the scene owner, in capture order, not on raster workers.
    pub output: FrameReservation,
}

#[derive(Default)]
struct VectorWorker {
    arena: FrameArena,
    cache: PixelTileCache,
    compiled_vector: Option<VectorFrameCompiler>,
    compiled_camera: Option<RetainedFrameRenderer>,
}

pub(super) struct NativeFrameStages {
    // One independent arena/cache per real render team. No global raster lock:
    // nonconsecutive frame contents and cache identities decide reuse.
    workers: Vec<Mutex<VectorWorker>>,
}

impl NativeFrameStages {
    pub fn new(render_teams: usize) -> Self {
        Self {
            workers: (0..render_teams)
                .map(|_| Mutex::new(VectorWorker::default()))
                .collect(),
        }
    }
}

impl PipelineStages for NativeFrameStages {
    type Frame = NativeFrameJob;
    type Prepared = NativeFrameJob;
    type Rasterized = (FrameBuffer, FrameReservation);
    type Output = FrameReservation;
    type Error = NativeFrameError;

    fn prepare(&self, frame: NativeFrameJob, _: &TeamPlan) -> Result<NativeFrameJob, Self::Error> {
        // All live scene access has finished on its serial owner.
        Ok(frame)
    }

    fn rasterize(
        &self,
        job: NativeFrameJob,
        team: &TeamPlan,
    ) -> Result<Self::Rasterized, Self::Error> {
        let frame = match job.frame {
            NativeFrame::Camera(frame) => frame
                .render(team.threads())
                .map_err(NativeFrameError::Renderer)?,
            NativeFrame::Compiled {
                job,
                config,
                camera,
            } => {
                // Reconstruction is inside the render-team worker, not the
                // serial source or prepare stage. It returns a local Stage
                // which is compiled here and dropped before conversion.
                let stage = COMPILED_ENDPOINTS
                    .with(|cache| cache.borrow_mut().materialize(&job))
                    .map_err(NativeFrameError::Bundle)?;
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
                    .map_err(|_| {
                        NativeFrameError::WorkerState("compiled worker state was poisoned")
                    })?;
                if let Some(camera) = camera {
                    if worker.compiled_camera.is_none() {
                        worker.compiled_camera = Some(
                            RetainedFrameRenderer::new(config)
                                .map_err(NativeFrameError::Renderer)?,
                        );
                    }
                    worker
                        .compiled_camera
                        .as_mut()
                        .ok_or(NativeFrameError::WorkerState(
                            "missing compiled camera renderer",
                        ))?
                        .prepare_with_camera(&stage, &camera)
                        .map_err(NativeFrameError::Renderer)?
                        .render(team.threads())
                        .map_err(NativeFrameError::Renderer)?
                } else {
                    if worker.compiled_vector.is_none() {
                        worker.compiled_vector = Some(
                            VectorFrameCompiler::new(config).map_err(NativeFrameError::Renderer)?,
                        );
                    }
                    let VectorWorker {
                        compiled_vector,
                        arena,
                        cache,
                        ..
                    } = &mut *worker;
                    compiled_vector
                        .as_mut()
                        .ok_or(NativeFrameError::WorkerState(
                            "missing compiled vector compiler",
                        ))?
                        .capture(&stage, 0)
                        .map_err(NativeFrameError::Renderer)?
                        .render_cached(team.threads(), arena, cache)
                        .map_err(NativeFrameError::Renderer)?
                        .0
                }
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
                let VectorWorker { arena, cache, .. } = &mut *worker;
                frame
                    .render_cached(team.threads(), arena, cache)
                    .map_err(NativeFrameError::Renderer)?
                    .0
            }
        };
        Ok((frame, job.output))
    }

    fn convert(
        &self,
        (frame, mut output): Self::Rasterized,
        _: &TeamPlan,
    ) -> Result<FrameReservation, Self::Error> {
        let format = output.frame().layout().format();
        match format {
            PixelFormat::Rgba8 => {
                rgba16f_to_rgba8(&frame, output.frame_mut()).map_err(NativeFrameError::Frame)?;
            }
            PixelFormat::Bgra8 | PixelFormat::Nv12 | PixelFormat::P010 => {
                let layout = FrameLayout::tight(
                    PixelFormat::Rgba8,
                    frame.layout().width(),
                    frame.layout().height(),
                )
                .map_err(NativeFrameError::Frame)?;
                let mut scratch = FrameBuffer::new(layout);
                rgba16f_to_rgba8(&frame, &mut scratch).map_err(NativeFrameError::Frame)?;
                match format {
                    PixelFormat::Bgra8 => swap_rb8(&scratch, output.frame_mut()),
                    PixelFormat::Nv12 => rgba_to_nv12(
                        &scratch,
                        output.frame_mut(),
                        ColorRange::Limited,
                        ChromaSiting::Left,
                    ),
                    PixelFormat::P010 => rgba_to_p010(
                        &scratch,
                        output.frame_mut(),
                        ColorRange::Limited,
                        ChromaSiting::Left,
                    ),
                    _ => unreachable!("conversion format was checked above"),
                }
                .map_err(NativeFrameError::Frame)?;
            }
            PixelFormat::Rgba16F => {
                return Err(NativeFrameError::WorkerState(
                    "RGBA16F is a renderer intermediate, not a native output format",
                ));
            }
        }
        Ok(output)
    }
}

/// Shared CPU capture-to-output pipeline for native front doors.
///
/// The caller owns the `OrderedEmitter` and its final artifact publication.
/// This adapter owns serial scene-to-IR capture, bounded raster/conversion
/// work on every planned render team, and worker-local retained tile caches.
/// The output ring capacity must equal `plan.frames_in_flight`.
/// Call `finish` successfully BEFORE finishing the emitter. Dropping an
/// unfinished adapter cancels the emitter before joining all pipeline workers.
///
/// Pixel admission must include one retained RGBA16F cache per affine render
/// team (or a retained camera frame on the owner and each compiled-camera
/// render team), in addition to the plan's in-flight surfaces. Plan NV12/BGRA/P010 conversions with an RGBA8 intermediate.
pub struct NativeFramePipeline {
    compiler: NativeCompiler,
    stream: Option<FrameStream<NativeFrameJob, NativeFrameError>>,
    output: EmitterHandle,
    output_format: PixelFormat,
    viewport: Viewport,
    renderer_config: RetainedFrameRendererConfig,
}

enum NativeCompiler {
    Vector(VectorFrameCompiler),
    Camera {
        renderer: Box<RetainedFrameRenderer>,
        camera: Camera,
    },
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
        let compiler = match camera {
            Some(camera) => NativeCompiler::Camera {
                renderer: Box::new(
                    RetainedFrameRenderer::new(config).map_err(RenderError::Renderer)?,
                ),
                camera,
            },
            None => NativeCompiler::Vector(
                VectorFrameCompiler::new(config).map_err(RenderError::Renderer)?,
            ),
        };
        let stages = NativeFrameStages::new(plan.render_teams.len());
        let stream = FrameStream::new(plan, stages, |_, output| {
            output.publish().map_err(NativeFrameError::Emitter)
        })
        .map_err(RenderError::Pipeline)?;
        Ok(Self {
            compiler,
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
        let NativeCompiler::Camera { camera, .. } = &mut self.compiler else {
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

    /// Admit and freeze one captured scene, without running callbacks on workers.
    ///
    /// Sequences must be reserved in increasing output order. Admission precedes
    /// IR freezing, including for a one-slot plan; no extra frozen queue exists.
    ///
    /// # Errors
    /// Refuses invalid scene data, closed/cancelled work, or output admission.
    /// A closed stream's detailed stage error is recovered by `finish`.
    pub fn capture(
        &mut self,
        stage: &fmn_mobject::Stage,
        sequence: u64,
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
        let frame = match &mut self.compiler {
            NativeCompiler::Vector(compiler) => {
                NativeFrame::Vector(compiler.capture(stage, 0).map_err(RenderError::Renderer)?)
            }
            NativeCompiler::Camera { renderer, camera } => NativeFrame::Camera(
                renderer
                    .prepare_with_camera(stage, camera)
                    .map_err(RenderError::Renderer)?,
            ),
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
        let camera = match &self.compiler {
            NativeCompiler::Camera { camera, .. } => Some(camera.clone()),
            NativeCompiler::Vector(_) => None,
        };
        let frame = NativeFrame::Compiled {
            job,
            config: self.renderer_config,
            camera,
        };
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
