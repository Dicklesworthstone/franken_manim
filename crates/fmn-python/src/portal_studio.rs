//! Host-CPython Studio composition. No Python dependency enters the native CLI.
//!
//! Capture uses the normal stepped lifecycle and the shared Lumen renderer.
//! Read-only playback releases the authored Scene. Interactive playback retains
//! its Scene and import context in the same worker service and protocol loop.
//! Source reload is a new process, never replay of a serialized Python closure.

use super::*;
use fmn_studio::{
    FrameEncoding, FramePayload, FrameStream, InspectorLimits, InspectorSnapshot, InspectorView,
    ProtocolDigest, ProtocolLimits, ServiceError, SpanRegistry, SupervisorRequest, WorkerErrorCode,
    WorkerResponse, WorkerService, protocol_digest,
};
use std::time::Duration;

pub(super) mod bundle;
pub(super) mod live;

pub(super) struct Capture {
    bundle: Option<bundle::BundleCapture>,
    renderer: RetainedFrameRenderer,
    tuning: portal_autotune::Report,
    camera: Camera,
    rgba: FrameBuffer,
    light: Option<Mob>,
    scene: String,
    recorded: fmn_studio::recorded::RecordedTimeline,
    failure: Option<String>,
    live: bool,
    live_refresh: bool,
    pub(super) timeline: OutputTimeline,
}

impl Capture {
    #[allow(clippy::too_many_arguments)]
    fn new(
        scene: String,
        build: ProtocolDigest,
        source: ProtocolDigest,
        width: u32,
        height: u32,
        fps: u32,
        threads: usize,
        max_frames: usize,
        max_bytes: usize,
        mut config: fmn_config::Config,
    ) -> PyResult<(Self, RuntimeConfig)> {
        if width == 0
            || height == 0
            || u64::from(width) * u64::from(height) > 16_777_216
            || !(1..=240).contains(&fps)
            || !(1..=96).contains(&threads)
        {
            return Err(PyValueError::new_err(
                "Studio requires at most 16M pixels, 1..240 FPS and 1..96 threads",
            ));
        }
        let recorded = fmn_studio::recorded::RecordedTimeline::new(
            scene.clone(),
            build,
            source,
            max_frames,
            max_bytes,
        )
        .map_err(native_error)?;
        config.camera.resolution = (width, height);
        config.camera.fps = fps;
        config.determinism.mode = fmn_config::config::DeterminismMode::Standard;
        let request = fmn_runtime::PlanRequest::standard(
            fmn_runtime::RenderIntent::Offline,
            fmn_runtime::SurfaceSpec::lumen(width, height),
            fmn_runtime::OutputPixelFormat::Rgba8,
        )
        .with_max_cpu_threads(threads)
        // Camera/ThreeD executes this engine even in standard artifact mode.
        // A FastCpu tile measurement must never be applied to that route.
        .with_engine(fmn_runtime::ExecutionEngine::CertifiedCpu);
        let plan = fmn::autotune::derive_execution_plan(
            &fmn_platform::fs::StdFs,
            &config,
            request,
            &fmn_platform::topology::HardwareTopology::current(),
        )
        .map_err(native_error)?;
        let engine = match plan.engine {
            // render_with_camera executes ThreeDJob's CPU route, not the
            // affine FastCpu FrameJob. Python/source execution is uncertified.
            fmn_runtime::ExecutionEngine::FastCpu | fmn_runtime::ExecutionEngine::CertifiedCpu => {
                EngineIdentity::certified()
            }
            _ => {
                return Err(CapabilityError::new_err(
                    "Python Studio capture requires a CPU execution plan",
                ));
            }
        };
        // The configured background (default #333333), as standalone fmn and
        // the planar FMTL player use: a planar bundle has no background track,
        // so this is the baseline the scene's camera must match on export.
        let background = fmn_core::color::Srgb::from_hex(&config.camera.background_color)
            .map_err(|error| PyValueError::new_err(error.to_string()))?
            .to_linear(config.camera.background_opacity);
        let camera = Camera::new(CameraConfig {
            resolution: (width, height),
            fps,
            background,
            ..CameraConfig::default()
        })
        .map_err(camera_error)?;
        let frame = FrameConfig::new(
            Viewport { width, height },
            ScreenMap {
                scale: f64::from(height) / config.sizes.frame_height,
                origin: [f64::from(width) / 2.0, f64::from(height) / 2.0],
                y_up: true,
            },
            camera.background(),
        )
        .with_aa_policy(config.render.aa);
        let renderer = RetainedFrameRenderer::new(RetainedFrameRendererConfig {
            frame,
            engine,
            tiling: Tiling {
                macro_tile: plan.macro_tile,
                fine_tile: plan.fine_tile,
            },
            threads: plan
                .render_teams
                .first()
                .map_or(1, fmn_runtime::TeamPlan::threads),
        })
        .map_err(native_error)?;
        let rgba = FrameBuffer::new(
            FrameLayout::tight(PixelFormat::Rgba8, width, height).map_err(native_error)?,
        );
        Ok((
            Self {
                bundle: None,
                renderer,
                tuning: portal_autotune::Report::from_plan(&plan),
                camera,
                rgba,
                light: None,
                scene,
                recorded,
                failure: None,
                live: false,
                live_refresh: false,
                timeline: OutputTimeline::default(),
            },
            RuntimeConfig::from_config(&config),
        ))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.bundle.as_ref().map_or_else(
            || self.recorded.frame_count() == 0,
            bundle::BundleCapture::is_empty,
        )
    }

