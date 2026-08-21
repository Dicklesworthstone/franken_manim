//! The production PyO3 `manimlib` bridge (§15.2, fm-aqv).
//!
//! The engine boundary is intentionally narrow. Ordinary Python semantics
//! (cooperative constructors, descriptors, mutable containers, copy/pickle,
//! and schema-generated modules) live in the embedded bootstrap. Rust owns
//! arena identity, RecordBuffer generations, lifecycle dispatch points, and
//! typed exception mapping.
//!
//! This is the sole authoritative crate allowed to contain unsafe code (D3).
//! The project-authored unsafe surface is confined to the two mechanical
//! CPython buffer slots on `PyRecordView`; every other operation is safe
//! Rust and no engine borrow crosses a Python callback.
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(test)]
mod corpus;
mod crossing;
mod ladder;
mod method_cache;
pub mod perf_harness;
mod report;

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::ffi::{CString, c_int, c_void};
use std::path::PathBuf;
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crossing::CrossingClass;

use fmn_frame::convert::rgba16f_to_rgba8;
use fmn_frame::{FrameLayout, PixelFormat};
use fmn_mobject::{
    JointType, Mob, Mobject, RecordBuffer, RecordError, RecordSchema, RecordView, Snapshot, Stage,
    StageError, Uniforms,
};
use fmn_output::{
    EmitterConfig, NativeArtifactReport, OrderedEmitter, PngSink, PngSinkConfig, PngTarget,
    SinkLimits, SinkReceipt,
};
use fmn_render::{
    Camera, CameraConfig, EngineIdentity, FrameConfig, RetainedFrameRenderer,
    RetainedFrameRendererConfig, ScreenMap, Tiling, Viewport,
};
use fmn_scene::{RuntimeConfig, Scene};
use pyo3::basic::CompareOp;
use pyo3::create_exception;
use pyo3::exceptions::{
    PyBufferError, PyImportError, PyKeyError, PyNotImplementedError, PyOSError, PyOverflowError,
    PyRuntimeError, PyTypeError, PyValueError,
};
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict, PyList, PyModule, PyTuple};

create_exception!(
    manimlib,
    StaleHandleError,
    PyRuntimeError,
    "The engine entry behind this proxy is stale, deleted, or unbound."
);
create_exception!(
    manimlib,
    ForeignStageError,
    PyRuntimeError,
    "A mobject cannot cross Scene arenas by reference; copy it instead."
);
create_exception!(
    manimlib,
    FamilyCycleError,
    PyValueError,
    "The requested submobject relation would make the family graph cyclic."
);
create_exception!(
    manimlib,
    CapabilityError,
    PyRuntimeError,
    "An optional host capability required by this operation is unavailable."
);
create_exception!(
    manimlib,
    TexError,
    PyValueError,
    "Native TeX parsing or span provenance failed."
);

type Engine = Rc<EngineState>;
type ProxyPairs = Vec<(Py<PyAny>, Py<PyAny>)>;
type SoundRequestFact = (String, i64, u32, f64, Option<f64>, Option<f64>);
type BoundingBoxRows = ([f64; 3], [f64; 3], [f64; 3]);
type PointRun = Vec<[f64; 3]>;
type AlignedPointRuns = (PointRun, PointRun);

const PORTAL_MAX_RENDER_FRAMES: u64 = 1_000_000;
const PORTAL_PICKLE_STATE_VERSION: u8 = 1;

/// One fmn-python render generation, retained across every `play` and `wait`.
///
/// Lumen owns the renderer state; Reel owns bounded ordered publication. This
/// portal adapter owns only their lifetime and the Python-facing report.
struct PortalRenderSession {
    renderer: RetainedFrameRenderer,
    camera: Camera,
    emitter: Option<OrderedEmitter>,
    receipt: SinkReceipt<NativeArtifactReport>,
    next_sequence: u64,
}

impl PortalRenderSession {
    fn new(
        destination: PathBuf,
        width: u32,
        height: u32,
        fps: u32,
        max_threads: usize,
        single_frame: bool,
    ) -> PyResult<(Self, RuntimeConfig)> {
        if width == 0 || height == 0 {
            return Err(PyValueError::new_err(
                "render resolution dimensions must be nonzero",
            ));
        }
        if fps == 0 {
            return Err(PyValueError::new_err("render fps must be nonzero"));
        }
        if max_threads == 0 {
            return Err(PyValueError::new_err("render thread limit must be nonzero"));
        }

        let mut config = fmn_config::Config::resolve(&[], None)
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?
            .config;
        config.camera.resolution = (width, height);
        config.camera.fps = fps;
        config.determinism.mode = fmn_config::config::DeterminismMode::Standard;

        let request = fmn_runtime::PlanRequest::standard(
            fmn_runtime::RenderIntent::Offline,
            fmn_runtime::SurfaceSpec::lumen(width, height),
            fmn_runtime::OutputPixelFormat::Rgba8,
        )
        .with_max_cpu_threads(max_threads);
        let plan = fmn_runtime::ExecutionPlan::derive(
            request,
            &fmn_platform::topology::HardwareTopology::current(),
            None,
        )
        .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        let engine = match plan.engine {
            fmn_runtime::ExecutionEngine::CertifiedCpu => EngineIdentity::certified(),
            fmn_runtime::ExecutionEngine::FastCpu => EngineIdentity::fast(),
            fmn_runtime::ExecutionEngine::Metal | fmn_runtime::ExecutionEngine::Cuda => {
                return Err(CapabilityError::new_err(
                    "fmn-python render currently accepts only CPU execution plans",
                ));
            }
        };
        let background = fmn_core::color::Srgb::from_hex(&config.camera.background_color)
            .map_err(|error| PyValueError::new_err(error.to_string()))?
            .to_linear(config.camera.background_opacity);
        let frame_config = FrameConfig::new(
            Viewport { width, height },
            ScreenMap {
                scale: f64::from(height) / config.sizes.frame_height,
                origin: [f64::from(width) / 2.0, f64::from(height) / 2.0],
            },
            background,
        )
        .with_aa_policy(config.render.aa);
        let renderer = RetainedFrameRenderer::new(RetainedFrameRendererConfig {
            frame: frame_config,
            tiling: Tiling {
                macro_tile: plan.macro_tile,
                fine_tile: plan.fine_tile,
            },
            engine,
            threads: plan
                .render_teams
                .first()
                .map_or(1, fmn_runtime::TeamPlan::threads),
        })
        .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        let camera = Camera::new(CameraConfig {
            resolution: (width, height),
            fps,
            background,
            ..CameraConfig::default()
        })
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let output_layout = FrameLayout::tight(PixelFormat::Rgba8, width, height)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let frame_bytes = u64::try_from(output_layout.total_bytes())
            .map_err(|_| PyOverflowError::new_err("render frame size exceeds u64"))?;
        let max_frames = if single_frame {
            1
        } else {
            PORTAL_MAX_RENDER_FRAMES
        };
        let max_stream_bytes = frame_bytes
            .checked_mul(max_frames)
            .ok_or_else(|| PyOverflowError::new_err("render stream budget exceeds u64"))?;
        let max_resident_bytes = frame_bytes
            .checked_mul(3)
            .and_then(|bytes| bytes.checked_add(4 * 1024 * 1024))
            .ok_or_else(|| PyOverflowError::new_err("render resident budget exceeds u64"))?;
        let max_artifact_bytes = max_stream_bytes
            .checked_mul(2)
            .ok_or_else(|| PyOverflowError::new_err("render artifact budget exceeds u64"))?;
        let limits = SinkLimits::new(
            max_frames,
            max_resident_bytes,
            max_stream_bytes,
            max_artifact_bytes,
        )
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let fs: Arc<dyn fmn_platform::fs::FileSystem> = Arc::new(fmn_platform::fs::StdFs);
        let target = if single_frame {
            PngTarget::Single(destination)
        } else {
            PngTarget::Sequence {
                directory: destination,
                stem: "frame".to_owned(),
                digits: 6,
            }
        };
        let (binding, receipt) = PngSink::new(
            fs,
            PngSinkConfig {
                target,
                width,
                height,
                first_sequence: 0,
                compression: fmn_codec::CompressionLevel::Default,
                threads: plan.output_team.threads().max(1),
                limits,
                profile: None,
            },
        )
        .map_err(|error| PyValueError::new_err(error.to_string()))?
        .into_binding(if single_frame {
            "python-png"
        } else {
            "python-png-sequence"
        });
        let emitter = OrderedEmitter::new(
            EmitterConfig::new(output_layout, plan.frames_in_flight, 0)
                .map_err(|error| PyRuntimeError::new_err(error.to_string()))?,
            vec![binding],
        )
        .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        let mut runtime_config = RuntimeConfig::from_config(&config);
        if single_frame {
            // Match the native front door's final-state PNG semantics: run
            // every segment to its semantic endpoint without ordinary frame
            // capture, then `_finish_render` publishes one explicit `show`.
            runtime_config.skip_animations = true;
            runtime_config.preview_while_skipping = false;
        }
        Ok((
            Self {
                renderer,
                camera,
                emitter: Some(emitter),
                receipt,
                next_sequence: 0,
            },
            runtime_config,
        ))
    }

    fn capture(
        &mut self,
        packet: fmn_scene::studio_bridge::FramePacket,
    ) -> Result<(), fmn_scene::IntegrationError> {
        let stage = packet.materialize_stage();
        // Portal coordinates follow manim's +Y-up camera plane, while every
        // FrameBuffer is already in top-row-first output orientation.  The
        // camera route owns that projection (including the Y inversion) for
        // vector content as well as 3D primitives.  Bypassing it for a
        // default affine frame reflects the delivered image vertically and
        // violates D-23's no-post-render-vflip contract.
        self.renderer
            .render_with_camera(&stage, &self.camera)
            .map_err(|error| fmn_scene::IntegrationError::new("lumen", error.to_string()))?;
        let mut reservation = self
            .emitter
            .as_ref()
            .ok_or_else(|| {
                fmn_scene::IntegrationError::new("reel", "render generation is already closed")
            })?
            .reserve(self.next_sequence)
            .map_err(|error| fmn_scene::IntegrationError::new("reel", error.to_string()))?;
        rgba16f_to_rgba8(self.renderer.frame(), reservation.frame_mut())
            .map_err(|error| fmn_scene::IntegrationError::new("reel", error.to_string()))?;
        reservation
            .publish()
            .map_err(|error| fmn_scene::IntegrationError::new("reel", error.to_string()))?;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| fmn_scene::IntegrationError::new("reel", "frame sequence exhausted"))?;
        Ok(())
    }

    fn bind_camera(
        &mut self,
        frame: fmn_scene::studio_bridge::CameraFrame,
        light_position: [f64; 3],
    ) -> PyResult<()> {
        *self.camera.frame_mut() = frame;
        self.camera
            .set_light_source_position(light_position)
            .map_err(camera_error)?;
        Ok(())
    }

    const fn frame_count(&self) -> u64 {
        self.next_sequence
    }

    fn abort(mut self) {
        self.cancel_and_join();
    }

    fn finish(mut self) -> PyResult<(NativeArtifactReport, String, usize)> {
        let renderer = self.renderer.config();
        self.emitter
            .take()
            .ok_or_else(|| PyRuntimeError::new_err("portal render generation is already closed"))?
            .finish()
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        let report = self
            .receipt
            .take()
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        Ok((report, renderer.engine.closure_string(), renderer.threads))
    }

    fn cancel_and_join(&mut self) {
        if let Some(emitter) = self.emitter.take() {
            emitter.cancel();
            let _ = emitter.finish();
        }
    }
}

impl Drop for PortalRenderSession {
    fn drop(&mut self) {
        self.cancel_and_join();
    }
}

/// One scene worker's runtime plus pin releases deferred by proxy destruction.
///
/// Python can decref a proxy at nearly any boundary. If that happens while a
/// Scene borrow is live, `Drop` queues the unpin instead of silently leaking
/// it or panicking through CPython. Every subsequent runtime borrow drains the
/// queue before exposing the Scene.
struct EngineState {
    scene: RefCell<Scene>,
    deferred_unpins: RefCell<Vec<Mob>>,
}

impl EngineState {
    fn new(scene: Scene) -> Self {
        Self {
            scene: RefCell::new(scene),
            deferred_unpins: RefCell::new(Vec::new()),
        }
    }

    fn drain_deferred_unpins(&self) {
        let pending = std::mem::take(&mut *self.deferred_unpins.borrow_mut());
        if pending.is_empty() {
            return;
        }
        let mut scene = self.scene.borrow_mut();
        for mob in pending {
            scene.stage_mut().unpin(mob);
        }
    }

    fn borrow(&self) -> Ref<'_, Scene> {
        self.drain_deferred_unpins();
        self.scene.borrow()
    }

    fn borrow_mut(&self) -> RefMut<'_, Scene> {
        self.drain_deferred_unpins();
        self.scene.borrow_mut()
    }

    fn release_pin(&self, mob: Mob) {
        if let Ok(mut scene) = self.scene.try_borrow_mut() {
            let pending = std::mem::take(&mut *self.deferred_unpins.borrow_mut());
            for deferred in pending {
                scene.stage_mut().unpin(deferred);
            }
            scene.stage_mut().unpin(mob);
        } else {
            self.deferred_unpins.borrow_mut().push(mob);
        }
    }
}

/// A detached proxy's builder state: a private, proxy-owned Stage (the
/// "nursery") plus the root handle its mobject was added with.
///
/// Arena residency without scene membership is a supported Stage mode, so
/// positional/geometry operations run through the exact same Stage code
/// path in both proxy states — the nursery before Scene.add, the scene's
/// stage after. Each nursery holds exactly one root: detached family
/// structure stays in the Python `submobjects` list until `Scene.add`
/// binds the whole graph.
struct Nursery {
    stage: Stage,
    root: Mob,
}

impl Nursery {
    fn new(mobject: Mobject) -> Self {
        let mut stage = Stage::new();
        let root = stage.add(mobject);
        Self { stage, root }
    }

    /// A nursery whose root is a native value tracker (§8.6): `kind` is
    /// 0 = Plain, 1 = Exponential, 2 = Complex (re/im in `value`/`im`).
    fn value_tracker(kind: u8, value: f64, im: f64) -> Self {
        let mut stage = Stage::new();
        let root = match kind {
            1 => stage.add_exponential_value_tracker(value),
            2 => stage.add_complex_value_tracker(value, im),
            _ => stage.add_value_tracker(value),
        };
        Self { stage, root }
    }
}

/// Subclassable Python proxy over either detached builder state (a private
/// nursery Stage) or one Stage-scoped Marionette handle.
#[pyclass(subclass, weakref, dict, unsendable, name = "_BridgeMobject")]
struct BridgeMobject {
    nursery: Option<Nursery>,
    engine: Option<Engine>,
    mob: Option<Mob>,
    initialized: bool,
}

/// Subclassable scene seam. The worker is deliberately single-threaded;
/// `unsendable` turns accidental cross-thread proxy use into a Python error.
#[pyclass(subclass, weakref, dict, unsendable, name = "_SceneCore")]
struct PyScene {
    engine: Engine,
    /// Handle → weakref(proxy), preserving one Python identity per live entry.
    proxies: RefCell<HashMap<Mob, Py<PyAny>>>,
    /// Optional production output session shared by every play/wait sink.
    render: Arc<Mutex<Option<PortalRenderSession>>>,
}

/// Owns one pinned RecordBuffer generation while a NumPy array exports it.
#[pyclass(unsendable, name = "_RecordViewOwner")]
struct PyRecordView {
    view: RecordView,
}

fn stage_error(error: StageError) -> PyErr {
    match error {
        StageError::StaleHandle => StaleHandleError::new_err(error.to_string()),
        StageError::CycleDetected => FamilyCycleError::new_err(error.to_string()),
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

fn bound_parts(proxy: &BridgeMobject) -> PyResult<(Engine, Mob)> {
    match (&proxy.engine, proxy.mob) {
        (Some(engine), Some(mob)) => Ok((Rc::clone(engine), mob)),
        _ => Err(StaleHandleError::new_err(
            "mobject is detached; add it to a Scene before using a Scene-only operation",
        )),
    }
}

fn same_engine(left: &Engine, right: &Engine) -> bool {
    Rc::ptr_eq(left, right)
}

/// The typed Python surface of a record sizing refusal (fm-vek.2):
/// schema stride and buffer shape overflows raise `OverflowError`, the
/// same exception class the bridge has always used for shape arithmetic.
fn record_error_to_py(error: RecordError) -> PyErr {
    match error {
        RecordError::StrideOverflow => {
            PyOverflowError::new_err("record dtype stride overflows usize")
        }
        RecordError::SizeOverflow { .. } => {
            PyOverflowError::new_err("RecordBuffer shape overflows usize")
        }
    }
}

fn with_buffer<T>(
    proxy: &Bound<'_, BridgeMobject>,
    operation: impl FnOnce(&mut RecordBuffer) -> T,
) -> PyResult<T> {
    let bound = {
        let cell = proxy.borrow();
        cell.engine
            .as_ref()
            .zip(cell.mob)
            .map(|(engine, mob)| (Rc::clone(engine), mob))
    };
    if let Some((engine, mob)) = bound {
        let mut scene = engine.borrow_mut();
        let stage = scene.stage_mut();
        stage.bake_placement(mob).map_err(stage_error)?;
        let entry = stage
            .get_mut(mob)
            .ok_or_else(|| StaleHandleError::new_err("mobject handle no longer resolves"))?;
        return Ok(operation(&mut entry.buffer));
    }
    let mut cell = proxy.borrow_mut();
    let nursery = cell
        .nursery
        .as_mut()
        .ok_or_else(|| StaleHandleError::new_err("mobject has no detached or bound state"))?;
    let root = nursery.root;
    nursery.stage.bake_placement(root).map_err(stage_error)?;
    let entry = nursery
        .stage
        .get_mut(root)
        .ok_or_else(|| StaleHandleError::new_err("nursery root no longer resolves"))?;
    Ok(operation(&mut entry.buffer))
}

fn with_buffer_ref<T>(
    proxy: &Bound<'_, BridgeMobject>,
    operation: impl FnOnce(&RecordBuffer) -> T,
) -> PyResult<T> {
    let bound = {
        let cell = proxy.borrow();
        cell.engine
            .as_ref()
            .zip(cell.mob)
            .map(|(engine, mob)| (Rc::clone(engine), mob))
    };
    if let Some((engine, mob)) = bound {
        // RecordBuffer is the live `mobject.data` contract. Materialize any
        // pending affine placement before a Python read so the zero-copy view
        // remains authoritative and exposes the same world-space points as
        // manim's data array (§8.2, fm-7if).
        let mut scene = engine.borrow_mut();
        let stage = scene.stage_mut();
        stage.bake_placement(mob).map_err(stage_error)?;
        let entry = stage
            .get(mob)
            .ok_or_else(|| StaleHandleError::new_err("mobject handle no longer resolves"))?;
        return Ok(operation(&entry.buffer));
    }
    let mut cell = proxy.borrow_mut();
    let nursery = cell
        .nursery
        .as_mut()
        .ok_or_else(|| StaleHandleError::new_err("mobject has no detached or bound state"))?;
    let root = nursery.root;
    // The nursery is one Stage code path with the bound branch: a pending
    // placement bakes before Python observes the buffer.
    nursery.stage.bake_placement(root).map_err(stage_error)?;
    let entry = nursery
        .stage
        .get(root)
        .ok_or_else(|| StaleHandleError::new_err("nursery root no longer resolves"))?;
    Ok(operation(&entry.buffer))
}

/// Route one positional/geometry operation to the proxy's Stage in either
/// state: the scene's stage when bound, the private nursery when detached.
/// This is the single seam every positional binding uses.
fn with_stage<T>(
    proxy: &Bound<'_, BridgeMobject>,
    operation: impl FnOnce(&mut Stage, Mob) -> T,
) -> PyResult<T> {
    let bound = {
        let cell = proxy.borrow();
        cell.engine
            .as_ref()
            .zip(cell.mob)
            .map(|(engine, mob)| (Rc::clone(engine), mob))
    };
    if let Some((engine, mob)) = bound {
        let mut scene = engine.borrow_mut();
        return Ok(operation(scene.stage_mut(), mob));
    }
    let mut cell = proxy.borrow_mut();
    let nursery = cell
        .nursery
        .as_mut()
        .ok_or_else(|| StaleHandleError::new_err("mobject has no detached or bound state"))?;
    let root = nursery.root;
    Ok(operation(&mut nursery.stage, root))
}

/// Reconstruct the current shared-anchor geometry under the VMobject's live
/// path-building controls.  The Python object owns these public mutable
/// attributes; Chisel owns every operation performed on the resulting path.
fn configured_quad_path(proxy: &Bound<'_, BridgeMobject>) -> PyResult<fmn_library::QuadPath> {
    let tolerance = proxy
        .getattr("tolerance_for_point_equality")?
        .extract::<f64>()?;
    let long_lines = proxy.getattr("long_lines")?.extract::<bool>()?;
    let points = with_stage(proxy, |stage, mob| {
        stage.get_points(mob).unwrap_or_default()
    })?;
    let mut path = fmn_library::QuadPath::from_points(points).map_err(native_error)?;
    path.set_tolerance_for_point_equality(tolerance);
    path.set_long_lines(long_lines);
    Ok(path)
}

/// Convert an `operator.index`-normalized Python subdivision count without
/// ever narrowing an attacker-controlled large integer. Returning the native
/// cap makes QuadPath produce its ordinary typed budget refusal.
fn bounded_subdivision_count(value: &Bound<'_, PyAny>) -> PyResult<usize> {
    if value
        .rich_compare(fmn_library::MAX_SUBDIVIDED_CURVES, CompareOp::Gt)?
        .is_truthy()?
    {
        Ok(fmn_library::MAX_SUBDIVIDED_CURVES)
    } else {
        value.extract::<usize>()
    }
}

/// Reference count coercion: non-positive values mean no split; positive
/// values must implement `__index__`, as NumPy's `linspace(num=...)` requires.
fn positive_subdivision_count(value: &Bound<'_, PyAny>) -> PyResult<usize> {
    if !value.rich_compare(0, CompareOp::Gt)?.is_truthy()? {
        return Ok(0);
    }
    let indexed = value
        .py()
        .import("operator")?
        .getattr("index")?
        .call1((value,))?;
    bounded_subdivision_count(&indexed)
}

/// Apply one native path-building operation without mutating the Stage and
/// return only the appended rows.  Python commits those rows through its
/// existing `append_points` surface so all non-geometry RecordBuffer lanes
/// follow the Reference's resize/copy rules.  Computing first also makes
/// every typed refusal atomic at the portal boundary.
fn quad_path_tail(
    proxy: &Bound<'_, BridgeMobject>,
    operation: impl FnOnce(&mut fmn_library::QuadPath) -> PyResult<()>,
) -> PyResult<Vec<[f64; 3]>> {
    let mut path = configured_quad_path(proxy)?;
    let old_len = path.num_points();
    operation(&mut path)?;
    path.points()
        .get(old_len..)
        .map(<[_]>::to_vec)
        .ok_or_else(|| PyRuntimeError::new_err("native path operation removed existing points"))
}

/// Copy one proxy's root entry into an unrelated Stage without copying its
/// descendants. Callers handle the same-Stage fast path before entering this
/// helper, so borrowing a bound source here can never alias `target`.
fn copy_proxy_entry_into(source: &Bound<'_, BridgeMobject>, target: &mut Stage) -> PyResult<Mob> {
    let bound = {
        let cell = source.borrow();
        cell.engine
            .as_ref()
            .zip(cell.mob)
            .map(|(engine, mob)| (Rc::clone(engine), mob))
    };
    if let Some((engine, mob)) = bound {
        return engine
            .borrow()
            .stage()
            .copy_entry_into(mob, target)
            .map_err(stage_error);
    }
    let cell = source.borrow();
    let nursery = cell.nursery.as_ref().ok_or_else(|| {
        StaleHandleError::new_err("partial source has no detached or bound state")
    })?;
    nursery
        .stage
        .copy_entry_into(nursery.root, target)
        .map_err(stage_error)
}

fn with_uniforms<T>(
    proxy: &Bound<'_, BridgeMobject>,
    operation: impl FnOnce(&mut Uniforms) -> PyResult<T>,
) -> PyResult<T> {
    let bound = {
        let cell = proxy.borrow();
        cell.engine
            .as_ref()
            .zip(cell.mob)
            .map(|(engine, mob)| (Rc::clone(engine), mob))
    };
    if let Some((engine, mob)) = bound {
        let mut scene = engine.borrow_mut();
        let entry = scene
            .stage_mut()
            .get_mut(mob)
            .ok_or_else(|| StaleHandleError::new_err("mobject handle no longer resolves"))?;
        return operation(entry.uniforms_mut());
    }
    let mut cell = proxy.borrow_mut();
    let nursery = cell
        .nursery
        .as_mut()
        .ok_or_else(|| StaleHandleError::new_err("mobject has no detached or bound state"))?;
    let root = nursery.root;
    let entry = nursery
        .stage
        .get_mut(root)
        .ok_or_else(|| StaleHandleError::new_err("nursery root no longer resolves"))?;
    operation(entry.uniforms_mut())
}

fn uniforms_snapshot(proxy: &Bound<'_, BridgeMobject>) -> PyResult<Uniforms> {
    let bound = {
        let cell = proxy.borrow();
        cell.engine
            .as_ref()
            .zip(cell.mob)
            .map(|(engine, mob)| (Rc::clone(engine), mob))
    };
    if let Some((engine, mob)) = bound {
        let scene = engine.borrow();
        return scene
            .stage()
            .get(mob)
            .map(|entry| *entry.uniforms())
            .ok_or_else(|| StaleHandleError::new_err("mobject handle no longer resolves"));
    }
    let cell = proxy.borrow();
    cell.nursery
        .as_ref()
        .and_then(|nursery| nursery.stage.get(nursery.root))
        .map(|entry| *entry.uniforms())
        .ok_or_else(|| StaleHandleError::new_err("mobject has no detached or bound state"))
}

fn extract_string_list(value: &Bound<'_, PyAny>, label: &str) -> PyResult<Vec<String>> {
    let mut result = Vec::new();
    for item in value.try_iter()? {
        result.push(
            item?.extract::<String>().map_err(|_| {
                PyTypeError::new_err(format!("{label} entries must all be strings"))
            })?,
        );
    }
    Ok(result)
}

fn extract_shape_width(value: &Bound<'_, PyAny>) -> PyResult<usize> {
    if let Ok(width) = value.extract::<usize>() {
        return Ok(width);
    }
    let mut width = 1usize;
    let mut dimensions = 0usize;
    for dimension in value.try_iter().map_err(|_| {
        PyTypeError::new_err("data_dtype shape must be an integer or an iterable of integers")
    })? {
        let dimension = dimension?.extract::<usize>().map_err(|_| {
            PyTypeError::new_err("data_dtype shape dimensions must be non-negative integers")
        })?;
        width = width
            .checked_mul(dimension)
            .ok_or_else(|| PyOverflowError::new_err("data_dtype lane count overflows usize"))?;
        dimensions += 1;
    }
    if dimensions == 0 {
        return Err(PyValueError::new_err(
            "data_dtype shape declares no dimensions",
        ));
    }
    Ok(width)
}

fn validate_field_dtype(value: &Bound<'_, PyAny>) -> PyResult<()> {
    let numpy = value.py().import("numpy").map_err(|error| {
        PyImportError::new_err(format!(
            "NumPy is required to interpret a three-item data_dtype entry: {error}"
        ))
    })?;
    let dtype = numpy.getattr("dtype")?.call1((value,))?;
    let kind: String = dtype.getattr("kind")?.extract()?;
    let itemsize: usize = dtype.getattr("itemsize")?.extract()?;
    let is_native: bool = dtype.getattr("isnative")?.extract()?;
    if kind != "f" || itemsize != std::mem::size_of::<f32>() || !is_native {
        return Err(PyTypeError::new_err(
            "RecordBuffer data_dtype fields must use native-endian float32",
        ));
    }
    Ok(())
}

/// Accept both the compact bridge descriptor `(name, lanes)` and NumPy's
/// ordinary `(name, dtype, shape)` field descriptor used by manim subclasses.
fn parse_schema(proxy: &Bound<'_, BridgeMobject>) -> PyResult<RecordSchema> {
    let dtype = proxy.getattr("data_dtype")?;
    let mut names = Vec::new();
    let mut widths = Vec::new();
    let mut seen = HashSet::new();
    for item in dtype.try_iter()? {
        let item = item?;
        let tuple = item.cast::<PyTuple>().map_err(|_| {
            PyTypeError::new_err("data_dtype entries must be (name, lanes) or (name, dtype, shape)")
        })?;
        let name = tuple
            .get_item(0)?
            .extract::<String>()
            .map_err(|_| PyTypeError::new_err("data_dtype field names must be strings"))?;
        let width = match tuple.len() {
            2 => tuple.get_item(1)?.extract::<usize>().map_err(|_| {
                PyTypeError::new_err("two-item data_dtype entries must use an integer lane count")
            })?,
            3 => {
                validate_field_dtype(&tuple.get_item(1)?)?;
                extract_shape_width(&tuple.get_item(2)?)?
            }
            _ => {
                return Err(PyTypeError::new_err(
                    "data_dtype entries must contain exactly two or three items",
                ));
            }
        };
        if name.is_empty() {
            return Err(PyValueError::new_err(
                "data_dtype field names cannot be empty",
            ));
        }
        if width == 0 {
            return Err(PyValueError::new_err(format!(
                "data_dtype field `{name}` has zero lanes"
            )));
        }
        if !seen.insert(name.clone()) {
            return Err(PyValueError::new_err(format!(
                "data_dtype field `{name}` is declared more than once"
            )));
        }
        names.push(name);
        widths.push(width);
    }
    if names.is_empty() {
        return Err(PyValueError::new_err("data_dtype declares no fields"));
    }
    widths.iter().try_fold(0usize, |stride, width| {
        stride
            .checked_add(*width)
            .ok_or_else(|| PyOverflowError::new_err("data_dtype stride overflows usize"))
    })?;

    let aligned = extract_string_list(&proxy.getattr("aligned_data_keys")?, "aligned_data_keys")?;
    let pointlike = extract_string_list(
        &proxy.getattr("pointlike_data_keys")?,
        "pointlike_data_keys",
    )?;
    for key in aligned.iter().chain(pointlike.iter()) {
        if !seen.contains(key) {
            return Err(PyValueError::new_err(format!(
                "record key `{key}` is not declared by data_dtype"
            )));
        }
    }
    let fields: Vec<(&str, usize)> = names
        .iter()
        .zip(widths.iter().copied())
        .map(|(name, width)| (name.as_str(), width))
        .collect();
    let aligned_refs: Vec<&str> = aligned.iter().map(String::as_str).collect();
    let pointlike_refs: Vec<&str> = pointlike.iter().map(String::as_str).collect();
    // The stride was already proved above; the fallible schema constructor
    // (fm-vek.2) re-proves it and any refusal surfaces as the same typed
    // Python exception.
    RecordSchema::new(&fields, &aligned_refs, &pointlike_refs).map_err(record_error_to_py)
}

fn proxy_children<'py>(proxy: &Bound<'py, PyAny>) -> PyResult<Vec<Bound<'py, PyAny>>> {
    let children = proxy.getattr("submobjects")?;
    children.try_iter()?.collect()
}

fn collect_proxy_graph<'py>(root: &Bound<'py, PyAny>) -> PyResult<Vec<Py<PyAny>>> {
    fn visit<'py>(
        proxy: &Bound<'py, PyAny>,
        seen: &mut HashSet<usize>,
        visiting: &mut HashSet<usize>,
        output: &mut Vec<Py<PyAny>>,
    ) -> PyResult<()> {
        proxy
            .cast::<BridgeMobject>()
            .map_err(|_| PyTypeError::new_err("submobjects must be Mobject instances"))?;
        let marker = proxy.as_ptr() as usize;
        if visiting.contains(&marker) {
            return Err(FamilyCycleError::new_err(
                "submobjects would create a family cycle",
            ));
        }
        if !seen.insert(marker) {
            return Ok(());
        }
        visiting.insert(marker);
        output.push(proxy.clone().unbind());
        for child in proxy_children(proxy)? {
            visit(&child, seen, visiting, output)?;
        }
        visiting.remove(&marker);
        Ok(())
    }

    let mut output = Vec::new();
    visit(root, &mut HashSet::new(), &mut HashSet::new(), &mut output)?;
    Ok(output)
}

fn register_proxy(
    py: Python<'_>,
    scene: &Bound<'_, PyScene>,
    mob: Mob,
    proxy: &Bound<'_, PyAny>,
) -> PyResult<()> {
    let weakref = py.import("weakref")?.call_method1("ref", (proxy,))?;
    scene
        .borrow()
        .proxies
        .borrow_mut()
        .insert(mob, weakref.unbind());
    Ok(())
}

fn live_proxy<'py>(
    py: Python<'py>,
    scene: &Bound<'py, PyScene>,
    mob: Mob,
) -> Option<Bound<'py, PyAny>> {
    let weak = {
        let scene = scene.borrow();
        scene
            .proxies
            .borrow()
            .get(&mob)
            .map(|weak| weak.clone_ref(py))
    }?;
    let target = weak.bind(py).call0().ok()?;
    (!target.is_none()).then_some(target)
}

fn bind_graph<'py>(
    py: Python<'py>,
    scene: &Bound<'py, PyScene>,
    root: &Bound<'py, BridgeMobject>,
) -> PyResult<Mob> {
    let graph = collect_proxy_graph(root.as_any())?;
    let engine = Rc::clone(&scene.borrow().engine);

    for object in &graph {
        let proxy = object.bind(py).cast::<BridgeMobject>()?;
        let cell = proxy.borrow();
        if let Some(existing) = &cell.engine
            && !same_engine(existing, &engine)
        {
            return Err(ForeignStageError::new_err(
                "a bound mobject belongs to a different Scene",
            ));
        }
        if cell.engine.is_none() && (!cell.initialized || cell.nursery.is_none()) {
            return Err(StaleHandleError::new_err(
                "uninitialized _BridgeMobject cannot enter a Scene",
            ));
        }
    }

    for object in &graph {
        let proxy = object.bind(py).cast::<BridgeMobject>()?;
        if proxy.borrow().engine.is_some() {
            continue;
        }
        // Adoption: transfer the nursery family into the scene's stage by
        // content (the two-scene copy policy), then retire the nursery.
        let mob = {
            let cell = proxy.borrow();
            let nursery = cell.nursery.as_ref().expect("validated detached state");
            let mut runtime = engine.borrow_mut();
            let mob = nursery
                .stage
                .copy_into(nursery.root, runtime.stage_mut())
                .map_err(stage_error)?;
            runtime.stage_mut().pin(mob).map_err(stage_error)?;
            mob
        };
        {
            let mut cell = proxy.borrow_mut();
            cell.nursery = None;
            cell.engine = Some(Rc::clone(&engine));
            cell.mob = Some(mob);
        }
        register_proxy(py, scene, mob, proxy.as_any())?;
        proxy.setattr("_scene", scene)?;
    }

    // Every Python access completed before the arena borrow. The edge
    // mutation below therefore cannot re-enter Python.
    let mut relations = Vec::new();
    for object in &graph {
        let parent = object.bind(py).cast::<BridgeMobject>()?;
        let parent_mob = parent.borrow().mob.expect("bound above");
        let mut children = Vec::new();
        for child in proxy_children(parent.as_any())? {
            let child = child.cast::<BridgeMobject>()?;
            children.push(child.borrow().mob.expect("graph bound every child"));
        }
        relations.push((parent_mob, children));
    }
    {
        let mut runtime = engine.borrow_mut();
        for (parent, children) in relations {
            for child in children {
                runtime
                    .stage_mut()
                    .attach(parent, child)
                    .map_err(stage_error)?;
            }
        }
    }
    root.borrow()
        .mob
        .ok_or_else(|| StaleHandleError::new_err("root did not bind"))
}

#[pymethods]
impl PyRecordView {
    /// CPython's buffer slot. This is the entire project-authored unsafe FFI
    /// surface: validate the destination, publish the pinned generation's
    /// stable pointer, and make the exporter own the lifetime.
    unsafe fn __getbuffer__(
        slf: Bound<'_, Self>,
        view: *mut ffi::Py_buffer,
        flags: c_int,
    ) -> PyResult<()> {
        if view.is_null() {
            return Err(PyBufferError::new_err(
                "CPython supplied a null Py_buffer destination",
            ));
        }
        let (data, byte_len, writable) = {
            let owner = slf.borrow();
            (
                owner.view.foreign_data_ptr(),
                owner.view.foreign_byte_len(),
                owner.view.is_writable(),
            )
        };
        if !writable && flags & ffi::PyBUF_WRITABLE == ffi::PyBUF_WRITABLE {
            return Err(PyBufferError::new_err(
                "the RecordBuffer view was exported read-only",
            ));
        }
        let byte_len = isize::try_from(byte_len)
            .map_err(|_| PyOverflowError::new_err("RecordBuffer exceeds Py_ssize_t"))?;
        let format = if flags & ffi::PyBUF_FORMAT == ffi::PyBUF_FORMAT {
            CString::new("B")
                .expect("static buffer format contains no NUL")
                .into_raw()
        } else {
            ptr::null_mut()
        };
        let owner = slf.into_any();
        // SAFETY: `view` was checked non-null. `data` belongs to the
        // RecordView stored in `owner`; assigning owner.into_ptr() transfers
        // one Python reference to Py_buffer.obj, so CPython keeps that view
        // (and its Arc generation) alive until release. Shape/stride pointers
        // refer to fields inside this Py_buffer, exactly as CPython permits.
        unsafe {
            (*view).obj = owner.into_ptr();
            (*view).buf = data.cast::<c_void>();
            (*view).len = byte_len;
            (*view).readonly = i32::from(!writable);
            (*view).itemsize = 1;
            (*view).format = format;
            (*view).ndim = 1;
            (*view).shape = if flags & ffi::PyBUF_ND == ffi::PyBUF_ND {
                &raw mut (*view).len
            } else {
                ptr::null_mut()
            };
            (*view).strides = if flags & ffi::PyBUF_STRIDES == ffi::PyBUF_STRIDES {
                &raw mut (*view).itemsize
            } else {
                ptr::null_mut()
            };
            (*view).suboffsets = ptr::null_mut();
            (*view).internal = ptr::null_mut();
        }
        Ok(())
    }

    unsafe fn __releasebuffer__(&self, view: *mut ffi::Py_buffer) {
        if view.is_null() {
            return;
        }
        // SAFETY: this slot receives the exact Py_buffer initialized above.
        // A non-null format was allocated with CString::into_raw once, and
        // CPython invokes release at most once for this export.
        let format = unsafe { (*view).format };
        if !format.is_null() {
            // SAFETY: paired with CString::into_raw in __getbuffer__.
            drop(unsafe { CString::from_raw(format) });
            // SAFETY: leave no dangling pointer for diagnostics.
            unsafe {
                (*view).format = ptr::null_mut();
            }
        }
    }
}

const UNIFORM_NAMES: &[&str] = &[
    "is_fixed_in_frame",
    "shading",
    "clip_planes",
    "anti_alias_width",
    "joint_type",
    "flat_stroke",
    "scale_stroke_with_zoom",
    "stroke_behind",
    "depth_test",
    "use_winding_fill",
];

fn uniform_value<'py>(
    py: Python<'py>,
    uniforms: Uniforms,
    name: &str,
) -> PyResult<Bound<'py, PyAny>> {
    match name {
        "is_fixed_in_frame" => Ok(uniforms.is_fixed_in_frame.into_pyobject(py)?.into_any()),
        "shading" => Ok(uniforms.shading.to_vec().into_pyobject(py)?.into_any()),
        "clip_planes" => Ok(uniforms
            .clip_planes
            .iter()
            .map(|plane| plane.to_vec())
            .collect::<Vec<_>>()
            .into_pyobject(py)?
            .into_any()),
        "anti_alias_width" => Ok(uniforms.anti_alias_width.into_pyobject(py)?.into_any()),
        "joint_type" => Ok(uniforms.joint_type.to_code().into_pyobject(py)?.into_any()),
        "flat_stroke" => Ok(uniforms
            .flat_stroke
            .into_pyobject(py)?
            .to_owned()
            .into_any()),
        "scale_stroke_with_zoom" => Ok(uniforms
            .scale_stroke_with_zoom
            .into_pyobject(py)?
            .to_owned()
            .into_any()),
        "stroke_behind" => Ok(uniforms
            .stroke_behind
            .into_pyobject(py)?
            .to_owned()
            .into_any()),
        "depth_test" => Ok(uniforms.depth_test.into_pyobject(py)?.to_owned().into_any()),
        "use_winding_fill" => Ok(uniforms
            .use_winding_fill
            .into_pyobject(py)?
            .to_owned()
            .into_any()),
        _ => Err(PyKeyError::new_err(name.to_owned())),
    }
}

fn set_uniform(uniforms: &mut Uniforms, name: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
    match name {
        "is_fixed_in_frame" => uniforms.is_fixed_in_frame = value.extract()?,
        "shading" => {
            let values: Vec<f64> = value.extract()?;
            uniforms.shading = values.try_into().map_err(|values: Vec<f64>| {
                PyValueError::new_err(format!(
                    "shading requires exactly 3 values, got {}",
                    values.len()
                ))
            })?;
        }
        "clip_planes" => {
            let values: Vec<Vec<f64>> = value.extract()?;
            if values.len() != 4 || values.iter().any(|plane| plane.len() != 4) {
                return Err(PyValueError::new_err(
                    "clip_planes requires exactly four four-value planes",
                ));
            }
            for (destination, source) in uniforms.clip_planes.iter_mut().zip(values) {
                destination.copy_from_slice(&source);
            }
        }
        "anti_alias_width" => uniforms.anti_alias_width = value.extract()?,
        "joint_type" => uniforms.joint_type = JointType::from_code(value.extract()?),
        "flat_stroke" => uniforms.flat_stroke = value.extract()?,
        "scale_stroke_with_zoom" => uniforms.scale_stroke_with_zoom = value.extract()?,
        "stroke_behind" => uniforms.stroke_behind = value.extract()?,
        "depth_test" => uniforms.depth_test = value.extract()?,
        "use_winding_fill" => uniforms.use_winding_fill = value.extract()?,
        _ => return Err(PyKeyError::new_err(name.to_owned())),
    }
    Ok(())
}

fn isolated_native_state<'py>(
    py: Python<'py>,
    stage: &Stage,
    mob: Mob,
) -> PyResult<Bound<'py, PyDict>> {
    if !stage.updater_ids(mob).is_empty() {
        return Err(PyValueError::new_err(
            "native updater callables cannot enter Python pickle state",
        ));
    }

    // Python's pickle memo owns the proxy graph: every child is reduced once
    // and aliases are rebuilt by pickle. Isolate this one native entry so a
    // bound parent's arena family cannot be serialized again inside the
    // parent's payload and then duplicated when Python reconnects children.
    let mut isolated = Stage::new();
    let root = stage
        .copy_entry_into(mob, &mut isolated)
        .map_err(stage_error)?;
    isolated.add_to_scene(root).map_err(stage_error)?;

    let entry = isolated
        .get(root)
        .ok_or_else(|| StaleHandleError::new_err("isolated pickle root no longer resolves"))?;
    let fields: Vec<(String, usize)> = entry
        .buffer
        .schema()
        .fields()
        .iter()
        .map(|field| (field.name.clone(), field.width))
        .collect();
    let snapshot = isolated.snapshot_bytes().map_err(native_error)?;
    let state = PyDict::new(py);
    state.set_item("version", PORTAL_PICKLE_STATE_VERSION)?;
    state.set_item("snapshot", PyBytes::new(py, &snapshot))?;
    // Detached become/restore uses this cheap schema identity without
    // decoding either operand. The authoritative state remains the FMNA
    // snapshot above.
    state.set_item("fields", fields)?;
    Ok(state)
}

fn native_state<'py>(
    py: Python<'py>,
    proxy: &Bound<'py, BridgeMobject>,
) -> PyResult<Bound<'py, PyDict>> {
    let cell = proxy.borrow();
    if let (Some(engine), Some(mob)) = (&cell.engine, cell.mob) {
        let runtime = engine.borrow();
        return isolated_native_state(py, runtime.stage(), mob);
    }
    let nursery = cell
        .nursery
        .as_ref()
        .ok_or_else(|| StaleHandleError::new_err("mobject has no detached or bound state"))?;
    isolated_native_state(py, &nursery.stage, nursery.root)
}

fn restore_native_state(
    proxy: &Bound<'_, BridgeMobject>,
    state: &Bound<'_, PyDict>,
) -> PyResult<()> {
    {
        let cell = proxy.borrow();
        if cell.engine.is_some() || cell.mob.is_some() {
            return Err(PyRuntimeError::new_err(
                "cannot restore detached pickle state over a bound mobject",
            ));
        }
    }
    let version: u8 = state
        .get_item("version")?
        .ok_or_else(|| PyKeyError::new_err("version"))?
        .extract()?;
    if version != PORTAL_PICKLE_STATE_VERSION {
        return Err(PyValueError::new_err(format!(
            "unsupported Python portal pickle-state version {version}"
        )));
    }
    let declared_fields: Vec<(String, usize)> = state
        .get_item("fields")?
        .ok_or_else(|| PyKeyError::new_err("fields"))?
        .extract()?;
    let snapshot = state
        .get_item("snapshot")?
        .ok_or_else(|| PyKeyError::new_err("snapshot"))?;
    let snapshot = snapshot
        .cast::<PyBytes>()
        .map_err(|_| PyTypeError::new_err("pickle-state snapshot must be bytes"))?;

    // Durable decode validates container checksum/schema/version, arena
    // invariants, the 256 MiB encoded cap, and the aggregate decoded-
    // allocation budget before this proxy is touched. Decode and restore a
    // private candidate nursery first so every refusal is atomic.
    let mut stage = Stage::new();
    let decoded = Snapshot::from_bytes(snapshot.as_bytes(), &stage).map_err(native_error)?;
    if !decoded.updaters.entries.is_empty() {
        return Err(PyValueError::new_err(
            "Python pickle state cannot restore native updater identities without callables",
        ));
    }
    stage.restore(&decoded.snapshot);
    let [root] = stage.roots() else {
        return Err(PyValueError::new_err(
            "Python pickle state must contain exactly one native root",
        ));
    };
    let root = *root;
    if stage.family(root).len() != 1 {
        return Err(PyValueError::new_err(
            "Python pickle state native root must not contain a family graph",
        ));
    }
    let actual_fields: Vec<(String, usize)> = stage
        .get(root)
        .expect("validated snapshot root is live")
        .buffer
        .schema()
        .fields()
        .iter()
        .map(|field| (field.name.clone(), field.width))
        .collect();
    if declared_fields != actual_fields {
        return Err(PyValueError::new_err(
            "Python pickle-state field summary does not match its native snapshot",
        ));
    }
    let nursery = Nursery { stage, root };

    let mut cell = proxy.borrow_mut();
    cell.nursery = Some(nursery);
    cell.mob = None;
    cell.engine = None;
    cell.initialized = true;
    Ok(())
}

fn numpy_array<'py>(
    py: Python<'py>,
    proxy: &Bound<'py, BridgeMobject>,
    writable: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let view = with_buffer(proxy, |buffer| buffer.export_view(writable))?;
    let len = view.len();
    let stride_bytes = view
        .schema()
        .stride()
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or_else(|| PyOverflowError::new_err("NumPy stride overflows usize"))?;
    let descriptors = PyList::empty(py);
    for field in view.schema().fields() {
        descriptors.append((field.name.as_str(), "=f4", (field.width,)))?;
    }
    let owner = Py::new(py, PyRecordView { view })?;
    let numpy = py.import("numpy").map_err(|error| {
        PyImportError::new_err(format!(
            "NumPy is required for the live `data` view: {error}"
        ))
    })?;
    let dtype = numpy.getattr("dtype")?.call1((descriptors,))?;
    if dtype.getattr("itemsize")?.extract::<usize>()? != stride_bytes {
        return Err(PyRuntimeError::new_err(
            "NumPy packed the all-f32 RecordBuffer dtype at an unexpected itemsize",
        ));
    }
    let kwargs = PyDict::new(py);
    kwargs.set_item("dtype", dtype)?;
    kwargs.set_item("buffer", owner)?;
    kwargs.set_item("strides", (stride_bytes,))?;
    numpy.getattr("ndarray")?.call(((len,),), Some(&kwargs))
}

fn flat_records(proxy: &Bound<'_, BridgeMobject>) -> PyResult<(RecordSchema, usize, Vec<f32>)> {
    with_buffer_ref(proxy, |buffer| {
        let schema = buffer.schema().clone();
        let mut records = Vec::with_capacity(buffer.len() * schema.stride());
        for index in 0..buffer.len() {
            for field in schema.fields() {
                records.extend(
                    buffer
                        .read(index, &field.name)
                        .expect("iterated schema field exists"),
                );
            }
        }
        (schema, buffer.len(), records)
    })
}

fn location_compatible(
    left: &Bound<'_, BridgeMobject>,
    right: &Bound<'_, BridgeMobject>,
) -> PyResult<()> {
    let left_engine = left.borrow().engine.as_ref().map(Rc::clone);
    let right_engine = right.borrow().engine.as_ref().map(Rc::clone);
    match (left_engine, right_engine) {
        (Some(left), Some(right)) if same_engine(&left, &right) => Ok(()),
        (None, None) => Ok(()),
        _ => Err(ForeignStageError::new_err(
            "interpolation operands must all be detached or bound to one Scene",
        )),
    }
}

fn new_bound_proxy<'py>(
    py: Python<'py>,
    scene: &Bound<'py, PyScene>,
    old_proxy: &Bound<'py, PyAny>,
    engine: &Engine,
    mob: Mob,
) -> PyResult<Bound<'py, PyAny>> {
    let class = old_proxy.get_type();
    let proxy = class.call_method1("__new__", (&class,))?;
    {
        let bridge = proxy.cast::<BridgeMobject>()?;
        let mut cell = bridge.borrow_mut();
        cell.nursery = None;
        cell.engine = Some(Rc::clone(engine));
        cell.mob = Some(mob);
        cell.initialized = true;
    }
    engine
        .borrow_mut()
        .stage_mut()
        .pin(mob)
        .map_err(stage_error)?;
    register_proxy(py, scene, mob, &proxy)?;
    proxy.setattr("_scene", scene)?;
    Ok(proxy)
}

#[pymethods]
impl BridgeMobject {
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn py_new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        Self {
            nursery: Some(Nursery::new(Mobject::new())),
            engine: None,
            mob: None,
            initialized: false,
        }
    }

    #[classattr]
    fn data_dtype() -> Vec<(&'static str, usize)> {
        vec![("point", 3), ("rgba", 4)]
    }

    #[classattr]
    fn aligned_data_keys() -> Vec<&'static str> {
        vec!["point"]
    }

    #[classattr]
    fn pointlike_data_keys() -> Vec<&'static str> {
        vec!["point"]
    }

    /// Allocate detached RecordBuffer state from the subclass's dtype, then
    /// drive the three initialization hooks through normal Python MRO.
    fn _engine_init(slf: &Bound<'_, Self>) -> PyResult<()> {
        if slf.borrow().initialized {
            return Err(PyRuntimeError::new_err(
                "Mobject engine initialization may run only once",
            ));
        }
        let schema = parse_schema(slf)?;
        {
            let mut cell = slf.borrow_mut();
            cell.nursery = Some(Nursery::new(Mobject::from_buffer(
                RecordBuffer::new(schema, 0).map_err(record_error_to_py)?,
            )));
            cell.initialized = true;
        }
        // No engine or proxy borrow is live across these calls. Each hook
        // dispatches through the fm-zoi method-resolution cache (one native
        // →Python method_dispatch crossing per hook).
        crossing::record(CrossingClass::MethodDispatch);
        method_cache::call_cached0(slf.as_any(), "init_data")?;
        crossing::record(CrossingClass::MethodDispatch);
        method_cache::call_cached0(slf.as_any(), "init_points")?;
        crossing::record(CrossingClass::MethodDispatch);
        method_cache::call_cached0(slf.as_any(), "init_uniforms")?;
        Ok(())
    }

    fn init_data(_slf: &Bound<'_, Self>) {}

    fn init_points(_slf: &Bound<'_, Self>) {}

    fn init_uniforms(_slf: &Bound<'_, Self>) {}

    fn resize(slf: &Bound<'_, Self>, len: usize) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        // The sizing proof lives in the fallible resize itself (fm-vek.2);
        // a refusal surfaces to Python as a typed OverflowError, and the
        // buffer (plus every exported NumPy view) is left untouched.
        with_buffer(slf, |buffer| buffer.resize(len))?.map_err(record_error_to_py)
    }

    /// Reference `resize_preserving_order`, exposed only to the Python skin's
    /// semantic methods (not as a public manim API addition).
    fn _resize_preserving_order(slf: &Bound<'_, Self>, len: usize) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_buffer(slf, |buffer| buffer.resize_preserving_order(len))?.map_err(record_error_to_py)
    }

    fn n_records(slf: &Bound<'_, Self>) -> PyResult<usize> {
        crossing::record(CrossingClass::Other);
        with_buffer_ref(slf, RecordBuffer::len)
    }

    fn revision(slf: &Bound<'_, Self>) -> PyResult<u64> {
        crossing::record(CrossingClass::Other);
        with_buffer_ref(slf, RecordBuffer::revision)
    }

    fn field_names(slf: &Bound<'_, Self>) -> PyResult<Vec<String>> {
        crossing::record(CrossingClass::Other);
        with_buffer_ref(slf, |buffer| {
            buffer
                .schema()
                .fields()
                .iter()
                .map(|field| field.name.clone())
                .collect()
        })
    }

    fn get_field(slf: &Bound<'_, Self>, field: &str, index: usize) -> PyResult<Vec<f32>> {
        crossing::record(CrossingClass::Other);
        with_buffer_ref(slf, |buffer| buffer.read(index, field))?
            .ok_or_else(|| PyKeyError::new_err(format!("no `{field}` record at index {index}")))
    }

    fn set_field(
        slf: &Bound<'_, Self>,
        field: &str,
        index: usize,
        values: Vec<f32>,
    ) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        if with_buffer(slf, |buffer| buffer.write(index, field, &values))? {
            Ok(())
        } else {
            Err(PyKeyError::new_err(format!(
                "no writable `{field}` record at index {index} with {} lanes",
                values.len()
            )))
        }
    }

    /// Owned f64 rows for one RecordBuffer field. Arbitrary Python point-map
    /// callables operate on NumPy-owned arrays, then commit through the
    /// engine-mediated write path instead of leaving a zero-copy writer alive
    /// across bounding-box cache installation.
    fn _field_rows(slf: &Bound<'_, Self>, field: &str) -> PyResult<Vec<Vec<f64>>> {
        crossing::record(CrossingClass::Other);
        with_buffer_ref(slf, |buffer| {
            if !buffer
                .schema()
                .fields()
                .iter()
                .any(|candidate| candidate.name == field)
            {
                return None;
            }
            Some(
                (0..buffer.len())
                    .map(|index| {
                        buffer
                            .read(index, field)
                            .expect("validated schema field exists")
                            .into_iter()
                            .map(f64::from)
                            .collect()
                    })
                    .collect(),
            )
        })?
        .ok_or_else(|| PyKeyError::new_err(format!("no `{field}` record field")))
    }

    /// Atomically validate and engine-write one complete field column.
    fn _set_field_rows(slf: &Bound<'_, Self>, field: &str, rows: Vec<Vec<f64>>) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_buffer(slf, |buffer| {
            let Some(field_index) = buffer
                .schema()
                .fields()
                .iter()
                .position(|candidate| candidate.name == field)
            else {
                return Err(PyKeyError::new_err(format!("no `{field}` record field")));
            };
            let width = buffer.schema().fields()[field_index].width;
            if rows.len() != buffer.len() || rows.iter().any(|row| row.len() != width) {
                return Err(PyValueError::new_err(format!(
                    "field `{field}` expects {} rows of width {width}",
                    buffer.len()
                )));
            }
            for (index, row) in rows.iter().enumerate() {
                let values = row.iter().map(|value| *value as f32).collect::<Vec<_>>();
                let wrote = buffer.write(index, field, &values);
                debug_assert!(wrote, "validated field row must be writable");
            }
            Ok(())
        })?
    }

    #[pyo3(signature = (writable = true))]
    fn _data_array<'py>(slf: &Bound<'py, Self>, writable: bool) -> PyResult<Bound<'py, PyAny>> {
        crossing::record(CrossingClass::Other);
        numpy_array(slf.py(), slf, writable)
    }

    #[staticmethod]
    fn _uniform_names() -> Vec<&'static str> {
        UNIFORM_NAMES.to_vec()
    }

    fn _get_uniform<'py>(slf: &Bound<'py, Self>, name: &str) -> PyResult<Bound<'py, PyAny>> {
        crossing::record(CrossingClass::Other);
        uniform_value(slf.py(), uniforms_snapshot(slf)?, name)
    }

    fn _set_uniform(slf: &Bound<'_, Self>, name: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_uniforms(slf, |uniforms| set_uniform(uniforms, name, value))
    }

    // ------------------------------------------------ positional primitives
    //
    // fm-d3gt: the engine seam under the bootstrap's Reference-signature
    // positional surface. Each primitive routes to the ONE Stage
    // implementation via `with_stage`; pivots arrive pre-resolved from the
    // Python layer (which reads them off the same Stage bounding box), so
    // family distribution in the detached state stays exact.

    /// Whether this proxy is bound to a Scene's stage. When false its Stage
    /// is the private nursery, whose family is exactly one root — the
    /// bootstrap then distributes transforms over the Python family list.
    fn _is_bound(slf: &Bound<'_, Self>) -> bool {
        let cell = slf.borrow();
        cell.engine.is_some() && cell.mob.is_some()
    }

    /// `(min, mid, max)` rows of the Stage-visible family bounding box.
    fn _get_bbox(slf: &Bound<'_, Self>) -> PyResult<BoundingBoxRows> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| {
            let bbox = stage.get_bounding_box(mob);
            (bbox.min, bbox.mid, bbox.max)
        })
    }

    /// A still-current bounding box installed by the Reference compatibility
    /// path, rather than lazily materialized from point extrema.
    fn _get_installed_bbox(slf: &Bound<'_, Self>) -> PyResult<Option<BoundingBoxRows>> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| {
            stage
                .installed_bounding_box_cache(mob)
                .map(|bbox| (bbox.min, bbox.mid, bbox.max))
        })
    }

    /// Whether this entry itself has point records (Reference `has_points`,
    /// not recursing into the family).
    fn _has_points(slf: &Bound<'_, Self>) -> PyResult<bool> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| {
            stage
                .get_points(mob)
                .is_some_and(|points| !points.is_empty())
        })
    }

    /// `Stage::shift`: translate the Stage-visible family.
    fn _shift(slf: &Bound<'_, Self>, vector: [f64; 3]) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| {
            stage.shift(mob, vector);
        })
    }

    /// `Stage::scale_about` with an explicit pre-resolved pivot.
    fn _scale_about(slf: &Bound<'_, Self>, factor: f64, about_point: [f64; 3]) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| {
            stage.scale_about(mob, factor, Some(about_point), None);
        })
    }

    /// `Stage::stretch_about` with an explicit pre-resolved pivot.
    fn _stretch_about(
        slf: &Bound<'_, Self>,
        factor: f64,
        dim: usize,
        about_point: [f64; 3],
    ) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        if dim > 2 {
            return Err(PyValueError::new_err("stretch dim must be 0, 1, or 2"));
        }
        with_stage(slf, |stage, mob| {
            stage.stretch_about(mob, factor, dim, Some(about_point), None);
        })
    }

    /// `Stage::rotate` with an explicit pre-resolved pivot.
    fn _rotate_about(
        slf: &Bound<'_, Self>,
        angle: f64,
        axis: [f64; 3],
        about_point: [f64; 3],
    ) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| {
            stage.rotate(mob, angle, axis, Some(about_point), None);
        })
    }

    /// `Stage::to_edge` (`align_on_border`): the single-target engine path
    /// used when the proxy is bound; the bootstrap's detached branch
    /// decomposes over the frame radii instead.
    fn _to_edge(slf: &Bound<'_, Self>, direction: [f64; 3], buff: f64) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| {
            stage.to_edge(mob, direction, buff);
        })
    }

    /// The frame half-extents `(FRAME_X_RADIUS, FRAME_Y_RADIUS)` the border
    /// alignment surface is defined against.
    #[staticmethod]
    fn _frame_radii() -> (f64, f64) {
        (
            fmn_core::constants::FRAME_X_RADIUS,
            fmn_core::constants::FRAME_Y_RADIUS,
        )
    }

    /// fmn-core's one color model (D4): parse `#RRGGBB`/`#RGB` into sRGB
    /// components in `[0, 1]`. Anything else is a precise refusal — the
    /// bootstrap never hand-rolls color arithmetic.
    #[staticmethod]
    fn _hex_to_rgb(value: &str) -> PyResult<(f64, f64, f64)> {
        fmn_core::color::Srgb::from_hex(value)
            .map(|color| (color.r, color.g, color.b))
            .map_err(|error| PyValueError::new_err(format!("invalid color {value:?}: {error}")))
    }

    /// Format sRGB components as the Reference portal's uppercase
    /// `colour.rgb2hex(..., force_long=True)`. The tiny subtraction is
    /// colour 0.1.5's declared half-boundary rule (`FLOAT_ERROR = 5e-7`),
    /// which intentionally maps an exact 0.5 component to `0x7F`.
    #[staticmethod]
    fn _rgb_to_hex(rgb: (f64, f64, f64)) -> String {
        let quantize = |component: f64| (component * 255.0 + 0.5 - 5e-7) as u8;
        let [r, g, b] = [quantize(rgb.0), quantize(rgb.1), quantize(rgb.2)];
        format!("#{r:02X}{g:02X}{b:02X}")
    }

    /// Route both Reference `color_gradient` branches through fmn-core's
    /// single color authority. Python performs only its normal public color
    /// coercion; all interpolation and sample-position semantics live here.
    #[staticmethod]
    fn _color_gradient(
        colors: Vec<[f64; 3]>,
        length: usize,
        interp_by_hsl: bool,
    ) -> PyResult<Vec<[f64; 3]>> {
        if length > 0 && colors.len() < 2 {
            return Err(PyValueError::new_err(
                "color_gradient needs at least two reference colors",
            ));
        }
        let colors = colors
            .into_iter()
            .map(|[r, g, b]| fmn_core::color::Srgb { r, g, b })
            .collect::<Vec<_>>();
        let gradient = if interp_by_hsl {
            fmn_core::color::color_gradient_by_hsl(&colors, length)
        } else {
            fmn_core::color::color_gradient(&colors, length)
        };
        Ok(gradient
            .into_iter()
            .map(|color| [color.r, color.g, color.b])
            .collect())
    }

    /// Public `interpolate_color` delegates to fmn-core's single declared
    /// color model, including the Reference's opt-in HSL branch.
    #[staticmethod]
    fn _interpolate_color(
        color1: [f64; 3],
        color2: [f64; 3],
        alpha: f64,
        interp_by_hsl: bool,
    ) -> [f64; 3] {
        let color1 = fmn_core::color::Srgb {
            r: color1[0],
            g: color1[1],
            b: color1[2],
        };
        let color2 = fmn_core::color::Srgb {
            r: color2[0],
            g: color2[1],
            b: color2[2],
        };
        let color = if interp_by_hsl {
            fmn_core::color::interpolate_color_by_hsl(color1, color2, alpha)
        } else {
            fmn_core::color::interpolate_color(color1, color2, alpha)
        };
        [color.r, color.g, color.b]
    }

    /// Public `average_color` is fmn-core's Reference-compatible RMS color
    /// average, not a second portal-side color model.
    #[staticmethod]
    fn _average_color(colors: Vec<[f64; 3]>) -> [f64; 3] {
        let colors = colors
            .into_iter()
            .map(|[r, g, b]| fmn_core::color::Srgb { r, g, b })
            .collect::<Vec<_>>();
        let color = fmn_core::color::average_color(&colors);
        [color.r, color.g, color.b]
    }

    /// Chisel owns the exact clamped integer interpolation used throughout
    /// the animation and path surfaces.
    #[staticmethod]
    fn _integer_interpolate(start: i64, end: i64, alpha: f64) -> (i64, f64) {
        fmn_library::integer_interpolate(start, end, alpha)
    }

    /// Preflight the Python-proxy half of `Mobject.add_n_more_submobjects`
    /// against Marionette's one family-alignment resource ceiling.
    #[staticmethod]
    fn _aligned_submobject_target(current: usize, additional: usize) -> PyResult<usize> {
        let requested = current.checked_add(additional).ok_or_else(|| {
            stage_error(fmn_mobject::StageError::SubmobjectBudgetExceeded {
                requested: usize::MAX,
                max: fmn_mobject::MAX_ALIGNED_SUBMOBJECTS,
            })
        })?;
        if requested > fmn_mobject::MAX_ALIGNED_SUBMOBJECTS {
            return Err(stage_error(
                fmn_mobject::StageError::SubmobjectBudgetExceeded {
                    requested,
                    max: fmn_mobject::MAX_ALIGNED_SUBMOBJECTS,
                },
            ));
        }
        Ok(requested)
    }

    /// `Mobject.become` over `Stage::become_mobject`: per-member data,
    /// uniform, and placement assignment across zipped families after the
    /// Python proxy graph has run Reference `align_family`. Schema drift or
    /// an unsynchronized proxy/native graph remains a precise refusal.
    #[pyo3(signature = (other, match_updaters = false))]
    fn _become(
        slf: &Bound<'_, Self>,
        other: &Bound<'_, BridgeMobject>,
        match_updaters: bool,
    ) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        let self_bound = {
            let cell = slf.borrow();
            cell.engine.as_ref().map(Rc::clone).zip(cell.mob)
        };
        if let Some((engine, mob)) = self_bound {
            let (other_engine, other_mob) = bound_parts(&other.borrow())?;
            if !same_engine(&engine, &other_engine) {
                return Err(ForeignStageError::new_err(
                    "become endpoints must belong to one Scene",
                ));
            }
            return engine
                .borrow_mut()
                .stage_mut()
                .become_mobject(mob, other_mob, match_updaters)
                .map_err(stage_error);
        }
        // Detached self: bring a copy of the source family into the
        // nursery, become it, and drop the temp — the Reference's
        // become-is-a-data-copy semantics without any scene requirement.
        let other_location = {
            let cell = other.borrow();
            (cell.engine.as_ref().map(Rc::clone), cell.mob)
        };
        let mut self_cell = slf.borrow_mut();
        let root = self_cell
            .nursery
            .as_ref()
            .map(|nursery| nursery.root)
            .ok_or_else(|| StaleHandleError::new_err("mobject has no detached or bound state"))?;
        let nursery = self_cell.nursery.as_mut().expect("checked above");
        let temp = match other_location {
            (Some(other_engine), Some(other_mob)) => {
                let scene = other_engine.borrow();
                scene
                    .stage()
                    .copy_into(other_mob, &mut nursery.stage)
                    .map_err(stage_error)?
            }
            _ => {
                let other_cell = other.borrow();
                let other_nursery = other_cell.nursery.as_ref().ok_or_else(|| {
                    StaleHandleError::new_err("become source has no detached or bound state")
                })?;
                other_nursery
                    .stage
                    .copy_into(other_nursery.root, &mut nursery.stage)
                    .map_err(stage_error)?
            }
        };
        let outcome = nursery
            .stage
            .become_mobject(root, temp, match_updaters)
            .map_err(stage_error);
        nursery.stage.delete(temp).map_err(stage_error)?;
        outcome
    }

    /// `TracingTail` construction: create the native tracer — a bound
    /// stage entry whose native dt-updater follows the traced mobject's
    /// center (fmn-library fields.rs) — and bind THIS proxy to it.
    #[pyo3(signature = (scene, traced, time_traced, stroke_color, stroke_width_taper, stroke_opacity_taper))]
    fn _init_native_tracer(
        slf: &Bound<'_, Self>,
        scene: &Bound<'_, PyScene>,
        traced: &Bound<'_, BridgeMobject>,
        time_traced: f64,
        stroke_color: Option<&Bound<'_, PyAny>>,
        stroke_width_taper: Vec<f64>,
        stroke_opacity_taper: Vec<f64>,
    ) -> PyResult<()> {
        if slf.borrow().engine.is_some() {
            return Err(PyRuntimeError::new_err(
                "a tracing tail initializes before scene entry",
            ));
        }
        let engine = Rc::clone(&scene.borrow().engine);
        let (traced_engine, traced_mob) = bound_parts(&traced.borrow())?;
        if !same_engine(&engine, &traced_engine) {
            return Err(ForeignStageError::new_err(
                "the traced mobject belongs to a different Scene",
            ));
        }
        let mut tail = fmn_library::TracingTail::new()
            .with_time_traced(time_traced)
            .with_stroke_width_taper(stroke_width_taper)
            .with_stroke_opacity_taper(stroke_opacity_taper);
        if let Some(color) = stroke_color {
            tail = tail.with_stroke_color(srgb_from_py(color)?);
        }
        let mob = {
            let mut runtime = engine.borrow_mut();
            let mob = tail
                .add_to_stage(runtime.stage_mut(), traced_mob)
                .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
            runtime.stage_mut().pin(mob).map_err(stage_error)?;
            mob
        };
        {
            let mut cell = slf.borrow_mut();
            cell.nursery = None;
            cell.engine = Some(Rc::clone(&engine));
            cell.mob = Some(mob);
            cell.initialized = true;
        }
        register_proxy(slf.py(), scene, mob, slf.as_any())?;
        slf.as_any().setattr("_scene", scene)?;
        Ok(())
    }

    /// `Stage::save_state`: snapshot this entry's family for `Restore`.
    fn _save_state(slf: &Bound<'_, Self>) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| stage.save_state(mob))?
            .map(|_| ())
            .map_err(stage_error)
    }

    /// Recreate the Reference's `saved_state` pointer after a detached
    /// mobject and its previously-copied state have both entered a Scene.
    fn _link_saved_state(slf: &Bound<'_, Self>, saved: &Bound<'_, BridgeMobject>) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        let (engine, mob) = bound_parts(&slf.borrow())?;
        let (saved_engine, saved_mob) = bound_parts(&saved.borrow())?;
        if !same_engine(&engine, &saved_engine) {
            return Err(ForeignStageError::new_err(
                "saved state belongs to a different Scene",
            ));
        }
        engine
            .borrow_mut()
            .stage_mut()
            .link_saved_state(mob, saved_mob)
            .map_err(stage_error)
    }

    /// Reference `get_start`: this entry's own first world-space point.
    fn _get_start(slf: &Bound<'_, Self>) -> PyResult<[f64; 3]> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| stage.get_start(mob))?
            .ok_or_else(|| PyValueError::new_err("Cannot get points of Mobject with no points"))
    }

    /// Reference `get_end`: this entry's own last world-space point.
    fn _get_end(slf: &Bound<'_, Self>) -> PyResult<[f64; 3]> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| stage.get_end(mob))?
            .ok_or_else(|| PyValueError::new_err("Cannot get points of Mobject with no points"))
    }

    /// Materialize this entry's object-to-world placement into its point
    /// records before exposing manim's writable `get_points()` view.
    /// This changes representation only; geometry and placement together
    /// remain identical.
    fn _bake_placement(slf: &Bound<'_, Self>) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| stage.bake_placement(mob))?
            .map(|_| ())
            .map_err(stage_error)
    }

    /// Preserve the Reference's opt-in transformed bounding-box rows after a
    /// Python callable has written the pointlike records. Marionette keys the
    /// value to the current subtree signature, so all later mutations still
    /// invalidate it through the ordinary native revision path.
    fn _cache_bbox_rows(slf: &Bound<'_, Self>, rows: [[f64; 3]; 3]) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| {
            stage.install_bounding_box_cache(
                mob,
                fmn_mobject::BoundingBox {
                    min: rows[0],
                    mid: rows[1],
                    max: rows[2],
                },
            );
        })
    }

    /// Native family matrix application for the common linear transform
    /// branch. The bootstrap distributes this over detached nursery roots but
    /// calls it once for a scene-bound family.
    fn _apply_matrix(
        slf: &Bound<'_, Self>,
        matrix: [[f64; 3]; 3],
        about_point: Option<[f64; 3]>,
        about_edge: Option<[f64; 3]>,
    ) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| {
            stage.apply_points_function(
                mob,
                |point| {
                    [
                        matrix[0][0] * point[0] + matrix[0][1] * point[1] + matrix[0][2] * point[2],
                        matrix[1][0] * point[0] + matrix[1][1] * point[1] + matrix[1][2] * point[2],
                        matrix[2][0] * point[0] + matrix[2][1] * point[1] + matrix[2][2] * point[2],
                    ]
                },
                about_point,
                about_edge,
            );
        })
    }

    /// Chisel's shared-anchor encoding for a corner polyline, installed
    /// into the current Stage entry with the normal schema/revision rules.
    fn _set_points_as_corners(slf: &Bound<'_, Self>, anchors: Vec<[f64; 3]>) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        let mut path = fmn_library::QuadPath::new();
        path.set_points_as_corners(&anchors).map_err(native_error)?;
        with_stage(slf, |stage, mob| stage.set_points(mob, path.points()))?.map_err(stage_error)
    }

    /// Build a complete shared-anchor run from separate anchors and handles.
    /// The caller commits the result through `VMobject.set_points`, retaining
    /// Python's exact structured-record resize semantics.
    fn _set_anchors_and_handles_points(
        &self,
        anchors: Vec<[f64; 3]>,
        handles: Vec<[f64; 3]>,
    ) -> PyResult<Vec<[f64; 3]>> {
        crossing::record(CrossingClass::Other);
        let mut path = fmn_library::QuadPath::new();
        path.set_anchors_and_handles(&anchors, &handles)
            .map_err(native_error)?;
        Ok(path.points().to_vec())
    }

    /// Chisel `start_new_path`, returned as append-only geometry rows.
    fn _start_new_path_points(slf: &Bound<'_, Self>, point: [f64; 3]) -> PyResult<Vec<[f64; 3]>> {
        crossing::record(CrossingClass::Other);
        quad_path_tail(slf, |path| {
            path.start_new_path(point);
            Ok(())
        })
    }

    /// Construct a complete cubic-start operation before committing any row.
    /// BN-13's one error-bounded cubic-to-quadratic converter owns the emitted
    /// topology; the Reference's fixed heuristic approximation is retired.
    fn _add_cubic_bezier_curve_points(
        slf: &Bound<'_, Self>,
        anchor1: [f64; 3],
        handle1: [f64; 3],
        handle2: [f64; 3],
        anchor2: [f64; 3],
    ) -> PyResult<Vec<[f64; 3]>> {
        crossing::record(CrossingClass::Other);
        quad_path_tail(slf, |path| {
            path.start_new_path(anchor1);
            path.add_cubic_bezier_curve_to(handle1, handle2, anchor2)
                .map(|_| ())
                .map_err(native_error)
        })
    }

    /// BN-13 cubic reduction appended to the current shared-anchor path.
    fn _add_cubic_bezier_curve_to_points(
        slf: &Bound<'_, Self>,
        handle1: [f64; 3],
        handle2: [f64; 3],
        anchor: [f64; 3],
    ) -> PyResult<Vec<[f64; 3]>> {
        crossing::record(CrossingClass::Other);
        quad_path_tail(slf, |path| {
            path.add_cubic_bezier_curve_to(handle1, handle2, anchor)
                .map(|_| ())
                .map_err(native_error)
        })
    }

    /// Append one native quadratic segment, including null-curve policy and
    /// break-marker avoidance.
    fn _add_quadratic_bezier_curve_to_points(
        slf: &Bound<'_, Self>,
        handle: [f64; 3],
        anchor: [f64; 3],
        allow_null_curve: bool,
    ) -> PyResult<Vec<[f64; 3]>> {
        crossing::record(CrossingClass::Other);
        quad_path_tail(slf, |path| {
            path.add_quadratic_bezier_curve_to(handle, anchor, allow_null_curve)
                .map(|_| ())
                .map_err(native_error)
        })
    }

    /// Append one native line, honoring both `long_lines` and null-line
    /// policy from the live Python VMobject.
    fn _add_line_to_points(
        slf: &Bound<'_, Self>,
        point: [f64; 3],
        allow_null_line: bool,
    ) -> PyResult<Vec<[f64; 3]>> {
        crossing::record(CrossingClass::Other);
        quad_path_tail(slf, |path| {
            path.add_line_to(point, allow_null_line)
                .map(|_| ())
                .map_err(native_error)
        })
    }

    /// Continue the current native path with a reflected quadratic handle.
    fn _add_smooth_curve_to_points(
        slf: &Bound<'_, Self>,
        point: [f64; 3],
    ) -> PyResult<Vec<[f64; 3]>> {
        crossing::record(CrossingClass::Other);
        quad_path_tail(slf, |path| {
            path.add_smooth_curve_to(point)
                .map(|_| ())
                .map_err(native_error)
        })
    }

    /// Continue the current native path with BN-13 cubic reduction.
    fn _add_smooth_cubic_curve_to_points(
        slf: &Bound<'_, Self>,
        handle: [f64; 3],
        point: [f64; 3],
    ) -> PyResult<Vec<[f64; 3]>> {
        crossing::record(CrossingClass::Other);
        quad_path_tail(slf, |path| {
            path.add_smooth_cubic_curve_to(handle, point)
                .map(|_| ())
                .map_err(native_error)
        })
    }

    /// Append a native endpoint arc under the public threshold and explicit
    /// component policy. BN-09 owns the default density.
    fn _add_arc_to_points(
        slf: &Bound<'_, Self>,
        point: [f64; 3],
        angle: f64,
        n_components: Option<usize>,
        threshold: f64,
    ) -> PyResult<Vec<[f64; 3]>> {
        crossing::record(CrossingClass::Other);
        if !threshold.is_finite() || threshold < 0.0 {
            return Err(PyValueError::new_err(
                "arc threshold must be a finite non-negative number",
            ));
        }
        quad_path_tail(slf, |path| {
            path.add_arc_to_with_threshold(point, angle, n_components, threshold)
                .map(|_| ())
                .map_err(native_error)
        })
    }

    /// Append a sequence of native straight quadratic segments atomically.
    fn _add_points_as_corners_points(
        slf: &Bound<'_, Self>,
        points: Vec<[f64; 3]>,
    ) -> PyResult<Vec<[f64; 3]>> {
        crossing::record(CrossingClass::Other);
        quad_path_tail(slf, |path| {
            path.add_points_as_corners(&points)
                .map(|_| ())
                .map_err(native_error)
        })
    }

    /// Append a complete validated native subpath atomically.
    fn _add_subpath_points(
        slf: &Bound<'_, Self>,
        points: Vec<[f64; 3]>,
    ) -> PyResult<Vec<[f64; 3]>> {
        crossing::record(CrossingClass::Other);
        quad_path_tail(slf, |path| {
            path.add_subpath(&points).map(|_| ()).map_err(native_error)
        })
    }

    /// Close only the current native subpath and return its appended rows.
    fn _close_path_points(slf: &Bound<'_, Self>, smooth: bool) -> PyResult<Vec<[f64; 3]>> {
        crossing::record(CrossingClass::Other);
        quad_path_tail(slf, |path| {
            path.close_path(smooth).map(|_| ()).map_err(native_error)
        })
    }

    fn _has_new_path_started(slf: &Bound<'_, Self>) -> PyResult<bool> {
        crossing::record(CrossingClass::Other);
        Ok(configured_quad_path(slf)?.has_new_path_started())
    }

    fn _is_path_closed(slf: &Bound<'_, Self>) -> PyResult<bool> {
        crossing::record(CrossingClass::Other);
        Ok(configured_quad_path(slf)?.is_closed())
    }

    fn _path_area_vector(slf: &Bound<'_, Self>) -> PyResult<[f64; 3]> {
        crossing::record(CrossingClass::Other);
        Ok(configured_quad_path(slf)?.area_vector())
    }

    fn _path_unit_normal(slf: &Bound<'_, Self>) -> PyResult<[f64; 3]> {
        crossing::record(CrossingClass::Other);
        Ok(configured_quad_path(slf)?.unit_normal())
    }

    fn _consider_path_points_equal(
        slf: &Bound<'_, Self>,
        p0: [f64; 3],
        p1: [f64; 3],
    ) -> PyResult<bool> {
        crossing::record(CrossingClass::Other);
        Ok(configured_quad_path(slf)?.consider_points_equal(p0, p1))
    }

    fn _is_path_smooth(slf: &Bound<'_, Self>, angle_tol: f64) -> PyResult<bool> {
        crossing::record(CrossingClass::Other);
        Ok(configured_quad_path(slf)?.is_smooth(angle_tol))
    }

    /// Rebuild this VMobject's handles through Chisel's one anchor-mode
    /// implementation, then commit through the Stage's RecordBuffer path.
    fn _change_anchor_mode(slf: &Bound<'_, Self>, mode: &str) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        let mode = match mode {
            "jagged" => fmn_library::AnchorMode::Jagged,
            "approx_smooth" => fmn_library::AnchorMode::ApproxSmooth,
            "true_smooth" => fmn_library::AnchorMode::TrueSmooth,
            other => {
                return Err(PyValueError::new_err(format!(
                    "unknown VMobject anchor mode {other:?}; expected jagged, approx_smooth, or true_smooth"
                )));
            }
        };
        let mut path = configured_quad_path(slf)?;
        path.change_anchor_mode(mode).map_err(native_error)?;
        with_stage(slf, |stage, mob| stage.set_points(mob, path.points()))?.map_err(stage_error)
    }

    /// Chisel's bounded longest-curve insertion over an arbitrary valid
    /// shared-anchor point run.  This is intentionally a pure helper: the
    /// Python surface owns Reference family recursion and commits the result
    /// through `VMobject.set_points`, so RecordBuffer style lanes and view
    /// generations follow the same path as every other portal point edit.
    fn _insert_n_curves_to_point_list(
        &self,
        n: usize,
        points: Vec<[f64; 3]>,
        tolerance: f64,
    ) -> PyResult<Vec<[f64; 3]>> {
        crossing::record(CrossingClass::Other);
        fmn_library::QuadPath::insert_n_curves_to_point_list(n, &points, tolerance)
            .map_err(native_error)
    }

    /// Run Marionette's complete VMobject subpath-alignment algorithm. A
    /// same-Scene pair can mutate its shared Stage directly. Detached,
    /// mixed-state, and cross-Scene pairs are aligned as complete native
    /// entry copies first; Python then commits the two proven point runs
    /// through each receiver's ordinary RecordBuffer resize semantics.
    fn _align_vmobject_points(
        slf: &Bound<'_, Self>,
        other: &Bound<'_, BridgeMobject>,
        tolerance: f64,
    ) -> PyResult<Option<AlignedPointRuns>> {
        crossing::record(CrossingClass::FieldWrite);
        let left_bound = {
            let cell = slf.borrow();
            cell.engine.as_ref().map(Rc::clone).zip(cell.mob)
        };
        let right_bound = {
            let cell = other.borrow();
            cell.engine.as_ref().map(Rc::clone).zip(cell.mob)
        };
        if let (Some((left_engine, left_mob)), Some((right_engine, right_mob))) =
            (&left_bound, &right_bound)
            && same_engine(left_engine, right_engine)
        {
            left_engine
                .borrow_mut()
                .stage_mut()
                .align_points_with_tolerance(*left_mob, *right_mob, tolerance)
                .map_err(stage_error)?;
            return Ok(None);
        }

        let mut staging = Stage::new();
        let left = copy_proxy_entry_into(slf, &mut staging)?;
        let right = copy_proxy_entry_into(other, &mut staging)?;
        staging
            .align_points_with_tolerance(left, right, tolerance)
            .map_err(stage_error)?;
        let left_points = staging
            .get_points(left)
            .ok_or_else(|| StaleHandleError::new_err("aligned receiver no longer resolves"))?;
        let right_points = staging
            .get_points(right)
            .ok_or_else(|| StaleHandleError::new_err("aligned peer no longer resolves"))?;
        Ok(Some((left_points, right_points)))
    }

    /// Chisel's bounded sharp-curve subdivision over this entry's current
    /// shared-anchor path. The caller plans a whole requested family before
    /// committing any member, so a malformed threshold or budget refusal
    /// cannot leave a partially subdivided Python family.
    fn _subdivide_sharp_curve_points(
        slf: &Bound<'_, Self>,
        angle_threshold: f64,
    ) -> PyResult<Vec<[f64; 3]>> {
        crossing::record(CrossingClass::Other);
        let mut path = configured_quad_path(slf)?;
        path.subdivide_sharp_curves(angle_threshold)
            .map_err(native_error)?;
        Ok(path.points().to_vec())
    }

    /// Apply caller-supplied subdivision counts through Chisel's bounded
    /// preflight. Python evaluates the host callback exactly once for every
    /// live quadratic before entering this method, so no Stage borrow crosses
    /// user code and a whole requested family can be planned before its first
    /// RecordBuffer write.
    fn _subdivide_curve_points_by_counts(
        slf: &Bound<'_, Self>,
        subdivision_counts: &Bound<'_, PyAny>,
    ) -> PyResult<Vec<[f64; 3]>> {
        crossing::record(CrossingClass::Other);
        let mut path = configured_quad_path(slf)?;
        let mut counts = Vec::with_capacity(path.num_curves());
        for count in subdivision_counts.try_iter()? {
            let count = count?;
            counts.push(bounded_subdivision_count(&count)?);
        }
        if counts.len() != path.num_curves() {
            return Err(PyValueError::new_err(format!(
                "subdivision count length {} does not match the live curve count {}",
                counts.len(),
                path.num_curves()
            )));
        }
        let cursor = Cell::new(0usize);
        path.subdivide_curves_by_condition(|_| {
            let index = cursor.get();
            cursor.set(index + 1);
            counts
                .get(index)
                .copied()
                .unwrap_or(fmn_library::MAX_SUBDIVIDED_CURVES)
        })
        .map_err(native_error)?;
        Ok(path.points().to_vec())
    }

    /// Reference `subdivide_intersections` with both the strict crossing test
    /// and the bounded split performed in Chisel. Count coercion remains lazy:
    /// just like the Reference, an invalid positive count is never inspected
    /// when no curve intersects the captured anchor path.
    fn _subdivide_intersection_curve_points(
        slf: &Bound<'_, Self>,
        intersection_path: Vec<[f64; 3]>,
        n_subdivisions: &Bound<'_, PyAny>,
    ) -> PyResult<Vec<[f64; 3]>> {
        crossing::record(CrossingClass::Other);
        let mut path = configured_quad_path(slf)?;
        let intersects = path
            .bezier_tuples()
            .any(|[b0, b1, _]| fmn_library::line_intersects_path(b0, b1, &intersection_path));
        if !intersects {
            return Ok(path.points().to_vec());
        }
        let count = positive_subdivision_count(n_subdivisions)?;
        path.subdivide_curves_by_condition(|[b0, b1, _]| {
            if fmn_library::line_intersects_path(b0, b1, &intersection_path) {
                count
            } else {
                0
            }
        })
        .map_err(native_error)?;
        Ok(path.points().to_vec())
    }

    /// Revalidate a Python-authored VMobject point run and refresh the native
    /// shared-anchor metadata through Marionette's one `Stage::set_points`
    /// path.  The Python layer owns Reference record-resize semantics; this
    /// call owns the geometry invariant and derived joint-angle column.
    fn _refresh_vmobject_path_metadata(slf: &Bound<'_, Self>) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| {
            let points = stage.get_points(mob).ok_or(StageError::StaleHandle)?;
            stage.set_points(mob, &points)
        })?
        .map_err(stage_error)
    }

    /// Chisel's anchor-mode smoothing over this entry's current world-space
    /// shared-anchor path. Python owns family recursion so `recurse=False`
    /// remains exact without introducing a second Stage traversal contract.
    fn _make_smooth(slf: &Bound<'_, Self>, approx: bool) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        let mut path = configured_quad_path(slf)?;
        if path.num_points() < 3 {
            return Ok(());
        }
        path.make_smooth(approx).map_err(native_error)?;
        with_stage(slf, |stage, mob| stage.set_points(mob, path.points()))?.map_err(stage_error)
    }

    /// Reference `reverse_points` through Marionette's family operation,
    /// which reverses every record row with its point and repairs path
    /// break handles/base normals.
    fn _reverse_points(slf: &Bound<'_, Self>, repair_recurse: bool) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| {
            stage.reverse_family_points_with_scope(mob, repair_recurse)
        })?
        .map_err(stage_error)
    }

    /// Whether this node's updater traversal is suspended. Ancestor pruning
    /// is applied by the scene target collector; this is the Reference's
    /// public self-state query used by `Mobject.update`.
    fn _is_updating_suspended(slf: &Bound<'_, Self>) -> PyResult<bool> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| stage.is_updating_suspended(mob))
    }

    /// Route `Mobject.suspend_updating` to Marionette's durable updater flag.
    fn _suspend_updating(slf: &Bound<'_, Self>, recurse: bool) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| stage.suspend_updating(mob, recurse))
    }

    /// Clear suspension on the selected family and ancestor chain. Python
    /// owns the immediate callback pass so no Stage borrow crosses a host
    /// callable; `Mobject.resume_updating` invokes it after this returns.
    fn _resume_updating(slf: &Bound<'_, Self>, recurse: bool) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| {
            stage.resume_updating(mob, recurse, false);
        })
    }

    /// Clear Marionette-owned updater slots alongside the bootstrap's host
    /// callback list. Python owns callable identity; Marionette owns native
    /// updater identities installed by engine-backed mobject features.
    fn _clear_native_updaters(slf: &Bound<'_, Self>, recurse: bool) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| stage.clear_updaters(mob, recurse))
    }

    /// Run only Marionette-owned updaters for this mobject. The bootstrap
    /// first performs the matching Python family pass outside any Stage
    /// borrow, preserving the portal's callback-safety boundary.
    fn _update_native_mobject(slf: &Bound<'_, Self>, dt: f64, recurse: bool) -> PyResult<()> {
        with_stage(slf, |stage, mob| {
            stage.update_mobject_with_recurse(mob, dt, recurse);
        })
    }

    /// True arc length over this entry's current world-space shared-anchor
    /// path. The optional sampling parameter on manim's VMobject surface is
    /// deliberately unnecessary: Chisel's error-bounded quadrature is the
    /// definition (BN-03), including for negative `path_arc` lines.
    fn _get_arc_length(slf: &Bound<'_, Self>) -> PyResult<f64> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| {
            let points = stage.get_points(mob).unwrap_or_default();
            fmn_library::VMobject::from_points(points)
                .path()
                .map(|path| path.get_arc_length())
        })?
        .map_err(native_error)
    }

    /// Atlas's `Line.get_vector` after Python performs Reference-style virtual
    /// endpoint dispatch (notably for `DashedLine`'s first/last children).
    fn _line_vector(_slf: &Bound<'_, Self>, start: [f64; 3], end: [f64; 3]) -> PyResult<[f64; 3]> {
        crossing::record(CrossingClass::Other);
        let line = fmn_library::line::Line::new(start, end)
            .build()
            .map_err(native_error)?;
        Ok(fmn_library::line::line_vector(&line))
    }

    /// Atlas's `Line.get_unit_vector` after virtual endpoint dispatch.
    fn _line_unit_vector(
        _slf: &Bound<'_, Self>,
        start: [f64; 3],
        end: [f64; 3],
    ) -> PyResult<[f64; 3]> {
        crossing::record(CrossingClass::Other);
        let line = fmn_library::line::Line::new(start, end)
            .build()
            .map_err(native_error)?;
        Ok(fmn_library::line::line_unit_vector(&line))
    }

    /// Atlas's planar `Line.get_angle` after virtual endpoint dispatch.
    fn _line_angle(_slf: &Bound<'_, Self>, start: [f64; 3], end: [f64; 3]) -> PyResult<f64> {
        crossing::record(CrossingClass::Other);
        let line = fmn_library::line::Line::new(start, end)
            .build()
            .map_err(native_error)?;
        Ok(fmn_library::line::line_angle(&line))
    }

    /// Atlas's deterministic-math `Line.get_slope`.
    fn _line_slope(_slf: &Bound<'_, Self>, start: [f64; 3], end: [f64; 3]) -> PyResult<f64> {
        crossing::record(CrossingClass::Other);
        let line = fmn_library::line::Line::new(start, end)
            .build()
            .map_err(native_error)?;
        Ok(fmn_library::line::line_slope(&line))
    }

    /// Atlas's `Line.get_projection`, preserving all three coordinates.
    fn _line_projection(
        _slf: &Bound<'_, Self>,
        start: [f64; 3],
        end: [f64; 3],
        point: [f64; 3],
    ) -> PyResult<[f64; 3]> {
        crossing::record(CrossingClass::Other);
        let line = fmn_library::line::Line::new(start, end)
            .build()
            .map_err(native_error)?;
        Ok(fmn_library::line::line_projection(&line, point))
    }

    /// The project contract's true-arclength `point_from_proportion`
    /// (BN-03), routed to Chisel through Marionette for both proxy states.
    fn _point_from_proportion(slf: &Bound<'_, Self>, alpha: f64) -> PyResult<[f64; 3]> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| stage.point_from_proportion(mob, alpha))?.map_err(stage_error)
    }

    /// Reference `VMobject.pointwise_become_partial`, routed to the same
    /// Marionette operation Choreo's creation animations use. The source may
    /// be detached or scene-bound; an unrelated Stage contributes a temporary
    /// root-entry copy so record data never crosses an active arena borrow.
    fn _pointwise_become_partial(
        slf: &Bound<'_, Self>,
        source: &Bound<'_, BridgeMobject>,
        a: f64,
        b: f64,
    ) -> PyResult<()> {
        if !a.is_finite() || !b.is_finite() {
            return Err(PyValueError::new_err("partial-curve bounds must be finite"));
        }
        crossing::record(CrossingClass::FieldWrite);

        let self_bound = {
            let cell = slf.borrow();
            cell.engine
                .as_ref()
                .zip(cell.mob)
                .map(|(engine, mob)| (Rc::clone(engine), mob))
        };
        if let Some((engine, mob)) = self_bound {
            let source_bound = {
                let cell = source.borrow();
                cell.engine
                    .as_ref()
                    .zip(cell.mob)
                    .map(|(source_engine, source_mob)| (Rc::clone(source_engine), source_mob))
            };
            if let Some((source_engine, source_mob)) = source_bound
                && same_engine(&engine, &source_engine)
            {
                return engine
                    .borrow_mut()
                    .stage_mut()
                    .pointwise_become_partial(mob, source_mob, a, b)
                    .map_err(stage_error);
            }

            let mut runtime = engine.borrow_mut();
            let stage = runtime.stage_mut();
            let temporary = copy_proxy_entry_into(source, stage)?;
            let result = stage
                .pointwise_become_partial(mob, temporary, a, b)
                .map_err(stage_error);
            let cleanup = stage.delete(temporary).map_err(stage_error);
            result.and(cleanup)
        } else {
            let mut cell = slf.borrow_mut();
            let nursery = cell.nursery.as_mut().ok_or_else(|| {
                StaleHandleError::new_err("partial target has no detached or bound state")
            })?;
            if slf.is(source) {
                return nursery
                    .stage
                    .pointwise_become_partial(nursery.root, nursery.root, a, b)
                    .map_err(stage_error);
            }
            let temporary = copy_proxy_entry_into(source, &mut nursery.stage)?;
            let result = nursery
                .stage
                .pointwise_become_partial(nursery.root, temporary, a, b)
                .map_err(stage_error);
            let cleanup = nursery.stage.delete(temporary).map_err(stage_error);
            result.and(cleanup)
        }
    }

    /// Reference `Surface.pointwise_become_partial`, routed to the same
    /// Marionette UV-grid operation used by Choreo's creation animations.
    /// Surface resolution is Python constructor state, so the authored
    /// public method supplies the source grid explicitly after resolving the
    /// Reference's preferred-axis default. As with the VMobject sibling, an
    /// unrelated Stage contributes a temporary root-entry copy and no Python
    /// operation runs while either Stage is borrowed.
    fn _surface_pointwise_become_partial(
        slf: &Bound<'_, Self>,
        source: &Bound<'_, BridgeMobject>,
        resolution: (usize, usize),
        axis: usize,
        a: f64,
        b: f64,
    ) -> PyResult<()> {
        if !a.is_finite() || !b.is_finite() {
            return Err(PyValueError::new_err(
                "partial-surface bounds must be finite",
            ));
        }
        crossing::record(CrossingClass::FieldWrite);

        let self_bound = {
            let cell = slf.borrow();
            cell.engine
                .as_ref()
                .zip(cell.mob)
                .map(|(engine, mob)| (Rc::clone(engine), mob))
        };
        if let Some((engine, mob)) = self_bound {
            let source_bound = {
                let cell = source.borrow();
                cell.engine
                    .as_ref()
                    .zip(cell.mob)
                    .map(|(source_engine, source_mob)| (Rc::clone(source_engine), source_mob))
            };
            if let Some((source_engine, source_mob)) = source_bound
                && same_engine(&engine, &source_engine)
            {
                return engine
                    .borrow_mut()
                    .stage_mut()
                    .surface_pointwise_become_partial(mob, source_mob, resolution, axis, a, b)
                    .map_err(stage_error);
            }

            let mut runtime = engine.borrow_mut();
            let stage = runtime.stage_mut();
            let temporary = copy_proxy_entry_into(source, stage)?;
            let result = stage
                .surface_pointwise_become_partial(mob, temporary, resolution, axis, a, b)
                .map_err(stage_error);
            let cleanup = stage.delete(temporary).map_err(stage_error);
            result.and(cleanup)
        } else {
            let mut cell = slf.borrow_mut();
            let nursery = cell.nursery.as_mut().ok_or_else(|| {
                StaleHandleError::new_err("partial surface target has no detached or bound state")
            })?;
            if slf.is(source) {
                return nursery
                    .stage
                    .surface_pointwise_become_partial(
                        nursery.root,
                        nursery.root,
                        resolution,
                        axis,
                        a,
                        b,
                    )
                    .map_err(stage_error);
            }
            let temporary = copy_proxy_entry_into(source, &mut nursery.stage)?;
            let result = nursery
                .stage
                .surface_pointwise_become_partial(nursery.root, temporary, resolution, axis, a, b)
                .map_err(stage_error);
            let cleanup = nursery.stage.delete(temporary).map_err(stage_error);
            result.and(cleanup)
        }
    }

    /// Atlas/Chisel's true-arclength dash placement expressed as the
    /// curve-index windows consumed by `pointwise_become_partial`.
    fn _dash_curve_intervals(
        slf: &Bound<'_, Self>,
        num_dashes: usize,
        positive_space_ratio: f64,
    ) -> PyResult<Vec<(f64, f64)>> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| {
            let source =
                fmn_library::VMobject::from_points(stage.get_points(mob).unwrap_or_default());
            fmn_library::vmobject::dash_curve_intervals(
                &source,
                num_dashes,
                positive_space_ratio,
                0.0,
            )
        })?
        .map_err(native_error)
    }

    /// Position an existing Python-owned tip with Atlas's true terminal
    /// tangent.  Only the completed point run crosses back to Python, which
    /// preserves the tip proxy's identity and style records.
    fn _position_tip_points(
        slf: &Bound<'_, Self>,
        tip: &Bound<'_, BridgeMobject>,
        at_start: bool,
    ) -> PyResult<Vec<[f64; 3]>> {
        crossing::record(CrossingClass::Other);
        let path = configured_quad_path(slf)?;
        let shape = fmn_library::VMobject::from_path(&path);
        let tip_points = with_stage(tip, |stage, mob| stage.get_points(mob).unwrap_or_default())?;
        let end = if at_start {
            fmn_library::TipEnd::Start
        } else {
            fmn_library::TipEnd::End
        };
        Ok(fmn_library::tip::position_tip(
            &shape,
            fmn_library::VMobject::from_points(tip_points),
            end,
        )
        .points()
        .to_vec())
    }

    /// Pull this shaft back to a positioned tip's base through Atlas's
    /// true-arc-length trim (BN-03).  The detached native result is complete
    /// before Marionette commits the point run, so geometry refusal cannot
    /// partially rewrite the live record buffer.
    fn _trim_to_tip(
        slf: &Bound<'_, Self>,
        tip: &Bound<'_, BridgeMobject>,
        at_start: bool,
    ) -> PyResult<()> {
        let path = configured_quad_path(slf)?;
        let shape = fmn_library::VMobject::from_path(&path);
        let tip_points = with_stage(tip, |stage, mob| stage.get_points(mob).unwrap_or_default())?;
        let tip = fmn_library::VMobject::from_points(tip_points);
        let end = if at_start {
            fmn_library::TipEnd::Start
        } else {
            fmn_library::TipEnd::End
        };
        let points = fmn_library::tip::trim_to_tip(shape, &tip, end)
            .points()
            .to_vec();
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| stage.set_points(mob, &points))?.map_err(stage_error)
    }

    /// `Arc.get_arc_center` over this entry's current world-space points.
    fn _arc_center(slf: &Bound<'_, Self>) -> PyResult<[f64; 3]> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| {
            fmn_library::arc::arc_center_of(&stage.get_points(mob).unwrap_or_default())
        })?
        .ok_or_else(|| {
            PyValueError::new_err("an arc center requires at least one quadratic component")
        })
    }

    /// `Arc.get_start_angle` over live transformed geometry.
    fn _arc_start_angle(slf: &Bound<'_, Self>) -> PyResult<f64> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| {
            fmn_library::arc::start_angle_of(&stage.get_points(mob).unwrap_or_default())
        })?
        .ok_or_else(|| {
            PyValueError::new_err("an arc start angle requires at least one quadratic component")
        })
    }

    /// `Arc.get_stop_angle` over live transformed geometry.
    fn _arc_stop_angle(slf: &Bound<'_, Self>) -> PyResult<f64> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| {
            fmn_library::arc::stop_angle_of(&stage.get_points(mob).unwrap_or_default())
        })?
        .ok_or_else(|| {
            PyValueError::new_err("an arc stop angle requires at least one quadratic component")
        })
    }

    /// `Circle.get_radius` over this entry's current world-space points.
    fn _circle_radius(slf: &Bound<'_, Self>) -> PyResult<f64> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| {
            let points = stage.get_points(mob).unwrap_or_default();
            fmn_library::arc::radius_of(&fmn_library::VMobject::from_points(points))
        })
    }

    /// `Circle.point_at_angle` over its current world-space path.
    fn _circle_point_at_angle(slf: &Bound<'_, Self>, angle: f64) -> PyResult<[f64; 3]> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| {
            let points = stage.get_points(mob).unwrap_or_default();
            fmn_library::arc::point_at_angle(&fmn_library::VMobject::from_points(points), angle)
        })?
        .ok_or_else(|| PyValueError::new_err("point_at_angle requires a nonempty circle path"))
    }

    /// `Stage::put_start_and_end_on` over the Stage-visible family.
    fn _put_start_and_end_on(
        slf: &Bound<'_, Self>,
        start: [f64; 3],
        end: [f64; 3],
    ) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| {
            stage.put_start_and_end_on(mob, start, end)
        })?
        .map_err(stage_error)
    }

    // ------------------------------------------------- native builders
    //
    // fm-d3gt: the schema-class constructor seam. Each method drives one
    // fmn-library builder and installs the built family via
    // `install_native_tree`; the returned nested `(shell, children)` specs
    // are hung on the Python family lists by the bootstrap.

    /// `CubicBezier(a0, h0, h1, a1)` over Chisel's one bounded
    /// cubic-to-quadratic converter. The detached native value is complete
    /// before it replaces the constructing proxy, so conversion refusal is
    /// failure-atomic at the Python boundary.
    fn _build_cubic_bezier<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        a0: [f64; 3],
        h0: [f64; 3],
        h1: [f64; 3],
        a1: [f64; 3],
    ) -> PyResult<Bound<'py, PyList>> {
        let built = fmn_library::CubicBezier::new(a0, h0, h1, a1)
            .build()
            .map_err(native_error)?;
        install_native_tree(slf, factory, built)
    }

    /// `Polygon(*vertices)` over Atlas's shared-anchor polygon builder.
    fn _build_polygon<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        vertices: Vec<[f64; 3]>,
    ) -> PyResult<Bound<'py, PyList>> {
        install_native_tree(slf, factory, fmn_library::Polygon::new(vertices).build())
    }

    /// `Polyline(*vertices)` over Atlas's open shared-anchor polygon builder.
    fn _build_polyline<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        vertices: Vec<[f64; 3]>,
    ) -> PyResult<Bound<'py, PyList>> {
        install_native_tree(
            slf,
            factory,
            fmn_library::Polygon::polyline(vertices).build(),
        )
    }

    /// `VectorizedPoint(location)` over Atlas's one-record native value.
    fn _build_vectorized_point<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        location: [f64; 3],
    ) -> PyResult<Bound<'py, PyList>> {
        install_native_tree(
            slf,
            factory,
            fmn_library::vmobject::vectorized_point(location),
        )
    }

    /// `SVGMobject(file_name=…, svg_string=…)` over Chisel's hardened
    /// user-SVG document processor (fm-5wq.4.50, G2 criterion 4). The file
    /// is read natively under the processor's declared byte budget — the
    /// size is checked against filesystem metadata *before* the read, so a
    /// bomb file never allocates — and every processor refusal (bombs,
    /// nesting, DOCTYPE, unsupported features, malformed XML) is fmn-geom's
    /// typed `SvgError`, surfaced verbatim as the named Python error.
    fn _build_svg_mobject<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        file_name: &str,
        svg_string: Option<&str>,
    ) -> PyResult<Bound<'py, PyList>> {
        let limits = fmn_library::svg::SvgLimits::default();
        let document = if let Some(text) = svg_string {
            fmn_library::svg::SvgDocument::parse_with_limits(text.as_bytes(), &limits)
                .map_err(native_error)?
        } else {
            let metadata = std::fs::metadata(file_name).map_err(|error| {
                PyOSError::new_err(format!("SVGMobject cannot read {file_name:?}: {error}"))
            })?;
            if !metadata.is_file() {
                return Err(PyOSError::new_err(format!(
                    "SVGMobject source {file_name:?} is not a regular file"
                )));
            }
            let bytes = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
            if bytes > limits.max_bytes {
                return Err(native_error(fmn_library::svg::SvgError::TooLarge {
                    bytes,
                    limit: limits.max_bytes,
                }));
            }
            let contents = std::fs::read(file_name).map_err(|error| {
                PyOSError::new_err(format!("SVGMobject cannot read {file_name:?}: {error}"))
            })?;
            fmn_library::svg::SvgDocument::parse_with_limits(&contents, &limits)
                .map_err(native_error)?
        };
        install_native_tree(slf, factory, fmn_library::svg_document_mobject(&document))
    }

    /// Split a source's live shared-anchor run with Atlas's
    /// `CurvesAsSubmobjects` builder. Python reapplies the source style to
    /// each returned child, matching the Reference's constructor body.
    fn _build_curves_as_submobjects<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        source: &Bound<'_, BridgeMobject>,
    ) -> PyResult<Bound<'py, PyList>> {
        let points = with_stage(source, |stage, mob| {
            stage.get_points(mob).unwrap_or_default()
        })?;
        let source = fmn_library::VMobject::from_points(points);
        install_native_tree(
            slf,
            factory,
            fmn_library::vmobject::curves_as_submobjects(&source),
        )
    }

    /// `RegularPolygon(n, radius, start_angle)`: the bounded native
    /// compass-direction kernel owns both the vertex count and orientation.
    fn _build_regular_polygon<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        n: usize,
        radius: f64,
        start_angle: Option<f64>,
    ) -> PyResult<Bound<'py, PyList>> {
        let mut polygon = fmn_library::RegularPolygon::new(n).radius(radius);
        if let Some(angle) = start_angle {
            polygon = polygon.start_angle(angle);
        }
        install_native_tree(slf, factory, polygon.build().map_err(native_error)?)
    }

    /// `ArrowTip(...)` over the one native tip builder.  As in the
    /// Reference, values other than the two special style codes fall back
    /// to the ordinary triangular tip.
    #[allow(clippy::too_many_arguments)]
    fn _build_arrow_tip<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        angle: f64,
        width: f64,
        length: f64,
        tip_style: i64,
    ) -> PyResult<Bound<'py, PyList>> {
        let tip_style = match tip_style {
            1 => fmn_library::TipStyle::InnerSmooth,
            2 => fmn_library::TipStyle::Dot,
            _ => fmn_library::TipStyle::Triangle,
        };
        let built = fmn_library::ArrowTip::new()
            .angle(angle)
            .width(width)
            .length(length)
            .tip_style(tip_style)
            .build();
        install_native_tree(slf, factory, built)
    }

    /// Mutate a detached or scene-bound polygon through Atlas's native
    /// rounded-corner algorithm.  The complete result is built before the
    /// Stage write, so malformed geometry cannot leave a partial mutation.
    fn _round_polygon_corners(
        slf: &Bound<'_, Self>,
        vertices: Vec<[f64; 3]>,
        radius: Option<f64>,
    ) -> PyResult<()> {
        let rounded = fmn_library::Polygon::new(vertices)
            .round_corners(radius)
            .map_err(native_error)?;
        with_stage(slf, |stage, mob| stage.set_points(mob, rounded.points()))?.map_err(stage_error)
    }

    /// `Rectangle(width, height)` over the polygon shelf.
    fn _build_rectangle<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        width: f64,
        height: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let built = fmn_library::Rectangle::new()
            .width(width)
            .height(height)
            .build()
            .map_err(native_error)?;
        install_native_tree(slf, factory, built)
    }

    /// `RoundedRectangle(width, height, corner_radius)` over the same Atlas
    /// rectangle builder.  Atlas rounds only after applying the requested
    /// extent, so the corners remain circular rather than stretched arcs.
    fn _build_rounded_rectangle<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        width: f64,
        height: f64,
        corner_radius: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let built = fmn_library::Rectangle::new()
            .width(width)
            .height(height)
            .corner_radius(corner_radius)
            .build()
            .map_err(native_error)?;
        install_native_tree(slf, factory, built)
    }

    /// `ScreenRectangle(aspect_ratio, height)` over Atlas's frame helper.
    fn _build_screen_rectangle<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        aspect_ratio: f64,
        height: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let built = fmn_library::poly::screen_rectangle(aspect_ratio, height)
            .build()
            .map_err(native_error)?;
        install_native_tree(slf, factory, built)
    }

    /// `SurroundingRectangle(mobject, buff)`: feed Marionette's
    /// authoritative family extent into Atlas's one shape-matcher
    /// implementation.  `has_extent` distinguishes an empty family from a
    /// genuine zero-size family at the origin.
    fn _build_surrounding_rectangle<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        min: [f64; 3],
        max: [f64; 3],
        has_extent: bool,
        buff: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let extent = has_extent.then_some((min, max));
        let built = fmn_library::SurroundingRectangle::from_extent(extent)
            .buff(buff)
            .build();
        install_native_tree(slf, factory, built)
    }

    /// `Cross(mobject, ...)` over Atlas's tapered two-arm matcher. The
    /// source proxy contributes its authoritative live family geometry;
    /// Atlas alone derives the extent, arm paths, and default taper.
    fn _build_cross<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        extent: Option<([f64; 3], [f64; 3])>,
        color: &Bound<'_, PyAny>,
    ) -> PyResult<Bound<'py, PyList>> {
        let target = matcher_extent_vmobject(extent);
        let built = fmn_library::cross(&target, srgb_from_py(color)?, 6.0);
        install_native_tree(slf, factory, built)
    }

    /// `Underline(mobject, ...)` over Atlas's extent-driven tapered rule.
    /// Non-finite placement controls refuse before any nursery mutation.
    fn _build_underline<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        extent: Option<([f64; 3], [f64; 3])>,
        color: &Bound<'_, PyAny>,
        buff: f64,
        stretch_factor: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        if !buff.is_finite() || !stretch_factor.is_finite() {
            return Err(PyValueError::new_err(
                "Underline buff and stretch_factor must be finite",
            ));
        }
        let target = matcher_extent_vmobject(extent);
        let built = fmn_library::underline(&target, srgb_from_py(color)?, buff, stretch_factor);
        install_native_tree(slf, factory, built)
    }

    /// The paired `pifont` marks are native drawn paths (BN-08), wrapped in
    /// one empty TexText-family root so inherited indexing and selector
    /// behavior retain the Reference's one-glyph family shape.
    fn _build_checkmark<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        color: &Bound<'_, PyAny>,
    ) -> PyResult<Bound<'py, PyList>> {
        let mark = fmn_library::checkmark(srgb_from_py(color)?);
        let tree = fmn_library::VMobject::new().with_children([mark]);
        install_native_tree(slf, factory, tree)
    }

    /// Native drawn sibling of [`Self::_build_checkmark`].
    fn _build_exmark<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        color: &Bound<'_, PyAny>,
    ) -> PyResult<Bound<'py, PyList>> {
        let mark = fmn_library::exmark(srgb_from_py(color)?);
        let tree = fmn_library::VMobject::new().with_children([mark]);
        install_native_tree(slf, factory, tree)
    }

    /// Retarget a scene-bound `SurroundingRectangle` without replacing its
    /// arena entry. Atlas remains the one geometry implementation; only the
    /// newly built world-space point run and primitive hint replace the live
    /// entry, while the Python layer reapplies its existing style.
    fn _rebuild_surrounding_rectangle(
        slf: &Bound<'_, Self>,
        min: [f64; 3],
        max: [f64; 3],
        has_extent: bool,
        buff: f64,
    ) -> PyResult<()> {
        let extent = has_extent.then_some((min, max));
        let built = fmn_library::SurroundingRectangle::from_extent(extent)
            .buff(buff)
            .build();
        let points = built.points().to_vec();
        let shape = built.shape();
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| {
            stage.set_points(mob, &points)?;
            stage.set_shape(mob, shape);
            Ok(())
        })?
        .map_err(stage_error)
    }

    /// Native `Brace(mobject, direction, buff)` over the target's live
    /// world-space family geometry. The returned point index tracks the
    /// analytic curl tip through later affine transforms.
    fn _build_brace<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        target: &Bound<'_, BridgeMobject>,
        direction: [f64; 3],
        buff: f64,
    ) -> PyResult<(Bound<'py, PyList>, usize)> {
        let points = with_stage(target, |stage, mob| {
            stage
                .family(mob)
                .into_iter()
                .flat_map(|member| stage.get_points(member).unwrap_or_default())
                .collect::<Vec<_>>()
        })?;
        let target = fmn_library::VMobject::from_points(points);
        let brace = fmn_library::Brace::around(&target, direction).buff(buff);
        install_brace_tree(slf, factory, brace)
    }

    /// Native `LineBrace`: Atlas owns its arbitrary-angle geometry and this
    /// portal only installs the resulting retained family.
    fn _build_line_brace<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        start: [f64; 3],
        end: [f64; 3],
        direction: [f64; 3],
        buff: f64,
    ) -> PyResult<(Bound<'py, PyList>, usize)> {
        let brace = fmn_library::line_brace(start, end, direction).buff(buff);
        install_brace_tree(slf, factory, brace)
    }

    /// `ValueTracker` initialization: replace the detached nursery with a
    /// native tracker entry (`Stage::add_value_tracker` and kin) —
    /// state-real in both proxy states, and `copy_into` carries the
    /// tracker through scene adoption.
    fn _init_value_tracker(slf: &Bound<'_, Self>, kind: u8, value: f64, im: f64) -> PyResult<()> {
        let mut cell = slf.borrow_mut();
        if cell.engine.is_some() {
            return Err(PyRuntimeError::new_err(
                "a value tracker initializes before scene entry",
            ));
        }
        cell.nursery = Some(Nursery::value_tracker(kind, value, im));
        cell.initialized = true;
        Ok(())
    }

    /// The decoded scalar tracker value (Plain or Exponential).
    fn _tracker_value(slf: &Bound<'_, Self>) -> PyResult<f64> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| stage.tracker_value(mob))?
            .ok_or_else(|| StaleHandleError::new_err("no scalar value tracker behind this proxy"))
    }

    /// The complex tracker value as `(re, im)`.
    fn _tracker_complex_value(slf: &Bound<'_, Self>) -> PyResult<(f64, f64)> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| stage.tracker_complex_value(mob))?
            .ok_or_else(|| StaleHandleError::new_err("no complex value tracker behind this proxy"))
    }

    fn _set_tracker_value(slf: &Bound<'_, Self>, value: f64) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| stage.set_tracker_value(mob, value))?.map_err(stage_error)
    }

    fn _set_tracker_complex_value(slf: &Bound<'_, Self>, re: f64, im: f64) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| {
            stage.set_tracker_complex_value(mob, re, im)
        })?
        .map_err(stage_error)
    }

    fn _increment_tracker_value(slf: &Bound<'_, Self>, d_value: f64) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| {
            stage.increment_tracker_value(mob, d_value)
        })?
        .map_err(stage_error)
    }

    /// `Stage::set_z_index` for this entry alone; the bootstrap
    /// distributes over the family list in both proxy states.
    fn _set_z_index(slf: &Bound<'_, Self>, z_index: i32) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| {
            stage.set_z_index(mob, z_index, false);
        })
    }

    /// `space_ops.rotate_vector` over the ONE rotation implementation
    /// (fmn-geom's scipy-exact quaternion `rotation_matrix`, the same
    /// kernel `Stage::rotate` composes).
    #[staticmethod]
    fn _rotate_vector(vector: [f64; 3], angle: f64, axis: [f64; 3]) -> [f64; 3] {
        let matrix = fmn_library::rotation_matrix(angle, axis);
        [
            matrix[0][0] * vector[0] + matrix[0][1] * vector[1] + matrix[0][2] * vector[2],
            matrix[1][0] * vector[0] + matrix[1][1] * vector[1] + matrix[1][2] * vector[2],
            matrix[2][0] * vector[0] + matrix[2][1] * vector[1] + matrix[2][2] * vector[2],
        ]
    }

    /// Chisel's deterministic polar-angle kernel behind the familiar public
    /// utility function.
    #[staticmethod]
    fn _angle_of_vector(vector: [f64; 3]) -> f64 {
        fmn_library::angle_of_vector(vector)
    }

    /// Chisel's deterministic three-dimensional angle kernel. The Python
    /// surface retains the Reference's arbitrary-dimensional fallback.
    #[staticmethod]
    fn _angle_between_vectors(v1: [f64; 3], v2: [f64; 3]) -> f64 {
        fmn_library::angle_between_vectors(v1, v2)
    }

    /// Chisel's exact strict-crossing predicate for a line segment and a
    /// polygonal path. Only xy coordinates participate, matching the pinned
    /// Reference for both two- and three-dimensional inputs.
    #[staticmethod]
    fn _line_intersects_path(start: [f64; 3], end: [f64; 3], path: Vec<[f64; 3]>) -> bool {
        fmn_library::line_intersects_path(start, end, &path)
    }

    /// `Arc(start_angle, angle, radius, arc_center)` over the arc shelf.
    #[allow(clippy::too_many_arguments)]
    fn _build_arc<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        start_angle: f64,
        angle: f64,
        radius: f64,
        arc_center: [f64; 3],
        n_components: Option<usize>,
    ) -> PyResult<Bound<'py, PyList>> {
        let mut arc = fmn_library::Arc::new()
            .start_angle(start_angle)
            .angle(angle)
            .radius(radius)
            .arc_center(arc_center);
        if let Some(n) = n_components {
            arc = arc.n_components(n);
        }
        let built = arc.build().map_err(native_error)?;
        install_native_tree(slf, factory, built)
    }

    /// `ArcBetweenPoints(start, end, angle)` over the native true-arc shelf.
    fn _build_arc_between_points<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        start: [f64; 3],
        end: [f64; 3],
        angle: f64,
        n_components: Option<usize>,
    ) -> PyResult<Bound<'py, PyList>> {
        let mut arc = fmn_library::ArcBetweenPoints::new(start, end).angle(angle);
        if let Some(n) = n_components {
            arc = arc.n_components(n);
        }
        let built = arc.build().map_err(native_error)?;
        install_native_tree(slf, factory, built)
    }

    /// The curved-arrow pair over Atlas's native tip-attachment algebra.
    fn _build_curved_arrow<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        start: [f64; 3],
        end: [f64; 3],
        angle: f64,
        n_components: Option<usize>,
        double: bool,
    ) -> PyResult<Bound<'py, PyList>> {
        let style = fmn_library::Style::default();
        let mut arc = fmn_library::ArcBetweenPoints::new(start, end)
            .angle(angle)
            .style(style);
        if let Some(n) = n_components {
            arc = arc.n_components(n);
        }
        let shaft = arc.build().map_err(native_error)?;
        let built = fmn_library::tip::attach_tip(
            shaft,
            fmn_library::ArrowTip::new(),
            fmn_library::TipEnd::End,
        );
        let built = if double {
            fmn_library::tip::attach_tip(
                built,
                fmn_library::ArrowTip::new(),
                fmn_library::TipEnd::Start,
            )
        } else {
            built
        };
        install_native_tree(slf, factory, built)
    }

    /// `Circle(start_angle, radius, arc_center)` — the native circle
    /// builder, keeping its semantic shape tag.
    fn _build_circle<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        start_angle: f64,
        radius: f64,
        arc_center: [f64; 3],
    ) -> PyResult<Bound<'py, PyList>> {
        let built = fmn_library::Circle::new()
            .start_angle(start_angle)
            .radius(radius)
            .arc_center(arc_center)
            .build();
        install_native_tree(slf, factory, built)
    }

    /// `Ellipse(width, height, arc_center)` over the native stretched-circle
    /// builder. The builder deliberately demotes the circle shape hint.
    fn _build_ellipse<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        width: f64,
        height: f64,
        arc_center: [f64; 3],
        start_angle: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let built = fmn_library::Ellipse::new()
            .width(width)
            .height(height)
            .arc_center(arc_center)
            .start_angle(start_angle)
            .build();
        install_native_tree(slf, factory, built)
    }

    /// `AnnularSector` and `Sector` share Atlas's one concentric-arc
    /// implementation; `Sector` supplies `inner_radius = 0` in Python.
    #[allow(clippy::too_many_arguments)]
    fn _build_annular_sector<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        angle: f64,
        start_angle: f64,
        inner_radius: f64,
        outer_radius: f64,
        arc_center: [f64; 3],
    ) -> PyResult<Bound<'py, PyList>> {
        let built = fmn_library::AnnularSector::new()
            .angle(angle)
            .start_angle(start_angle)
            .inner_radius(inner_radius)
            .outer_radius(outer_radius)
            .arc_center(arc_center)
            .build()
            .map_err(native_error)?;
        install_native_tree(slf, factory, built)
    }

    /// `Annulus(inner_radius, outer_radius, center)` with a counter-wound
    /// inner contour for a real nonzero-winding hole.
    fn _build_annulus<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        inner_radius: f64,
        outer_radius: f64,
        center: [f64; 3],
    ) -> PyResult<Bound<'py, PyList>> {
        let built = fmn_library::Annulus::new()
            .inner_radius(inner_radius)
            .outer_radius(outer_radius)
            .center(center)
            .build();
        install_native_tree(slf, factory, built)
    }

    /// `Dot(point, radius)` — a filled disc with the Reference defaults.
    fn _build_dot<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        point: [f64; 3],
        radius: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let built = fmn_library::Dot::new().point(point).radius(radius).build();
        install_native_tree(slf, factory, built)
    }

    /// `DotCloud(points, color, opacity, radius, glow_factor,
    /// anti_alias_width)` over the pointcloud shelf — the DotCloud record
    /// schema (`point`/`radius`/`rgba`/`glow_factor`), not a VMobject.
    #[allow(clippy::too_many_arguments)]
    fn _build_dot_cloud<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        points: Vec<[f64; 3]>,
        color: Option<&Bound<'py, PyAny>>,
        opacity: f64,
        radius: f64,
        glow_factor: f64,
        anti_alias_width: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let mut cloud = fmn_library::DotCloud::new(points)
            .with_radius(radius)
            .with_glow_factor(glow_factor)
            .with_anti_alias_width(anti_alias_width);
        if let Some(color) = color {
            cloud = cloud.colored(srgb_from_py(color)?, opacity);
        } else if opacity != 1.0 {
            cloud = cloud.colored(
                fmn_core::color::Srgb::from_hex("#888888").expect("grey"),
                opacity,
            );
        }
        install_native_tree(slf, factory, cloud)
    }

    /// `ImageMobject(path, height)`: Python owns local path resolution, then
    /// Atlas/fmn-codec own format sniffing, bounded decode, quad geometry,
    /// and the immutable renderer resource. No decoded pixel copy returns to
    /// Python and no second image implementation exists in the portal.
    fn _build_image<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        payload: &Bound<'py, PyBytes>,
        height: f64,
        opacity: f64,
        z_index: i32,
    ) -> PyResult<Bound<'py, PyList>> {
        let image = fmn_library::ImageMobject::from_bytes(payload.as_bytes())
            .map_err(native_error)?
            .with_height(height)
            .with_opacity(opacity)
            .with_z_index(z_index);
        install_native_tree(slf, factory, image)
    }

    /// Sample the durable image attached to this entry using the Reference's
    /// axis-aligned family-box convention. Placement is baked first, so the
    /// result observes live shifts/scales exactly as ordinary point reads do.
    fn _image_point_to_rgb(slf: &Bound<'_, Self>, point: [f64; 3]) -> PyResult<Option<[f64; 3]>> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| -> PyResult<Option<[f64; 3]>> {
            stage.bake_placement(mob).map_err(stage_error)?;
            let entry = stage.get(mob).ok_or_else(|| {
                StaleHandleError::new_err("image mobject handle no longer resolves")
            })?;
            let image = entry.image_resource().ok_or_else(|| {
                PyRuntimeError::new_err("image mobject has no durable image resource")
            })?;
            let points = entry
                .buffer
                .read_column("point")
                .ok_or_else(|| PyRuntimeError::new_err("image mobject has no point field"))?;
            let mut min_x = f64::INFINITY;
            let mut max_x = f64::NEG_INFINITY;
            let mut min_y = f64::INFINITY;
            let mut max_y = f64::NEG_INFINITY;
            for row in points.as_chunks::<3>().0 {
                min_x = min_x.min(f64::from(row[0]));
                max_x = max_x.max(f64::from(row[0]));
                min_y = min_y.min(f64::from(row[1]));
                max_y = max_y.max(f64::from(row[1]));
            }
            let width = max_x - min_x;
            let height = max_y - min_y;
            if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
                return Ok(None);
            }
            let x_alpha = (point[0] - min_x) / width;
            let y_alpha = (max_y - point[1]) / height;
            if !(0.0..=1.0).contains(&x_alpha) || !(0.0..=1.0).contains(&y_alpha) {
                return Ok(None);
            }
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let pixel_x = (f64::from(image.width() - 1) * x_alpha) as usize;
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let pixel_y = (f64::from(image.height() - 1) * y_alpha) as usize;
            let offset = pixel_y
                .checked_mul(image.width() as usize)
                .and_then(|row| row.checked_add(pixel_x))
                .and_then(|pixel| pixel.checked_mul(4))
                .ok_or_else(|| PyOverflowError::new_err("image sample offset overflows usize"))?;
            let rgb = image.pixels().get(offset..offset + 3).ok_or_else(|| {
                PyRuntimeError::new_err("image sample falls outside the durable resource")
            })?;
            Ok(Some([
                f64::from(rgb[0]) / 255.0,
                f64::from(rgb[1]) / 255.0,
                f64::from(rgb[2]) / 255.0,
            ]))
        })?
    }

    /// Pixel dimensions of the same durable resource consumed by Lumen.
    fn _image_dimensions(slf: &Bound<'_, Self>) -> PyResult<(u32, u32)> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| -> PyResult<(u32, u32)> {
            let entry = stage.get(mob).ok_or_else(|| {
                StaleHandleError::new_err("image mobject handle no longer resolves")
            })?;
            let image = entry.image_resource().ok_or_else(|| {
                PyRuntimeError::new_err("image mobject has no durable image resource")
            })?;
            Ok((image.width(), image.height()))
        })?
    }

    /// `Prism(width, height, depth)` over the solids shelf: six sampled
    /// quads as an SGroup family.
    fn _build_prism<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        width: f64,
        height: f64,
        depth: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        install_native_tree(slf, factory, fmn_library::Prism::new(width, height, depth))
    }

    /// `Cube(side_length)` over the solids shelf.
    fn _build_cube<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        side_length: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        install_native_tree(slf, factory, fmn_library::Cube::new(side_length))
    }

    /// `VCube(side_length, ...)` over Atlas's six vectorized faces. Style
    /// is part of the native build so the first observable Python state is
    /// already the Reference's filled, unstroked cube.
    fn _build_vcube<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        side_length: f64,
        fill_color: &Bound<'_, PyAny>,
        fill_opacity: f64,
        stroke_width: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let cube = fmn_library::VCube::new(side_length)
            .fill_color(srgb_from_py(fill_color)?)
            .fill_opacity(fill_opacity)
            .stroke_width(stroke_width);
        install_native_tree(slf, factory, cube)
    }

    /// `VPrism(width, height, depth)` over Atlas's vectorized cube stretch.
    #[allow(clippy::too_many_arguments)]
    fn _build_vprism<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        width: f64,
        height: f64,
        depth: f64,
        fill_color: &Bound<'_, PyAny>,
        fill_opacity: f64,
        stroke_width: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let prism = fmn_library::VPrism::new(width, height, depth)
            .fill_color(srgb_from_py(fill_color)?)
            .fill_opacity(fill_opacity)
            .stroke_width(stroke_width);
        install_native_tree(slf, factory, prism)
    }

    /// The twelve native pentagons of the Reference `Dodecahedron`.
    #[allow(clippy::too_many_arguments)]
    fn _build_dodecahedron<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        fill_color: &Bound<'_, PyAny>,
        fill_opacity: f64,
        stroke_color: &Bound<'_, PyAny>,
        stroke_width: f64,
        shading: [f64; 3],
    ) -> PyResult<Bound<'py, PyList>> {
        let solid = fmn_library::Dodecahedron::new()
            .fill_color(srgb_from_py(fill_color)?)
            .fill_opacity(fill_opacity)
            .stroke_color(srgb_from_py(stroke_color)?)
            .stroke_width(stroke_width)
            .shading(shading);
        install_native_tree(slf, factory, solid)
    }

    /// `Prismify(vmobject, depth, direction)`: Atlas owns the straight-edge
    /// extrusion geometry. The bootstrap reapplies the source's live style
    /// arrays to the returned base/walls/top, matching the Reference's
    /// `copy`/`match_style` construction without a second point kernel.
    fn _build_prismify<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        source: &Bound<'_, BridgeMobject>,
        depth: f64,
        direction: [f64; 3],
    ) -> PyResult<Bound<'py, PyList>> {
        let points = with_stage(source, |stage, mob| {
            stage.get_points(mob).unwrap_or_default()
        })?;
        let source = fmn_library::VMobject::from_points(points);
        let prism = fmn_library::Prismify::new(source)
            .depth(depth)
            .direction(direction);
        install_native_tree(slf, factory, prism)
    }

    /// `Sphere(radius, ...)` over the solids shelf: the Reference's UV
    /// grid with radial true normals.
    #[allow(clippy::too_many_arguments)]
    fn _build_sphere<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        radius: f64,
        u_range: (f64, f64),
        v_range: (f64, f64),
        resolution: (usize, usize),
        true_normals: bool,
        clockwise: bool,
    ) -> PyResult<Bound<'py, PyList>> {
        let sphere = fmn_library::Sphere::new(radius)
            .u_range(u_range.0, u_range.1)
            .v_range(v_range.0, v_range.1)
            .resolution(resolution.0, resolution.1)
            .true_normals(true_normals)
            .clockwise(clockwise);
        install_native_tree(slf, factory, sphere.build())
    }

    /// `Cylinder(height, radius, axis, ...)` over the solids shelf: the
    /// Reference UV grid followed by its radius/depth/axis transform.
    #[allow(clippy::too_many_arguments)]
    fn _build_cylinder<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        height: f64,
        radius: f64,
        axis: [f64; 3],
        u_range: (f64, f64),
        v_range: (f64, f64),
        resolution: (usize, usize),
        preferred_creation_axis: usize,
        epsilon: f64,
        normal_nudge: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let cylinder = fmn_library::Cylinder::new(height, radius)
            .axis(axis)
            .u_range(u_range.0, u_range.1)
            .v_range(v_range.0, v_range.1)
            .resolution(resolution.0, resolution.1)
            .preferred_creation_axis(preferred_creation_axis)
            .epsilon(epsilon)
            .normal_nudge(normal_nudge);
        install_native_tree(slf, factory, cylinder.build())
    }

    /// `Torus(r1, r2, ...)` over Atlas's native UV-grid sampler.
    #[allow(clippy::too_many_arguments)]
    fn _build_torus<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        r1: f64,
        r2: f64,
        u_range: (f64, f64),
        v_range: (f64, f64),
        resolution: (usize, usize),
        preferred_creation_axis: usize,
        epsilon: f64,
        normal_nudge: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let torus = fmn_library::Torus::new(r1, r2)
            .u_range(u_range.0, u_range.1)
            .v_range(v_range.0, v_range.1)
            .resolution(resolution.0, resolution.1)
            .preferred_creation_axis(preferred_creation_axis)
            .epsilon(epsilon)
            .normal_nudge(normal_nudge);
        install_native_tree(slf, factory, torus.build())
    }

    /// `Cone(height, radius, axis, ...)` over Atlas's native tapered
    /// cylinder, preserving the Reference's non-centered `v = (0, 1)` grid.
    #[allow(clippy::too_many_arguments)]
    fn _build_cone<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        height: f64,
        radius: f64,
        axis: [f64; 3],
        u_range: (f64, f64),
        v_range: (f64, f64),
        resolution: (usize, usize),
        preferred_creation_axis: usize,
        epsilon: f64,
        normal_nudge: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let cone = fmn_library::Cone::new(height, radius)
            .axis(axis)
            .u_range(u_range.0, u_range.1)
            .v_range(v_range.0, v_range.1)
            .resolution(resolution.0, resolution.1)
            .preferred_creation_axis(preferred_creation_axis)
            .epsilon(epsilon)
            .normal_nudge(normal_nudge);
        install_native_tree(slf, factory, cone.build())
    }

    /// `Line3D(start, end, width, ...)` over Atlas's thin-cylinder builder.
    #[allow(clippy::too_many_arguments)]
    fn _build_line3d<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        start: [f64; 3],
        end: [f64; 3],
        width: f64,
        u_range: (f64, f64),
        v_range: (f64, f64),
        resolution: (usize, usize),
        preferred_creation_axis: usize,
        epsilon: f64,
        normal_nudge: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let line = fmn_library::Line3D::new(start, end)
            .width(width)
            .u_range(u_range.0, u_range.1)
            .v_range(v_range.0, v_range.1)
            .resolution(resolution.0, resolution.1)
            .preferred_creation_axis(preferred_creation_axis)
            .epsilon(epsilon)
            .normal_nudge(normal_nudge);
        install_native_tree(slf, factory, line.build())
    }

    /// `Disk3D(radius, ...)` over Atlas's native polar surface grid.
    #[allow(clippy::too_many_arguments)]
    fn _build_disk3d<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        radius: f64,
        u_range: (f64, f64),
        v_range: (f64, f64),
        resolution: (usize, usize),
        preferred_creation_axis: usize,
        epsilon: f64,
        normal_nudge: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let disk = fmn_library::Disk3D::new(radius)
            .u_range(u_range.0, u_range.1)
            .v_range(v_range.0, v_range.1)
            .resolution(resolution.0, resolution.1)
            .preferred_creation_axis(preferred_creation_axis)
            .epsilon(epsilon)
            .normal_nudge(normal_nudge);
        install_native_tree(slf, factory, disk.build())
    }

    /// `Square3D(side_length, ...)` over Atlas's native planar surface grid.
    #[allow(clippy::too_many_arguments)]
    fn _build_square3d<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        side_length: f64,
        u_range: (f64, f64),
        v_range: (f64, f64),
        resolution: (usize, usize),
        preferred_creation_axis: usize,
        epsilon: f64,
        normal_nudge: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let square = fmn_library::Square3D::new(side_length)
            .u_range(u_range.0, u_range.1)
            .v_range(v_range.0, v_range.1)
            .resolution(resolution.0, resolution.1)
            .preferred_creation_axis(preferred_creation_axis)
            .epsilon(epsilon)
            .normal_nudge(normal_nudge);
        install_native_tree(slf, factory, square.build())
    }

    /// `Cylinder.uv_func` through the exact object-space parameterization
    /// sampled by the native builder.
    #[staticmethod]
    fn _cylinder_uv(u: f64, v: f64) -> [f64; 3] {
        fmn_library::Cylinder::uv_func(u, v)
    }

    /// `Torus.uv_func` through the exact native function sampled by build.
    #[staticmethod]
    fn _torus_uv(r1: f64, r2: f64, u: f64, v: f64) -> [f64; 3] {
        fmn_library::Torus::new(r1, r2).uv_func(u, v)
    }

    /// `Cone.uv_func` through the exact native function sampled by build.
    #[staticmethod]
    fn _cone_uv(u: f64, v: f64) -> [f64; 3] {
        fmn_library::Cone::uv_func(u, v)
    }

    /// `Disk3D.uv_func` through the exact native function sampled by build.
    #[staticmethod]
    fn _disk3d_uv(u: f64, v: f64) -> [f64; 3] {
        fmn_library::Disk3D::uv_func(u, v)
    }

    /// `Square3D.uv_func` through the exact native function sampled by build.
    #[staticmethod]
    fn _square3d_uv(u: f64, v: f64) -> [f64; 3] {
        fmn_library::Square3D::uv_func(u, v)
    }

    /// `Sphere.uv_func` through the exact function used by the native
    /// surface builder (including its fmn-dmath transcendental path).
    #[staticmethod]
    fn _sphere_uv(radius: f64, clockwise: bool, u: f64, v: f64) -> [f64; 3] {
        fmn_library::Sphere::new(radius)
            .clockwise(clockwise)
            .uv_func(u, v)
    }

    /// `ParametricSurface(uv_func, ...)`: the native sampler over a
    /// Python callable. The callable runs during construction only (no
    /// engine borrow is held); its first error aborts the build.
    fn _build_parametric_surface<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        uv_func: &Bound<'py, PyAny>,
        u_range: (f64, f64),
        v_range: (f64, f64),
        resolution: (usize, usize),
    ) -> PyResult<Bound<'py, PyList>> {
        let func = uv_func.clone().unbind();
        let error_cell: Rc<RefCell<Option<PyErr>>> = Rc::new(RefCell::new(None));
        let closure_errors = Rc::clone(&error_cell);
        let surface = fmn_library::ParametricSurface::new(move |u, v| {
            Python::attach(|py| {
                let sample = func
                    .bind(py)
                    .call1((u, v))
                    .and_then(|value| value.extract::<Vec<f64>>());
                match sample {
                    Ok(point) if point.len() >= 3 => [point[0], point[1], point[2]],
                    Ok(_) => {
                        if closure_errors.borrow().is_none() {
                            *closure_errors.borrow_mut() = Some(PyValueError::new_err(
                                "uv_func must return three components",
                            ));
                        }
                        [0.0; 3]
                    }
                    Err(error) => {
                        if closure_errors.borrow().is_none() {
                            *closure_errors.borrow_mut() = Some(error);
                        }
                        [0.0; 3]
                    }
                }
            })
        })
        .u_range(u_range.0, u_range.1)
        .v_range(v_range.0, v_range.1)
        .resolution(resolution.0, resolution.1)
        .build();
        if let Some(error) = error_cell.borrow_mut().take() {
            return Err(error);
        }
        install_native_tree(slf, factory, surface)
    }

    /// `ParametricCurve(t_func, ...)`: Atlas owns the bounded range
    /// sampling and Chisel owns the shared-anchor smoothing. The Python
    /// callback is evaluated only while constructing the detached value;
    /// its first exception is preserved verbatim.
    fn _build_parametric_curve<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        t_func: &Bound<'py, PyAny>,
        t_range: &Bound<'py, PyAny>,
        epsilon: f64,
        discontinuities: Vec<f64>,
        use_smoothing: bool,
    ) -> PyResult<Bound<'py, PyList>> {
        let func = t_func.clone().unbind();
        let error_cell: Rc<RefCell<Option<PyErr>>> = Rc::new(RefCell::new(None));
        let closure_errors = Rc::clone(&error_cell);
        let curve = fmn_library::ParametricCurve::new(move |t| {
            Python::attach(|py| {
                let sample = func
                    .bind(py)
                    .call1((t,))
                    .and_then(|value| value.extract::<Vec<f64>>());
                match sample {
                    Ok(point) if point.len() >= 3 => [point[0], point[1], point[2]],
                    Ok(_) => {
                        if closure_errors.borrow().is_none() {
                            *closure_errors.borrow_mut() =
                                Some(PyValueError::new_err("t_func must return three components"));
                        }
                        [0.0; 3]
                    }
                    Err(error) => {
                        if closure_errors.borrow().is_none() {
                            *closure_errors.borrow_mut() = Some(error);
                        }
                        [0.0; 3]
                    }
                }
            })
        })
        .t_range(range3(t_range)?)
        .epsilon(epsilon)
        .discontinuities(discontinuities)
        .use_smoothing(use_smoothing)
        .build();
        if let Some(error) = error_cell.borrow_mut().take() {
            return Err(error);
        }
        install_native_tree(slf, factory, curve.map_err(native_error)?)
    }

    /// `FunctionGraph(function, ...)`: Atlas owns the bounded scalar graph
    /// sampling while Python retains the caller-visible function metadata.
    /// The callback is construction-only and its first exception crosses the
    /// portal unchanged.
    fn _build_function_graph<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        function: &Bound<'py, PyAny>,
        x_range: &Bound<'py, PyAny>,
        epsilon: f64,
        discontinuities: Vec<f64>,
        use_smoothing: bool,
    ) -> PyResult<Bound<'py, PyList>> {
        let function = function.clone().unbind();
        let error_cell: Rc<RefCell<Option<PyErr>>> = Rc::new(RefCell::new(None));
        let closure_errors = Rc::clone(&error_cell);
        let graph = fmn_library::FunctionGraph::new(move |x| {
            Python::attach(|py| match function.bind(py).call1((x,)) {
                Ok(value) => match value.extract::<f64>() {
                    Ok(value) => value,
                    Err(error) => {
                        if closure_errors.borrow().is_none() {
                            *closure_errors.borrow_mut() = Some(error);
                        }
                        f64::NAN
                    }
                },
                Err(error) => {
                    if closure_errors.borrow().is_none() {
                        *closure_errors.borrow_mut() = Some(error);
                    }
                    f64::NAN
                }
            })
        })
        .x_range(range3(x_range)?)
        .epsilon(epsilon)
        .discontinuities(discontinuities)
        .use_smoothing(use_smoothing)
        .build();
        if let Some(error) = error_cell.borrow_mut().take() {
            return Err(error);
        }
        install_native_tree(slf, factory, graph.map_err(native_error)?)
    }

    /// `ImplicitFunction(func, ...)`: Chisel extracts the bounded zero set
    /// and Atlas materializes its shared-anchor path. Python callback errors
    /// remain the authority even when later native samples observe the NaN
    /// sentinel used to finish the bounded traversal.
    #[allow(clippy::too_many_arguments)]
    fn _build_implicit_function<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        func: &Bound<'py, PyAny>,
        x_range: (f64, f64),
        y_range: (f64, f64),
        min_depth: u32,
        max_quads: usize,
        use_smoothing: bool,
    ) -> PyResult<Bound<'py, PyList>> {
        let func = func.clone().unbind();
        let error_cell: Rc<RefCell<Option<PyErr>>> = Rc::new(RefCell::new(None));
        let closure_errors = Rc::clone(&error_cell);
        let implicit = fmn_library::ImplicitFunction::new(move |x, y| {
            Python::attach(|py| match func.bind(py).call1((x, y)) {
                Ok(value) => match value.extract::<f64>() {
                    Ok(value) => value,
                    Err(error) => {
                        if closure_errors.borrow().is_none() {
                            *closure_errors.borrow_mut() = Some(error);
                        }
                        f64::NAN
                    }
                },
                Err(error) => {
                    if closure_errors.borrow().is_none() {
                        *closure_errors.borrow_mut() = Some(error);
                    }
                    f64::NAN
                }
            })
        })
        .x_range([x_range.0, x_range.1])
        .y_range([y_range.0, y_range.1])
        .min_depth(min_depth)
        .max_quads(max_quads)
        .use_smoothing(use_smoothing)
        .build();
        if let Some(error) = error_cell.borrow_mut().take() {
            return Err(error);
        }
        install_native_tree(slf, factory, implicit.map_err(native_error)?)
    }

    /// Build the native VectorField geometry from already-evaluated portal
    /// samples. Python owns callback dispatch so it can release the Scene
    /// borrow; Atlas/Lumen still own arrow geometry, tanh length mapping,
    /// joint policy, and per-record stroke columns.
    #[allow(clippy::too_many_arguments)]
    fn _build_vector_field_samples<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        sample_points: Vec<[f64; 3]>,
        out_vects: Vec<[f64; 3]>,
        output_norms: Vec<f64>,
        max_displayed_vect_len: f64,
        stroke_width: f64,
        stroke_opacity: f64,
        tip_width_ratio: f64,
        tip_len_to_width: f64,
        flat_stroke: bool,
        use_default_color_map: bool,
        color: Option<&Bound<'py, PyAny>>,
        magnitude_range: Option<(f64, f64)>,
    ) -> PyResult<Bound<'py, PyList>> {
        let n = sample_points.len();
        if n < 2 {
            return Err(PyValueError::new_err(
                "VectorField needs at least two sample points",
            ));
        }
        let sample_budget = fmn_library::SamplingBudget::DEFAULT.max_samples();
        if n > sample_budget {
            return Err(PyValueError::new_err(format!(
                "VectorField sample grid exceeds the {sample_budget}-point resource budget"
            )));
        }
        if out_vects.len() != n || output_norms.len() != n {
            return Err(PyValueError::new_err(format!(
                "VectorField callback returned {} vectors and {} norms for {n} samples",
                out_vects.len(),
                output_norms.len()
            )));
        }
        if max_displayed_vect_len.is_nan() || max_displayed_vect_len <= 0.0 {
            return Err(PyValueError::new_err(
                "VectorField max displayed length must be positive and non-NaN",
            ));
        }
        if [
            stroke_width,
            stroke_opacity,
            tip_width_ratio,
            tip_len_to_width,
        ]
        .iter()
        .any(|value| !value.is_finite())
        {
            return Err(PyValueError::new_err(
                "VectorField style controls must be finite",
            ));
        }

        let mut style = fmn_library::VectorFieldStyle {
            stroke_width,
            stroke_opacity,
            tip_width_ratio,
            tip_len_to_width,
            flat_stroke,
            magnitude_range: Some(
                magnitude_range
                    .unwrap_or_else(|| (0.0, output_norms.iter().copied().fold(0.0, f64::max))),
            ),
            ..fmn_library::VectorFieldStyle::default()
        };
        if !use_default_color_map {
            style.color_map = None;
        }
        if let Some(color) = color {
            style.color = Some(srgb_from_py(color)?);
            style.color_map = None;
        }
        let geometry = fmn_library::fields::vector_field_geometry(
            &style,
            &sample_points,
            &out_vects,
            &output_norms,
            max_displayed_vect_len,
        );
        let stroke_color = style
            .color
            .unwrap_or(fmn_core::constants::DEFAULT_MOBJECT_COLOR);
        let mut visual_style = fmn_library::Style::default().stroke(
            stroke_color,
            style.stroke_width,
            style.stroke_opacity,
        );
        visual_style.fill_opacity = 0.0;
        let vmob = fmn_library::VMobject::from_points(geometry.points)
            .with_style(visual_style)
            .with_joint_type(fmn_mobject::JointType::NoJoint)
            .with_flat_stroke(style.flat_stroke)
            .with_stroke_profile(geometry.stroke_widths);
        let mut tree = fmn_mobject::Mobject::from(vmob);
        if !geometry.stroke_rgba.is_empty() {
            #[allow(clippy::cast_possible_truncation)]
            let rgba: Vec<f32> = geometry
                .stroke_rgba
                .iter()
                .flat_map(|row| row.iter().map(|value| *value as f32))
                .collect();
            tree.buffer.write_range("stroke_rgba", 0, &rgba);
        }
        install_native_tree(slf, factory, tree)
    }

    /// The finished Atlas implementation of the Reference's older
    /// `VectorField.get_sample_points` rectilinear helper.
    #[staticmethod]
    #[allow(clippy::too_many_arguments)]
    fn _vector_field_grid_sample_points(
        center: [f64; 3],
        width: f64,
        height: f64,
        depth: f64,
        x_density: f64,
        y_density: f64,
        z_density: f64,
    ) -> PyResult<Vec<[f64; 3]>> {
        if [x_density, y_density, z_density]
            .iter()
            .any(|density| !density.is_finite() || *density <= 0.0)
        {
            return Err(PyValueError::new_err(
                "VectorField sample-point densities must be positive and finite",
            ));
        }
        let points = fmn_library::fields::grid_sample_points(
            center,
            [width / 2.0, height / 2.0, depth / 2.0],
            [x_density, y_density, z_density],
            fmn_library::SamplingBudget::DEFAULT,
        );
        if points.is_empty() && width >= 0.0 && height >= 0.0 && depth >= 0.0 {
            return Err(PyValueError::new_err(format!(
                "VectorField sample-point grid exceeds the {}-point resource budget",
                fmn_library::SamplingBudget::DEFAULT.max_samples()
            )));
        }
        Ok(points)
    }

    /// The native 3b1b colormap interpolation used by the field helpers.
    #[staticmethod]
    fn _vector_field_gradient(min_value: f64, max_value: f64, values: Vec<f64>) -> Vec<[f64; 3]> {
        fmn_library::fields::colormap_gradient(
            &fmn_core::constants::COLORMAP_3B1B,
            min_value,
            max_value,
            &values,
        )
        .into_iter()
        .map(|color| [color.r, color.g, color.b])
        .collect()
    }

    /// `SurfaceMesh(uv_surface, ...)` — the rebuild oracle: the source
    /// surface reconstructs from its stored solid params and the native
    /// wireframe samples it; the bootstrap re-seats the mesh onto the
    /// source's current center/scale afterwards.
    #[allow(clippy::too_many_arguments)]
    fn _build_surface_mesh<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        source_kind: &str,
        source_radius: f64,
        resolution: (usize, usize),
        normal_nudge: f64,
        stroke_width: f64,
        stroke_color: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyList>> {
        let surface = match source_kind {
            "sphere" => fmn_library::Sphere::new(source_radius).build(),
            other => {
                return Err(PyValueError::new_err(format!(
                    "SurfaceMesh over `{other}` awaits its native rebuild \
                     path; spheres are native"
                )));
            }
        };
        let mut mesh = fmn_library::SurfaceMesh::new(surface)
            .resolution(resolution.0, resolution.1)
            .normal_nudge(normal_nudge)
            .stroke_width(stroke_width);
        if let Some(color) = stroke_color {
            mesh = mesh.stroke_color(srgb_from_py(color)?);
        }
        install_native_tree(slf, factory, mesh.build())
    }

    /// `DecimalNumber(number, ...)` over the numbers shelf (the de-TeX'd
    /// native builder with glyph-recycling updates).
    #[allow(clippy::too_many_arguments)]
    fn _build_decimal_number<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        number: f64,
        num_decimal_places: usize,
        min_total_width: usize,
        include_sign: bool,
        group_with_commas: bool,
        digit_buff_per_font_unit: f64,
        show_ellipsis: bool,
        unit: Option<String>,
        include_background_rectangle: bool,
        edge_to_fix: [f64; 3],
        font_size: f64,
        color: Option<&Bound<'py, PyAny>>,
        stroke_width: f64,
        fill_opacity: f64,
        fill_border_width: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let mut decimal = fmn_library::DecimalNumber::new(number)
            .num_decimal_places(num_decimal_places)
            .min_total_width(min_total_width)
            .include_sign(include_sign)
            .group_with_commas(group_with_commas)
            .digit_buff_per_font_unit(digit_buff_per_font_unit)
            .show_ellipsis(show_ellipsis)
            .include_background_rectangle(include_background_rectangle)
            .edge_to_fix(edge_to_fix)
            .font_size(font_size)
            .stroke_width(stroke_width)
            .fill_opacity(fill_opacity)
            .fill_border_width(fill_border_width);
        if let Some(unit) = &unit {
            decimal = decimal.unit(unit);
        }
        if let Some(color) = color {
            decimal = decimal.color(srgb_from_py(color)?);
        }
        let built = with_font_book(|book| decimal.build(book).map_err(native_error))?;
        install_native_tree(slf, factory, built.into_vmob())
    }

    /// `Elbow(width, angle)` over Atlas's native corner-mark builder.
    fn _build_elbow<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        width: f64,
        angle: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let built = fmn_library::Elbow::new().width(width).angle(angle).build();
        install_native_tree(slf, factory, built)
    }

    /// `Line(start, end, buff, path_arc)` over the line shelf.
    fn _build_line<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        start: [f64; 3],
        end: [f64; 3],
        buff: f64,
        path_arc: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let built = fmn_library::line::Line::new(start, end)
            .buff(buff)
            .path_arc(path_arc)
            .build()
            .map_err(native_error)?;
        install_native_tree(slf, factory, built)
    }

    /// Rebuild a scene-bound Line without changing its arena or Python identity.
    /// Atlas computes the complete point run first, so invalid arc parameters
    /// cannot partially mutate the live Marionette entry.
    fn _rebuild_line(
        slf: &Bound<'_, Self>,
        start: [f64; 3],
        end: [f64; 3],
        buff: f64,
        path_arc: f64,
    ) -> PyResult<()> {
        let built = fmn_library::line::Line::new(start, end)
            .buff(buff)
            .path_arc(path_arc)
            .build()
            .map_err(native_error)?;
        let points = built.points().to_vec();
        let shape = built.shape();
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| {
            stage.set_points(mob, &points)?;
            stage.set_shape(mob, shape);
            Ok(())
        })?
        .map_err(stage_error)
    }

    /// `DashedLine(start, end, dash_length, positive_space_ratio)`.
    #[allow(clippy::too_many_arguments)]
    fn _build_dashed_line<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        start: [f64; 3],
        end: [f64; 3],
        dash_length: f64,
        positive_space_ratio: f64,
        path_arc: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let built = fmn_library::line::DashedLine::new(start, end)
            .dash_length(dash_length)
            .positive_space_ratio(positive_space_ratio)
            .path_arc(path_arc)
            .build()
            .map_err(native_error)?;
        install_native_tree(slf, factory, built)
    }

    /// The bounded native dash-count rule over the line's current virtual
    /// endpoints. Atlas measures curved lines by true arc length (BN-03), so
    /// this stays in lockstep with `_build_dashed_line` rather than
    /// re-deriving its arithmetic in the Python skin.
    fn _calculate_dashed_line_num_dashes(
        _slf: &Bound<'_, Self>,
        start: [f64; 3],
        end: [f64; 3],
        dash_length: f64,
        positive_space_ratio: f64,
        path_arc: f64,
    ) -> PyResult<usize> {
        crossing::record(CrossingClass::Other);
        fmn_library::line::DashedLine::new(start, end)
            .dash_length(dash_length)
            .positive_space_ratio(positive_space_ratio)
            .path_arc(path_arc)
            .num_dashes()
            .map_err(native_error)
    }

    /// `TangentLine(vmob, alpha, length, d_alpha)` over Atlas's BN-03
    /// true-arclength tangent construction. The source is materialized from
    /// its live Marionette records in either detached or scene-bound state.
    fn _build_tangent_line<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        source: &Bound<'_, BridgeMobject>,
        alpha: f64,
        length: f64,
        d_alpha: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let points = with_stage(source, |stage, mob| {
            stage.get_points(mob).unwrap_or_default()
        })?;
        let source = fmn_library::VMobject::from_points(points);
        let built = fmn_library::line::tangent_line(
            &source,
            alpha,
            length,
            d_alpha,
            fmn_library::Style::default(),
        );
        install_native_tree(slf, factory, built)
    }

    /// `StrokeArrow(start, end, ...)` over Atlas's single-path terminal
    /// stroke taper. Every public ratio reaches the native builder; the
    /// Python skin owns only Reference object lifecycle and record dispatch.
    #[allow(clippy::too_many_arguments)]
    fn _build_stroke_arrow<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        start: [f64; 3],
        end: [f64; 3],
        stroke_width: f64,
        buff: f64,
        path_arc: f64,
        tip_width_ratio: f64,
        tip_len_to_width: f64,
        max_tip_length_to_length_ratio: f64,
        max_width_to_length_ratio: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let built = fmn_library::line::StrokeArrow::new(start, end)
            .buff(buff)
            .path_arc(path_arc)
            .tip_width_ratio(tip_width_ratio)
            .tip_len_to_width(tip_len_to_width)
            .max_tip_length_to_length_ratio(max_tip_length_to_length_ratio)
            .max_width_to_length_ratio(max_width_to_length_ratio)
            .style(fmn_library::Style::default().stroke(
                fmn_core::constants::DEFAULT_LIGHT_COLOR,
                stroke_width,
                1.0,
            ))
            .build()
            .map_err(native_error)?;
        install_native_tree(slf, factory, built)
    }

    /// Rebuild a StrokeArrow in place while retaining the root proxy and its
    /// arena identity. Stage preserves the other record columns across the
    /// resize; the native stroke profile then replaces the width column.
    #[allow(clippy::too_many_arguments)]
    fn _rebuild_stroke_arrow(
        slf: &Bound<'_, Self>,
        start: [f64; 3],
        end: [f64; 3],
        stroke_width: f64,
        buff: f64,
        path_arc: f64,
        tip_width_ratio: f64,
        tip_len_to_width: f64,
        max_tip_length_to_length_ratio: f64,
        max_width_to_length_ratio: f64,
    ) -> PyResult<()> {
        let built = fmn_library::line::StrokeArrow::new(start, end)
            .buff(buff)
            .path_arc(path_arc)
            .tip_width_ratio(tip_width_ratio)
            .tip_len_to_width(tip_len_to_width)
            .max_tip_length_to_length_ratio(max_tip_length_to_length_ratio)
            .max_width_to_length_ratio(max_width_to_length_ratio)
            .style(fmn_library::Style::default().stroke(
                fmn_core::constants::DEFAULT_LIGHT_COLOR,
                stroke_width,
                1.0,
            ))
            .build()
            .map_err(native_error)?;
        let points = built.points().to_vec();
        let shape = built.shape();
        #[allow(clippy::cast_possible_truncation)]
        let widths: Vec<f32> = built
            .stroke_profile()
            .unwrap_or_default()
            .iter()
            .map(|width| *width as f32)
            .collect();
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| {
            stage.set_points(mob, &points)?;
            stage.set_shape(mob, shape);
            let entry = stage.get_mut(mob).ok_or(StageError::StaleHandle)?;
            entry.buffer.write_range("stroke_width", 0, &widths);
            Ok(())
        })?
        .map_err(stage_error)
    }

    /// `Arrow(start, end, ...)`: one filled path with the native tip
    /// proportions and caller-selected Reference ratio caps.
    #[allow(clippy::too_many_arguments)]
    fn _build_arrow<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        start: [f64; 3],
        end: [f64; 3],
        buff: f64,
        path_arc: f64,
        thickness: f64,
        tip_width_ratio: f64,
        tip_angle: f64,
        max_tip_length_to_length_ratio: f64,
        max_width_to_length_ratio: f64,
    ) -> PyResult<(Bound<'py, PyList>, usize)> {
        let (built, tip_index) = fmn_library::line::Arrow::new(start, end)
            .buff(buff)
            .path_arc(path_arc)
            .thickness(thickness)
            .tip_width_ratio(tip_width_ratio)
            .tip_angle(tip_angle)
            .max_tip_length_to_length_ratio(max_tip_length_to_length_ratio)
            .max_width_to_length_ratio(max_width_to_length_ratio)
            .build_with_tip_index()
            .map_err(native_error)?;
        Ok((install_native_tree(slf, factory, built)?, tip_index))
    }

    /// Rebuild a scene-bound filled Arrow at new endpoints without changing
    /// its arena identity. Atlas owns the outline/tip proportions; Marionette
    /// writes the new world-space run and resets any old affine placement.
    #[allow(clippy::too_many_arguments)]
    fn _rebuild_arrow(
        slf: &Bound<'_, Self>,
        start: [f64; 3],
        end: [f64; 3],
        buff: f64,
        path_arc: f64,
        thickness: f64,
        tip_width_ratio: f64,
        tip_angle: f64,
        max_tip_length_to_length_ratio: f64,
        max_width_to_length_ratio: f64,
    ) -> PyResult<usize> {
        let (built, tip_index) = fmn_library::line::Arrow::new(start, end)
            .buff(buff)
            .path_arc(path_arc)
            .thickness(thickness)
            .tip_width_ratio(tip_width_ratio)
            .tip_angle(tip_angle)
            .max_tip_length_to_length_ratio(max_tip_length_to_length_ratio)
            .max_width_to_length_ratio(max_width_to_length_ratio)
            .build_with_tip_index()
            .map_err(native_error)?;
        let points = built.points().to_vec();
        let shape = built.shape();
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| {
            stage.set_points(mob, &points)?;
            stage.set_shape(mob, shape);
            Ok(())
        })?
        .map_err(stage_error)?;
        Ok(tip_index)
    }

    /// `Arrow.get_key_dimensions` through Atlas's single ratio-cap formula.
    #[staticmethod]
    #[allow(clippy::too_many_arguments)]
    fn _arrow_key_dimensions(
        length: f64,
        thickness: f64,
        tip_width_ratio: f64,
        tip_angle: f64,
        max_tip_length_to_length_ratio: f64,
        max_width_to_length_ratio: f64,
    ) -> (f64, f64, f64) {
        fmn_library::line::Arrow::new([0.0; 3], [1.0, 0.0, 0.0])
            .thickness(thickness)
            .tip_width_ratio(tip_width_ratio)
            .tip_angle(tip_angle)
            .max_tip_length_to_length_ratio(max_tip_length_to_length_ratio)
            .max_width_to_length_ratio(max_width_to_length_ratio)
            .key_dimensions(length)
    }

    /// `NumberLine(x_range, **config)` over the coords shelf.
    fn _build_number_line<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        x_range: &Bound<'py, PyAny>,
        config: &Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyList>> {
        let line = number_line_from_config(range3(x_range)?, config)?;
        let built = with_font_book(|book| line.build_numbered(book).map_err(native_error))?;
        install_native_tree(slf, factory, built.into_vmob())
    }

    /// `Axes(...)` over the coords shelf; children are `[x_axis, y_axis]`.
    #[allow(clippy::too_many_arguments)]
    fn _build_axes<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        x_range: &Bound<'py, PyAny>,
        y_range: &Bound<'py, PyAny>,
        axis_config: Option<&Bound<'py, PyDict>>,
        x_axis_config: Option<&Bound<'py, PyDict>>,
        y_axis_config: Option<&Bound<'py, PyDict>>,
        height: Option<f64>,
        width: Option<f64>,
        unit_size: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let axes = axes_builder(
            range3(x_range)?,
            range3(y_range)?,
            axis_config,
            x_axis_config,
            y_axis_config,
            height,
            width,
            unit_size,
        )?;
        let built = with_font_book(|book| axes.build(book).map_err(native_error))?;
        install_native_tree(slf, factory, built.into_vmob())
    }

    /// `ThreeDAxes(...)`; children are `[x_axis, y_axis, z_axis]`.
    #[allow(clippy::too_many_arguments)]
    fn _build_three_d_axes<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        x_range: &Bound<'py, PyAny>,
        y_range: &Bound<'py, PyAny>,
        z_range: &Bound<'py, PyAny>,
        axis_config: Option<&Bound<'py, PyDict>>,
        x_axis_config: Option<&Bound<'py, PyDict>>,
        y_axis_config: Option<&Bound<'py, PyDict>>,
        z_axis_config: Option<&Bound<'py, PyDict>>,
        height: Option<f64>,
        width: Option<f64>,
        depth: Option<f64>,
        unit_size: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let mut axes = fmn_library::ThreeDAxes::new()
            .x_range(range3(x_range)?)
            .y_range(range3(y_range)?)
            .z_range(range3(z_range)?)
            .axis_config(axis_config_from(axis_config)?)
            .x_axis_config(axis_config_from(x_axis_config)?)
            .y_axis_config(axis_config_from(y_axis_config)?)
            .z_axis_config(axis_config_from(z_axis_config)?)
            .unit_size(unit_size);
        if let Some(height) = height {
            axes = axes.height(height);
        }
        if let Some(width) = width {
            axes = axes.width(width);
        }
        if let Some(depth) = depth {
            axes = axes.depth(depth);
        }
        let built = with_font_book(|book| axes.build(book).map_err(native_error))?;
        install_native_tree(slf, factory, built.into_vmob())
    }

    /// `NumberPlane(...)`; children are
    /// `[faded_lines, background_lines, x_axis, y_axis]`.
    #[allow(clippy::too_many_arguments)]
    fn _build_number_plane<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        x_range: &Bound<'py, PyAny>,
        y_range: &Bound<'py, PyAny>,
        axis_config: Option<&Bound<'py, PyDict>>,
        x_axis_config: Option<&Bound<'py, PyDict>>,
        y_axis_config: Option<&Bound<'py, PyDict>>,
        background_line_style: Option<&Bound<'py, PyDict>>,
        faded_line_style: Option<&Bound<'py, PyDict>>,
        faded_line_ratio: usize,
        height: Option<f64>,
        width: Option<f64>,
        unit_size: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let mut plane = fmn_library::NumberPlane::new()
            .x_range(range3(x_range)?)
            .y_range(range3(y_range)?)
            .axis_config(axis_config_from(axis_config)?)
            .x_axis_config(axis_config_from(x_axis_config)?)
            .y_axis_config(axis_config_from(y_axis_config)?)
            .background_line_style(line_family_style_from(background_line_style)?)
            .faded_line_style(faded_line_style_from(faded_line_style)?)
            .faded_line_ratio(faded_line_ratio)
            .unit_size(unit_size);
        if let Some(height) = height {
            plane = plane.height(height);
        }
        if let Some(width) = width {
            plane = plane.width(width);
        }
        let built = with_font_book(|book| plane.build(book).map_err(native_error))?;
        install_native_tree(slf, factory, built.into_vmob())
    }

    /// `ComplexPlane(...)`; same family shape as `NumberPlane`.
    #[allow(clippy::too_many_arguments)]
    fn _build_complex_plane<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        x_range: &Bound<'py, PyAny>,
        y_range: &Bound<'py, PyAny>,
        axis_config: Option<&Bound<'py, PyDict>>,
        x_axis_config: Option<&Bound<'py, PyDict>>,
        y_axis_config: Option<&Bound<'py, PyDict>>,
        background_line_style: Option<&Bound<'py, PyDict>>,
        faded_line_style: Option<&Bound<'py, PyDict>>,
        faded_line_ratio: usize,
        height: Option<f64>,
        width: Option<f64>,
        unit_size: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let plane = complex_plane_builder(
            range3(x_range)?,
            range3(y_range)?,
            axis_config,
            x_axis_config,
            y_axis_config,
            background_line_style,
            faded_line_style,
            faded_line_ratio,
            height,
            width,
            unit_size,
        )?;
        let built = with_font_book(|book| plane.build(book).map_err(native_error))?;
        install_native_tree(slf, factory, built.into_vmob())
    }

    /// `BulletedList(*items, ...)` over Atlas's de-TeX'd native list
    /// composition. Bundled text glyphs, list labels, arrangement, and the
    /// initial family topology all come from the one fmn-library builder.
    fn _build_bulleted_list<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        items: Vec<String>,
        buff: f64,
        aligned_edge: [f64; 3],
        numbered: bool,
        font_size: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let item_refs: Vec<&str> = items.iter().map(String::as_str).collect();
        let built = with_font_book(|book| {
            fmn_library::BulletedList::new(&item_refs)
                .buff(buff)
                .aligned_edge(aligned_edge)
                .numbered(numbered)
                .font_size(font_size)
                .build(book)
                .map_err(native_error)
        })?;
        install_native_tree(slf, factory, built.vmob)
    }

    /// `Title(*text_parts, ...)` over Atlas's de-TeX'd title composition.
    /// The native builder owns bundled-font layout, source-part grouping,
    /// frame-top placement, and optional underline geometry.
    #[allow(clippy::too_many_arguments)]
    fn _build_title<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        text_parts: Vec<String>,
        font_size: f64,
        include_underline: bool,
        underline_width: f64,
        match_underline_width_to_text: bool,
        underline_buff: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let part_refs: Vec<&str> = text_parts.iter().map(String::as_str).collect();
        let built = with_font_book(|book| {
            fmn_library::Title::new(&part_refs)
                .font_size(font_size)
                .include_underline(include_underline)
                .underline_width(underline_width)
                .match_underline_width_to_text(match_underline_width_to_text)
                .underline_buff(underline_buff)
                .build(book)
                .map_err(native_error)
        })?;
        install_native_tree(slf, factory, built.vmob)
    }

    /// `Matrix`/`TexMatrix` over Atlas's native scalar grid engine. The
    /// portal decides only that every entry belongs to the string route;
    /// entry typesetting, placement, brackets, height capping, and ellipses
    /// all remain inside the native builder.
    #[allow(clippy::too_many_arguments)]
    fn _build_tex_matrix<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        entries: Vec<Vec<String>>,
        v_buff: f64,
        h_buff: f64,
        bracket_h_buff: f64,
        bracket_v_buff: f64,
        height: Option<f64>,
        element_alignment_corner: [f64; 3],
        ellipses_row: Option<isize>,
        ellipses_col: Option<isize>,
        ellipses_height_ratio: f64,
        ellipses_width_ratio: f64,
        font_size: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let entry_refs: Vec<Vec<&str>> = entries
            .iter()
            .map(|row| row.iter().map(String::as_str).collect())
            .collect();
        let mut builder = fmn_library::TexMatrix::new(entry_refs)
            .v_buff(v_buff)
            .h_buff(h_buff)
            .bracket_h_buff(bracket_h_buff)
            .bracket_v_buff(bracket_v_buff)
            .element_alignment_corner(element_alignment_corner)
            .ellipses_ratios(ellipses_height_ratio, ellipses_width_ratio)
            .font_size(font_size);
        if let Some(height) = height {
            builder = builder.height(height);
        }
        if let Some(row) = ellipses_row {
            builder = builder.ellipses_row(row);
        }
        if let Some(column) = ellipses_col {
            builder = builder.ellipses_col(column);
        }
        let built = with_tex_engine(|engine| builder.build(engine).map_err(native_error))?;
        install_native_tree(slf, factory, built.vmob)
    }

    /// `DecimalMatrix`/`IntegerMatrix` over Atlas's native number grid.
    /// `integer=true` selects the dedicated Integer shelf when the public
    /// constructor retains its default zero decimal places; a non-zero
    /// explicit value follows the Reference's DecimalMatrix inheritance.
    #[allow(clippy::too_many_arguments)]
    fn _build_decimal_matrix<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        entries: Vec<Vec<f64>>,
        integer: bool,
        num_decimal_places: usize,
        v_buff: f64,
        h_buff: f64,
        bracket_h_buff: f64,
        bracket_v_buff: f64,
        height: Option<f64>,
        element_alignment_corner: [f64; 3],
        ellipses_row: Option<isize>,
        ellipses_col: Option<isize>,
        ellipses_height_ratio: f64,
        ellipses_width_ratio: f64,
        font_size: f64,
    ) -> PyResult<Bound<'py, PyList>> {
        let built = if integer && num_decimal_places == 0 {
            let mut builder = fmn_library::IntegerMatrix::new(entries)
                .v_buff(v_buff)
                .h_buff(h_buff)
                .bracket_h_buff(bracket_h_buff)
                .bracket_v_buff(bracket_v_buff)
                .element_alignment_corner(element_alignment_corner)
                .ellipses_ratios(ellipses_height_ratio, ellipses_width_ratio)
                .font_size(font_size);
            if let Some(height) = height {
                builder = builder.height(height);
            }
            if let Some(row) = ellipses_row {
                builder = builder.ellipses_row(row);
            }
            if let Some(column) = ellipses_col {
                builder = builder.ellipses_col(column);
            }
            with_tex_engine(|engine| {
                with_font_book(|book| builder.build(engine, book).map_err(native_error))
            })?
        } else {
            let mut builder = fmn_library::DecimalMatrix::new(entries)
                .num_decimal_places(num_decimal_places)
                .v_buff(v_buff)
                .h_buff(h_buff)
                .bracket_h_buff(bracket_h_buff)
                .bracket_v_buff(bracket_v_buff)
                .element_alignment_corner(element_alignment_corner)
                .ellipses_ratios(ellipses_height_ratio, ellipses_width_ratio)
                .font_size(font_size);
            if let Some(height) = height {
                builder = builder.height(height);
            }
            if let Some(row) = ellipses_row {
                builder = builder.ellipses_row(row);
            }
            if let Some(column) = ellipses_col {
                builder = builder.ellipses_col(column);
            }
            with_tex_engine(|engine| {
                with_font_book(|book| builder.build(engine, book).map_err(native_error))
            })?
        };
        install_native_tree(slf, factory, built.vmob)
    }

    /// `Text(...)` over the Scribe bridge: one glyph per child from the
    /// bundled FontBook, decorations trailing.
    #[allow(clippy::too_many_arguments)]
    fn _build_text<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        text: &str,
        markup: bool,
        font_size: f64,
        justify: bool,
        indent: f64,
        line_width: Option<f64>,
        disable_ligatures: bool,
    ) -> PyResult<Bound<'py, PyList>> {
        let mut builder = if markup {
            fmn_library::Text::markup(text)
        } else {
            fmn_library::Text::new(text)
        };
        builder = builder
            .font_size(font_size)
            .ligatures(!disable_ligatures)
            .justify(justify)
            .indent(indent);
        if let Some(width) = line_width {
            builder = builder.width(width);
        }
        let built = with_font_book(|book| builder.build(book).map_err(native_error))?;
        let spans: Vec<(usize, usize)> =
            built.layout.glyphs.iter().map(|glyph| glyph.span).collect();
        let paths: Vec<Vec<usize>> = (0..spans.len()).map(|index| vec![index]).collect();
        slf.as_any().setattr("_string_sub_spans", spans)?;
        slf.as_any().setattr("_string_sub_paths", paths)?;
        install_native_tree(slf, factory, built.vmob)
    }

    /// `Tex(...)` / `TexText(...)` over fmd-math. An unsupported construct
    /// is fmd-math's typed refusal, surfaced VERBATIM (the fm-rqc ratchet
    /// consumes the named constructs from this exact message).
    ///
    /// With more than one part, glyph children regroup per part by the
    /// typeset's native source spans (`typeset.subs[i].span` — §11.4's
    /// span map, no heuristic splitting), matching the Reference's
    /// per-`SingleStringTex` submobject structure.
    #[allow(clippy::too_many_arguments)]
    fn _build_tex<'py>(
        slf: &Bound<'py, Self>,
        factory: &Bound<'py, PyAny>,
        parts: Vec<String>,
        separator: &str,
        text_mode: bool,
        font_size: f64,
        t2c: Option<&Bound<'py, PyDict>>,
        group_single_part: bool,
    ) -> PyResult<Bound<'py, PyList>> {
        let source = parts.join(separator);
        let pairs = t2c_pairs(t2c)?;
        let refs: Vec<(&str, fmn_core::color::Srgb)> = pairs
            .iter()
            .map(|(needle, color)| (needle.as_str(), *color))
            .collect();
        let built = with_tex_engine(|engine| {
            if text_mode {
                fmn_library::TexText::new(&source)
                    .font_size(font_size)
                    .t2c(&refs)
                    .build(engine)
            } else {
                fmn_library::Tex::new(&source)
                    .font_size(font_size)
                    .t2c(&refs)
                    .build(engine)
            }
            // VERBATIM: fmd-math's named-construct refusal is the ratchet's
            // input; never wrap it in a generic message.
            .map_err(tex_error)
        })?;
        let spans: Vec<(usize, usize)> = built
            .typeset
            .subs
            .iter()
            .map(|sub| (sub.span.start, sub.span.end))
            .collect();
        if parts.is_empty() || (parts.len() == 1 && !group_single_part) {
            let paths: Vec<Vec<usize>> = (0..spans.len()).map(|index| vec![index]).collect();
            slf.as_any().setattr("_string_sub_spans", spans)?;
            slf.as_any().setattr("_string_sub_paths", paths)?;
            return install_native_tree(slf, factory, built.vmob);
        }
        // Half-open byte ranges of each part in the joined source; a part
        // owns its trailing separator so every source byte has one owner.
        let mut ranges = Vec::with_capacity(parts.len());
        let mut cursor = 0usize;
        for (index, part) in parts.iter().enumerate() {
            let start = cursor;
            cursor += part.len();
            if index + 1 < parts.len() {
                cursor += separator.len();
            }
            ranges.push((start, cursor));
        }
        let subs = &built.typeset.subs;
        let mut tree = Mobject::from(built.vmob.clone());
        let children = std::mem::take(&mut tree.submobjects);
        if children.len() != spans.len() {
            return Err(PyRuntimeError::new_err(format!(
                "native Tex span table has {} entries for {} primitives",
                spans.len(),
                children.len()
            )));
        }
        let mut buckets: Vec<Vec<Mobject>> = (0..parts.len()).map(|_| Vec::new()).collect();
        let mut paths = vec![Vec::new(); children.len()];
        for (index, child) in children.into_iter().enumerate() {
            let start = subs.get(index).map_or(0, |sub| sub.span.start);
            let part = ranges
                .iter()
                .position(|&(from, to)| from <= start && start < to)
                .unwrap_or(parts.len() - 1);
            paths[index] = vec![part, buckets[part].len()];
            buckets[part].push(child);
        }
        tree.submobjects = buckets
            .into_iter()
            .map(|kids| {
                // A vmobject-schema group node, so the style surface sees
                // the stroke/fill fields on the part wrapper too.
                let mut node = Mobject::from(fmn_library::vmobject::v_group(std::iter::empty::<
                    fmn_library::VMobject,
                >()));
                node.submobjects = kids;
                node
            })
            .collect();
        slf.as_any().setattr("_string_sub_spans", spans)?;
        slf.as_any().setattr("_string_sub_paths", paths)?;
        install_native_tree(slf, factory, tree)
    }

    /// `NumberLine.add_numbers`: rebuild the native line at the proxy's
    /// current width, run the native labeler at `font_size`, and return
    /// the trailing label group (shifted onto the current center) as one
    /// shell spec. Same rebuild caveat as `_axes_label_shells`.
    #[staticmethod]
    #[allow(clippy::too_many_arguments)]
    fn _number_line_label_shells<'py>(
        factory: &Bound<'py, PyAny>,
        x_range: &Bound<'py, PyAny>,
        config: &Bound<'py, PyDict>,
        font_size: f64,
        current_width: f64,
        current_center: [f64; 3],
        x_values: Option<Vec<f64>>,
        excluding: Option<Vec<f64>>,
        direction: Option<[f64; 3]>,
        buff: Option<f64>,
    ) -> PyResult<Bound<'py, PyList>> {
        let mut line = number_line_from_config(range3(x_range)?, config)?
            .numbers_font_size(font_size)
            .width(current_width);
        if let Some(direction) = direction {
            line = line.line_to_number_direction(direction);
        }
        if let Some(buff) = buff {
            line = line.line_to_number_buff(buff);
        }
        let mut built = line.build().map_err(native_error)?;
        let before = built.vmob().children().len();
        with_font_book(|book| {
            built
                .add_numbers(book, x_values.as_deref(), excluding.as_deref())
                .map_err(native_error)
        })?;
        let groups: Vec<Mobject> = built.vmob().children()[before..]
            .iter()
            .map(|group| Mobject::from(group.clone().shifted(current_center)))
            .collect();
        native_shell_specs(factory.py(), factory, groups)
    }

    /// `Axes.add_coordinate_labels`: rebuild the native axes at the
    /// proxy's CURRENT width/height, run the native labeler, and return
    /// the two trailing label groups (shifted onto the proxy's current
    /// center) as shell specs for `x_axis`/`y_axis`.
    ///
    /// Until live-state cores land (fm-p107 territory), the rebuild
    /// reproduces uniform rescales and translations exactly; a rotated or
    /// stretched axes would label at the unrotated positions.
    #[staticmethod]
    #[allow(clippy::too_many_arguments)]
    fn _axes_label_shells<'py>(
        factory: &Bound<'py, PyAny>,
        x_range: &Bound<'py, PyAny>,
        y_range: &Bound<'py, PyAny>,
        axis_config: Option<&Bound<'py, PyDict>>,
        x_axis_config: Option<&Bound<'py, PyDict>>,
        y_axis_config: Option<&Bound<'py, PyDict>>,
        unit_size: f64,
        current_width: f64,
        current_height: f64,
        current_center: [f64; 3],
        x_values: Option<Vec<f64>>,
        y_values: Option<Vec<f64>>,
        excluding: Vec<f64>,
    ) -> PyResult<(Bound<'py, PyList>, Bound<'py, PyList>)> {
        let axes = axes_builder(
            range3(x_range)?,
            range3(y_range)?,
            axis_config,
            x_axis_config,
            y_axis_config,
            Some(current_height),
            Some(current_width),
            unit_size,
        )?;
        let mut built = with_font_book(|book| axes.build(book).map_err(native_error))?;
        let before_x = built.x_axis().vmob().children().len();
        let before_y = built.y_axis().vmob().children().len();
        with_font_book(|book| {
            built
                .add_coordinate_labels(
                    book,
                    x_values.as_deref(),
                    y_values.as_deref(),
                    Some(&excluding),
                )
                .map_err(native_error)
        })?;
        let py = factory.py();
        let label_specs =
            |axis: &fmn_library::NumberLine, before: usize| -> PyResult<Bound<'py, PyList>> {
                let groups: Vec<Mobject> = axis.vmob().children()[before..]
                    .iter()
                    .map(|group| Mobject::from(group.clone().shifted(current_center)))
                    .collect();
                native_shell_specs(py, factory, groups)
            };
        Ok((
            label_specs(built.x_axis(), before_x)?,
            label_specs(built.y_axis(), before_y)?,
        ))
    }

    /// `ComplexPlane.add_coordinate_labels`: rebuild at the proxy's
    /// current width/height (with `font_size` routed to the axes'
    /// `decimal_number_config`), run the native labeler, and return the
    /// trailing label group (shifted onto the current center) as one
    /// shell spec. Same rebuild caveat as `_axes_label_shells`.
    #[staticmethod]
    #[allow(clippy::too_many_arguments)]
    fn _complex_plane_label_shells<'py>(
        factory: &Bound<'py, PyAny>,
        x_range: &Bound<'py, PyAny>,
        y_range: &Bound<'py, PyAny>,
        axis_config: Option<&Bound<'py, PyDict>>,
        x_axis_config: Option<&Bound<'py, PyDict>>,
        y_axis_config: Option<&Bound<'py, PyDict>>,
        background_line_style: Option<&Bound<'py, PyDict>>,
        faded_line_style: Option<&Bound<'py, PyDict>>,
        faded_line_ratio: usize,
        unit_size: f64,
        current_width: f64,
        current_height: f64,
        current_center: [f64; 3],
        numbers: Option<Vec<[f64; 2]>>,
        font_size: Option<f64>,
    ) -> PyResult<Bound<'py, PyList>> {
        let mut axis_cfg = axis_config_from(axis_config)?;
        let mut x_cfg = axis_config_from(x_axis_config)?;
        let mut y_cfg = axis_config_from(y_axis_config)?;
        if let Some(font_size) = font_size {
            axis_cfg.number_font_size = Some(font_size);
            x_cfg.number_font_size = Some(font_size);
            y_cfg.number_font_size = Some(font_size);
        }
        let plane = fmn_library::ComplexPlane::new()
            .x_range(range3(x_range)?)
            .y_range(range3(y_range)?)
            .axis_config(axis_cfg)
            .x_axis_config(x_cfg)
            .y_axis_config(y_cfg)
            .background_line_style(line_family_style_from(background_line_style)?)
            .faded_line_style(faded_line_style_from(faded_line_style)?)
            .faded_line_ratio(faded_line_ratio)
            .unit_size(unit_size)
            .height(current_height)
            .width(current_width);
        let mut built = with_font_book(|book| plane.build(book).map_err(native_error))?;
        let before = built.vmob().children().len();
        with_font_book(|book| {
            match &numbers {
                Some(values) => built.add_coordinate_labels_for(values, book),
                None => built.add_coordinate_labels(book),
            }
            .map_err(native_error)
        })?;
        let groups: Vec<Mobject> = built.vmob().children()[before..]
            .iter()
            .map(|group| Mobject::from(group.clone().shifted(current_center)))
            .collect();
        native_shell_specs(factory.py(), factory, groups)
    }

    fn _replace_submobjects(
        slf: &Bound<'_, Self>,
        children: Vec<Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let mut seen = HashSet::new();
        let mut proxies = Vec::with_capacity(children.len());
        for child in children {
            let child = child
                .cast_into::<BridgeMobject>()
                .map_err(|_| PyTypeError::new_err("submobjects must be Mobject instances"))?;
            let marker = child.as_ptr() as usize;
            if !seen.insert(marker) {
                return Err(PyValueError::new_err(
                    "one submobject cannot appear twice under the same parent",
                ));
            }
            proxies.push(child);
        }
        let location_of = |proxy: &Bound<'_, BridgeMobject>| {
            let cell = proxy.borrow();
            (
                cell.engine.as_ref().map(Rc::clone),
                cell.mob,
                cell.initialized && cell.nursery.is_some(),
            )
        };
        let mut child_locations: Vec<_> = proxies.iter().map(&location_of).collect();

        let parent_locator = |slf: &Bound<'_, Self>| {
            let cell = slf.borrow();
            (
                cell.engine.as_ref().map(Rc::clone),
                cell.mob,
                cell.initialized && cell.nursery.is_some(),
            )
        };
        let mut parent_location = parent_locator(slf);
        if parent_location.0.is_none() {
            if !parent_location.2 {
                return Err(StaleHandleError::new_err(
                    "uninitialized mobject cannot own submobjects",
                ));
            }
            let bound_child = proxies
                .iter()
                .zip(&child_locations)
                .find_map(|(proxy, location)| location.0.is_some().then_some(proxy));
            match bound_child {
                None => {
                    if child_locations.iter().any(|(_, _, detached)| !detached) {
                        return Err(ForeignStageError::new_err(
                            "a detached parent may contain only detached mobjects",
                        ));
                    }
                    // The Python live list is authoritative until Scene.add
                    // binds the complete graph in one transaction.
                    return Ok(());
                }
                Some(child) => {
                    // Mirror adoption (fm-p107): a detached parent
                    // ingesting a bound child adopts INTO the child's
                    // scene first — the Reference's global-mobject model;
                    // the parent is typically scene-added right after.
                    let scene_object = child.getattr("_scene")?;
                    let scene = scene_object.cast::<PyScene>().map_err(|_| {
                        PyRuntimeError::new_err("bound proxy's `_scene` is not a Scene")
                    })?;
                    bind_graph(slf.py(), scene, slf)?;
                    parent_location = parent_locator(slf);
                }
            }
        }
        let (Some(engine), Some(parent), _) = parent_location else {
            return Err(StaleHandleError::new_err(
                "parent adoption did not bind the mobject",
            ));
        };

        // Adoption-on-attach (fm-d3gt): a detached child joining a
        // scene-bound parent adopts into the parent's scene first, through
        // the SAME bind_graph path Scene.add uses — nursery copy_into,
        // pinning, proxy registration, `_scene`, Python identity intact.
        // Mixed lists work; a child bound to a different scene still
        // refuses below.
        if child_locations
            .iter()
            .any(|(child_engine, _, detached)| child_engine.is_none() && *detached)
        {
            let scene_object = slf.getattr("_scene")?;
            let scene = scene_object
                .cast::<PyScene>()
                .map_err(|_| PyRuntimeError::new_err("bound proxy's `_scene` is not a Scene"))?;
            for (proxy, location) in proxies.iter().zip(&child_locations) {
                if location.0.is_none() && location.2 {
                    // bind_graph registers proxies and sets `_scene` itself.
                    bind_graph(slf.py(), scene, proxy)?;
                }
            }
            child_locations = proxies.iter().map(&location_of).collect();
        }

        let mut candidate = Vec::with_capacity(child_locations.len());
        for (child_engine, child_mob, _) in child_locations {
            let (Some(child_engine), Some(child_mob)) = (child_engine, child_mob) else {
                return Err(ForeignStageError::new_err(
                    "a bound parent may contain only mobjects from the same Scene",
                ));
            };
            if !same_engine(&engine, &child_engine) {
                return Err(ForeignStageError::new_err(
                    "submobject belongs to a different Scene",
                ));
            }
            candidate.push(child_mob);
        }

        let mut runtime = engine.borrow_mut();
        let old = runtime
            .stage()
            .get(parent)
            .ok_or_else(|| StaleHandleError::new_err("parent handle no longer resolves"))?
            .submobjects()
            .to_vec();
        for child in &old {
            runtime.stage_mut().detach(parent, *child);
        }
        let mut attached = Vec::new();
        for child in &candidate {
            if let Err(error) = runtime.stage_mut().attach(parent, *child) {
                for added in attached {
                    runtime.stage_mut().detach(parent, added);
                }
                for original in old {
                    runtime
                        .stage_mut()
                        .attach(parent, original)
                        .expect("previously valid family edge restores");
                }
                return Err(stage_error(error));
            }
            attached.push(*child);
        }
        Ok(())
    }

    fn family_size(slf: &Bound<'_, Self>) -> PyResult<usize> {
        crossing::record(CrossingClass::Other);
        let location = {
            let cell = slf.borrow();
            cell.engine
                .as_ref()
                .zip(cell.mob)
                .map(|(engine, mob)| (Rc::clone(engine), mob))
        };
        if let Some((engine, mob)) = location {
            return Ok(engine.borrow().stage().family(mob).len());
        }
        Ok(collect_proxy_graph(slf.as_any())?.len())
    }

    fn interpolate(
        slf: &Bound<'_, Self>,
        start: &Bound<'_, BridgeMobject>,
        target: &Bound<'_, BridgeMobject>,
        alpha: f64,
    ) -> PyResult<()> {
        location_compatible(slf, start)?;
        location_compatible(slf, target)?;
        let (start_schema, start_len, start_records) = flat_records(start)?;
        let (target_schema, target_len, target_records) = flat_records(target)?;
        if start_schema != target_schema || start_len != target_len {
            return Err(PyValueError::new_err(
                "interpolation endpoints require identical schemas and record counts; align first",
            ));
        }
        let (own_schema, own_len, _) = flat_records(slf)?;
        if own_schema != start_schema || own_len != start_len {
            return Err(PyValueError::new_err(
                "interpolating mobject does not match its endpoints",
            ));
        }
        let alpha = alpha as f32;
        // fm-zoi GIL discipline (§17.4): the mixing kernel touches only
        // owned f32 vectors, so it runs with the GIL released and the
        // interpreter overlaps the native conversion. Bit-identical lane
        // order: from + (to - from) * alpha, record-major.
        let mixed: Vec<f32> = slf.py().detach(move || {
            start_records
                .iter()
                .zip(target_records.iter())
                .map(|(from, to)| from + (to - from) * alpha)
                .collect()
        });
        crossing::record(CrossingClass::Other);
        with_buffer(slf, |buffer| {
            let fields: Vec<(String, usize)> = buffer
                .schema()
                .fields()
                .iter()
                .map(|field| (field.name.clone(), field.width))
                .collect();
            let mut cursor = 0usize;
            for index in 0..buffer.len() {
                for (field, width) in &fields {
                    let end = cursor + width;
                    let wrote = buffer.write(index, field, &mixed[cursor..end]);
                    debug_assert!(wrote, "schema and loop are identical");
                    cursor = end;
                }
            }
        })
    }

    /// Engine-side family copy, returning Python proxy shells for the
    /// bootstrap's shallow/deep `__dict__` pass. Detached graphs return None.
    fn _copy_family_shells<'py>(slf: &Bound<'py, Self>) -> PyResult<Option<ProxyPairs>> {
        let (engine, root) = {
            let cell = slf.borrow();
            match (&cell.engine, cell.mob) {
                (Some(engine), Some(mob)) => (Rc::clone(engine), mob),
                _ => return Ok(None),
            }
        };
        let scene_object = slf.getattr("_scene")?;
        let scene = scene_object
            .cast::<PyScene>()
            .map_err(|_| PyRuntimeError::new_err("bound proxy's `_scene` is not a Scene"))?;
        let copy_map = engine
            .borrow_mut()
            .stage_mut()
            .copy_family_mapped(root)
            .map_err(stage_error)?;
        let py = slf.py();
        let mut pairs = Vec::with_capacity(copy_map.len());
        for &(old, new) in copy_map.pairs() {
            let old_proxy = live_proxy(py, scene, old).ok_or_else(|| {
                PyRuntimeError::new_err("engine family member has no live Python proxy during copy")
            })?;
            let new_proxy = new_bound_proxy(py, scene, &old_proxy, &engine, new)?;
            pairs.push((old_proxy.unbind(), new_proxy.unbind()));
        }
        Ok(Some(pairs))
    }

    /// Copy one detached nursery into another proxy through Marionette's
    /// cross-stage transfer. Unlike the pickle-shaped record-state path,
    /// this preserves renderer-owned entry metadata such as placements,
    /// shape tags, render primitives, and durable image resources.
    fn _copy_detached_state_to(
        slf: &Bound<'_, Self>,
        target: &Bound<'_, BridgeMobject>,
    ) -> PyResult<()> {
        if slf.is(target) {
            return Err(PyValueError::new_err(
                "detached copy target must be a distinct mobject",
            ));
        }
        let nursery = {
            let source = slf.borrow();
            if source.engine.is_some() || source.mob.is_some() {
                return Err(PyRuntimeError::new_err(
                    "detached-state copy requires a detached source mobject",
                ));
            }
            let source_nursery = source.nursery.as_ref().ok_or_else(|| {
                PyRuntimeError::new_err("detached source mobject has no native nursery")
            })?;
            let mut stage = Stage::new();
            let root = source_nursery
                .stage
                .copy_into(source_nursery.root, &mut stage)
                .map_err(stage_error)?;
            Nursery { stage, root }
        };
        let mut destination = target.borrow_mut();
        if destination.engine.is_some() || destination.mob.is_some() {
            return Err(PyRuntimeError::new_err(
                "detached-state copy cannot overwrite a bound target mobject",
            ));
        }
        destination.nursery = Some(nursery);
        destination.initialized = true;
        Ok(())
    }

    fn _engine_state<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyDict>> {
        native_state(slf.py(), slf)
    }

    fn _restore_engine_state(slf: &Bound<'_, Self>, state: &Bound<'_, PyDict>) -> PyResult<()> {
        restore_native_state(slf, state)
    }

    fn is_alive(slf: &Bound<'_, Self>) -> bool {
        crossing::record(CrossingClass::Other);
        let cell = slf.borrow();
        match (&cell.engine, cell.mob) {
            (Some(engine), Some(mob)) => engine.borrow().stage().contains(mob),
            _ => cell.nursery.is_some(),
        }
    }

    fn delete(slf: &Bound<'_, Self>) -> PyResult<()> {
        let (engine, mob) = bound_parts(&slf.borrow())?;
        engine
            .borrow_mut()
            .stage_mut()
            .delete(mob)
            .map_err(stage_error)
    }

    fn noop(_slf: &Bound<'_, Self>) {}
}

impl Drop for BridgeMobject {
    fn drop(&mut self) {
        if let (Some(engine), Some(mob)) = (&self.engine, self.mob) {
            engine.release_pin(mob);
        }
    }
}

fn scene_proxy_handles(
    scene: &Bound<'_, PyScene>,
    objects: &Bound<'_, PyTuple>,
) -> PyResult<Vec<Mob>> {
    let engine = Rc::clone(&scene.borrow().engine);
    let mut handles = Vec::with_capacity(objects.len());
    for object in objects.iter() {
        let proxy = object.cast::<BridgeMobject>().map_err(|_| {
            PyTypeError::new_err("Scene membership operations require Mobject instances")
        })?;
        let (object_engine, mob) = bound_parts(&proxy.borrow())?;
        if !same_engine(&engine, &object_engine) {
            return Err(ForeignStageError::new_err(
                "mobject belongs to a different Scene",
            ));
        }
        handles.push(mob);
    }
    Ok(handles)
}

fn bind_scene_mobjects(
    scene: &Bound<'_, PyScene>,
    objects: &Bound<'_, PyTuple>,
    method: &str,
) -> PyResult<Vec<Mob>> {
    let mut proxies = Vec::with_capacity(objects.len());
    for object in objects.iter() {
        proxies.push(object.cast_into::<BridgeMobject>().map_err(|_| {
            PyTypeError::new_err(format!("Scene.{method} accepts only Mobject instances"))
        })?);
    }

    let py = scene.py();
    proxies
        .into_iter()
        .map(|proxy| bind_graph(py, scene, &proxy))
        .collect()
}

/// Resolve `object` only when it is the exact top-level member that the
/// Reference's `Scene.replace` membership test would find. Non-mobjects,
/// detached mobjects, foreign-scene mobjects, and family descendants are all
/// absent from this scene and therefore make replacement a side-effect-free
/// no-op, including leaving replacement arguments detached.
fn scene_root_handle(scene: &Bound<'_, PyScene>, object: &Bound<'_, PyAny>) -> Option<Mob> {
    let proxy = object.cast::<BridgeMobject>().ok()?;
    let engine = Rc::clone(&scene.borrow().engine);
    let (object_engine, mob) = {
        let cell = proxy.borrow();
        (Rc::clone(cell.engine.as_ref()?), cell.mob?)
    };
    if !same_engine(&engine, &object_engine) {
        return None;
    }
    engine.borrow().mobjects().contains(&mob).then_some(mob)
}

/// Collect one unsuspended updater subtree in the Reference's child-first
/// order. A suspended parent prunes its entire subtree even when descendants
/// are not individually marked suspended. The explicit stack keeps a valid
/// deeply nested family from consuming the scene worker's call stack.
fn collect_update_targets(stage: &Stage, root: Mob, targets: &mut Vec<Mob>) {
    let mut stack = vec![(root, false)];
    while let Some((mob, visited)) = stack.pop() {
        if visited {
            if !targets.contains(&mob) {
                targets.push(mob);
            }
            continue;
        }
        if stage.is_updating_suspended(mob) || targets.contains(&mob) {
            continue;
        }
        stack.push((mob, true));
        if let Some(entry) = stage.get(mob) {
            stack.extend(
                entry
                    .submobjects()
                    .iter()
                    .rev()
                    .map(|&child| (child, false)),
            );
        }
    }
}

/// Mobjects receiving updater dispatch this frame, in the same child-first,
/// suspension-pruned order as Marionette's native updater pass.
fn update_targets(scene: &Bound<'_, PyScene>) -> Vec<Mob> {
    let scene_cell = scene.borrow();
    let runtime = scene_cell.engine.borrow();
    let mut targets = Vec::new();
    for &root in runtime.stage().roots() {
        collect_update_targets(runtime.stage(), root, &mut targets);
    }
    targets
}

/// The Reference keeps the camera frame at the head of `Scene.mobjects`, so
/// its host-language updaters run before every drawable-root updater.  The
/// portal deliberately keeps CameraFrame out of Marionette's drawable Stage;
/// recover that same update ordering through the real Python frame object.
fn camera_frame<'py>(scene: &Bound<'py, PyScene>) -> PyResult<Bound<'py, PyAny>> {
    scene.getattr("frame")
}

fn run_proxy_updaters(proxy: &Bound<'_, PyAny>, dt: f64) -> PyResult<()> {
    let py = proxy.py();
    crossing::record(CrossingClass::Other);
    let updaters = proxy.getattr("updaters")?;
    let snapshot: Vec<Py<PyAny>> = updaters
        .try_iter()?
        .map(|item| item.map(Bound::unbind))
        .collect::<PyResult<_>>()?;
    for updater in snapshot {
        crossing::record(CrossingClass::UpdaterCall);
        let args = PyTuple::new(
            py,
            [updater.bind(py).clone(), dt.into_pyobject(py)?.into_any()],
        )?;
        method_cache::call_cached1(proxy, "_dispatch_updater", &args)?;
    }
    Ok(())
}

/// Run the portal half of Scene.update_mobjects with no Scene/Stage borrow
/// live. Choreo's stepped play releases exactly at this call site; ordinary
/// `Scene.update(dt)` uses the same helper before its native updater pass.
fn run_python_updaters(scene: &Bound<'_, PyScene>, dt: f64) -> PyResult<u64> {
    let targets = update_targets(scene);
    let py = scene.py();
    let python_start = Instant::now();
    run_proxy_updaters(&camera_frame(scene)?, dt)?;
    for target in targets {
        let Some(proxy) = live_proxy(py, scene, target) else {
            continue;
        };
        run_proxy_updaters(&proxy, dt)?;
    }
    Ok(u64::try_from(python_start.elapsed().as_nanos()).unwrap_or(u64::MAX))
}

/// Model `Mobject.resume_updating(call_updater=True)` for the host-language
/// updater layer after Choreo has finished an animation lifecycle.  This is
/// deliberately root-scoped; the subsequent scene-wide zero-`dt` pass is a
/// separate Reference event.
fn run_resumed_python_updaters(scene: &Bound<'_, PyScene>, roots: &[Mob]) -> PyResult<()> {
    let targets = {
        let scene_cell = scene.borrow();
        let runtime = scene_cell.engine.borrow();
        let mut targets = Vec::new();
        for &root in roots {
            collect_update_targets(runtime.stage(), root, &mut targets);
        }
        targets
    };
    let py = scene.py();
    for target in targets {
        if let Some(proxy) = live_proxy(py, scene, target) {
            run_proxy_updaters(&proxy, 0.0)?;
        }
    }
    Ok(())
}

fn has_python_updaters(scene: &Bound<'_, PyScene>) -> PyResult<bool> {
    let py = scene.py();
    if camera_frame(scene)?.getattr("updaters")?.is_truthy()? {
        return Ok(true);
    }
    for target in update_targets(scene) {
        if let Some(proxy) = live_proxy(py, scene, target)
            && proxy.getattr("updaters")?.is_truthy()?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

struct PortalPngRequest {
    destination: String,
    width: u32,
    height: u32,
    fps: u32,
    threads: usize,
    seed: u64,
    single_frame: bool,
}

fn begin_portal_png(slf: &Bound<'_, PyScene>, request: PortalPngRequest) -> PyResult<()> {
    let PortalPngRequest {
        destination,
        width,
        height,
        fps,
        threads,
        seed,
        single_frame,
    } = request;
    if destination.is_empty() {
        return Err(PyValueError::new_err(
            "render destination must not be empty",
        ));
    }
    let render_slot = {
        let scene = slf.borrow();
        if !scene.proxies.borrow().is_empty()
            || !scene.engine.borrow().stage().roots().is_empty()
            || scene.engine.borrow().stage().time() != 0.0
        {
            return Err(PyRuntimeError::new_err(
                "render configuration must be installed before Scene construction mutates engine state",
            ));
        }
        Arc::clone(&scene.render)
    };
    let (session, runtime_config) = PortalRenderSession::new(
        PathBuf::from(destination),
        width,
        height,
        fps,
        threads,
        single_frame,
    )?;
    let replacement = match Scene::new(runtime_config, seed) {
        Ok(scene) => Rc::new(EngineState::new(scene)),
        Err(error) => {
            session.abort();
            return Err(PyRuntimeError::new_err(error.to_string()));
        }
    };
    let mut render = match render_slot.lock() {
        Ok(render) => render,
        Err(_) => {
            session.abort();
            return Err(PyRuntimeError::new_err(
                "portal render session lock was poisoned",
            ));
        }
    };
    if render.is_some() {
        session.abort();
        return Err(PyRuntimeError::new_err(
            "a portal render generation is already active",
        ));
    }
    slf.borrow_mut().engine = replacement;
    *render = Some(session);
    Ok(())
}

#[pymethods]
impl PyScene {
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn py_new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<Self> {
        let runtime = Scene::new(RuntimeConfig::default(), 0)
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        Ok(Self {
            engine: Rc::new(EngineState::new(runtime)),
            proxies: RefCell::new(HashMap::new()),
            render: Arc::new(Mutex::new(None)),
        })
    }

    /// Start one no-clobber native PNG-sequence generation before lifecycle
    /// execution. Configuration is accepted only while the Scene is pristine,
    /// so changing fps/seed cannot reinterpret already-created engine state.
    #[pyo3(signature = (destination, width, height, fps, threads, seed))]
    fn _begin_png_sequence(
        slf: &Bound<'_, Self>,
        destination: String,
        width: u32,
        height: u32,
        fps: u32,
        threads: usize,
        seed: u64,
    ) -> PyResult<()> {
        begin_portal_png(
            slf,
            PortalPngRequest {
                destination,
                width,
                height,
                fps,
                threads,
                seed,
                single_frame: false,
            },
        )
    }

    /// Start one atomic final-state PNG generation. The scene advances every
    /// segment to its semantic endpoint without intermediate raster work;
    /// `_finish_render` captures and publishes exactly one frame.
    #[pyo3(signature = (destination, width, height, fps, threads, seed))]
    fn _begin_png(
        slf: &Bound<'_, Self>,
        destination: String,
        width: u32,
        height: u32,
        fps: u32,
        threads: usize,
        seed: u64,
    ) -> PyResult<()> {
        begin_portal_png(
            slf,
            PortalPngRequest {
                destination,
                width,
                height,
                fps,
                threads,
                seed,
                single_frame: true,
            },
        )
    }

    /// Cancel an active generation and join its ordered output worker.
    fn _abort_render(slf: &Bound<'_, Self>) -> PyResult<()> {
        let render = Arc::clone(&slf.borrow().render);
        let session = render
            .lock()
            .map_err(|_| PyRuntimeError::new_err("portal render session lock was poisoned"))?
            .take();
        if let Some(session) = session {
            session.abort();
        }
        Ok(())
    }

    /// Finalize the active native generation and return its exact artifact
    /// receipt: path, frames, bytes, digest, renderer identity, threads.
    #[pyo3(signature = (camera=None, light_position=None))]
    fn _finish_render(
        slf: &Bound<'_, Self>,
        camera: Option<PyRef<'_, PyCameraFrameCore>>,
        light_position: Option<[f64; 3]>,
    ) -> PyResult<(String, u64, u64, String, String, usize)> {
        let engine = Rc::clone(&slf.borrow().engine);
        let render = Arc::clone(&slf.borrow().render);
        match (camera, light_position) {
            (Some(camera), Some(light_position)) => render
                .lock()
                .map_err(|_| PyRuntimeError::new_err("portal render session lock was poisoned"))?
                .as_mut()
                .ok_or_else(|| PyRuntimeError::new_err("no portal render generation is active"))?
                .bind_camera(camera.frame.clone(), light_position)?,
            (None, None) => {}
            _ => {
                return Err(PyValueError::new_err(
                    "camera frame and light position must be supplied together",
                ));
            }
        }
        let needs_final_capture = render
            .lock()
            .map_err(|_| PyRuntimeError::new_err("portal render session lock was poisoned"))?
            .as_ref()
            .is_some_and(|session| session.frame_count() == 0);
        if needs_final_capture {
            let mut sink = PortalSceneSink {
                render: Arc::clone(&render),
                ..PortalSceneSink::default()
            };
            engine
                .borrow_mut()
                .show(&mut sink)
                .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        }
        let session = render
            .lock()
            .map_err(|_| PyRuntimeError::new_err("portal render session lock was poisoned"))?
            .take()
            .ok_or_else(|| PyRuntimeError::new_err("no portal render generation is active"))?;
        // ubs:ignore — finalizes a frame-render session; no token, secret, or randomness exists.
        let (report, engine, threads) = session.finish()?;
        Ok((
            report.path.to_string_lossy().into_owned(),
            report.frame_count,
            report.bytes,
            report.digest.to_hex(),
            engine,
            threads,
        ))
    }

    #[pyo3(signature = (*mobjects))]
    fn add<'py>(
        slf: &Bound<'py, Self>,
        mobjects: &Bound<'py, PyTuple>,
    ) -> PyResult<Bound<'py, Self>> {
        let handles = bind_scene_mobjects(slf, mobjects, "add")?;
        slf.borrow()
            .engine
            .borrow_mut()
            .add(&handles)
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        Ok(slf.clone())
    }

    #[pyo3(signature = (*mobjects))]
    fn bring_to_front<'py>(
        slf: &Bound<'py, Self>,
        mobjects: &Bound<'py, PyTuple>,
    ) -> PyResult<Bound<'py, Self>> {
        let handles = bind_scene_mobjects(slf, mobjects, "bring_to_front")?;
        slf.borrow()
            .engine
            .borrow_mut()
            .bring_to_front(&handles)
            .map_err(native_error)?;
        Ok(slf.clone())
    }

    #[pyo3(signature = (*mobjects))]
    fn bring_to_back<'py>(
        slf: &Bound<'py, Self>,
        mobjects: &Bound<'py, PyTuple>,
    ) -> PyResult<Bound<'py, Self>> {
        let handles = bind_scene_mobjects(slf, mobjects, "bring_to_back")?;
        slf.borrow()
            .engine
            .borrow_mut()
            .bring_to_back(&handles)
            .map_err(native_error)?;
        Ok(slf.clone())
    }

    #[pyo3(signature = (mobject, *replacements))]
    fn replace<'py>(
        slf: &Bound<'py, Self>,
        mobject: &Bound<'py, PyAny>,
        replacements: &Bound<'py, PyTuple>,
    ) -> PyResult<Bound<'py, Self>> {
        let Some(source) = scene_root_handle(slf, mobject) else {
            return Ok(slf.clone());
        };
        let replacements = bind_scene_mobjects(slf, replacements, "replace")?;
        slf.borrow()
            .engine
            .borrow_mut()
            .replace(source, &replacements)
            .map_err(native_error)?;
        Ok(slf.clone())
    }

    fn get_mobjects<'py>(slf: &Bound<'py, Self>) -> Vec<Py<PyAny>> {
        Self::_engine_roots(slf)
    }

    #[pyo3(signature = (*mobjects_to_keep))]
    fn remove_all_except<'py>(
        slf: &Bound<'py, Self>,
        mobjects_to_keep: &Bound<'py, PyTuple>,
    ) -> PyResult<Bound<'py, Self>> {
        let handles = bind_scene_mobjects(slf, mobjects_to_keep, "remove_all_except")?;
        slf.borrow()
            .engine
            .borrow_mut()
            .remove_all_except(&handles)
            .map_err(native_error)?;
        Ok(slf.clone())
    }

    #[pyo3(signature = (*mobjects))]
    fn remove<'py>(
        slf: &Bound<'py, Self>,
        mobjects: &Bound<'py, PyTuple>,
    ) -> PyResult<Bound<'py, Self>> {
        let handles = scene_proxy_handles(slf, mobjects)?;
        slf.borrow().engine.borrow_mut().remove(&handles);
        Ok(slf.clone())
    }

    fn clear<'py>(slf: &Bound<'py, Self>) -> Bound<'py, Self> {
        slf.borrow().engine.borrow_mut().clear();
        slf.clone()
    }

    fn root_count(&self) -> usize {
        crossing::record(CrossingClass::Other);
        self.engine.borrow().stage().roots().len()
    }

    fn time(&self) -> f64 {
        crossing::record(CrossingClass::Other);
        self.engine.borrow().stage().time()
    }

    #[pyo3(signature = (sound_file, time_offset=0.0, gain=None, gain_to_background=None))]
    fn _add_sound(
        &self,
        sound_file: String,
        time_offset: f64,
        gain: Option<f64>,
        gain_to_background: Option<f64>,
    ) -> PyResult<()> {
        crossing::record(CrossingClass::Other);
        self.engine
            .borrow_mut()
            .add_sound(sound_file, time_offset, gain, gain_to_background)
            .map(|_| ())
            .map_err(native_error)
    }

    /// Engine-truth diagnostics for the permanent bridge acceptance suite.
    /// Each tuple is `(path, frame, fps, offset, gain, background_gain)`.
    fn _sound_request_facts(&self) -> Vec<SoundRequestFact> {
        crossing::record(CrossingClass::Other);
        self.engine
            .borrow()
            .sound_requests()
            .iter()
            .map(|request| {
                (
                    request.sound_file.to_string_lossy().into_owned(),
                    request.time.frames(),
                    request.time.fps(),
                    request.time_offset,
                    request.gain,
                    request.gain_to_background,
                )
            })
            .collect()
    }

    fn _engine_roots<'py>(slf: &Bound<'py, Self>) -> Vec<Py<PyAny>> {
        let py = slf.py();
        let roots = slf.borrow().engine.borrow().stage().roots().to_vec();
        roots
            .into_iter()
            .filter_map(|mob| live_proxy(py, slf, mob).map(Bound::unbind))
            .collect()
    }

    /// Rung 0 (always-correct default): Python updater callbacks run with
    /// no Scene/Stage borrow live, one native→Python crossing per updater.
    /// After they finish, Marionette advances time and runs native updaters.
    fn update(slf: &Bound<'_, Self>, dt: f64) -> PyResult<()> {
        let python_ns = run_python_updaters(slf, dt)?;
        let native_start = Instant::now();
        slf.borrow().engine.borrow_mut().stage_mut().update(dt);
        let native_ns = u64::try_from(native_start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        crossing::record_phase(python_ns, native_ns);
        Ok(())
    }

    /// Rung 1 (explicit opt-in, fm-zoi §15.2 Rev 4): the updater phase
    /// crosses native→Python ONCE per frame. The bootstrap's
    /// `Scene._dispatch_updater_batch` staticmethod iterates the same target
    /// list in the same order, snapshots each mobject's `updaters` at that
    /// mobject's turn (identical to rung 0's lazy snapshot), and invokes
    /// `_dispatch_updater` per updater inside Python. The batch's return is
    /// the single batched dirty-propagation crossing for the whole callback
    /// group. Declared semantics: identical ordering and identical
    /// observable state after each frame; liveness of proxies is resolved
    /// once at frame start (frame-atomic).
    fn update_batched(slf: &Bound<'_, Self>, dt: f64) -> PyResult<()> {
        let targets = update_targets(slf);
        let py = slf.py();
        let mut batch = Vec::with_capacity(targets.len() + 1);
        batch.push(camera_frame(slf)?);
        for target in targets {
            if let Some(proxy) = live_proxy(py, slf, target) {
                batch.push(proxy);
            }
        }
        let python_start = Instant::now();
        if !batch.is_empty() {
            crossing::record(CrossingClass::MethodDispatch);
            let args = PyTuple::new(
                py,
                [
                    PyTuple::new(py, batch)?.into_any(),
                    dt.into_pyobject(py)?.into_any(),
                ],
            )?;
            method_cache::call_static_cached1(slf.as_any(), "_dispatch_updater_batch", &args)?;
            // The batch return transfers the frame's accumulated dirty state
            // to native in one crossing (batched per callback group).
            crossing::record(CrossingClass::DirtyPropagation);
        }
        let python_ns = u64::try_from(python_start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let native_start = Instant::now();
        slf.borrow().engine.borrow_mut().stage_mut().update(dt);
        let native_ns = u64::try_from(native_start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        crossing::record_phase(python_ns, native_ns);
        Ok(())
    }

    fn run_transform(
        slf: &Bound<'_, Self>,
        mobject: &Bound<'_, BridgeMobject>,
        target: &Bound<'_, BridgeMobject>,
        steps: usize,
    ) -> PyResult<()> {
        let engine = Rc::clone(&slf.borrow().engine);
        for endpoint in [mobject, target] {
            let (endpoint_engine, _) = bound_parts(&endpoint.borrow())?;
            if !same_engine(&engine, &endpoint_engine) {
                return Err(ForeignStageError::new_err(
                    "transform endpoints must belong to this Scene",
                ));
            }
        }
        crossing::record(CrossingClass::MethodDispatch);
        let copy = method_cache::call_cached0(mobject.as_any(), "__copy__")?;
        for step in 0..=steps {
            let alpha = if steps == 0 {
                1.0
            } else {
                step as f64 / steps as f64
            };
            crossing::record(CrossingClass::MethodDispatch);
            let args = PyTuple::new(
                slf.py(),
                [
                    copy.clone(),
                    target.as_any().clone(),
                    alpha.into_pyobject(slf.py())?.into_any(),
                ],
            )?;
            method_cache::call_cached1(mobject.as_any(), "interpolate", &args)?;
        }
        Ok(())
    }

    /// setup → construct → tear_down through Python MRO. tear_down runs even
    /// when construct raises; the original construct exception remains primary.
    fn _run_lifecycle(slf: &Bound<'_, Self>) -> PyResult<()> {
        crossing::record(CrossingClass::MethodDispatch);
        method_cache::call_cached0(slf.as_any(), "setup")?;
        crossing::record(CrossingClass::MethodDispatch);
        let construct = method_cache::call_cached0(slf.as_any(), "construct");
        crossing::record(CrossingClass::MethodDispatch);
        let teardown = method_cache::call_cached0(slf.as_any(), "tear_down");
        match (construct, teardown) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(_), Ok(_)) => Ok(()),
        }
    }

    /// Cut T2 (fm-d3gt): drive the engine's six-step play contract
    /// (`fmn_scene::Scene::play`) for MoveToTarget-shaped animations.
    /// Both proxies must already be arena-resident in this scene; the
    /// bootstrap adopts them beforehand. Rendering is a later tranche:
    /// captures flow through a frame-counting probe sink whose recorded
    /// alphas are returned (ordered, one per captured frame).
    /// A camera lerp (cut T3) may ride the segment: `camera` is the
    /// `(live_core, target_core)` pair; with no mobject pairs the segment
    /// is a native wait carrying the camera. State-exact at every capture
    /// boundary and set exactly to the target state at segment end.
    /// Each spec is `(kind, mobject, target, run_time, rate_func,
    /// lag_ratio, params)` and builds one native fmn-anim animation.
    /// Composition kinds (`animation_group`, `lagged_start`, `succession`)
    /// carry nested specs under `params["members"]` and the construction
    /// lag under `params["lag_ratio"]`; the native module owns the group
    /// timing derivation (`build_timings`, the Reference's rule).
    #[pyo3(signature = (specs, callbacks, camera, run_time, rate_func, lag_ratio))]
    fn _play_animations(
        slf: &Bound<'_, Self>,
        specs: Vec<Bound<'_, PyAny>>,
        callbacks: Vec<Option<Py<PyAny>>>,
        camera: Option<(Bound<'_, PyCameraFrameCore>, Bound<'_, PyCameraFrameCore>)>,
        run_time: Option<f64>,
        rate_func: Option<Bound<'_, PyAny>>,
        lag_ratio: Option<f64>,
    ) -> PyResult<Vec<f64>> {
        let engine = Rc::clone(&slf.borrow().engine);
        if callbacks.len() != specs.len() {
            return Err(PyValueError::new_err(
                "animation callback table must align one-for-one with specs",
            ));
        }
        let callback_count = callbacks.iter().flatten().count();
        if callback_count != 0 && callback_count != callbacks.len() {
            return Err(PyNotImplementedError::new_err(
                "mixing Python-authored and native mobject animations awaits \
                 the per-animation finish release; camera-frame animation may \
                 still accompany a Python-authored play",
            ));
        }

        // Resolve every Python-side value before the engine borrow.
        let play_rate = rate_func.as_ref().map(rate_func_from_py).transpose()?;
        let mut resolved = Vec::with_capacity(specs.len());
        for spec in &specs {
            resolved.push(parse_anim_spec(&engine, spec)?);
        }

        let release_for_python_updaters = has_python_updaters(slf)?;
        for callback in callbacks.iter().flatten() {
            crossing::record(CrossingClass::MethodDispatch);
            callback.bind(slf.py()).call_method0("begin")?;
        }
        let (animations, start_time) = {
            let mut scene = engine.borrow_mut();
            let start_time = scene.stage().time();
            let mut animations: Vec<Box<dyn fmn_anim::Animation>> =
                Vec::with_capacity(resolved.len());
            for spec in resolved {
                animations.push(build_native_animation(scene.stage_mut(), spec)?);
            }
            (animations, start_time)
        };
        let effective_run_time = run_time.unwrap_or(fmn_anim::DEFAULT_ANIMATION_RUN_TIME);
        let camera_lerp = camera
            .map(|(live, target)| -> PyResult<CameraLerp> {
                Ok(CameraLerp {
                    start: live.borrow().frame.clone(),
                    end: target.borrow().frame.clone(),
                    core: live.unbind(),
                    start_time,
                    run_time: effective_run_time,
                    rate: play_rate.clone().unwrap_or_default(),
                })
            })
            .transpose()?;
        let overrides = fmn_scene::PlayOverrides {
            run_time,
            rate_func: play_rate,
            lag_ratio,
        };
        let mut sink = PortalSceneSink {
            camera: camera_lerp,
            render: Arc::clone(&slf.borrow().render),
            ..PortalSceneSink::default()
        };
        let map_play_error = |error: fmn_scene::SceneError| {
            let text = error.to_string();
            if text.contains("become between records of different schemas") {
                // The engine's precise cross-schema refusal, plus the design
                // it awaits: revealing a sampled Surface is the Reference's
                // Surface.pointwise_become_partial u-slice mechanism — an
                // fmn-anim/fmn-mobject tranche, not a binding.
                PyRuntimeError::new_err(format!(
                    "{text}; revealing a sampled Surface awaits the engine's \
                     surface partial-reveal mechanism \
                     (Surface.pointwise_become_partial)"
                ))
            } else {
                PyRuntimeError::new_err(text)
            }
        };
        if animations.is_empty() {
            if sink.camera.is_none() {
                return Ok(Vec::new());
            }
            if release_for_python_updaters {
                let mut wait = engine
                    .borrow_mut()
                    .begin_stepped_wait(Some(effective_run_time), &mut sink)
                    .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
                let drive_result: PyResult<()> = (|| {
                    loop {
                        let release = engine
                            .borrow_mut()
                            .prepare_stepped_wait_frame(&mut wait)
                            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
                        let Some(release) = release else {
                            break;
                        };
                        sink.apply_camera_before_updaters(slf.py(), release.time.to_f64())?;
                        let python_ns = run_python_updaters(slf, release.dt)?;
                        let native_start = Instant::now();
                        engine
                            .borrow_mut()
                            .complete_stepped_wait_frame(&mut wait, &mut sink)
                            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
                        let native_ns =
                            u64::try_from(native_start.elapsed().as_nanos()).unwrap_or(u64::MAX);
                        crossing::record_phase(python_ns, native_ns);
                    }
                    sink.finish_camera_before_updaters(slf.py())?;
                    run_python_updaters(slf, 0.0)?;
                    engine.borrow_mut().stage_mut().update(0.0);
                    Ok(())
                })();
                if let Err(error) = drive_result {
                    engine.borrow_mut().abort_stepped_wait(wait, &mut sink);
                    return Err(error);
                }
                engine
                    .borrow_mut()
                    .finish_stepped_wait(wait, &mut sink)
                    .map(|_| ())
                    .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
            } else {
                engine
                    .borrow_mut()
                    .wait(Some(effective_run_time), &mut sink)
                    .map(|_| ())
                    .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
            }
        } else if release_for_python_updaters || callback_count != 0 {
            // Choreo stops after interpolation + rational clock advance. The
            // Scene RefCell borrow is then genuinely gone while Python
            // updaters run; completing the same frame performs native scene
            // updaters, event dispatch, immutable freeze, and capture.
            let mut play = engine
                .borrow_mut()
                .begin_stepped_play(animations, overrides, &mut sink)
                .map_err(&map_play_error)?
                .ok_or_else(|| PyRuntimeError::new_err("a nonempty play did not open"))?;
            let drive_result: PyResult<()> = (|| {
                loop {
                    if callback_count != 0 {
                        loop {
                            let animation = engine
                                .borrow_mut()
                                .prepare_stepped_play_animation(&mut play)
                                .map_err(&map_play_error)?;
                            let Some(animation) = animation else {
                                break;
                            };
                            let callback = callbacks[animation.animation_index]
                                .as_ref()
                                .ok_or_else(|| {
                                    PyRuntimeError::new_err(
                                        "a Python animation slot lost its callback",
                                    )
                                })?;
                            crossing::record(CrossingClass::MethodDispatch);
                            callback
                                .bind(slf.py())
                                .call_method1("interpolate", (animation.alpha,))?;
                        }
                    }
                    let release = engine
                        .borrow_mut()
                        .prepare_stepped_play_frame(&mut play)
                        .map_err(&map_play_error)?;
                    let Some(release) = release else {
                        break;
                    };
                    sink.apply_camera_before_updaters(slf.py(), release.time.to_f64())?;
                    let python_ns = run_python_updaters(slf, release.dt)?;
                    let native_start = Instant::now();
                    engine
                        .borrow_mut()
                        .complete_stepped_play_frame(&mut play, &mut sink)
                        .map_err(&map_play_error)?;
                    let native_ns =
                        u64::try_from(native_start.elapsed().as_nanos()).unwrap_or(u64::MAX);
                    crossing::record_phase(python_ns, native_ns);
                }
                for callback in callbacks.iter().flatten() {
                    crossing::record(CrossingClass::MethodDispatch);
                    callback.bind(slf.py()).call_method0("finish")?;
                }
                sink.finish_camera_before_updaters(slf.py())?;
                Ok(())
            })();
            if let Err(error) = drive_result {
                engine.borrow_mut().abort_stepped_play(play, &mut sink);
                return Err(error);
            }
            // Reference Scene.finish_animations first finishes every
            // animation (which resumes animation-suspended mobjects), then
            // performs one final update_mobjects(0). Split that updater pass
            // at the same host-language release boundary as ordinary frames:
            // Python first, native second, with no Scene borrow crossing the
            // callback.
            let resumed = match engine
                .borrow_mut()
                .finish_stepped_play_animations(&mut play)
                .map_err(&map_play_error)
            {
                Ok(resumed) => resumed,
                Err(error) => {
                    engine.borrow_mut().abort_stepped_play(play, &mut sink);
                    return Err(error);
                }
            };
            if let Err(error) = run_resumed_python_updaters(slf, &resumed) {
                engine.borrow_mut().abort_stepped_play(play, &mut sink);
                return Err(error);
            }
            if let Err(error) = run_python_updaters(slf, 0.0) {
                engine.borrow_mut().abort_stepped_play(play, &mut sink);
                return Err(error);
            }
            engine
                .borrow_mut()
                .finish_stepped_play(play, &mut sink)
                .map(|_| ())
                .map_err(&map_play_error)?;
        } else {
            engine
                .borrow_mut()
                .play(animations, overrides, &mut sink)
                .map(|_| ())
                .map_err(&map_play_error)?;
        }
        let PortalSceneSink {
            alphas,
            camera,
            camera_error,
            render: _,
            camera_preapplied: _,
            camera_finished_before_updaters,
        } = sink;
        if let Some(error) = camera_error {
            return Err(error);
        }
        if let Some(camera) = camera
            && !camera_finished_before_updaters
        {
            camera.finish_exact(slf.py())?;
        }
        Ok(alphas)
    }

    /// Engine-truth structural facts (the corpus baselines): draw-list
    /// root count, total family membership, and the aggregate family bbox
    /// (zero boxes skipped) — measured on the Stage directly, so the
    /// numbers never depend on Python proxy liveness or GC timing.
    #[allow(clippy::type_complexity)]
    fn _engine_facts(slf: &Bound<'_, Self>) -> PyResult<(usize, usize, [f64; 3], [f64; 3])> {
        let engine = Rc::clone(&slf.borrow().engine);
        let scene = engine.borrow();
        let stage = scene.stage();
        let roots = stage.roots().to_vec();
        let mut family_total = 0usize;
        let mut low = [f64::INFINITY; 3];
        let mut high = [f64::NEG_INFINITY; 3];
        let mut any_box = false;
        for &root in &roots {
            family_total += stage.family(root).len();
            let bbox = stage.get_bounding_box(root);
            if bbox.min == [0.0; 3] && bbox.max == [0.0; 3] {
                continue;
            }
            any_box = true;
            for axis in 0..3 {
                low[axis] = low[axis].min(bbox.min[axis]);
                high[axis] = high[axis].max(bbox.max[axis]);
            }
        }
        if !any_box {
            low = [0.0; 3];
            high = [0.0; 3];
        }
        Ok((roots.len(), family_total, low, high))
    }

    /// Adopt a detached mobject graph into this scene's arena WITHOUT
    /// adding it to the draw list — the `.animate` target seam.
    fn _adopt(slf: &Bound<'_, Self>, mobject: &Bound<'_, BridgeMobject>) -> PyResult<()> {
        bind_graph(slf.py(), slf, mobject)?;
        Ok(())
    }

    /// Attach a [`PyFieldProbe`] to a bound mobject: a native updater
    /// records `field[lane]` of record 0 every frame (diagnostics only).
    fn _record_field_probe(
        slf: &Bound<'_, Self>,
        mobject: &Bound<'_, BridgeMobject>,
        field: String,
        lane: usize,
    ) -> PyResult<PyFieldProbe> {
        let engine = Rc::clone(&slf.borrow().engine);
        let (mob_engine, mob) = bound_parts(&mobject.borrow())?;
        if !same_engine(&engine, &mob_engine) {
            return Err(ForeignStageError::new_err(
                "the probe target must belong to this Scene",
            ));
        }
        let values: Rc<RefCell<Vec<f64>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&values);
        engine
            .borrow_mut()
            .stage_mut()
            .add_updater(
                mob,
                move |stage: &mut fmn_mobject::Stage, target: Mob| {
                    // Animation begin-time copies carry updaters; record
                    // only the original entry, not its starting/target
                    // copies.
                    if target != mob {
                        return;
                    }
                    if let Some(entry) = stage.get(target)
                        && let Some(lanes) = entry.buffer.read(0, &field)
                        && let Some(value) = lanes.get(lane)
                    {
                        sink.borrow_mut().push(f64::from(*value));
                    }
                },
                false,
            )
            .map_err(stage_error)?;
        Ok(PyFieldProbe { values })
    }

    /// `Scene.wait(duration)` over the native wait segment. An active portal
    /// render generation captures every immutable frame through Lumen + Reel;
    /// ordinary lifecycle probes keep the same sink with output disabled.
    #[pyo3(signature = (duration = None))]
    fn _wait(slf: &Bound<'_, Self>, duration: Option<f64>) -> PyResult<()> {
        let engine = Rc::clone(&slf.borrow().engine);
        if !has_python_updaters(slf)? {
            let mut sink = PortalSceneSink {
                render: Arc::clone(&slf.borrow().render),
                ..PortalSceneSink::default()
            };
            return engine
                .borrow_mut()
                .wait(duration, &mut sink)
                .map(|_| ())
                .map_err(|error| PyRuntimeError::new_err(error.to_string()));
        }

        // The Reference performs one zero-dt Scene.update_mobjects pass
        // before planning wait frames. Run the Python half while unborrowed;
        // begin_stepped_wait immediately follows with the native half.
        run_python_updaters(slf, 0.0)?;
        let mut sink = PortalSceneSink {
            render: Arc::clone(&slf.borrow().render),
            ..PortalSceneSink::default()
        };
        let mut wait = engine
            .borrow_mut()
            .begin_stepped_wait(duration, &mut sink)
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
        let drive_result: PyResult<()> = (|| {
            loop {
                let release = engine
                    .borrow_mut()
                    .prepare_stepped_wait_frame(&mut wait)
                    .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
                let Some(release) = release else {
                    break;
                };
                let python_ns = run_python_updaters(slf, release.dt)?;
                let native_start = Instant::now();
                engine
                    .borrow_mut()
                    .complete_stepped_wait_frame(&mut wait, &mut sink)
                    .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
                let native_ns =
                    u64::try_from(native_start.elapsed().as_nanos()).unwrap_or(u64::MAX);
                crossing::record_phase(python_ns, native_ns);
            }
            Ok(())
        })();
        if let Err(error) = drive_result {
            engine.borrow_mut().abort_stepped_wait(wait, &mut sink);
            return Err(error);
        }
        engine
            .borrow_mut()
            .finish_stepped_wait(wait, &mut sink)
            .map(|_| ())
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))
    }

    fn _checkpoint_bytes(&self) -> PyResult<Vec<u8>> {
        self.engine
            .borrow_mut()
            .state_bytes()
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))
    }
}

/// The engine's named rate-function catalog (fmn-core's single-argument
/// rate functions, the same pointers Choreo composes). Parameterized and
/// arbitrary Python callables refuse precisely — a per-frame Python
/// rate_func is a crossing-budget decision for a later rung.
fn anim_error(error: fmn_anim::AnimError) -> PyErr {
    PyRuntimeError::new_err(error.to_string())
}

/// Timing/lifecycle slot for a top-level Python-authored Animation.
///
/// Its interpolation is intentionally empty: Choreo yields immediately after
/// this slot in step 2, the bridge releases the Scene borrow, and invokes the
/// Python animation at the slot's exact raw alpha before advancing to the next
/// animation. The slot still owns native animating/remover semantics and the
/// segment's authoritative timing.
struct PythonAnimationSlot {
    state: fmn_anim::AnimState,
}

impl PythonAnimationSlot {
    fn new(mobject: Mob, remover: bool) -> Self {
        let mut config = fmn_anim::AnimConfig {
            name: "PythonAnimationSlot".to_owned(),
            ..fmn_anim::AnimConfig::default()
        };
        config.remover = remover;
        Self {
            state: fmn_anim::AnimState::new(mobject, config),
        }
    }
}

impl fmn_anim::Animation for PythonAnimationSlot {
    fn state(&self) -> &fmn_anim::AnimState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut fmn_anim::AnimState {
        &mut self.state
    }

    fn interpolate_submobject(
        &mut self,
        _stage: &mut fmn_mobject::Stage,
        _mobs: &[Mob],
        _sub_alpha: f64,
    ) {
    }
}

/// One parsed play spec: a leaf animation or a composition carrying
/// nested members (fm-d3gt, the explicit-animation seam).
struct AnimSpec {
    kind: String,
    mob: Option<Mob>,
    target: Option<Mob>,
    run_time: Option<f64>,
    rate: Option<fmn_anim::RateFunc>,
    lag: Option<f64>,
    shift: [f64; 3],
    scale: f64,
    angle: f64,
    axis: [f64; 3],
    about_point: Option<[f64; 3]>,
    about_edge: [f64; 3],
    about_edge_opt: Option<[f64; 3]>,
    path_arc: f64,
    path_arc_axis: [f64; 3],
    stroke_color: Option<[f64; 3]>,
    stroke_width: Option<f64>,
    bounds_kind: String,
    mobs: Vec<Mob>,
    pairs: Vec<(Mob, Mob)>,
    int_round: String,
    point: [f64; 3],
    point_color: Option<[f64; 3]>,
    color: Option<[f64; 3]>,
    scale_factor: f64,
    scale_value: f64,
    rotation_angle: f64,
    n_wiggles: f64,
    scale_about_point: Option<[f64; 3]>,
    rotate_about_point: Option<[f64; 3]>,
    time_width: f64,
    taper_width: f64,
    direction: [f64; 3],
    amplitude: f64,
    time_span: Option<(f64, f64)>,
    group_lag: f64,
    remover: bool,
    suspend_mobject_updating: bool,
    surface_resolution: (usize, usize),
    surface_axis: usize,
    source_keys: Vec<(Mob, String)>,
    target_keys: Vec<(Mob, String)>,
    members: Vec<AnimSpec>,
}

#[allow(clippy::type_complexity)]
fn parse_anim_spec(engine: &Engine, spec: &Bound<'_, PyAny>) -> PyResult<AnimSpec> {
    let (kind, mobject, target, run_time, rate, lag, params): (
        String,
        Option<Bound<'_, BridgeMobject>>,
        Option<Bound<'_, BridgeMobject>>,
        Option<f64>,
        Option<Bound<'_, PyAny>>,
        Option<f64>,
        Bound<'_, PyDict>,
    ) = spec.extract()?;
    let rate = rate.as_ref().map(rate_func_from_py).transpose()?;
    let resolve = |proxy: &Bound<'_, BridgeMobject>| -> PyResult<Mob> {
        let (mob_engine, mob) = bound_parts(&proxy.borrow())?;
        if same_engine(engine, &mob_engine) {
            Ok(mob)
        } else {
            Err(ForeignStageError::new_err(
                "play endpoints must belong to this Scene",
            ))
        }
    };
    let get3 = |name: &str, default: [f64; 3]| -> PyResult<[f64; 3]> {
        match params.get_item(name)? {
            Some(value) => Ok(value.extract()?),
            None => Ok(default),
        }
    };
    let get1 = |name: &str, default: f64| -> PyResult<f64> {
        match params.get_item(name)? {
            Some(value) => Ok(value.extract()?),
            None => Ok(default),
        }
    };
    let get_bool = |name: &str, default: bool| -> PyResult<bool> {
        match params.get_item(name)? {
            Some(value) => value.extract(),
            None => Ok(default),
        }
    };
    let get_usize = |name: &str, default: usize| -> PyResult<usize> {
        match params.get_item(name)? {
            Some(value) => value.extract(),
            None => Ok(default),
        }
    };
    let get_usize_pair = |name: &str, default: (usize, usize)| -> PyResult<(usize, usize)> {
        match params.get_item(name)? {
            Some(value) => value.extract(),
            None => Ok(default),
        }
    };
    let get_keyed_parts = |name: &str| -> PyResult<Vec<(Mob, String)>> {
        let Some(values) = params.get_item(name)? else {
            return Ok(Vec::new());
        };
        values
            .try_iter()?
            .map(|item| {
                let (proxy, key): (Bound<'_, BridgeMobject>, String) = item?.extract()?;
                Ok((resolve(&proxy)?, key))
            })
            .collect()
    };
    let members = match params.get_item("members")? {
        Some(list) => list
            .try_iter()?
            .map(|item| parse_anim_spec(engine, &item?))
            .collect::<PyResult<Vec<_>>>()?,
        None => Vec::new(),
    };
    Ok(AnimSpec {
        kind,
        mob: mobject.as_ref().map(&resolve).transpose()?,
        target: target.as_ref().map(&resolve).transpose()?,
        run_time,
        rate,
        lag,
        shift: get3("shift", [0.0; 3])?,
        scale: get1("scale", 1.0)?,
        angle: get1("angle", std::f64::consts::PI)?,
        axis: get3("axis", [0.0, 0.0, 1.0])?,
        about_point: params
            .get_item("about_point")?
            .map(|value| value.extract())
            .transpose()?,
        about_edge: get3("about_edge", [0.0; 3])?,
        about_edge_opt: params
            .get_item("about_edge")?
            .map(|value| value.extract())
            .transpose()?,
        path_arc: get1("path_arc", 0.0)?,
        path_arc_axis: get3("path_arc_axis", [0.0, 0.0, 1.0])?,
        stroke_color: params
            .get_item("stroke_color")?
            .map(|value| value.extract())
            .transpose()?,
        stroke_width: params
            .get_item("stroke_width")?
            .map(|value| value.extract())
            .transpose()?,
        bounds_kind: match params.get_item("bounds_kind")? {
            Some(value) => value.extract()?,
            None => String::new(),
        },
        mobs: match params.get_item("mobs")? {
            Some(values) => values
                .try_iter()?
                .map(|item| {
                    let proxy: Bound<'_, BridgeMobject> = item?.extract()?;
                    resolve(&proxy)
                })
                .collect::<PyResult<Vec<_>>>()?,
            None => Vec::new(),
        },
        pairs: match params.get_item("matched_pairs")? {
            Some(values) => values
                .try_iter()?
                .map(|item| {
                    let (a, b): (Bound<'_, BridgeMobject>, Bound<'_, BridgeMobject>) =
                        item?.extract()?;
                    Ok((resolve(&a)?, resolve(&b)?))
                })
                .collect::<PyResult<Vec<_>>>()?,
            None => Vec::new(),
        },
        int_round: match params.get_item("int_round")? {
            Some(value) => value.extract()?,
            None => String::new(),
        },
        point: get3("point", [0.0; 3])?,
        point_color: params
            .get_item("point_color")?
            .map(|value| value.extract())
            .transpose()?,
        color: params
            .get_item("color")?
            .map(|value| value.extract())
            .transpose()?,
        scale_factor: get1("scale_factor", 1.2)?,
        scale_value: get1("scale_value", 1.1)?,
        rotation_angle: get1("rotation_angle", 0.01 * std::f64::consts::TAU)?,
        n_wiggles: get1("n_wiggles", 6.0)?,
        scale_about_point: params
            .get_item("scale_about_point")?
            .map(|value| value.extract())
            .transpose()?,
        rotate_about_point: params
            .get_item("rotate_about_point")?
            .map(|value| value.extract())
            .transpose()?,
        time_width: get1("time_width", 0.3)?,
        taper_width: get1("taper_width", 0.05)?,
        direction: get3("direction", [0.0, 1.0, 0.0])?,
        amplitude: get1("amplitude", 0.2)?,
        time_span: params
            .get_item("time_span")?
            .map(|value| value.extract())
            .transpose()?,
        group_lag: get1("lag_ratio", 0.0)?,
        remover: get_bool("remover", false)?,
        suspend_mobject_updating: get_bool("suspend_mobject_updating", false)?,
        surface_resolution: get_usize_pair("surface_resolution", (0, 0))?,
        surface_axis: get_usize("surface_axis", 1)?,
        source_keys: get_keyed_parts("source_keys")?,
        target_keys: get_keyed_parts("target_keys")?,
        members,
    })
}

/// Build one native animation from a parsed spec, recursing into
/// composition members. The native constructors own all timing math.
#[allow(clippy::too_many_lines)]
fn build_native_animation(
    stage: &mut fmn_mobject::Stage,
    spec: AnimSpec,
) -> PyResult<Box<dyn fmn_anim::Animation>> {
    let need_target = |target: Option<Mob>| {
        target.ok_or_else(|| PyValueError::new_err("this animation requires a target"))
    };
    let need_mob = |mob: Option<Mob>| {
        mob.ok_or_else(|| PyValueError::new_err("this animation requires a mobject"))
    };
    let is_composition = matches!(
        spec.kind.as_str(),
        "animation_group"
            | "broadcast"
            | "cyclic_replace"
            | "swap"
            | "flash"
            | "flashy_fade_in"
            | "lagged_start"
            | "show_creation_then_destruction_around"
            | "show_creation_then_fade_around"
            | "show_creation_then_fade_out"
            | "succession"
            | "transform_matching_parts"
            | "transform_matching_shapes"
            | "transform_matching_strings"
            | "transform_matching_tex"
    );
    let mut animation: Box<dyn fmn_anim::Animation> = match spec.kind.as_str() {
        "python_callback" => Box::new(PythonAnimationSlot::new(need_mob(spec.mob)?, spec.remover)),
        "animation_group" | "lagged_start" => {
            let mut members = Vec::with_capacity(spec.members.len());
            for member in spec.members {
                members.push(build_native_animation(stage, member)?);
            }
            let mut group =
                fmn_anim::AnimationGroup::with_lag_ratio(stage, members, spec.group_lag)
                    .map_err(anim_error)?;
            if spec.kind == "lagged_start" {
                group = group.with_name("LaggedStart");
            }
            Box::new(group)
        }
        "succession" => {
            let mut members = Vec::with_capacity(spec.members.len());
            for member in spec.members {
                members.push(build_native_animation(stage, member)?);
            }
            Box::new(
                fmn_anim::Succession::with_lag_ratio(stage, members, spec.group_lag)
                    .map_err(anim_error)?,
            )
        }
        "flash" => {
            let mut members = Vec::with_capacity(spec.members.len());
            for member in spec.members {
                members.push(build_native_animation(stage, member)?);
            }
            Box::new(
                fmn_anim::flash(stage, members, spec.group_lag).map_err(anim_error)?,
            )
        }
        "show_creation_then_fade_out" => {
            let mut members = Vec::with_capacity(spec.members.len());
            for member in spec.members {
                members.push(build_native_animation(stage, member)?);
            }
            Box::new(
                fmn_anim::show_creation_then_fade_out(
                    stage,
                    members,
                    spec.group_lag,
                    spec.remover,
                )
                .map_err(anim_error)?,
            )
        }
        "broadcast"
        | "flashy_fade_in"
        | "show_creation_then_destruction_around"
        | "show_creation_then_fade_around" => {
            let mut members = Vec::with_capacity(spec.members.len());
            for member in spec.members {
                members.push(build_native_animation(stage, member)?);
            }
            let name = match spec.kind.as_str() {
                "broadcast" => "Broadcast",
                "flashy_fade_in" => "FlashyFadeIn",
                "show_creation_then_destruction_around" => {
                    "ShowCreationThenDestructionAround"
                }
                "show_creation_then_fade_around" => "ShowCreationThenFadeAround",
                _ => unreachable!(),
            };
            let mut group = fmn_anim::AnimationGroup::with_lag_ratio(
                stage,
                members,
                spec.group_lag,
            )
            .map_err(anim_error)?
            .with_name(name);
            fmn_anim::Animation::state_mut(&mut group).config.remover = spec.remover;
            Box::new(group)
        }
        "transform_matching_tex" | "transform_matching_strings" => {
            let members = fmn_anim::transform_matching_keys(
                stage,
                need_mob(spec.mob)?,
                need_target(spec.target)?,
                &spec.source_keys,
                &spec.target_keys,
            )
            .map_err(anim_error)?;
            let name = if spec.kind == "transform_matching_strings" {
                "TransformMatchingStrings"
            } else {
                "TransformMatchingTex"
            };
            Box::new(
                fmn_anim::AnimationGroup::new(stage, members)
                    .map_err(anim_error)?
                    .with_name(name),
            )
        }
        "transform" => {
            let mut transform =
                fmn_anim::Transform::new(need_mob(spec.mob)?, need_target(spec.target)?);
            if spec.path_arc != 0.0 {
                transform = transform.with_path_arc(spec.path_arc, spec.path_arc_axis);
            }
            Box::new(transform)
        }
        "replacement_transform" => Box::new(fmn_anim::replacement_transform(
            need_mob(spec.mob)?,
            need_target(spec.target)?,
        )),
        "transform_from_copy" => Box::new(
            fmn_anim::transform_from_copy(stage, need_mob(spec.mob)?, need_target(spec.target)?)
                .map_err(anim_error)?,
        ),
        "fade_in" => Box::new(
            fmn_anim::fade_in(stage, need_mob(spec.mob)?, spec.shift, spec.scale)
                .map_err(anim_error)?,
        ),
        "fade_out" => Box::new(
            fmn_anim::fade_out(stage, need_mob(spec.mob)?, spec.shift, spec.scale)
                .map_err(anim_error)?,
        ),
        "fade_in_from_point" => Box::new(
            fmn_anim::fade_in_from_point(stage, need_mob(spec.mob)?, spec.point)
                .map_err(anim_error)?,
        ),
        "fade_out_to_point" => Box::new(
            fmn_anim::fade_out_to_point(stage, need_mob(spec.mob)?, spec.point)
                .map_err(anim_error)?,
        ),
        "v_fade_in" => Box::new(fmn_anim::v_fade_in(need_mob(spec.mob)?)),
        "v_fade_out" => Box::new(fmn_anim::v_fade_out(need_mob(spec.mob)?)),
        "v_fade_in_then_out" => {
            Box::new(fmn_anim::v_fade_in_then_out(need_mob(spec.mob)?))
        }
        "show_creation" => Box::new(fmn_anim::show_creation(need_mob(spec.mob)?)),
        "show_surface_creation" => Box::new(fmn_anim::show_surface_creation(
            need_mob(spec.mob)?,
            spec.surface_resolution,
            spec.surface_axis,
        )),
        "uncreate" => Box::new(fmn_anim::uncreate(need_mob(spec.mob)?)),
        "uncreate_surface" => Box::new(fmn_anim::uncreate_surface(
            need_mob(spec.mob)?,
            spec.surface_resolution,
            spec.surface_axis,
        )),
        "maintain_position_relative_to" => Box::new(fmn_anim::MaintainPositionRelativeTo::new(
            // fm-5wq.4.62: update.py:53 — the construction-time offset is
            // captured here, at spec build, exactly the Reference's
            // `self.diff = mobject.get_center() - tracked.get_center()`.
            stage,
            need_mob(spec.mob)?,
            need_target(spec.target)?,
        )),
        "show_increasing_subsets" | "show_submobjects_one_by_one" => {
            // fm-5wq.4.58: Choreo's subset reveal. The constructors seed
            // the Reference default rounding; an explicit `int_func` from
            // the bootstrap overrides it as data.
            let mut subsets = if spec.kind == "show_submobjects_one_by_one" {
                fmn_anim::show_submobjects_one_by_one(stage, need_mob(spec.mob)?)
            } else {
                fmn_anim::show_increasing_subsets(stage, need_mob(spec.mob)?)
            }
            .map_err(anim_error)?;
            match spec.int_round.as_str() {
                "" => {}
                "round" => subsets = subsets.with_int_round(fmn_anim::IntRound::Round),
                "ceil" => subsets = subsets.with_int_round(fmn_anim::IntRound::Ceil),
                other => {
                    return Err(PyValueError::new_err(format!(
                        "int_func {other:?} is not a native rounding rule \
                         (np.round or np.ceil)"
                    )));
                }
            }
            Box::new(subsets)
        }
        "write" => {
            let mut write = fmn_anim::write(stage, need_mob(spec.mob)?);
            if let Some(rgb) = spec.stroke_color {
                #[allow(clippy::cast_possible_truncation)]
                let rgb = [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32];
                write = write.with_stroke_color(Some(rgb));
            }
            Box::new(write)
        }
        "draw_border_then_fill" => {
            let mut border = fmn_anim::DrawBorderThenFill::new(need_mob(spec.mob)?);
            if let Some(width) = spec.stroke_width {
                border = border.with_stroke_width(width);
            }
            if let Some(rgb) = spec.stroke_color {
                #[allow(clippy::cast_possible_truncation)]
                let rgb = [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32];
                border = border.with_stroke_color(Some(rgb));
            }
            Box::new(border)
        }
        "show_partial" => {
            // The two Reference bounds vocabularies; the bootstrap
            // classifies a subclass's `get_bounds` into one of them and
            // refuses anything else before the spec is built.
            let bounds = match spec.bounds_kind.as_str() {
                "creation" => fmn_anim::RevealBounds::Creation,
                "passing_flash" => fmn_anim::RevealBounds::PassingFlash {
                    time_width: spec.time_width,
                },
                other => {
                    return Err(PyValueError::new_err(format!(
                        "ShowPartial bounds rule `{other}` is not a native reveal rule"
                    )));
                }
            };
            Box::new(fmn_anim::ShowPartial::new(need_mob(spec.mob)?, bounds))
        }
        "move_along_path" => Box::new(fmn_anim::MoveAlongPath::new(
            need_mob(spec.mob)?,
            need_target(spec.target)?,
        )),
        "transform_matching_parts" | "transform_matching_shapes" => {
            let members = fmn_anim::transform_matching_parts(
                stage,
                need_mob(spec.mob)?,
                need_target(spec.target)?,
                &spec.pairs,
            )
            .map_err(anim_error)?;
            let name = if spec.kind == "transform_matching_shapes" {
                "TransformMatchingShapes"
            } else {
                "TransformMatchingParts"
            };
            Box::new(
                fmn_anim::AnimationGroup::new(stage, members)
                    .map_err(anim_error)?
                    .with_name(name),
            )
        }
        "cyclic_replace" | "swap" => {
            let name = if spec.kind == "swap" {
                "Swap"
            } else {
                "CyclicReplace"
            };
            let transforms = fmn_anim::cyclic_replace(stage, &spec.mobs, spec.path_arc)
                .map_err(anim_error)?;
            let members: Vec<Box<dyn fmn_anim::Animation>> = transforms
                .into_iter()
                .map(|mut transform| {
                    fmn_anim::Animation::state_mut(&mut transform).config.name = name.to_owned();
                    Box::new(transform) as Box<dyn fmn_anim::Animation>
                })
                .collect();
            Box::new(
                fmn_anim::AnimationGroup::new(stage, members)
                    .map_err(anim_error)?
                    .with_name(name),
            )
        }
        "rotate" => {
            let mut rotating = fmn_anim::rotate(need_mob(spec.mob)?, spec.angle)
                .with_axis(spec.axis)
                .with_about_edge(spec.about_edge);
            if let Some(point) = spec.about_point {
                rotating = rotating.with_about_point(point);
            }
            Box::new(rotating)
        }
        "rotating" => {
            // Rotating's Reference defaults (TAU, 5 s, linear) live in the
            // native constructor; both pivots stay None unless given,
            // exactly the Reference's signature.
            let mut rotating = fmn_anim::Rotating::new(need_mob(spec.mob)?)
                .with_angle(spec.angle)
                .with_axis(spec.axis);
            if let Some(point) = spec.about_point {
                rotating = rotating.with_about_point(point);
            }
            if let Some(edge) = spec.about_edge_opt {
                rotating = rotating.with_about_edge(edge);
            }
            Box::new(rotating)
        }
        "grow_from_point"
        | "grow_from_center"
        | "grow_from_edge"
        | "grow_arrow"
        | "spin_in_from_nothing" => {
            #[allow(clippy::cast_possible_truncation)]
            let point_color = spec
                .point_color
                .map(|rgb| [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32]);
            let mut growing =
                fmn_anim::grow_from_point(stage, need_mob(spec.mob)?, spec.point, point_color)
                    .map_err(anim_error)?;
            if spec.path_arc != 0.0 {
                growing = growing.with_path_arc(spec.path_arc, spec.path_arc_axis);
            }
            fmn_anim::Animation::state_mut(&mut growing).config.name = match spec.kind.as_str() {
                "grow_from_center" => "GrowFromCenter",
                "grow_from_edge" => "GrowFromEdge",
                "grow_arrow" => "GrowArrow",
                "spin_in_from_nothing" => "SpinInFromNothing",
                _ => "GrowFromPoint",
            }
            .to_owned();
            Box::new(growing)
        }
        "focus_on" => Box::new(fmn_anim::focus_on(
            need_mob(spec.mob)?,
            need_target(spec.target)?,
            spec.remover,
        )),
        "indicate" => {
            #[allow(clippy::cast_possible_truncation)]
            let color = spec
                .color
                .map(|rgb| [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32]);
            Box::new(
                fmn_anim::indicate(stage, need_mob(spec.mob)?, spec.scale_factor, color)
                .map_err(anim_error)?,
            )
        }
        "circle_indicate" => Box::new(fmn_anim::circle_indicate(
            need_mob(spec.mob)?,
            need_target(spec.target)?,
            spec.remover,
        )),
        "turn_inside_out" => Box::new(
            fmn_anim::turn_inside_out(stage, need_mob(spec.mob)?, spec.path_arc)
                .map_err(anim_error)?,
        ),
        "wiggle_out_then_in" => {
            let mut wiggle = fmn_anim::WiggleOutThenIn::new(need_mob(spec.mob)?)
                .with_scale_value(spec.scale_value)
                .with_rotation_angle(spec.rotation_angle)
                .with_n_wiggles(spec.n_wiggles);
            if let Some(point) = spec.scale_about_point {
                wiggle = wiggle.with_scale_about_point(point);
            }
            if let Some(point) = spec.rotate_about_point {
                wiggle = wiggle.with_rotate_about_point(point);
            }
            Box::new(wiggle)
        }
        "show_passing_flash" | "show_creation_then_destruction" => {
            let mut flash = fmn_anim::show_passing_flash(need_mob(spec.mob)?, spec.time_width);
            if spec.kind == "show_creation_then_destruction" {
                fmn_anim::Animation::state_mut(&mut flash).config.name =
                    "ShowCreationThenDestruction".to_owned();
            }
            Box::new(flash)
        }
        "v_show_passing_flash" => Box::new(
            fmn_anim::VShowPassingFlash::new(need_mob(spec.mob)?)
                .with_time_width(spec.time_width)
                .with_taper_width(spec.taper_width),
        ),
        "apply_wave" => Box::new(fmn_anim::apply_wave(
            stage,
            need_mob(spec.mob)?,
            spec.direction,
            spec.amplitude,
        )),
        "fade_transform" => Box::new(
            fmn_anim::fade_transform(stage, need_mob(spec.mob)?, need_target(spec.target)?)
                .map_err(anim_error)?,
        ),
        "fade_transform_pieces" => Box::new(
            fmn_anim::fade_transform_pieces(
                stage,
                need_mob(spec.mob)?,
                need_target(spec.target)?,
            )
            .map_err(anim_error)?,
        ),
        "restore" => {
            let mut restore = fmn_anim::restore(stage, need_mob(spec.mob)?).map_err(anim_error)?;
            if spec.path_arc != 0.0 {
                restore = restore.with_path_arc(spec.path_arc, spec.path_arc_axis);
            }
            fmn_anim::Animation::state_mut(&mut restore)
                .config
                .remover = spec.remover;
            Box::new(restore)
        }
        other => {
            return Err(PyValueError::new_err(format!(
                "animation kind `{other}` is not routed to the native shelf"
            )));
        }
    };
    {
        let config = &mut animation.state_mut().config;
        if let Some(value) = spec.run_time {
            config.run_time = value;
        }
        if let Some(rate) = spec.rate {
            config.rate_func = rate;
        }
        if let Some(value) = spec.lag
            && !is_composition
        {
            config.lag_ratio = value;
        }
        if let Some(span) = spec.time_span {
            config.time_span = Some(span);
        }
        config.suspend_mobject_updating = spec.suspend_mobject_updating;
    }
    Ok(animation)
}

/// A play-surface rate value: a catalog NAME, or a pre-sampled curve (a
/// sequence of at least two floats on the uniform `[0, 1]` grid) — the
/// bootstrap samples pure Python callables into the latter before the
/// segment runs, so no interpreter crossing ever happens mid-segment.
fn rate_func_from_py(value: &Bound<'_, PyAny>) -> PyResult<fmn_anim::RateFunc> {
    if let Ok(name) = value.extract::<String>() {
        return named_rate_func(&name);
    }
    let samples: Vec<f64> = value.extract().map_err(|_| {
        PyTypeError::new_err("rate_func must be a catalog name or a pre-sampled sequence of floats")
    })?;
    if samples.len() < 2 {
        return Err(PyValueError::new_err(
            "a sampled rate curve needs at least two samples",
        ));
    }
    if samples.iter().any(|sample| !sample.is_finite()) {
        return Err(PyValueError::new_err(
            "a sampled rate curve must be finite everywhere",
        ));
    }
    Ok(fmn_anim::RateFunc::Sampled(samples.into()))
}

fn named_rate_func(name: &str) -> PyResult<fmn_anim::RateFunc> {
    let function: fn(f64) -> f64 = match name {
        "linear" => fmn_core::rate::linear,
        "smooth" => fmn_core::rate::smooth,
        "rush_into" => fmn_core::rate::rush_into,
        "rush_from" => fmn_core::rate::rush_from,
        "slow_into" => fmn_core::rate::slow_into,
        "double_smooth" => fmn_core::rate::double_smooth,
        "there_and_back" => fmn_core::rate::there_and_back,
        "lingering" => fmn_core::rate::lingering,
        other => {
            return Err(PyValueError::new_err(format!(
                "rate_func `{other}` is not in the engine's named catalog \
                 (linear, smooth, rush_into, rush_from, slow_into, \
                 double_smooth, there_and_back, lingering); parameterized \
                 and custom callables await the crossing-budget rung"
            )));
        }
    };
    Ok(fmn_anim::RateFunc::Base(function))
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    (1.0 - t) * a + t * b
}

/// Cut T3 (fm-d3gt): the camera-frame interpolation a play segment
/// carries. The camera core is its OWN pyclass cell, independent of the
/// Scene RefCell the segment driver holds — so per-frame interpolation
/// inside the sink never crosses the live engine borrow, and no Python
/// dispatch happens mid-segment (the lerp is pure Rust). Semantics mirror
/// the Reference's frame Transform: componentwise quaternion lerp
/// (normalized on write, scipy's own read-side rule), linear
/// center/shape/fovy.
struct CameraLerp {
    core: Py<PyCameraFrameCore>,
    start: fmn_scene::studio_bridge::CameraFrame,
    end: fmn_scene::studio_bridge::CameraFrame,
    start_time: f64,
    run_time: f64,
    rate: fmn_anim::RateFunc,
}

impl CameraLerp {
    fn apply(&self, py: Python<'_>, raw_alpha: f64) -> PyResult<()> {
        let alpha = self.rate.eval(raw_alpha.clamp(0.0, 1.0));
        let mut core = self.core.bind(py).borrow_mut();
        let (start, end) = (&self.start, &self.end);
        let center = [
            lerp(start.center()[0], end.center()[0], alpha),
            lerp(start.center()[1], end.center()[1], alpha),
            lerp(start.center()[2], end.center()[2], alpha),
        ];
        let shape = [
            lerp(start.shape()[0], end.shape()[0], alpha),
            lerp(start.shape()[1], end.shape()[1], alpha),
        ];
        let fovy = lerp(start.field_of_view(), end.field_of_view(), alpha);
        let orientation = [
            lerp(start.orientation()[0], end.orientation()[0], alpha),
            lerp(start.orientation()[1], end.orientation()[1], alpha),
            lerp(start.orientation()[2], end.orientation()[2], alpha),
            lerp(start.orientation()[3], end.orientation()[3], alpha),
        ];
        core.frame.set_center(center).map_err(camera_error)?;
        core.frame.set_shape(shape).map_err(camera_error)?;
        core.frame.set_field_of_view(fovy).map_err(camera_error)?;
        core.frame
            .set_orientation(orientation)
            .map_err(camera_error)?;
        Ok(())
    }

    fn finish_exact(&self, py: Python<'_>) -> PyResult<()> {
        let mut core = self.core.bind(py).borrow_mut();
        core.frame = self.end.clone();
        Ok(())
    }
}

/// The portal's lifecycle sink: validates and records capture alphas, applies
/// camera interpolation, and forwards immutable packets into an optional
/// production Lumen/Reel generation.
#[derive(Default)]
struct PortalSceneSink {
    alphas: Vec<f64>,
    camera: Option<CameraLerp>,
    camera_error: Option<PyErr>,
    render: Arc<Mutex<Option<PortalRenderSession>>>,
    camera_preapplied: bool,
    camera_finished_before_updaters: bool,
}

impl PortalSceneSink {
    fn apply_camera_at_time(&self, py: Python<'_>, time: f64) -> PyResult<()> {
        let Some(camera) = &self.camera else {
            return Ok(());
        };
        let raw = if camera.run_time > 0.0 {
            (time - camera.start_time) / camera.run_time
        } else {
            1.0
        };
        camera.apply(py, raw)
    }

    fn apply_camera_before_updaters(&mut self, py: Python<'_>, time: f64) -> PyResult<()> {
        self.apply_camera_at_time(py, time)?;
        self.camera_preapplied = true;
        Ok(())
    }

    fn finish_camera_before_updaters(&mut self, py: Python<'_>) -> PyResult<()> {
        if let Some(camera) = &self.camera {
            camera.finish_exact(py)?;
            self.camera_finished_before_updaters = true;
        }
        Ok(())
    }
}

impl fmn_scene::SceneSink for PortalSceneSink {
    fn capture(
        &mut self,
        _reason: fmn_scene::CaptureReason,
        packet: fmn_scene::studio_bridge::FramePacket,
    ) -> Result<(), fmn_scene::IntegrationError> {
        self.alphas.push(packet.alpha());
        if self.camera.is_some() && self.camera_error.is_none() && !self.camera_preapplied {
            // The GIL is held by the pymethod driving this segment;
            // attach re-enters it. Only the camera core cell is borrowed
            // — never the Scene.
            Python::attach(|py| {
                if let Err(error) = self.apply_camera_at_time(py, packet.time().to_f64()) {
                    self.camera_error = Some(error);
                }
            });
        }
        let mut render = self.render.lock().map_err(|_| {
            fmn_scene::IntegrationError::new("portal-render", "render session lock was poisoned")
        })?;
        if let Some(session) = render.as_mut() {
            session.capture(packet)?;
        }
        Ok(())
    }
}

/// A per-frame record-field recorder (test/diagnostic seam): a NATIVE
/// stage updater appends one lane of a mobject's first record at every
/// frame update — inside the six-step update slot, no Python crossing,
/// so it may observe an engine-driven play segment mid-flight.
#[pyclass(unsendable, name = "_FieldProbe")]
struct PyFieldProbe {
    values: Rc<RefCell<Vec<f64>>>,
}

#[pymethods]
impl PyFieldProbe {
    fn values(&self) -> Vec<f64> {
        self.values.borrow().clone()
    }
}

/// Deterministic GIL-release verification probe (fm-zoi §17.4).
///
/// Holds only `Arc` atomics, so it is `Send` and usable from any Python
/// thread. The intended protocol (see tests/bridge.py):
///
/// 1. a Python worker thread spins on [`PyGilProbe::native_started`], then
///    calls [`PyGilProbe::tick`] in a loop — each tick requires the GIL;
/// 2. the main thread calls [`PyGilProbe::run_native`], which flips
///    `started` and runs a deterministic native kernel with the GIL
///    RELEASED (`Python::detach`), returning the tick count observed at
///    kernel end;
/// 3. observed > 0 proves the interpreter made progress during a long
///    native wait. If the GIL were held across the kernel, the worker could
///    never tick after `started` and the probe deterministically returns 0.
///
/// No wall-clock assertions anywhere: termination depends only on the fixed
/// work-unit count, and the pass/fail signal is a counter.
#[pyclass(name = "_GilProbe")]
struct PyGilProbe {
    progress: Arc<AtomicUsize>,
    started: Arc<AtomicBool>,
}

#[pymethods]
impl PyGilProbe {
    #[new]
    fn new() -> Self {
        Self {
            progress: Arc::new(AtomicUsize::new(0)),
            started: Arc::new(AtomicBool::new(false)),
        }
    }

    /// One unit of Python-thread progress. Requires the GIL to execute —
    /// that is the point of the probe.
    fn tick(&self) {
        self.progress.fetch_add(1, Ordering::Relaxed);
    }

    /// Ticks observed so far.
    fn observed(&self) -> usize {
        self.progress.load(Ordering::Acquire)
    }

    /// Whether the native kernel has begun (the worker waits for this).
    fn native_started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }

    /// Run `work_units` iterations of a deterministic native kernel with the
    /// GIL released; return the number of Python ticks observed at the end.
    /// This is the seam shape every long native wait (compilation,
    /// rasterization, conversion, output) uses: owned `Send` state in,
    /// `py.detach`, owned result out.
    fn run_native(&self, py: Python<'_>, work_units: u64) -> usize {
        let progress = Arc::clone(&self.progress);
        let started = Arc::clone(&self.started);
        py.detach(move || {
            started.store(true, Ordering::Release);
            // SplitMix64-style mixing; black_box keeps the kernel honest
            // (un-elidable) while remaining fully deterministic.
            let mut acc = 0x9E37_79B9_7F4A_7C15_u64;
            for i in 0..work_units {
                acc = acc.wrapping_add(i).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                acc ^= acc >> 31;
                std::hint::black_box(acc);
            }
            progress.load(Ordering::Acquire)
        })
    }
}

// --------------------------------------------------------------------------
// The native-builder seam (fm-d3gt): designated manimlib classes construct
// by calling an fmn-library builder, whose built `fmn_mobject::Mobject`
// family is split across proxy nurseries — the root's own records replace
// the constructing proxy's nursery, and every descendant becomes a fresh
// factory-made shell hung on the Python family list. Native geometry is the
// ONE implementation (D4); the bootstrap never re-derives point math.

thread_local! {
    /// The bundled typesetting handle numbered builders take, parsed once
    /// per interpreter thread (the worker is single-threaded by design).
    static FONT_BOOK: std::cell::OnceCell<fmn_library::FontBook> =
        const { std::cell::OnceCell::new() };
}

thread_local! {
    /// The math-typesetting engine over the default fmd-math pack,
    /// constructed once per interpreter thread like [`FONT_BOOK`].
    static TEX_ENGINE: std::cell::OnceCell<fmn_library::TexEngine> =
        const { std::cell::OnceCell::new() };
}

fn with_tex_engine<T>(
    operation: impl FnOnce(&fmn_library::TexEngine) -> PyResult<T>,
) -> PyResult<T> {
    TEX_ENGINE.with(|cell| {
        if cell.get().is_none() {
            let engine =
                fmn_library::TexEngine::new("fmd-math/pack/default", None).map_err(|error| {
                    PyRuntimeError::new_err(format!("fmd-math engine unavailable: {error}"))
                })?;
            let _ = cell.set(engine);
        }
        operation(cell.get().expect("set above"))
    })
}

/// `t2c=` entries as owned pairs the borrowed builder slices point into.
fn t2c_pairs(t2c: Option<&Bound<'_, PyDict>>) -> PyResult<Vec<(String, fmn_core::color::Srgb)>> {
    let mut pairs = Vec::new();
    if let Some(map) = t2c {
        for (key, value) in map.iter() {
            let key: String = key
                .extract()
                .map_err(|_| PyTypeError::new_err("t2c keys must be strings"))?;
            pairs.push((key, srgb_from_py(&value)?));
        }
    }
    Ok(pairs)
}

fn with_font_book<T>(operation: impl FnOnce(&fmn_library::FontBook) -> PyResult<T>) -> PyResult<T> {
    FONT_BOOK.with(|cell| {
        if cell.get().is_none() {
            let book = fmn_library::FontBook::bundled().map_err(|error| {
                PyRuntimeError::new_err(format!("bundled FontBook unavailable: {error}"))
            })?;
            let _ = cell.set(book);
        }
        operation(cell.get().expect("set above"))
    })
}

fn native_error(error: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(error.to_string())
}

fn tex_error(error: impl std::fmt::Display) -> PyErr {
    TexError::new_err(error.to_string())
}

/// Materialize only the authoritative family extent needed by Atlas's
/// extent-driven matcher builders. No path geometry is reconstructed here.
fn matcher_extent_vmobject(extent: Option<([f64; 3], [f64; 3])>) -> fmn_library::VMobject {
    if let Some((min, max)) = extent {
        fmn_library::VMobject::from_points(vec![min, max])
    } else {
        fmn_library::VMobject::new()
    }
}

/// `(min, max)` or `(min, max, step)` — the Reference's RangeSpecifier.
fn range3(value: &Bound<'_, PyAny>) -> PyResult<[f64; 3]> {
    let items: Vec<f64> = value
        .extract()
        .map_err(|_| PyTypeError::new_err("a range specifier must be a sequence of numbers"))?;
    match items.len() {
        2 => Ok([items[0], items[1], 1.0]),
        3 => Ok([items[0], items[1], items[2]]),
        other => Err(PyValueError::new_err(format!(
            "a range specifier needs 2 or 3 entries, got {other}"
        ))),
    }
}

fn srgb_from_py(value: &Bound<'_, PyAny>) -> PyResult<fmn_core::color::Srgb> {
    if let Ok(text) = value.extract::<String>() {
        return fmn_core::color::Srgb::from_hex(&text)
            .map_err(|error| PyValueError::new_err(format!("invalid color {text:?}: {error}")));
    }
    let rgb: Vec<f64> = value
        .extract()
        .map_err(|_| PyTypeError::new_err("colors must be hex strings or (r, g, b) sequences"))?;
    if rgb.len() < 3 {
        return Err(PyValueError::new_err("an rgb color needs three components"));
    }
    Ok(fmn_core::color::Srgb {
        r: rgb[0],
        g: rgb[1],
        b: rgb[2],
    })
}

/// One axis-config entry onto [`fmn_library::AxisConfig`]. Returns false
/// for a key this record does not carry (the caller decides whether that
/// is an error or a class-specific key).
fn apply_axis_config_key(
    config: &mut fmn_library::AxisConfig,
    key: &str,
    value: &Bound<'_, PyAny>,
) -> PyResult<bool> {
    match key {
        "color" => config.color = Some(srgb_from_py(value)?),
        "stroke_width" => config.stroke_width = Some(value.extract()?),
        "unit_size" => config.unit_size = Some(value.extract()?),
        "include_ticks" => config.include_ticks = Some(value.extract()?),
        "tick_size" => config.tick_size = Some(value.extract()?),
        "longer_tick_multiple" => config.longer_tick_multiple = Some(value.extract()?),
        "tick_offset" => config.tick_offset = Some(value.extract()?),
        "big_tick_spacing" => config.big_tick_spacing = Some(value.extract()?),
        "include_numbers" => config.include_numbers = Some(value.extract()?),
        "line_to_number_direction" => {
            config.line_to_number_direction = Some(value.extract::<[f64; 3]>()?);
        }
        "line_to_number_buff" => config.line_to_number_buff = Some(value.extract()?),
        "include_tip" => config.include_tip = Some(value.extract()?),
        "numbers_to_exclude" => config.numbers_to_exclude = Some(value.extract()?),
        "decimal_number_config" => {
            let entries = value
                .cast::<PyDict>()
                .map_err(|_| PyTypeError::new_err("decimal_number_config must be a dict"))?;
            for (inner_key, inner_value) in entries.iter() {
                let inner_key: String = inner_key.extract()?;
                match inner_key.as_str() {
                    "num_decimal_places" => {
                        config.num_decimal_places = Some(inner_value.extract()?);
                    }
                    "font_size" => config.number_font_size = Some(inner_value.extract()?),
                    other => {
                        return Err(PyTypeError::new_err(format!(
                            "unsupported decimal_number_config key `{other}`"
                        )));
                    }
                }
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn axis_config_from(config: Option<&Bound<'_, PyDict>>) -> PyResult<fmn_library::AxisConfig> {
    let mut out = fmn_library::AxisConfig::default();
    if let Some(config) = config {
        for (key, value) in config.iter() {
            let key: String = key
                .extract()
                .map_err(|_| PyTypeError::new_err("axis config keys must be strings"))?;
            if !apply_axis_config_key(&mut out, &key, &value)? {
                return Err(PyTypeError::new_err(format!(
                    "unsupported axis config key `{key}`"
                )));
            }
        }
    }
    Ok(out)
}

fn line_family_style_from(
    style: Option<&Bound<'_, PyDict>>,
) -> PyResult<fmn_library::planes::LineFamilyStyle> {
    let mut out = fmn_library::planes::LineFamilyStyle::default();
    if let Some(style) = style {
        for (key, value) in style.iter() {
            let key: String = key.extract()?;
            match key.as_str() {
                "stroke_color" => out.stroke_color = srgb_from_py(&value)?,
                "stroke_width" => out.stroke_width = value.extract()?,
                "stroke_opacity" => out.stroke_opacity = value.extract()?,
                other => {
                    return Err(PyTypeError::new_err(format!(
                        "unsupported background_line_style key `{other}`"
                    )));
                }
            }
        }
    }
    Ok(out)
}

fn faded_line_style_from(
    style: Option<&Bound<'_, PyDict>>,
) -> PyResult<fmn_library::planes::FadedLineStyle> {
    let mut out = fmn_library::planes::FadedLineStyle::default();
    if let Some(style) = style {
        for (key, value) in style.iter() {
            let key: String = key.extract()?;
            match key.as_str() {
                "stroke_color" => out.stroke_color = Some(srgb_from_py(&value)?),
                "stroke_width" => out.stroke_width = value.extract()?,
                "stroke_opacity" => out.stroke_opacity = value.extract()?,
                other => {
                    return Err(PyTypeError::new_err(format!(
                        "unsupported faded_line_style key `{other}`"
                    )));
                }
            }
        }
    }
    Ok(out)
}

/// The Reference's NumberLine constructor surface onto the native builder:
/// the AxisConfig subset plus the NumberLine-only keys. Unknown keys
/// refuse precisely — never silently dropped.
fn number_line_from_config(
    x_range: [f64; 3],
    config: &Bound<'_, PyDict>,
) -> PyResult<fmn_library::NumberLine> {
    let mut axis_config = fmn_library::AxisConfig::default();
    let mut width: Option<f64> = None;
    let mut big_tick_numbers: Option<Vec<f64>> = None;
    let mut tip_size: Option<f64> = None;
    for (key, value) in config.iter() {
        let key: String = key.extract()?;
        match key.as_str() {
            "width" => width = value.extract()?,
            "big_tick_numbers" => big_tick_numbers = Some(value.extract()?),
            "tip_config" => {
                let entries = value
                    .cast::<PyDict>()
                    .map_err(|_| PyTypeError::new_err("tip_config must be a dict"))?;
                let mut tip_width: Option<f64> = None;
                let mut tip_length: Option<f64> = None;
                for (inner_key, inner_value) in entries.iter() {
                    let inner_key: String = inner_key.extract()?;
                    match inner_key.as_str() {
                        "width" => tip_width = Some(inner_value.extract()?),
                        "length" => tip_length = Some(inner_value.extract()?),
                        other => {
                            return Err(PyTypeError::new_err(format!(
                                "unsupported tip_config key `{other}`"
                            )));
                        }
                    }
                }
                match (tip_width, tip_length) {
                    (None, None) => {}
                    (Some(w), Some(l)) if w == l => tip_size = Some(w),
                    (Some(w), None) | (None, Some(w)) => tip_size = Some(w),
                    (Some(_), Some(_)) => {
                        return Err(PyValueError::new_err(
                            "the native arrow tip is square; tip_config width and \
                             length must agree",
                        ));
                    }
                }
            }
            other => {
                if !apply_axis_config_key(&mut axis_config, other, &value)? {
                    return Err(PyTypeError::new_err(format!(
                        "NumberLine() got an unexpected keyword argument `{other}`"
                    )));
                }
            }
        }
    }
    let mut line = fmn_library::create_axis(x_range, axis_config, width);
    if let Some(values) = big_tick_numbers {
        line = line.big_tick_numbers(values);
    }
    if let Some(size) = tip_size {
        line = line.tip_size(size);
    }
    Ok(line)
}

#[allow(clippy::too_many_arguments)]
fn axes_builder(
    x_range: [f64; 3],
    y_range: [f64; 3],
    axis_config: Option<&Bound<'_, PyDict>>,
    x_axis_config: Option<&Bound<'_, PyDict>>,
    y_axis_config: Option<&Bound<'_, PyDict>>,
    height: Option<f64>,
    width: Option<f64>,
    unit_size: f64,
) -> PyResult<fmn_library::Axes> {
    let mut axes = fmn_library::Axes::new()
        .x_range(x_range)
        .y_range(y_range)
        .axis_config(axis_config_from(axis_config)?)
        .x_axis_config(axis_config_from(x_axis_config)?)
        .y_axis_config(axis_config_from(y_axis_config)?)
        .unit_size(unit_size);
    if let Some(height) = height {
        axes = axes.height(height);
    }
    if let Some(width) = width {
        axes = axes.width(width);
    }
    Ok(axes)
}

#[allow(clippy::too_many_arguments)]
fn complex_plane_builder(
    x_range: [f64; 3],
    y_range: [f64; 3],
    axis_config: Option<&Bound<'_, PyDict>>,
    x_axis_config: Option<&Bound<'_, PyDict>>,
    y_axis_config: Option<&Bound<'_, PyDict>>,
    background_line_style: Option<&Bound<'_, PyDict>>,
    faded_line_style: Option<&Bound<'_, PyDict>>,
    faded_line_ratio: usize,
    height: Option<f64>,
    width: Option<f64>,
    unit_size: f64,
) -> PyResult<fmn_library::ComplexPlane> {
    let mut plane = fmn_library::ComplexPlane::new()
        .x_range(x_range)
        .y_range(y_range)
        .axis_config(axis_config_from(axis_config)?)
        .x_axis_config(axis_config_from(x_axis_config)?)
        .y_axis_config(axis_config_from(y_axis_config)?)
        .background_line_style(line_family_style_from(background_line_style)?)
        .faded_line_style(faded_line_style_from(faded_line_style)?)
        .faded_line_ratio(faded_line_ratio)
        .unit_size(unit_size);
    if let Some(height) = height {
        plane = plane.height(height);
    }
    if let Some(width) = width {
        plane = plane.width(width);
    }
    Ok(plane)
}

/// One factory shell per family node, recursively: the node's own records
/// become the shell's single-root nursery; descendants are returned as
/// nested `(shell, children)` specs for the bootstrap to hang on the
/// Python family lists.
fn native_shell_specs<'py>(
    py: Python<'py>,
    factory: &Bound<'py, PyAny>,
    nodes: Vec<Mobject>,
) -> PyResult<Bound<'py, PyList>> {
    let out = PyList::empty(py);
    for mut node in nodes {
        let children = std::mem::take(&mut node.submobjects);
        let shell = factory.call0()?;
        {
            let bridge = shell.cast::<BridgeMobject>().map_err(|_| {
                PyTypeError::new_err("the native shell factory must return a Mobject")
            })?;
            let mut cell = bridge.borrow_mut();
            cell.nursery = Some(Nursery::new(node));
            cell.initialized = true;
        }
        let child_specs = native_shell_specs(py, factory, children)?;
        out.append((shell, child_specs))?;
    }
    Ok(out)
}

/// Install a native brace and retain the analytic tip's point index. Point
/// identity, rather than a frozen coordinate, makes `get_tip()` live after
/// ordinary Mobject transforms.
fn install_brace_tree<'py>(
    slf: &Bound<'py, BridgeMobject>,
    factory: &Bound<'py, PyAny>,
    brace: fmn_library::Brace,
) -> PyResult<(Bound<'py, PyList>, usize)> {
    let tip = brace.tip();
    let built = brace.build();
    let tip_index = built
        .points()
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            let distance = |point: &&[f64; 3]| {
                let dx = point[0] - tip[0];
                let dy = point[1] - tip[1];
                let dz = point[2] - tip[2];
                dx * dx + dy * dy + dz * dz
            };
            distance(left).total_cmp(&distance(right))
        })
        .map_or(0, |(index, _)| index);
    Ok((install_native_tree(slf, factory, built)?, tip_index))
}

/// Install a built native family on a constructing proxy: the root's own
/// records replace `slf`'s nursery; descendants become factory shells.
fn install_native_tree<'py>(
    slf: &Bound<'py, BridgeMobject>,
    factory: &Bound<'py, PyAny>,
    tree: impl Into<Mobject>,
) -> PyResult<Bound<'py, PyList>> {
    let mut tree = tree.into();
    let children = std::mem::take(&mut tree.submobjects);
    {
        let mut cell = slf.borrow_mut();
        if cell.engine.is_some() {
            return Err(PyRuntimeError::new_err(
                "a native builder may only construct a detached mobject",
            ));
        }
        cell.nursery = Some(Nursery::new(tree));
        cell.initialized = true;
    }
    native_shell_specs(slf.py(), factory, children)
}

/// The engine-backed camera-frame state (fm-d3gt): a thin proxy over
/// Lumen's [`fmn_scene::studio_bridge::CameraFrame`], the ONE implementation of the
/// Reference's euler/orientation/shape/fov semantics (fm-0gy).
///
/// The bootstrap's `CameraFrame(Mobject)` owns one of these as its
/// authoritative state; every camera method delegates here, so orientation,
/// center, shape, and field of view round-trip exactly (D5, state-real).
/// This value is also the renderer-binding seam: final native PNG capture
/// hands the same `fmn_scene::studio_bridge::CameraFrame` to Lumen's `Camera`
/// unchanged.
#[pyclass(unsendable, name = "_CameraFrameCore")]
struct PyCameraFrameCore {
    frame: fmn_scene::studio_bridge::CameraFrame,
}

fn camera_error(error: fmn_scene::studio_bridge::CameraError) -> PyErr {
    PyValueError::new_err(error.to_string())
}

#[pymethods]
impl PyCameraFrameCore {
    #[new]
    fn py_new(
        frame_shape: [f64; 2],
        center_point: [f64; 3],
        fovy: f64,
        euler_axes: &str,
    ) -> PyResult<Self> {
        Ok(Self {
            frame: fmn_scene::studio_bridge::CameraFrame::new(
                frame_shape,
                center_point,
                fovy,
                euler_axes,
            )
            .map_err(camera_error)?,
        })
    }

    fn __copy__(&self) -> Self {
        Self {
            frame: self.frame.clone(),
        }
    }

    fn __deepcopy__(&self, _memo: &Bound<'_, PyAny>) -> Self {
        Self {
            frame: self.frame.clone(),
        }
    }

    fn center(&self) -> [f64; 3] {
        self.frame.center()
    }

    fn set_center(&mut self, center: [f64; 3]) -> PyResult<()> {
        self.frame.set_center(center).map_err(camera_error)?;
        Ok(())
    }

    fn shape(&self) -> (f64, f64) {
        let [width, height] = self.frame.shape();
        (width, height)
    }

    fn set_shape(&mut self, shape: [f64; 2]) -> PyResult<()> {
        self.frame.set_shape(shape).map_err(camera_error)?;
        Ok(())
    }

    fn aspect_ratio(&self) -> f64 {
        self.frame.aspect_ratio()
    }

    fn scale(&self) -> f64 {
        self.frame.scale()
    }

    fn orientation(&self) -> [f64; 4] {
        self.frame.orientation()
    }

    fn set_orientation(&mut self, orientation: [f64; 4]) -> PyResult<()> {
        self.frame
            .set_orientation(orientation)
            .map_err(camera_error)?;
        Ok(())
    }

    fn make_orientation_default(&mut self) {
        self.frame.make_orientation_default();
    }

    // Reference-verbatim pymethod name; the Rust naming lint does not apply.
    #[allow(clippy::wrong_self_convention)]
    fn to_default_state(&mut self) {
        self.frame.to_default_state();
    }

    fn euler_axes(&self) -> String {
        self.frame.euler_axes().to_owned()
    }

    fn set_euler_axes(&mut self, seq: &str) -> PyResult<()> {
        self.frame.set_euler_axes(seq).map_err(camera_error)?;
        Ok(())
    }

    fn euler_angles(&self) -> [f64; 3] {
        self.frame.euler_angles()
    }

    fn set_euler_angles(
        &mut self,
        theta: Option<f64>,
        phi: Option<f64>,
        gamma: Option<f64>,
    ) -> PyResult<()> {
        self.frame
            .set_euler_angles(theta, phi, gamma)
            .map_err(camera_error)?;
        Ok(())
    }

    fn increment_euler_angles(&mut self, dtheta: f64, dphi: f64, dgamma: f64) -> PyResult<()> {
        self.frame
            .increment_euler_angles(dtheta, dphi, dgamma)
            .map_err(camera_error)?;
        Ok(())
    }

    fn rotate(&mut self, angle: f64, axis: [f64; 3]) -> PyResult<()> {
        self.frame.rotate(angle, axis).map_err(camera_error)?;
        Ok(())
    }

    fn field_of_view(&self) -> f64 {
        self.frame.field_of_view()
    }

    fn set_field_of_view(&mut self, fovy: f64) -> PyResult<()> {
        self.frame.set_field_of_view(fovy).map_err(camera_error)?;
        Ok(())
    }

    fn focal_distance(&self) -> f64 {
        self.frame.focal_distance()
    }

    fn set_focal_distance(&mut self, focal_distance: f64) -> PyResult<()> {
        self.frame
            .set_focal_distance(focal_distance)
            .map_err(camera_error)?;
        Ok(())
    }

    fn view_matrix(&self) -> [[f64; 4]; 4] {
        self.frame.view_matrix()
    }

    // The `to_*`/`from_*` names mirror the Reference's Python API verbatim;
    // Rust's self-convention lint does not apply to a pymethod surface.
    #[allow(clippy::wrong_self_convention)]
    fn to_fixed_frame_point(&self, point: [f64; 3], relative: bool) -> [f64; 3] {
        self.frame.to_fixed_frame_point(point, relative)
    }

    #[allow(clippy::wrong_self_convention)]
    fn from_fixed_frame_point(&self, point: [f64; 3], relative: bool) -> [f64; 3] {
        self.frame.from_fixed_frame_point(point, relative)
    }

    fn implied_camera_location(&self) -> [f64; 3] {
        self.frame.implied_camera_location()
    }
}

fn execute_bootstrap(py: Python<'_>, module: &Bound<'_, PyModule>) -> PyResult<()> {
    // Direct ExtensionFileLoader users do not install the module in
    // sys.modules until after create_module returns, but our bootstrap must
    // assemble child packages during create_module. Pass the actual module
    // explicitly, then remove the temporary self-reference.
    module.add("_FMN_MODULE", module)?;
    module.add("_API_SCHEMA_TSV", include_str!("../../../API_SCHEMA.tsv"))?;
    module.add("_API_OVERLAY_TSV", include_str!("../../../API_OVERLAY.tsv"))?;
    let source = CString::new(include_str!("../python/manimlib_bootstrap.py"))
        .expect("embedded bootstrap contains no NUL");
    let globals = module.dict();
    let result = py.run(source.as_c_str(), Some(&globals), Some(&globals));
    module.delattr("_FMN_MODULE")?;
    result
}

fn populate_manimlib(py: Python<'_>, module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<BridgeMobject>()?;
    module.add_class::<PyScene>()?;
    module.add_class::<PyRecordView>()?;
    module.add_class::<PyGilProbe>()?;
    module.add_class::<PyCameraFrameCore>()?;
    module.add_class::<PyFieldProbe>()?;
    module.add_class::<ladder::PyBatchedUpdater>()?;
    module.add_class::<ladder::PyArrayUpdater>()?;
    module.add_class::<ladder::PyNativeUpdater>()?;
    module.add_function(wrap_pyfunction!(
        crossing::_crossing_stats_snapshot,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(crossing::_crossing_stats_reset, module)?)?;
    module.add_function(wrap_pyfunction!(method_cache::_method_cache_stats, module)?)?;
    module.add_function(wrap_pyfunction!(method_cache::_method_cache_reset, module)?)?;
    module.add_function(wrap_pyfunction!(report::_crossing_report, module)?)?;
    module.add("_StaleHandleError", py.get_type::<StaleHandleError>())?;
    module.add("_ForeignStageError", py.get_type::<ForeignStageError>())?;
    module.add("_FamilyCycleError", py.get_type::<FamilyCycleError>())?;
    module.add("_CapabilityError", py.get_type::<CapabilityError>())?;
    module.add("_TexError", py.get_type::<TexError>())?;
    module.add("__engine__", "FrankenManim")?;
    module.add(
        "__thread_policy__",
        "scene and mobject proxies are confined to their creating scene-worker thread",
    )?;
    execute_bootstrap(py, module)?;
    // Packaging identity is owned by Cargo, not a second hand-maintained
    // Python version string.  W11's wheel and console entry point both read
    // these sentinels after the schema bootstrap has assembled `manimlib`.
    // `PyModule::add` records names in PyO3's generated `__all__`.  The
    // Reference intentionally has no `__all__`, so packaging metadata is set
    // as an ordinary attribute after the bootstrap has removed that helper.
    module.setattr("__version__", env!("CARGO_PKG_VERSION"))?;
    module.setattr("__distribution__", "franken-manim")?;
    module.setattr("__franken_manim__", true)?;
    module.setattr("__abi_policy__", "cpython-3.13-full-abi")?;
    Ok(())
}

/// Initialize the direct extension module used by the embedded acceptance
/// suite, by developers loading the Cargo cdylib without a wheel, and as the
/// wheel package's private `manimlib.manimlib` native member.
#[pymodule(gil_used = true)]
fn manimlib(py: Python<'_>, module: &Bound<'_, PyModule>) -> PyResult<()> {
    populate_manimlib(py, module)
}

/// Serialize the Python-embedding acceptance suites: they share one
/// process-global interpreter and one `sys.modules["manimlib"]` slot, so
/// concurrent module construction races. Poison-tolerant: a panicked suite
/// must not wedge the others.
pub(crate) fn python_embedding_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|error| error.into_inner())
}

/// Run one embedded-Python suite with explicit owner-thread teardown.
///
/// PyO3's `unsendable` guard intentionally refuses to destroy a proxy on a
/// thread other than the one which created it. Merely serializing the Rust
/// test functions is therefore insufficient: the test harness may run the
/// next suite on another OS thread while the previous suite still has
/// module cycles in `sys.modules`. Keep the lock through module removal and
/// cyclic GC, capture every unraisable destructor error, and require the
/// temporary root module to become unreachable before releasing the lock.
#[cfg(any(test, feature = "gauntlet"))]
pub(crate) fn with_python_test_module<T>(
    suite: &'static str,
    body: impl for<'py> FnOnce(Python<'py>, &Bound<'py, PyModule>, &Bound<'py, PyDict>) -> T,
) -> T {
    let _lock = python_embedding_lock();
    Python::initialize();
    Python::attach(|py| {
        let sys = py.import("sys").expect("import sys");
        let gc = py.import("gc").expect("import gc");
        let weakref = py.import("weakref").expect("import weakref");
        let modules = sys.getattr("modules").expect("sys.modules");
        let module_names = || -> PyResult<HashSet<String>> {
            modules
                .call_method0("keys")?
                .try_iter()?
                .map(|item| item.and_then(|name| name.extract::<String>()))
                .collect()
        };
        let before = module_names().expect("snapshot sys.modules");
        assert!(
            before
                .iter()
                .all(|name| name != "manimlib" && !name.starts_with("manimlib.")),
            "{suite}: a prior Python suite leaked manimlib modules"
        );
        let videos_ref = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/videos_ref")
            .canonicalize()
            .ok();

        let hook_globals = PyDict::new(py);
        hook_globals
            .set_item("_fmn_unraisable", PyList::empty(py))
            .expect("install unraisable capture list");
        let hook_source = CString::new(
            r#"import sys as _fmn_sys
_fmn_old_unraisablehook = _fmn_sys.unraisablehook
def _fmn_capture_unraisable(event):
    _fmn_unraisable.append(
        f'{type(event.exc_value).__name__}: {event.exc_value}'
    )
_fmn_sys.unraisablehook = _fmn_capture_unraisable
"#,
        )
        .expect("unraisable hook source contains no NUL");
        py.run(
            hook_source.as_c_str(),
            Some(&hook_globals),
            Some(&hook_globals),
        )
        .expect("install unraisable hook");

        let module = PyModule::new(py, "manimlib").expect("create test module");
        modules
            .set_item("manimlib", &module)
            .expect("install manimlib");
        let module_weakref = weakref
            .getattr("ref")
            .and_then(|constructor| constructor.call1((&module,)))
            .expect("weak-reference the temporary manimlib module");
        let suite_globals = PyDict::new(py);
        suite_globals
            .set_item("__name__", format!("__fmn_{suite}_tests__"))
            .expect("set suite module name");

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            manimlib(py, &module).expect("initialize manimlib");
            body(py, &module, &suite_globals)
        }));
        // Python callbacks retain their globals, while suite globals retain
        // PyO3 instances and their callbacks. Some of those extension types
        // are intentionally outside Python's cyclic-GC graph, so the
        // embedding host must break the cycle explicitly on the owner.
        suite_globals.clear();

        let after = module_names().expect("snapshot suite-added modules");
        for name in after.difference(&before) {
            let is_manimlib = name == "manimlib" || name.starts_with("manimlib.");
            let is_corpus_module = videos_ref.as_ref().is_some_and(|root| {
                modules
                    .get_item(name)
                    .and_then(|module| module.getattr("__file__"))
                    .and_then(|path| path.extract::<String>())
                    .is_ok_and(|path| std::path::Path::new(&path).starts_with(root))
            });
            if is_manimlib || is_corpus_module {
                if let Ok(value) = modules.get_item(name)
                    && let Ok(owned_module) = value.cast_into::<PyModule>()
                {
                    owned_module.dict().clear();
                }
                let removal = modules.del_item(name);
                assert!(
                    removal.is_ok(),
                    "{suite}: remove module {name}: {:?}",
                    removal.err()
                );
            }
        }
        // PyO3 functions added with `module.add_function` keep the extension
        // module as their `__self__`. CPython does not collect that builtin
        // function <-> module-dict cycle by itself, so explicitly clear the
        // temporary module exactly as an embedding host would during worker
        // teardown. This also releases suite globals before the owner thread
        // can change.
        module.dict().clear();
        drop(module);
        gc.call_method0("collect")
            .expect("collect suite-owned Python cycles");

        let old_hook = hook_globals
            .get_item("_fmn_old_unraisablehook")
            .expect("lookup prior unraisable hook")
            .expect("prior unraisable hook exists");
        sys.setattr("unraisablehook", old_hook)
            .expect("restore prior unraisable hook");
        let unraisable: Vec<String> = hook_globals
            .get_item("_fmn_unraisable")
            .expect("lookup unraisable capture")
            .expect("unraisable capture exists")
            .extract()
            .expect("extract unraisable errors");
        assert!(
            unraisable.is_empty(),
            "{suite}: Python teardown emitted unraisable errors: {unraisable:?}"
        );
        let surviving_module = module_weakref.call0().expect("read module weak reference");
        let referrer_types = if surviving_module.is_none() {
            Vec::new()
        } else {
            gc.call_method1("get_referrers", (&surviving_module,))
                .expect("inspect leaked module referrers")
                .try_iter()
                .expect("iterate leaked module referrers")
                .map(|item| {
                    item.and_then(|value| value.get_type().name().map(|name| name.to_string()))
                        .unwrap_or_else(|error| format!("<unreadable: {error}>"))
                })
                .collect()
        };
        assert!(
            surviving_module.is_none(),
            "{suite}: temporary manimlib module survived owner-thread teardown; \
             referrer types: {referrer_types:?}"
        );
        assert!(
            module_names()
                .expect("verify restored sys.modules")
                .iter()
                .all(|name| name != "manimlib" && !name.starts_with("manimlib.")),
            "{suite}: embedded Python suite left a manimlib module installed"
        );

        match outcome {
            Ok(value) => value,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}

/// Structured result from the feature-gated in-process Gauntlet portal row.
#[cfg(feature = "gauntlet")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalGauntletReport {
    /// Published PNG-sequence directory.
    pub path: PathBuf,
    /// Number of lifecycle captures published.
    pub frame_count: u64,
    /// Total PNG bytes in the generation.
    pub bytes: u64,
    /// Canonical ordered PNG-tree digest.
    pub digest: String,
    /// Journal-ready Lumen engine identity.
    pub engine: String,
    /// Fixed render team width.
    pub threads: usize,
}

/// Drive one real Python `Scene` through its production Lumen/Reel route.
///
/// This adapter exists only for the Gauntlet feature used by
/// `fmn-conformance`; the installed wheel exposes the same route through the
/// `fmn-python` console rather than a Rust test hook.
#[cfg(feature = "gauntlet")]
pub fn run_portal_gauntlet_png_sequence(
    destination: &std::path::Path,
    seed: u64,
) -> Result<PortalGauntletReport, String> {
    run_portal_gauntlet_png(destination, seed, false)
}

/// Drive one real Python `Scene` to a single final-state PNG through Lumen/Reel.
#[cfg(feature = "gauntlet")]
pub fn run_portal_gauntlet_png_still(
    destination: &std::path::Path,
    seed: u64,
) -> Result<PortalGauntletReport, String> {
    run_portal_gauntlet_png(destination, seed, true)
}

#[cfg(feature = "gauntlet")]
fn run_portal_gauntlet_png(
    destination: &std::path::Path,
    seed: u64,
    single_frame: bool,
) -> Result<PortalGauntletReport, String> {
    let destination = destination
        .to_str()
        .ok_or_else(|| "Gauntlet portal destination is not UTF-8".to_owned())?
        .to_owned();
    with_python_test_module("portal Gauntlet", |py, _module, globals| {
        globals
            .set_item("_fmn_destination", &destination)
            .map_err(|error| error.to_string())?;
        globals
            .set_item("_fmn_seed", seed)
            .map_err(|error| error.to_string())?;
        globals
            .set_item("_fmn_single_frame", single_frame)
            .map_err(|error| error.to_string())?;
        let source = CString::new(
            r#"from manimlib import AnnularSector, Arrow, Axes, BLUE_C, BLUE_E, BraceLabel, BraceText, BulletedList, Checkmark, Circle, Cone, Cross, CubicBezier, CurvedDoubleArrow, DashedLine, DashedVMobject, Disk3D, Dot, Dodecahedron, Elbow, Exmark, FullScreenFadeRectangle, FullScreenRectangle, FunctionGraph, GREY_C, GrowArrow, GrowFromCenter, GrowFromEdge, GrowFromPoint, ImplicitFunction, Line, Line3D, Matrix, ParametricSurface, Polygon, Polyline, Prismify, RED, Rectangle, Rotate, Rotating, RoundedRectangle, Scene, ScreenRectangle, Square3D, StrokeArrow, TangentLine, TimeVaryingVectorField, Title, Torus, Underline, VCube, VGroup3D, VMobject, VPrism, Vector, VectorField, linear

class _GauntletPortalScene(Scene):
    # NumPy's compatibility RandomState accepts only 32-bit scalar seeds.
    # Keep the Gauntlet's full u64 for the native Scene/RngRoot below while
    # deriving the source-scene seed explicitly at this Python-only boundary.
    random_seed = _fmn_seed & 0xFFFF_FFFF

    def construct(self):
        frame_fade = FullScreenFadeRectangle(fill_opacity=0.04)
        frame_fill = FullScreenRectangle(
            height=0.45,
            fill_color=BLUE_E,
            fill_opacity=0.35,
        ).shift((0.0, 1.65, 0.0))
        frame_border = ScreenRectangle(
            height=3.5,
            stroke_color=GREY_C,
            stroke_width=1.0,
        )
        cubic = CubicBezier(
            (-2.6, 1.45, 0.0),
            (-2.2, 2.0, 0.0),
            (-1.4, 2.0, 0.0),
            (-1.0, 1.45, 0.0),
            stroke_color=BLUE_C,
            stroke_width=2.0,
        )
        elbow = Elbow(
            width=0.45,
            angle=0.35,
            stroke_color=RED,
            stroke_width=2.0,
        ).shift((1.9, 1.25, 0.0))
        native_path = VMobject(stroke_width=3.0)
        native_path.start_new_path((-2.0, -1.4, 0.0))
        native_path.add_cubic_bezier_curve_to(
            (-1.5, -0.6, 0.0),
            (1.5, -0.6, 0.0),
            (2.0, -1.4, 0.0),
        )
        native_path.add_arc_to((2.5, -0.9, 0.0), 0.65)
        native_path.add_subpath((
            (-0.6, -1.6, 0.0),
            (0.0, -1.1, 0.0),
            (0.6, -1.6, 0.0),
        )).close_path()
        native_path.make_jagged(recurse=False)
        portal_ops = VMobject(stroke_width=2.0).set_points((
            (-1.8, 0.0, 0.0),
            (-1.2, 0.0, 0.0),
            (-1.2, 0.6, 0.0),
        ))
        portal_ops.append_vectorized_mobject(
            VMobject(stroke_width=2.5).set_points_as_corners((
                (-0.8, 0.0, 0.0),
                (-0.2, 0.4, 0.0),
            ))
        )
        alignment_peer = VMobject().set_points_as_corners((
            (-1.8, 0.0, 0.0),
            (-1.4, 0.2, 0.0),
            (-1.0, 0.0, 0.0),
            (-0.6, 0.2, 0.0),
            (-0.2, 0.0, 0.0),
        ))
        portal_ops.align_points(alignment_peer)
        portal_ops.subdivide_sharp_curves(1.0, recurse=False)
        smooth_ops = VMobject(stroke_width=2.0).set_points_smoothly((
            (0.2, 0.0, 0.0),
            (0.8, 0.5, 0.0),
            (1.4, 0.0, 0.0),
        ))
        smooth_ops.subdivide_curves_by_condition(
            lambda b0, b1, b2: 1 if b1[1] > b0[1] else 0,
            recurse=False,
        )
        smooth_ops.subdivide_intersections(recurse=False, n_subdivisions=1)
        surface_source = ParametricSurface(
            lambda u, v: (u, v, 0.0),
            u_range=(-0.8, 0.8),
            v_range=(-0.5, 0.5),
            resolution=(9, 7),
        ).shift((2.0, 1.2, 0.0))
        surface_partial = surface_source.copy()
        surface_partial.pointwise_become_partial(
            surface_source, 0.2, 0.8, axis=0
        )
        matcher_target = Circle(radius=0.35).shift((2.35, -1.1, 0.0))
        matcher_cross = Cross(matcher_target, stroke_width=[0, 4, 0])
        matcher_underline = Underline(
            matcher_target, buff=0.08, stroke_width=[0, 2, 2, 0]
        )
        checkmark = Checkmark(font_size=24).shift((2.5, 0.35, 0.0))
        exmark = Exmark(font_size=24).shift((1.8, 0.35, 0.0))
        solid_torus = Torus(
            r1=0.35,
            r2=0.12,
            resolution=(9, 7),
            color=BLUE_C,
        ).shift((-2.55, 0.75, 0.0))
        solid_cone = Cone(
            height=0.65,
            radius=0.22,
            resolution=(9, 5),
            color=RED,
        ).shift((-2.0, 0.65, 0.0))
        solid_line = Line3D(
            (-2.7, 0.15, 0.0),
            (-1.8, 0.15, 0.0),
            width=0.08,
            resolution=(7, 5),
            color=BLUE_E,
        )
        solid_disk = Disk3D(
            radius=0.22,
            resolution=(3, 9),
            color=RED,
        ).shift((-1.45, 0.7, 0.0))
        solid_square = Square3D(
            side_length=0.4,
            resolution=(3, 3),
            color=BLUE_C,
        ).shift((-0.95, 0.7, 0.0))
        vector_solids = VGroup3D(
            VCube(
                side_length=0.35,
                fill_color=BLUE_C,
                stroke_width=0.5,
            ).shift((-0.45, 0.7, 0.0)),
            VPrism(
                width=0.45,
                height=0.3,
                depth=0.2,
                fill_color=RED,
            ).shift((0.05, 0.7, 0.0)),
        )
        vector_dodecahedron = Dodecahedron(
            fill_color=BLUE_E,
            stroke_color=BLUE_C,
            stroke_width=0.5,
        ).scale(0.12).shift((0.55, 0.7, 0.0))
        vector_prismify = Prismify(
            Polygon(
                (-0.2, -0.15, 0.0),
                (0.2, -0.15, 0.0),
                (0.0, 0.2, 0.0),
                fill_color=RED,
                fill_opacity=0.6,
                stroke_width=0.5,
            ),
            depth=0.15,
        ).shift((1.05, 0.7, 0.0))
        native_list = BulletedList(
            "Native text",
            "Deterministic layout",
            buff=0.12,
            font_size=16,
            color=BLUE_C,
        ).scale(0.22).shift((2.4, -1.65, 0.0))
        native_list.fade_all_but(0, opacity=0.4, scale_factor=0.8)
        native_title = Title(
            "Franken",
            "Manim",
            font_size=20,
            match_underline_width_to_text=True,
            underline_style=dict(stroke_width=1.5, stroke_color=RED),
            color=BLUE_E,
        ).scale(0.25).shift((0.0, -3.0, 0.0))
        native_matrix = Matrix(
            [["x", "1"], ["0", "y"]],
            h_buff=0.25,
            v_buff=0.2,
            element_config=dict(font_size=16, color=BLUE_C),
        ).scale(0.22).shift((-2.45, -1.75, 0.0))
        native_brace_label = BraceLabel(
            Circle(radius=0.28).shift((1.4, -1.7, 0.0)),
            "r",
            label_scale=0.35,
            label_buff=0.04,
            color=RED,
        ).scale(0.55)
        native_brace_text = BraceText(
            Circle(radius=0.24).shift((0.75, -1.7, 0.0)),
            "text",
            label_scale=0.25,
            label_buff=0.04,
            color=BLUE_C,
        ).scale(0.5)
        native_function_graph = FunctionGraph(
            lambda x: 0.3 * x * x - 0.2,
            x_range=(-0.7, 0.7, 0.1),
            color=BLUE_C,
            use_smoothing=False,
            stroke_width=1.5,
        ).shift((-0.1, 1.25, 0.0))
        native_implicit = ImplicitFunction(
            lambda x, y: x * x + y * y - 0.16,
            x_range=(-0.6, 0.6),
            y_range=(-0.6, 0.6),
            min_depth=2,
            max_quads=128,
            stroke_color=RED,
            stroke_width=1.5,
        ).shift((0.95, 1.25, 0.0))
        native_line = Line(
            (-1.15, 1.65, 0.0),
            (1.15, 1.65, 0.0),
            stroke_color=RED,
            stroke_width=1.5,
        )
        native_line.set_path_arc(-0.45).set_length(1.8).add_tip(
            length=0.18,
            width=0.14,
        )
        native_arrow = Arrow(
            (-2.35, 2.35, 0.0),
            (-1.35, 2.35, 0.0),
            buff=0.04,
            thickness=2.5,
            tip_width_ratio=4.0,
            max_tip_length_to_length_ratio=0.35,
            max_width_to_length_ratio=0.08,
            fill_color=BLUE_C,
        ).scale(0.9)
        native_arrow.set_thickness(2.75).put_start_and_end_on(
            (-2.3, 2.35, 0.0),
            (-1.4, 2.35, 0.0),
        )
        native_vector = Vector(
            (0.65, 0.2, 0.0),
            thickness=2.25,
            max_tip_length_to_length_ratio=0.4,
            max_width_to_length_ratio=0.09,
            fill_color=RED,
        ).shift((1.55, 2.25, 0.0))
        primitive_dot = Dot(
            (-3.0, 2.45, 0.0),
            radius=0.08,
            fill_color=RED,
        )
        primitive_rect = Rectangle(
            width=0.4,
            height=0.25,
            stroke_color=BLUE_C,
            stroke_width=1.0,
        ).surround(primitive_dot, buff=0.06)
        primitive_round = RoundedRectangle(
            width=0.45,
            height=0.28,
            corner_radius=0.07,
            stroke_color=RED,
            stroke_width=1.0,
        ).shift((3.0, 2.45, 0.0))
        primitive_polyline = Polyline(
            (2.7, 2.1, 0.0),
            (3.0, 2.3, 0.0),
            (3.3, 2.1, 0.0),
            stroke_color=BLUE_C,
            stroke_width=1.5,
        )
        native_dashed_line = DashedLine(
            (-3.25, 2.05, 0.0),
            (-2.55, 2.05, 0.0),
            dash_length=0.08,
            positive_space_ratio=0.6,
            stroke_color=BLUE_C,
            stroke_width=1.5,
        )
        native_stroke_arrow = StrokeArrow(
            (-3.25, 1.78, 0.0),
            (-2.55, 1.78, 0.0),
            buff=0.02,
            tip_width_ratio=3.0,
            stroke_color=RED,
            stroke_width=1.5,
        )
        tangent_source = Circle(radius=0.18).shift((2.25, 2.05, 0.0))
        native_tangent_line = TangentLine(
            tangent_source,
            0.125,
            length=0.55,
            stroke_color=RED,
            stroke_width=1.5,
        )
        field_axes = Axes(
            x_range=(-1.0, 1.0, 1.0),
            y_range=(-1.0, 1.0, 1.0),
            width=0.7,
            height=0.7,
        ).shift((-0.7, 2.0, 0.0))
        native_field = VectorField(
            lambda coords: coords * 0.0 + (0.35, 0.1),
            field_axes,
            sample_coords=((-1.0, 0.0), (0.0, 0.0), (1.0, 0.0)),
            max_vect_len=0.3,
            color=BLUE_C,
            stroke_width=1.5,
        )
        time_field_axes = Axes(
            x_range=(-1.0, 1.0, 1.0),
            y_range=(-1.0, 1.0, 1.0),
            width=0.7,
            height=0.7,
        ).shift((0.7, 2.0, 0.0))
        native_time_field = TimeVaryingVectorField(
            lambda coords, time: coords * 0.0 + (0.2 + time, -0.1),
            time_field_axes,
            sample_coords=((-1.0, 0.0), (0.0, 0.0), (1.0, 0.0)),
            max_vect_len=0.3,
            color=RED,
            stroke_width=1.5,
        )
        self.add(
            frame_fade,
            frame_fill,
            frame_border,
            cubic,
            elbow,
            Circle(radius=0.9),
            CurvedDoubleArrow((-1.5, -0.5, 0), (1.5, -0.5, 0)),
            AnnularSector(inner_radius=0.25, outer_radius=0.6).shift((0, 1, 0)),
            DashedVMobject(Circle(radius=0.45), num_dashes=6).shift((-1.25, 1, 0)),
            native_path,
            portal_ops,
            smooth_ops,
            surface_partial,
            matcher_cross,
            matcher_underline,
            checkmark,
            exmark,
            solid_torus,
            solid_cone,
            solid_line,
            solid_disk,
            solid_square,
            vector_solids,
            vector_dodecahedron,
            vector_prismify,
            native_list,
            native_title,
            native_matrix,
            native_brace_label,
            native_brace_text,
            native_function_graph,
            native_implicit,
            native_line,
            native_arrow,
            native_vector,
            primitive_dot,
            primitive_rect,
            primitive_round,
            primitive_polyline,
            native_dashed_line,
            native_stroke_arrow,
            tangent_source,
            native_tangent_line,
            native_field,
            native_time_field,
        )
        # All four growing classes share Choreo's native start-prep
        # Transform.  This production play proves the authored Python MRO and
        # constructor anchors reach real intermediate Lumen frames, including
        # point-color and edge/arrow specializations, rather than stopping at
        # import-compatible shells.
        self.play(
            GrowFromPoint(
                native_stroke_arrow,
                (-3.0, 1.55, 0.0),
                point_color=BLUE_C,
            ),
            GrowFromEdge(primitive_round, (-1.0, 0.0, 0.0)),
            GrowFromCenter(native_tangent_line),
            GrowArrow(native_arrow),
            # The public Animation -> Rotating -> Rotate hierarchy reaches
            # Choreo's absolute-pose rotation in the same real Lumen/Reel
            # frame stream.  Distinct pivots exercise both rotation routes.
            Rotate(primitive_polyline, angle=0.15),
            Rotating(
                primitive_rect,
                angle=-0.1,
                about_point=(0.0, 0.0, 0.0),
            ),
            run_time=1 / 30,
            rate_func=linear,
        )
        self.wait(1 / 30)

_fmn_scene = _GauntletPortalScene()
_fmn_begin = (
    _fmn_scene._begin_png
    if _fmn_single_frame
    else _fmn_scene._begin_png_sequence
)
_fmn_begin(_fmn_destination, 96, 54, 30, 1, _fmn_seed)
try:
    _fmn_scene.run()
    _fmn_report = _fmn_scene._finish_render(
        _fmn_scene.frame._core,
        _fmn_scene.camera.light_source.get_center(),
    )
except Exception:
    _fmn_scene._abort_render()
    raise
"#,
        )
        .expect("Gauntlet source contains no NUL");
        py.run(source.as_c_str(), Some(globals), Some(globals))
            .map_err(|error| error.to_string())?;
        let report = globals
            .get_item("_fmn_report")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Gauntlet portal script returned no report".to_owned())?
            .extract::<(String, u64, u64, String, String, usize)>()
            .map_err(|error| error.to_string())?;
        Ok(PortalGauntletReport {
            path: PathBuf::from(report.0),
            frame_count: report.1,
            bytes: report.2,
            digest: report.3,
            engine: report.4,
            threads: report.5,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_bridge_acceptance_suite() {
        crate::with_python_test_module("bridge acceptance", |py, _module, globals| {
            globals
                .set_item(
                    "__file__",
                    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/bridge.py"),
                )
                .expect("set bridge suite source path");
            globals
                .set_item("_expected_package_version", env!("CARGO_PKG_VERSION"))
                .expect("set bridge suite package version");
            let source = CString::new(include_str!("../tests/bridge.py"))
                .expect("test source contains no NUL");
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .expect("Python bridge acceptance suite");

            // fm-7if keeps affine motion out of object-space records until the
            // authoritative Python data surface demands synchronization. An
            // already exported zero-copy array must still observe engine
            // writes, and a later scalar read must materialize a pending
            // placement before exposing the buffer.
            let parent = globals
                .get_item("parent")
                .expect("globals lookup")
                .expect("bridge suite defines parent");
            let proxy = parent.cast::<BridgeMobject>().expect("parent proxy");
            let (engine, mob) = bound_parts(&proxy.borrow()).expect("bound parent");
            let data = parent.getattr("data").expect("live NumPy data");
            let before: Vec<f32> = data
                .get_item("point")
                .expect("point field")
                .get_item(0)
                .expect("first point")
                .call_method0("tolist")
                .expect("point list")
                .extract()
                .expect("f32 point");
            engine.borrow_mut().stage_mut().shift(mob, [2.0, -1.0, 0.0]);
            let viewed: Vec<f32> = data
                .get_item("point")
                .expect("point field")
                .get_item(0)
                .expect("first point")
                .call_method0("tolist")
                .expect("point list")
                .extract()
                .expect("f32 point");
            #[allow(clippy::cast_possible_truncation)]
            let expected_viewed = [
                (f64::from(before[0]) + 2.0) as f32,
                (f64::from(before[1]) - 1.0) as f32,
                before[2],
            ];
            assert_eq!(viewed, expected_viewed);
            assert!(
                engine
                    .borrow()
                    .stage()
                    .placement(mob)
                    .expect("live")
                    .is_identity(),
                "an attached view receives affine writes in-place"
            );
            drop(data);

            engine.borrow_mut().stage_mut().shift(mob, [1.0, 0.0, 0.0]);
            assert!(
                !engine
                    .borrow()
                    .stage()
                    .placement(mob)
                    .expect("live")
                    .is_identity(),
                "without a view, motion stays in the placement channel"
            );
            let read: Vec<f32> = parent
                .call_method1("get_field", ("point", 0))
                .expect("world-space field read")
                .extract()
                .expect("f32 point");
            #[allow(clippy::cast_possible_truncation)]
            let expected_read = [(f64::from(viewed[0]) + 1.0) as f32, viewed[1], viewed[2]];
            assert_eq!(read, expected_read);
            assert!(
                engine
                    .borrow()
                    .stage()
                    .placement(mob)
                    .expect("live")
                    .is_identity(),
                "an API read synchronizes placement back to RecordBuffer"
            );
        });
    }

    #[test]
    fn python_suite_cycles_are_collected_before_the_owner_thread_changes() {
        fn run_on_fresh_thread(suite: &'static str) -> std::thread::ThreadId {
            std::thread::spawn(move || {
                let owner = std::thread::current().id();
                crate::with_python_test_module(suite, |_py, module, _globals| {
                    let instance = module
                        .getattr("Mobject")
                        .and_then(|class| class.call0())
                        .expect("construct teardown probe");
                    instance
                        .setattr("_teardown_cycle", &instance)
                        .expect("make an owned Python cycle");
                    module
                        .setattr("_teardown_probe", instance)
                        .expect("retain the probe from the temporary module");
                });
                owner
            })
            .join()
            .expect("Python teardown probe thread")
        }

        let first = run_on_fresh_thread("first owner-thread teardown probe");
        let second = run_on_fresh_thread("second owner-thread teardown probe");
        assert_ne!(
            first, second,
            "the regression requires distinct test owners"
        );
    }

    #[test]
    fn cross_thread_unsendable_access_is_a_typed_refusal() {
        const CHILD_ENV: &str = "FMN_UNSENDABLE_PROBE_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            // PyO3 implements its `unsendable` refusal by catching a Rust
            // panic and translating it into `PanicException`. The default
            // panic hook necessarily writes the caught panic to stderr.
            // Contain that intentional negative control in a child copy of
            // this test binary so the ordinary all-target gate remains
            // stderr-clean without installing a process-global hook which
            // could hide an unrelated concurrent panic.
            let output = std::process::Command::new(
                std::env::current_exe().expect("locate current test binary"),
            )
            .args([
                "--exact",
                "tests::cross_thread_unsendable_access_is_a_typed_refusal",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .output()
            .expect("run isolated unsendable negative control");
            assert!(
                output.status.success(),
                "isolated unsendable negative control failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("unsendable, but sent to another thread"),
                "the isolated control did not exercise PyO3's thread guard: {stderr}"
            );
            return;
        }

        crate::with_python_test_module(
            "isolated unsendable negative control",
            |py, module, _globals| {
                let object = module
                    .getattr("Mobject")
                    .and_then(|class| class.call0())
                    .expect("construct thread-confined probe")
                    .unbind();
                let (object, error) = py.detach(|| {
                    std::thread::spawn(move || {
                        Python::attach(|py| {
                            let locals = PyDict::new(py);
                            locals
                                .set_item("_fmn_probe", object.bind(py))
                                .expect("install foreign-thread probe");
                            let source = CString::new(
                                "try:\n\
                                 \x20\x20\x20\x20_fmn_probe.n_records()\n\
                                 except BaseException as _fmn_error:\n\
                                 \x20\x20\x20\x20_fmn_error_text = str(_fmn_error)\n\
                                 else:\n\
                                 \x20\x20\x20\x20raise AssertionError('foreign-thread access succeeded')\n",
                            )
                            .expect("foreign-thread probe source contains no NUL");
                            py.run(source.as_c_str(), Some(&locals), Some(&locals))
                                .expect("catch the PyO3 refusal inside Python");
                            let error = locals
                                .get_item("_fmn_error_text")
                                .expect("lookup foreign-thread refusal")
                                .expect("foreign-thread refusal was recorded")
                                .extract::<String>()
                                .expect("extract foreign-thread refusal");
                            locals.clear();
                            (object, error)
                        })
                    })
                    .join()
                    .expect("foreign-thread refusal probe")
                });
                assert!(
                    error.contains("unsendable, but sent to another thread"),
                    "unexpected cross-thread refusal: {error}"
                );
                drop(object);
            },
        );
    }
}
