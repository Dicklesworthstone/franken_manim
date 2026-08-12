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

use std::cell::{Ref, RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::ffi::{CString, c_int, c_void};
use std::ptr;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use crossing::CrossingClass;

use fmn_mobject::{
    JointType, Mob, Mobject, RecordBuffer, RecordError, RecordSchema, RecordView, Stage,
    StageError, Uniforms,
};
use fmn_scene::{RuntimeConfig, Scene};
use pyo3::create_exception;
use pyo3::exceptions::{
    PyBufferError, PyImportError, PyKeyError, PyNotImplementedError, PyOverflowError,
    PyRuntimeError, PyTypeError, PyValueError,
};
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule, PyTuple};

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

type Engine = Rc<EngineState>;
type ProxyPairs = Vec<(Py<PyAny>, Py<PyAny>)>;
type SoundRequestFact = (String, i64, u32, f64, Option<f64>, Option<f64>);

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

fn record_state<'py>(
    py: Python<'py>,
    proxy: &Bound<'py, BridgeMobject>,
) -> PyResult<Bound<'py, PyDict>> {
    let state = PyDict::new(py);
    let (fields, len, records) = with_buffer_ref(proxy, |buffer| {
        let fields: Vec<(String, usize)> = buffer
            .schema()
            .fields()
            .iter()
            .map(|field| (field.name.clone(), field.width))
            .collect();
        let mut records = Vec::with_capacity(buffer.len() * buffer.schema().stride());
        for index in 0..buffer.len() {
            for field in buffer.schema().fields() {
                records.extend(
                    buffer
                        .read(index, &field.name)
                        .expect("iterated schema field exists"),
                );
            }
        }
        (fields, buffer.len(), records)
    })?;
    state.set_item("fields", fields)?;
    state.set_item("len", len)?;
    state.set_item("records", records)?;
    state.set_item("aligned_data_keys", proxy.getattr("aligned_data_keys")?)?;
    state.set_item("pointlike_data_keys", proxy.getattr("pointlike_data_keys")?)?;
    let uniforms = uniforms_snapshot(proxy)?;
    let uniform_state = PyDict::new(py);
    for &name in UNIFORM_NAMES {
        uniform_state.set_item(name, uniform_value(py, uniforms, name)?)?;
    }
    state.set_item("uniforms", uniform_state)?;
    let z_index = {
        let cell = proxy.borrow();
        if let (Some(engine), Some(mob)) = (&cell.engine, cell.mob) {
            engine.borrow().stage().z_index(mob)
        } else {
            cell.nursery
                .as_ref()
                .map_or(0, |nursery| nursery.stage.z_index(nursery.root))
        }
    };
    state.set_item("z_index", z_index)?;
    Ok(state)
}