    pub(super) fn bind_camera(
        &mut self,
        frame: fmn_scene::studio_bridge::CameraFrame,
        light: [f64; 3],
        background: Option<fmn_core::color::LinearRgba>,
        light_mob: Option<Mob>,
    ) -> PyResult<()> {
        *self.camera.frame_mut() = frame;
        self.camera
            .set_light_source_position(light)
            .map_err(camera_error)?;
        if let Some(background) = background {
            self.camera
                .set_background(background)
                .map_err(camera_error)?;
        }
        self.light = light_mob;
        if let Some(bundle) = &mut self.bundle {
            bundle.validate_camera(&self.camera)?;
        }
        Ok(())
    }

    pub(super) fn capture(
        &mut self,
        packet: fmn_scene::studio_bridge::FramePacket,
    ) -> Result<(), fmn_scene::IntegrationError> {
        // Live input is a paused edit boundary. play/wait still execute their
        // ordinary samples and updaters, but only the completed callback's
        // final state replaces the live view. Intermediate/failed callback
        // captures cannot publish a partial edit or grow recorded history.
        if self.bundle.is_some() && self.live {
            return Err(fmn_scene::IntegrationError::new(
                "fmtl-recording",
                "live Studio input cannot share a bundle export",
            ));
        }
        if self.live && !self.live_refresh {
            return Ok(());
        }
        if let Some(error) = &self.failure {
            return Err(fmn_scene::IntegrationError::new(
                "python-studio",
                error.clone(),
            ));
        }
        if let Err(error) = self.capture_inner(packet) {
            let message = error.to_string();
            self.failure = Some(message.clone());
            return Err(fmn_scene::IntegrationError::new("python-studio", message));
        }
        Ok(())
    }