fn restore_record_state(
    proxy: &Bound<'_, BridgeMobject>,
    state: &Bound<'_, PyDict>,
) -> PyResult<()> {
    if proxy.borrow().engine.is_some() {
        return Err(PyRuntimeError::new_err(
            "cannot restore detached pickle state over a bound mobject",
        ));
    }
    let fields: Vec<(String, usize)> = state
        .get_item("fields")?
        .ok_or_else(|| PyKeyError::new_err("fields"))?
        .extract()?;
    let len: usize = state
        .get_item("len")?
        .ok_or_else(|| PyKeyError::new_err("len"))?
        .extract()?;
    let records: Vec<f32> = state
        .get_item("records")?
        .ok_or_else(|| PyKeyError::new_err("records"))?
        .extract()?;
    let aligned: Vec<String> = state
        .get_item("aligned_data_keys")?
        .ok_or_else(|| PyKeyError::new_err("aligned_data_keys"))?
        .extract()?;
    let pointlike: Vec<String> = state
        .get_item("pointlike_data_keys")?
        .ok_or_else(|| PyKeyError::new_err("pointlike_data_keys"))?
        .extract()?;
    if fields.is_empty() {
        return Err(PyValueError::new_err(
            "restored record state declares no fields",
        ));
    }
    let mut names = HashSet::new();
    let mut stride = 0usize;
    for (name, width) in &fields {
        if name.is_empty() || *width == 0 || !names.insert(name.as_str()) {
            return Err(PyValueError::new_err(
                "restored record fields require unique non-empty names and positive widths",
            ));
        }
        stride = stride
            .checked_add(*width)
            .ok_or_else(|| PyOverflowError::new_err("restored record stride overflows usize"))?;
    }
    if aligned
        .iter()
        .chain(pointlike.iter())
        .any(|name| !names.contains(name.as_str()))
    {
        return Err(PyValueError::new_err(
            "restored aligned and pointlike keys must name record fields",
        ));
    }
    let field_refs: Vec<(&str, usize)> = fields
        .iter()
        .map(|(name, width)| (name.as_str(), *width))
        .collect();
    let aligned_refs: Vec<&str> = aligned.iter().map(String::as_str).collect();
    let pointlike_refs: Vec<&str> = pointlike.iter().map(String::as_str).collect();
    let schema = RecordSchema::new(&field_refs, &aligned_refs, &pointlike_refs)
        .map_err(record_error_to_py)?;
    let expected = len
        .checked_mul(stride)
        .ok_or_else(|| PyOverflowError::new_err("restored record shape overflows usize"))?;
    if records.len() != expected {
        return Err(PyValueError::new_err(format!(
            "restored record state has {} lanes, expected {expected}",
            records.len()
        )));
    }
    let mut buffer = RecordBuffer::new(schema, len).map_err(record_error_to_py)?;
    let mut cursor = 0usize;
    for index in 0..len {
        for (name, width) in &fields {
            let end = cursor.checked_add(*width).ok_or_else(|| {
                PyOverflowError::new_err("restored record cursor overflows usize")
            })?;
            let wrote = buffer.write(index, name, &records[cursor..end]);
            debug_assert!(wrote, "schema was constructed from these fields");
            cursor = end;
        }
    }
    let mut uniforms = Uniforms::default();
    let uniform_state = state
        .get_item("uniforms")?
        .ok_or_else(|| PyKeyError::new_err("uniforms"))?
        .cast_into::<PyDict>()?;
    for &name in UNIFORM_NAMES {
        if let Some(value) = uniform_state.get_item(name)? {
            set_uniform(&mut uniforms, name, &value)?;
        }
    }
    let z_index: i32 = state
        .get_item("z_index")?
        .map_or(Ok(0), |value| value.extract())?;
    let mut detached = Mobject::from_buffer(buffer).with_uniforms(uniforms);
    detached.z_index = z_index;
    let mut cell = proxy.borrow_mut();
    cell.nursery = Some(Nursery::new(detached));
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
    fn _get_bbox(slf: &Bound<'_, Self>) -> PyResult<([f64; 3], [f64; 3], [f64; 3])> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| {
            let bbox = stage.get_bounding_box(mob);
            (bbox.min, bbox.mid, bbox.max)
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

    /// Format sRGB components as the Reference's uppercase `#RRGGBB`.
    #[staticmethod]
    fn _rgb_to_hex(rgb: (f64, f64, f64)) -> String {
        fmn_core::color::Srgb {
            r: rgb.0,
            g: rgb.1,
            b: rgb.2,
        }
        .to_hex()
    }

    /// `Mobject.become` over `Stage::become_mobject`: per-member data,
    /// uniform, and placement assignment across zipped equal-shape
    /// families. Schema or family-shape drift is the engine's precise
    /// refusal (structural `align_family` awaits its binding).
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

    /// Chisel's shared-anchor encoding for a corner polyline, installed
    /// into the current Stage entry with the normal schema/revision rules.
    fn _set_points_as_corners(slf: &Bound<'_, Self>, anchors: Vec<[f64; 3]>) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        let mut path = fmn_library::QuadPath::new();
        path.set_points_as_corners(&anchors).map_err(native_error)?;
        with_stage(slf, |stage, mob| stage.set_points(mob, path.points()))?.map_err(stage_error)
    }

    /// Reference `reverse_points` through Marionette's family operation,
    /// which reverses every record row with its point and repairs path
    /// break handles/base normals.
    fn _reverse_points(slf: &Bound<'_, Self>) -> PyResult<()> {
        crossing::record(CrossingClass::FieldWrite);
        with_stage(slf, |stage, mob| stage.reverse_family_points(mob))?.map_err(stage_error)
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

    /// The project contract's true-arclength `point_from_proportion`
    /// (BN-03), routed to Chisel through Marionette for both proxy states.
    fn _point_from_proportion(slf: &Bound<'_, Self>, alpha: f64) -> PyResult<[f64; 3]> {
        crossing::record(CrossingClass::Other);
        with_stage(slf, |stage, mob| stage.point_from_proportion(mob, alpha))?.map_err(stage_error)
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
        color: Option<&Bound<'py, PyAny>>,
        magnitude_range: Option<(f64, f64)>,
    ) -> PyResult<Bound<'py, PyList>> {
        let n = sample_points.len();
        if n < 2 {
            return Err(PyValueError::new_err(
                "VectorField needs at least two sample points",
            ));
        }
        if out_vects.len() != n || output_norms.len() != n {
            return Err(PyValueError::new_err(format!(
                "VectorField callback returned {} vectors and {} norms for {n} samples",
                out_vects.len(),
                output_norms.len()
            )));
        }
        if !max_displayed_vect_len.is_finite() || max_displayed_vect_len <= 0.0 {
            return Err(PyValueError::new_err(
                "VectorField max displayed length must be positive and finite",
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

    /// `Arrow(start, end, ...)`: one filled path with the native tip
    /// proportions (thickness, tip ratios, ratio caps at the Reference
    /// defaults).
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
    ) -> PyResult<Bound<'py, PyList>> {
        let built = fmn_library::line::Arrow::new(start, end)
            .buff(buff)
            .path_arc(path_arc)
            .thickness(thickness)
            .tip_width_ratio(tip_width_ratio)
            .tip_angle(tip_angle)
            .build()
            .map_err(native_error)?;
        install_native_tree(slf, factory, built)
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
    ) -> PyResult<()> {
        let built = fmn_library::line::Arrow::new(start, end)
            .buff(buff)
            .path_arc(path_arc)
            .thickness(thickness)
            .tip_width_ratio(tip_width_ratio)
            .tip_angle(tip_angle)
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
            .map_err(native_error)
        })?;
        let spans: Vec<(usize, usize)> = built
            .typeset
            .subs
            .iter()
            .map(|sub| (sub.span.start, sub.span.end))
            .collect();
        if parts.is_empty() || (parts.len() == 1 && !group_single_part) {
            let paths: Vec<Vec<usize>> = (0..spans.len()).map(|index| vec![index]).collect();
            slf.as_any().setattr("_tex_sub_spans", spans)?;
            slf.as_any().setattr("_tex_sub_paths", paths)?;
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
        slf.as_any().setattr("_tex_sub_spans", spans)?;
        slf.as_any().setattr("_tex_sub_paths", paths)?;
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

    fn _engine_state<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyDict>> {
        record_state(slf.py(), slf)
    }

    fn _restore_engine_state(slf: &Bound<'_, Self>, state: &Bound<'_, PyDict>) -> PyResult<()> {
        restore_record_state(slf, state)
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

/// Run the portal half of Scene.update_mobjects with no Scene/Stage borrow
/// live. Choreo's stepped play releases exactly at this call site; ordinary
/// `Scene.update(dt)` uses the same helper before its native updater pass.
fn run_python_updaters(scene: &Bound<'_, PyScene>, dt: f64) -> PyResult<u64> {
    let targets = update_targets(scene);
    let py = scene.py();
    let python_start = Instant::now();
    for target in targets {
        let Some(proxy) = live_proxy(py, scene, target) else {
            continue;
        };
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
            method_cache::call_cached1(&proxy, "_dispatch_updater", &args)?;
        }
    }
    Ok(u64::try_from(python_start.elapsed().as_nanos()).unwrap_or(u64::MAX))
}

fn has_python_updaters(scene: &Bound<'_, PyScene>) -> PyResult<bool> {
    let py = scene.py();
    for target in update_targets(scene) {
        if let Some(proxy) = live_proxy(py, scene, target)
            && proxy.getattr("updaters")?.is_truthy()?
        {
            return Ok(true);
        }
    }
    Ok(false)
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
        })
    }

    #[pyo3(signature = (*mobjects))]
    fn add<'py>(
        slf: &Bound<'py, Self>,
        mobjects: &Bound<'py, PyTuple>,
    ) -> PyResult<Bound<'py, Self>> {
        let py = slf.py();
        let mut handles = Vec::with_capacity(mobjects.len());
        for object in mobjects.iter() {
            let proxy = object
                .cast::<BridgeMobject>()
                .map_err(|_| PyTypeError::new_err("Scene.add accepts only Mobject instances"))?;
            handles.push(bind_graph(py, slf, proxy)?);
        }
        slf.borrow()
            .engine
            .borrow_mut()
            .add(&handles)
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
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
        let mut batch = Vec::with_capacity(targets.len());
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
        let mut sink = AlphaProbeSink {
            camera: camera_lerp,
            ..AlphaProbeSink::default()
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
            engine
                .borrow_mut()
                .wait(Some(effective_run_time), &mut sink)
                .map(|_| ())
                .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
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
                Ok(())
            })();
            if let Err(error) = drive_result {
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
        let AlphaProbeSink {
            alphas,
            camera,
            camera_error,
        } = sink;
        if let Some(error) = camera_error {
            return Err(error);
        }
        if let Some(camera) = camera {
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

    /// `Scene.wait(duration)` over the native wait segment (NullSceneSink;
    /// rendering is a later tranche).
    #[pyo3(signature = (duration = None))]
    fn _wait(slf: &Bound<'_, Self>, duration: Option<f64>) -> PyResult<()> {
        let engine = Rc::clone(&slf.borrow().engine);
        if !has_python_updaters(slf)? {
            return engine
                .borrow_mut()
                .wait(duration, &mut fmn_scene::NullSceneSink)
                .map(|_| ())
                .map_err(|error| PyRuntimeError::new_err(error.to_string()));
        }

        // The Reference performs one zero-dt Scene.update_mobjects pass
        // before planning wait frames. Run the Python half while unborrowed;
        // begin_stepped_wait immediately follows with the native half.
        run_python_updaters(slf, 0.0)?;
        let mut sink = fmn_scene::NullSceneSink;
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
    point: [f64; 3],
    time_span: Option<(f64, f64)>,
    group_lag: f64,
    remover: bool,
    surface_resolution: (usize, usize),
    surface_axis: usize,
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
        point: get3("point", [0.0; 3])?,
        time_span: params
            .get_item("time_span")?
            .map(|value| value.extract())
            .transpose()?,
        group_lag: get1("lag_ratio", 0.0)?,
        remover: get_bool("remover", false)?,
        surface_resolution: get_usize_pair("surface_resolution", (0, 0))?,
        surface_axis: get_usize("surface_axis", 1)?,
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
        "animation_group" | "lagged_start" | "succession"
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
        "write" => {
            let mut write = fmn_anim::write(stage, need_mob(spec.mob)?);
            if let Some(rgb) = spec.stroke_color {
                #[allow(clippy::cast_possible_truncation)]
                let rgb = [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32];
                write = write.with_stroke_color(Some(rgb));
            }
            Box::new(write)
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
        "grow_from_center" => Box::new(
            fmn_anim::grow_from_center(stage, need_mob(spec.mob)?, None).map_err(anim_error)?,
        ),
        "grow_arrow" => {
            Box::new(fmn_anim::grow_arrow(stage, need_mob(spec.mob)?).map_err(anim_error)?)
        }
        "fade_transform" => Box::new(
            fmn_anim::fade_transform(stage, need_mob(spec.mob)?, need_target(spec.target)?)
                .map_err(anim_error)?,
        ),
        "restore" => {
            let mut restore = fmn_anim::restore(stage, need_mob(spec.mob)?).map_err(anim_error)?;
            if spec.path_arc != 0.0 {
                restore = restore.with_path_arc(spec.path_arc, spec.path_arc_axis);
            }
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

/// The portal's play sink: validates the capture boundary contract,
/// records each captured frame's alpha (rendering is a later tranche),
/// and — when a camera lerp rides the segment — writes the interpolated
/// camera state at every capture boundary.
#[derive(Default)]
struct AlphaProbeSink {
    alphas: Vec<f64>,
    camera: Option<CameraLerp>,
    camera_error: Option<PyErr>,
}

impl fmn_scene::SceneSink for AlphaProbeSink {
    fn capture(
        &mut self,
        _reason: fmn_scene::CaptureReason,
        packet: fmn_scene::studio_bridge::FramePacket,
    ) -> Result<(), fmn_scene::IntegrationError> {
        self.alphas.push(packet.alpha());
        if let Some(camera) = &self.camera
            && self.camera_error.is_none()
        {
            let raw = if camera.run_time > 0.0 {
                (packet.time().to_f64() - camera.start_time) / camera.run_time
            } else {
                1.0
            };
            // The GIL is held by the pymethod driving this segment;
            // attach re-enters it. Only the camera core cell is borrowed
            // — never the Scene.
            Python::attach(|py| {
                if let Err(error) = camera.apply(py, raw) {
                    self.camera_error = Some(error);
                }
            });
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
/// This value is also the future renderer-binding seam: a render tranche
/// hands the same `fmn_scene::studio_bridge::CameraFrame` to Lumen's `Camera` unchanged.
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

/// Initialize the extension module. The module name is intentionally
/// `manimlib`, so a built cdylib is directly importable under the Reference's
/// package name.
#[pymodule(gil_used = true)]
fn manimlib(py: Python<'_>, module: &Bound<'_, PyModule>) -> PyResult<()> {
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
    module.add("__engine__", "FrankenManim")?;
    module.add(
        "__thread_policy__",
        "scene and mobject proxies are confined to their creating scene-worker thread",
    )?;
    execute_bootstrap(py, module)
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
#[cfg(test)]
pub(crate) fn with_python_test_module(
    suite: &'static str,
    body: impl for<'py> FnOnce(Python<'py>, &Bound<'py, PyModule>, &Bound<'py, PyDict>),
) {
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
            body(py, &module, &suite_globals);
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

        if let Err(payload) = outcome {
            std::panic::resume_unwind(payload);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_bridge_acceptance_suite() {
        crate::with_python_test_module("bridge acceptance", |py, _module, globals| {
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