    fn capture_inner(&mut self, packet: fmn_scene::studio_bridge::FramePacket) -> PyResult<()> {
        let stage = packet.materialize_stage();
        if let Some(light) = self.light {
            if stage.get(light).is_none() {
                return Err(PyRuntimeError::new_err("Studio camera light is stale"));
            }
            self.camera
                .set_light_source_position(stage.get_center(light))
                .map_err(camera_error)?;
        }
        if let Some(bundle) = &mut self.bundle {
            return bundle.capture(packet, &stage, &self.camera);
        }
        self.renderer
            .render_with_camera(&stage, &self.camera)
            .map_err(native_error)?;
        portal_video::convert_frame(self.renderer.frame(), &mut self.rgba, None)
            .map_err(native_error)?;
        let (width, height) = self.camera.pixel_shape();
        let bytes = fmn_codec::encode_rgba8(
            width,
            height,
            self.rgba.plane(0),
            fmn_codec::CompressionLevel::Fast,
        );
        let config = self.renderer.config();
        let backend = fmn_studio::native::camera_capture_backend(&self.camera, config)
            .map_err(native_error)?;
        let stream = FrameStream {
            scene: self.scene.clone(),
            frame_index: 0,
            width,
            height,
            stride: 0,
            encoding: FrameEncoding::Png,
            payload: FramePayload::Pipe {
                digest: protocol_digest(&bytes),
                bytes,
            },
            render_backends: vec![backend],
        };
        let mut snapshot =
            InspectorSnapshot::capture(&stage, &SpanRegistry::new(), InspectorLimits::default())
                .map_err(native_error)?;
        // Geometry is world-space inspection. This read-only view deliberately
        // advertises no events or overlay-based picking for projected cameras.
        snapshot.view = Some(
            InspectorView::new(
                0,
                1,
                self.camera.fps(),
                Viewport { width, height },
                ScreenMap {
                    scale: if self.live {
                        f64::from(height) / fmn_core::constants::FRAME_HEIGHT
                    } else {
                        1.0 / self.camera.pixel_size()
                    },
                    origin: [f64::from(width) / 2.0, f64::from(height) / 2.0],
                    y_up: true,
                },
                false,
            )
            .map_err(native_error)?,
        );
        if self.live {
            self.recorded
                .replace_live_frame(stream, snapshot)
                .map_err(native_error)
        } else {
            self.recorded.push(stream, snapshot).map_err(native_error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn begin(
    slf: &Bound<'_, PyScene>,
    name: String,
    build: &str,
    source: &str,
    width: u32,
    height: u32,
    fps: u32,
    threads: usize,
    seed: u64,
    max_frames: usize,
    max_bytes: usize,
) -> PyResult<()> {
    portal_video::check_scene_ownership(slf)?;
    let config = portal_autotune::camera_snapshot(slf.py())?;
    portal_video::check_scene_ownership(slf)?;
    let (capture, runtime) = Capture::new(
        name,
        ProtocolDigest::from_hex(build).map_err(native_error)?,
        ProtocolDigest::from_hex(source).map_err(native_error)?,
        width,
        height,
        fps,
        threads,
        max_frames,
        max_bytes,
        config,
    )?;
    let replacement = Scene::new(runtime, seed).map_err(native_error)?;
    let slot = Arc::clone(&slf.borrow().render);
    let mut slot = slot
        .lock()
        .map_err(|_| PyRuntimeError::new_err("Studio generation lock poisoned"))?;
    if slot.is_some() {
        return Err(PyRuntimeError::new_err(
            "a render generation is already active",
        ));
    }
    crate::install_pristine_arena(slf, Rc::new(EngineState::new(replacement)))?;
    *slot = Some(PortalRenderSession::Preview(Box::new(capture)));
    Ok(())
}

pub(super) fn finish(slf: &Bound<'_, PyScene>) -> PyResult<Recording> {
    let original_engine = Rc::clone(&slf.borrow().engine);
    let render = Arc::clone(&slf.borrow().render);
    {
        let slot = render
            .lock()
            .map_err(|_| PyRuntimeError::new_err("Studio generation lock poisoned"))?;
        if !matches!(slot.as_ref(), Some(PortalRenderSession::Preview(c)) if c.bundle.is_none()) {
            return Err(PyRuntimeError::new_err("no Studio capture is active"));
        }
    }
    portal_playback::synchronize(slf)?;
    synchronize_portal_camera(slf)?;
    let exact_timing = slf.borrow().engine.borrow().has_frame_duration_requests();
    let empty = render
        .lock()
        .map_err(|_| PyRuntimeError::new_err("Studio generation lock poisoned"))?
        .as_ref()
        .is_some_and(|session| session.needs_final_capture(exact_timing));
    if empty {
        let engine = Rc::clone(&slf.borrow().engine);
        let mut sink = PortalSceneSink {
            render: Arc::clone(&render),
            ..PortalSceneSink::default()
        };
        engine.borrow_mut().show(&mut sink).map_err(native_error)?;
    }
    let mut slot = render
        .lock()
        .map_err(|_| PyRuntimeError::new_err("Studio generation lock poisoned"))?;
    // Camera descriptors can execute Python. A second owner check prevents
    // their re-entrant cancellation/replacement from consuming another session.
    if !matches!(slot.as_ref(), Some(PortalRenderSession::Preview(_)))
        || !Rc::ptr_eq(&original_engine, &slf.borrow().engine)
    {
        return Err(PyRuntimeError::new_err(
            "Studio capture ownership changed during finish",
        ));
    }
    let Some(PortalRenderSession::Preview(capture)) = slot.take() else {
        unreachable!()
    };
    if let Some(error) = capture.failure {
        return Err(PyRuntimeError::new_err(error));
    }
    Ok(Recording {
        worker: Some(CapturedWorker::Recorded(Box::new(capture.recorded))),
        tuning: capture.tuning,
    })
}

enum CapturedWorker {
    Recorded(Box<fmn_studio::recorded::RecordedTimeline>),
    Live(live::LiveWorker),
}

impl CapturedWorker {
    fn frame_count(&self) -> PyResult<usize> {
        match self {
            Self::Recorded(worker) => Ok(worker.frame_count()),
            Self::Live(worker) => worker.frame_count(),
        }
    }
}

impl WorkerService for CapturedWorker {
    fn build_id(&self) -> ProtocolDigest {
        match self {
            Self::Recorded(worker) => worker.build_id(),
            Self::Live(worker) => worker.build_id(),
        }
    }
    fn active_scene(&self) -> Option<&str> {
        match self {
            Self::Recorded(worker) => worker.active_scene(),
            Self::Live(worker) => worker.active_scene(),
        }
    }
    fn last_state_hash(&self) -> Option<ProtocolDigest> {
        match self {
            Self::Recorded(worker) => worker.last_state_hash(),
            Self::Live(worker) => worker.last_state_hash(),
        }
    }
    fn begin_session(&mut self, build: ProtocolDigest, bytes: usize) -> Result<(), ServiceError> {
        match self {
            Self::Recorded(worker) => worker.begin_session(build, bytes),
            Self::Live(worker) => worker.begin_session(build, bytes),
        }
    }
    fn handle(&mut self, request: SupervisorRequest) -> Result<WorkerResponse, ServiceError> {
        match self {
            Self::Recorded(worker) => worker.handle(request),
            Self::Live(worker) => worker.handle(request),
        }
    }
}

/// The prepared service is moved into the one deferred worker loop.
#[pyclass(name = "_StudioRecording", module = "manimlib")]
pub(super) struct Recording {
    worker: Option<CapturedWorker>,
    tuning: portal_autotune::Report,
}

struct CaptureContext(Option<Py<PyAny>>);

impl CaptureContext {
    fn close(&mut self) -> PyResult<()> {
        if let Some(context) = self.0.take() {
            Python::attach(|py| {
                context
                    .bind(py)
                    .call_method1("__exit__", (py.None(), py.None(), py.None()))
                    .map(|_| ())
            })
        } else {
            Ok(())
        }
    }
}

impl Drop for CaptureContext {
    fn drop(&mut self) {
        if let Err(error) = self.close() {
            Python::attach(|py| error.print(py));
        }
    }
}

/// A fresh portal worker answers Hello before executing authored scene code.
/// The supervisor adds the measured startup allowance to its first-frame
/// budget and retains the previous worker until that frame has validated.
struct DeferredRecording {
    build_id: ProtocolDigest,
    scene: String,
    capture: Option<Py<PyAny>>,
    worker: Option<CapturedWorker>,
    // Field order releases the native live service before its source context.
    context: Option<CaptureContext>,
    session: Option<(ProtocolDigest, usize)>,
}

impl WorkerService for DeferredRecording {
    fn build_id(&self) -> ProtocolDigest {
        self.build_id
    }

    fn active_scene(&self) -> Option<&str> {
        Some(&self.scene)
    }

    fn last_state_hash(&self) -> Option<ProtocolDigest> {
        self.worker
            .as_ref()
            .and_then(WorkerService::last_state_hash)
    }

    fn begin_session(
        &mut self,
        supervisor_build: ProtocolDigest,
        max_frame_bytes: usize,
    ) -> Result<(), ServiceError> {
        if let Some(worker) = self.worker.as_mut() {
            worker.begin_session(supervisor_build, max_frame_bytes)?;
        }
        self.session = Some((supervisor_build, max_frame_bytes));
        Ok(())
    }

    fn handle(&mut self, request: SupervisorRequest) -> Result<WorkerResponse, ServiceError> {
        if self.worker.is_none() {
            let (supervisor_build, max_frame_bytes) = self.session.ok_or_else(|| {
                ServiceError::new(
                    WorkerErrorCode::InvalidRequest,
                    "Studio worker has no session",
                )
            })?;
            // Consume before calling Python: a failed or hung scene must never
            // execute again on another request in this worker generation.
            let capture = self.capture.take().ok_or_else(|| {
                ServiceError::new(
                    WorkerErrorCode::ExecutionFailed,
                    "Studio scene capture has already failed",
                )
            })?;
            let (mut worker, context) = Python::attach(|py| -> PyResult<_> {
                let context = capture.call0(py)?;
                let value = context.bind(py).call_method0("__enter__")?;
                let context = CaptureContext(Some(context));
                let mut recording = value.extract::<PyRefMut<'_, Recording>>()?;
                let worker = recording
                    .worker
                    .take()
                    .ok_or_else(|| PyRuntimeError::new_err("Studio recording already served"))?;
                Ok((worker, context))
            })
            .map_err(|error| {
                ServiceError::new(WorkerErrorCode::ExecutionFailed, error.to_string())
            })?;
            self.context = Some(context);
            if worker.build_id() != self.build_id
                || worker.active_scene() != Some(self.scene.as_str())
            {
                return Err(ServiceError::new(
                    WorkerErrorCode::ExecutionFailed,
                    "Studio capture identity differs from its worker artifact",
                ));
            }
            worker.begin_session(supervisor_build, max_frame_bytes)?;
            self.worker = Some(worker);
        }
        self.worker
            .as_mut()
            .ok_or_else(|| {
                ServiceError::new(
                    WorkerErrorCode::ExecutionFailed,
                    "Studio capture is unavailable",
                )
            })?
            .handle(request)
    }
}

impl DeferredRecording {
    fn close_capture(&mut self) -> PyResult<()> {
        drop(self.worker.take());
        if let Some(mut context) = self.context.take() {
            context.close()?;
        }
        Ok(())
    }
}

#[pymethods]
impl Recording {
    #[staticmethod]
    fn serve_deferred(
        py: Python<'_>,
        build: &str,
        scene: String,
        capture: Py<PyAny>,
    ) -> PyResult<()> {
        if !capture.bind(py).is_callable() {
            return Err(PyTypeError::new_err("Studio capture must be callable"));
        }
        let build_id = ProtocolDigest::from_hex(build).map_err(native_error)?;
        let mut worker = DeferredRecording {
            build_id,
            scene,
            capture: Some(capture),
            worker: None,
            context: None,
            session: None,
        };
        py.detach(move || {
            let served = fmn_studio::serve_worker(
                &mut worker,
                &mut std::io::stdin().lock(),
                &mut std::io::stdout().lock(),
                ProtocolLimits::default(),
            )
            .map(|_| ())
            .map_err(|error| error.to_string());
            let closed = worker.close_capture().map_err(|error| error.to_string());
            match (served, closed) {
                (Err(error), Err(cleanup)) => {
                    Err(format!("{error}; source cleanup failed: {cleanup}"))
                }
                (Err(error), _) => Err(error),
                (Ok(()), closed) => closed,
            }
        })
        .map_err(PyRuntimeError::new_err)
    }

    /// Planner evidence; standard camera capture remains an uncertified artifact.
    #[getter]
    fn autotune(slf: &Bound<'_, Self>) -> PyResult<Py<PyDict>> {
        let report = slf.borrow().tuning.clone();
        report.to_python(slf.py())
    }

    #[getter]
    fn frame_count(&self) -> PyResult<usize> {
        self.worker
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("recording already served"))?
            .frame_count()
    }
    // The same native worker methods support in-process consumers and tests;
    // no second inspector JSON writer or frame decoder lives in the portal.
    fn inspect(&mut self, frame: i64) -> PyResult<String> {
        let worker = self
            .worker
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("recording already served"))?;
        let scene = worker.active_scene().unwrap_or_default().to_owned();
        worker
            .handle(fmn_studio::SupervisorRequest::Scrub {
                scene: scene.clone(),
                frame,
            })
            .map_err(native_error)?;
        let fmn_studio::WorkerResponse::StudioData { bytes, .. } = worker
            .handle(fmn_studio::SupervisorRequest::Inspect { scene })
            .map_err(native_error)?
        else {
            return Err(PyRuntimeError::new_err(
                "invalid recorded inspector response",
            ));
        };
        String::from_utf8(bytes).map_err(native_error)
    }
}

struct PythonBuilder {
    callback: Py<PyAny>,
    scene: String,
}
impl fmn_studio::RebuildDriver for PythonBuilder {
    fn rebuild(&mut self) -> Result<fmn_studio::WorkerArtifact, fmn_studio::BuildError> {
        Python::attach(|py| {
            let result = self
                .callback
                .call0(py)
                .map_err(|e| fmn_studio::BuildError::new(e.to_string()))?;
            type Launch = (String, Vec<String>, Vec<(String, String)>, String, String);
            let (executable, argv, env, cwd, build) = result
                .extract::<Launch>(py)
                .map_err(|e| fmn_studio::BuildError::new(e.to_string()))?;
            let build_id = ProtocolDigest::from_hex(&build)
                .map_err(|e| fmn_studio::BuildError::new(e.to_string()))?;
            Ok(fmn_studio::WorkerArtifact {
                executable: executable.into(),
                argv,
                env,
                cwd: Some(cwd.into()),
                build_id,
                prepare_scene: Some(self.scene.clone()),
            })
        })
    }
}

struct RunningHost {
    shutdown: Arc<AtomicBool>,
    frames: fmn_studio::FrameHub,
    thread: Option<std::thread::JoinHandle<Result<(), String>>>,
}

/// The existing bounded, content-based native watcher, owned by one portal
/// polling thread. No Python import or authored callback runs while scanning.
#[pyclass(name = "_StudioSourceWatch", module = "manimlib")]
pub(super) struct SourceWatcher {
    watch: fmn_studio::project_watch::SourceWatch,
    started: std::time::Instant,
}

#[pymethods]
impl SourceWatcher {
    #[getter]
    fn fingerprint(&self) -> String {
        self.watch.content_fingerprint().to_hex()
    }

    #[getter]
    fn files(&self) -> PyResult<Vec<(String, String)>> {
        self.watch
            .source_files()
            .map(|(path, digest)| {
                let path = path.to_str().ok_or_else(|| {
                    PyValueError::new_err("Studio source identity requires UTF-8 paths")
                })?;
                Ok((path.to_owned(), digest.to_hex()))
            })
            .collect()
    }

    fn poll(&mut self, py: Python<'_>) -> PyResult<bool> {
        py.detach(|| {
            self.watch
                .poll(self.started.elapsed())
                .map_err(|e| e.to_string())
        })
        .map_err(PyRuntimeError::new_err)
    }
}
impl Drop for RunningHost {
    fn drop(&mut self) {
        // Drop must not wait for a client which needs the GIL to build a reload.
        // The serving thread owns/reaps the disposable worker when it exits.
        self.shutdown.store(true, Ordering::Release);
        self.frames.close();
    }
}

#[pyclass(name = "_StudioHost", module = "manimlib")]
pub(super) struct Host {
    url: String,
    running: Option<RunningHost>,
}

#[pymethods]
impl Host {
    #[new]
    fn new(
        py: Python<'_>,
        builder: Py<PyAny>,
        scene: String,
        token: &str,
        port: u16,
        timeout: u64,
        startup_timeout: u64,
    ) -> PyResult<Self> {
        if !(1..=900).contains(&timeout) {
            return Err(PyValueError::new_err(
                "Studio worker timeout must be 1..900 seconds",
            ));
        }
        if !(1..=900).contains(&startup_timeout) {
            return Err(PyValueError::new_err(
                "Studio startup timeout must be 1..900 seconds",
            ));
        }
        if !builder.bind(py).is_callable() {
            return Err(PyTypeError::new_err("Studio builder must be callable"));
        }
        let token = fmn_studio::CapabilityToken::from_hex(token).map_err(native_error)?;
        let config = fmn_studio::StudioHostConfig {
            bind_addr: std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            // FrameHub::close wakes idle streams on shutdown. Retain the
            // native idle interval so a paused preview does not reconnect
            // continuously while the author is inspecting it.
            ..fmn_studio::StudioHostConfig::default()
        };
        let (host, session, frames) = py
            .detach(move || {
                fmn_studio::recorded::start_host(
                    &scene,
                    Box::new(PythonBuilder {
                        callback: builder,
                        scene: scene.clone(),
                    }),
                    token,
                    config,
                    Duration::from_secs(timeout),
                    Duration::from_secs(startup_timeout),
                )
                .map_err(|e| e.to_string())
            })
            .map_err(PyRuntimeError::new_err)?;
        let url = host.launch_url().map_err(native_error)?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&shutdown);
        let retained_frames = frames.clone();
        let thread = std::thread::Builder::new()
            .name("fmn-python-studio".into())
            .spawn(move || {
                let result = host.serve_until(&signal).map_err(|e| e.to_string());
                session.shutdown_worker();
                retained_frames.close();
                result
            })
            .map_err(native_error)?;
        Ok(Self {
            url,
            running: Some(RunningHost {
                shutdown,
                frames,
                thread: Some(thread),
            }),
        })
    }
    #[getter]
    fn url(&self) -> &str {
        &self.url
    }
    #[getter]
    fn alive(&self) -> bool {
        self.running
            .as_ref()
            .and_then(|r| r.thread.as_ref())
            .is_some_and(|t| !t.is_finished())
    }

    /// Directory inputs watch Python sources only; explicitly named files can
    /// also watch assets. Generated output, pyc caches and virtualenv trees are
    /// excluded so rendering itself does not cause an automatic reload loop.
    #[staticmethod]
    fn watch_sources(
        py: Python<'_>,
        paths: Vec<String>,
        debounce_ms: u64,
    ) -> PyResult<SourceWatcher> {
        if debounce_ms > 10_000 || paths.is_empty() || paths.len() > 64 {
            return Err(PyValueError::new_err(
                "watch requires 1..64 paths and debounce <= 10000 ms",
            ));
        }
        let watch = py
            .detach(move || {
                use fmn_studio::project_watch::{SourceWatch, WatchFilter, WatchLimits};
                SourceWatch::new_filtered(
                    paths.into_iter().map(PathBuf::from).collect(),
                    Vec::new(),
                    Duration::from_millis(debounce_ms),
                    WatchLimits::default(),
                    WatchFilter {
                        extensions: vec!["py".into(), "pyw".into()],
                        ignored_directories: [
                            "__pycache__",
                            ".venv",
                            "venv",
                            ".tox",
                            ".mypy_cache",
                            ".pytest_cache",
                        ]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    },
                )
                .map_err(|e| e.to_string())
            })
            .map_err(PyRuntimeError::new_err)?;
        Ok(SourceWatcher {
            watch,
            started: std::time::Instant::now(),
        })
    }

    fn close(&mut self, py: Python<'_>) -> PyResult<()> {
        if let Some(mut running) = self.running.take() {
            running.shutdown.store(true, Ordering::Release);
            running.frames.close();
            if let Some(thread) = running.thread.take() {
                py.detach(move || {
                    thread
                        .join()
                        .map_err(|_| "Studio host thread panicked".to_owned())?
                })
                .map_err(PyRuntimeError::new_err)?;
            }
        }
        Ok(())
    }
}

/// Real captured artifacts for the mandatory Gauntlet lifecycle row.
#[cfg(any(test, feature = "gauntlet"))]
pub struct PortalStudioGauntletReport {
    pub first_png: Vec<u8>,
    pub last_png: Vec<u8>,
    pub last_inspector: Vec<u8>,
    pub frames: u64,
    pub captures: u64,
    pub contexts_closed: u64,
}

/// Exercise the production capture and worker protocol without publishing an
/// offline render or claiming that Python callbacks are replayable.
#[cfg(any(test, feature = "gauntlet"))]
pub fn run_portal_gauntlet_studio_capture() -> Result<PortalStudioGauntletReport, String> {
    crate::with_python_test_module("Studio native capture", |py, _module, globals| {
        let source = std::ffi::CString::new(
            r#"
import contextlib
import json
import manimlib as m
_captures = 0
_contexts_closed = 0
@contextlib.contextmanager
def capture():
    global _captures, _contexts_closed
    _captures += 1
    s = m.Scene()
    s._begin_studio_capture('Moving', '11' * 32, '22' * 32, 96, 54, 8, 1, 0, 16, 1024*1024)
    s.camera._core.set_pixel_shape(96, 54)
    s.camera.fps = 8
    box = m.Square(fill_opacity=1)
    s.add(box)
    s.play(box.animate.shift(m.RIGHT), run_time=0.5)
    s.wait(0.25)
    r = s._finish_studio_capture()
    assert r.frame_count == 6, r.frame_count
    first, last = json.loads(r.inspect(0)), json.loads(r.inspect(5))
    assert first['view']['frame_count'] == 6
    assert first['view']['input_events'] is False
    assert first['nodes'] and last['nodes']
    assert first['scene_time'] < last['scene_time']
    assert first['nodes'] != last['nodes']
    assert json.loads(r.inspect(0)) == first, 'scrub changed captured state'
    static = m.Scene()
    static._begin_studio_capture('Still', '33' * 32, '44' * 32, 96, 54, 8, 1, 0, 16, 1024*1024)
    static.camera._core.set_pixel_shape(96, 54)
    static.camera.fps = 8
    static.add(m.Circle())
    assert static._finish_studio_capture().frame_count == 1
    try:
        yield r
    finally:
        _contexts_closed += 1
"#,
        )
        .unwrap();
        py.run(source.as_c_str(), Some(globals), Some(globals))
            .inspect_err(|e| e.print(py))
            .map_err(|e| e.to_string())?;
        let capture = globals
            .get_item("capture")
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Studio capture produced no callback".to_owned())?;
        let mut worker = DeferredRecording {
            build_id: ProtocolDigest::from_hex(&"11".repeat(32)).map_err(|e| e.to_string())?,
            scene: "Moving".to_owned(),
            capture: Some(capture.unbind()),
            worker: None,
            context: None,
            session: None,
        };
        worker
            .begin_session(protocol_digest(b"Gauntlet supervisor"), 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let captures = || -> Result<u64, String> {
            globals
                .get_item("_captures")
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "Studio capture produced no execution count".to_owned())?
                .extract::<u64>()
                .map_err(|e| e.to_string())
        };
        if captures()? != 0 || worker.worker.is_some() {
            return Err("Studio executed authored code before its first frame request".to_owned());
        }
        let png = |worker: &mut DeferredRecording, index| {
            let response = worker
                .handle(fmn_studio::SupervisorRequest::Scrub {
                    scene: "Moving".to_owned(),
                    frame: index,
                })
                .map_err(|e| e.to_string())?;
            match response {
                fmn_studio::WorkerResponse::Frame(FrameStream {
                    payload: FramePayload::Pipe { bytes, .. },
                    ..
                }) => Ok(bytes),
                _ => Err("Studio did not return an owned PNG".to_owned()),
            }
        };
        let first_png = png(&mut worker, 0)?;
        if png(&mut worker, 0)? != first_png {
            return Err("Studio readiness publication changed its captured first frame".to_owned());
        }
        let last_png = png(&mut worker, 5)?;
        let fmn_studio::WorkerResponse::StudioData {
            bytes: last_inspector,
            ..
        } = worker
            .handle(fmn_studio::SupervisorRequest::Inspect {
                scene: "Moving".to_owned(),
            })
            .map_err(|e| e.to_string())?
        else {
            return Err("Studio did not return its native inspector".to_owned());
        };
        let captures = captures()?;
        if captures != 1 {
            return Err("Studio re-executed authored code while scrubbing".to_owned());
        }
        let frames = worker
            .worker
            .as_ref()
            .ok_or_else(|| "Studio produced no captured timeline".to_owned())?
            .frame_count()
            .map_err(|e| e.to_string())? as u64;
        worker.close_capture().map_err(|e| e.to_string())?;
        let contexts_closed = globals
            .get_item("_contexts_closed")
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Studio produced no context-close count".to_owned())?
            .extract::<u64>()
            .map_err(|e| e.to_string())?;
        if contexts_closed != 1 {
            return Err("Studio did not close its capture context exactly once".to_owned());
        }
        Ok(PortalStudioGauntletReport {
            first_png,
            last_png,
            last_inspector,
            frames,
            captures,
            contexts_closed,
        })
    })
}

/// Exercise the same native gesture and history acceptance suite as the wheel
/// gate. The separate worker case stays in that gate because it launches an
/// installed interpreter; this seam retains real native Scene/Mobject state.
#[cfg(any(test, feature = "gauntlet"))]
pub fn run_portal_gauntlet_studio_editing() -> Result<u64, String> {
    crate::with_python_test_module("Studio native editing", |py, _module, globals| {
        let source = std::ffi::CString::new(include_str!("../tests/studio_editing.py"))
            .map_err(|error| error.to_string())?;
        py.run(source.as_c_str(), Some(globals), Some(globals))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        py.run(
            c"_suite = unittest.defaultTestLoader.loadTestsFromTestCase(NativeEditingTests)\n\
              _result = unittest.TextTestRunner(verbosity=2).run(_suite)\n\
              assert _result.wasSuccessful(), 'Studio native editing acceptance failed'\n\
              _checks = _result.testsRun\n",
            Some(globals),
            Some(globals),
        )
        .inspect_err(|error| error.print(py))
        .map_err(|error| error.to_string())?;
        globals
            .get_item("_checks")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Studio editing produced no acceptance count".to_owned())?
            .extract::<u64>()
            .map_err(|error| error.to_string())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn portal_studio_editing_preserves_native_gesture_and_history_semantics() {
        assert!(super::run_portal_gauntlet_studio_editing().unwrap() >= 21);
    }

    #[test]
    fn portal_studio_capture_uses_stepped_scene_and_native_inspector() {
        let report = super::run_portal_gauntlet_studio_capture().unwrap();
        let first =
            fmn_codec::decode_png(&report.first_png, &fmn_codec::PngLimits::default()).unwrap();
        let last =
            fmn_codec::decode_png(&report.last_png, &fmn_codec::PngLimits::default()).unwrap();
        assert_eq!(report.frames, 6);
        assert_eq!(report.captures, 1);
        assert_eq!(report.contexts_closed, 1);
        assert_eq!((first.width, first.height), (96, 54));
        assert_ne!(
            first.rgba, last.rgba,
            "animation must reach the captured native pixels"
        );
        assert!(!report.last_inspector.is_empty());
    }
}
