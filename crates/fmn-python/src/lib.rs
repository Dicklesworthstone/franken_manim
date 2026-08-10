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
    PyBufferError, PyImportError, PyKeyError, PyOverflowError, PyRuntimeError, PyTypeError,
    PyValueError,
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
        if parts.len() <= 1 {
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
        let mut buckets: Vec<Vec<Mobject>> = (0..parts.len()).map(|_| Vec::new()).collect();
        for (index, child) in children.into_iter().enumerate() {
            let start = subs.get(index).map_or(0, |sub| sub.span.start);
            let part = ranges
                .iter()
                .position(|&(from, to)| from <= start && start < to)
                .unwrap_or(parts.len() - 1);
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

        let parent_location = {
            let cell = slf.borrow();
            (
                cell.engine.as_ref().map(Rc::clone),
                cell.mob,
                cell.initialized && cell.nursery.is_some(),
            )
        };
        let (Some(engine), Some(parent), _) = parent_location else {
            if !parent_location.2 {
                return Err(StaleHandleError::new_err(
                    "uninitialized mobject cannot own submobjects",
                ));
            }
            if child_locations
                .iter()
                .any(|(child_engine, _, detached)| child_engine.is_some() || !detached)
            {
                return Err(ForeignStageError::new_err(
                    "a detached parent may contain only detached mobjects",
                ));
            }
            // The Python live list is authoritative until Scene.add binds the
            // complete graph in one transaction.
            return Ok(());
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

/// Mobjects receiving updater dispatch this frame, in stage order.
fn update_targets(scene: &Bound<'_, PyScene>) -> Vec<Mob> {
    let scene_cell = scene.borrow();
    let runtime = scene_cell.engine.borrow();
    let mut targets = Vec::new();
    for &root in runtime.stage().roots() {
        for member in runtime.stage().family(root) {
            if !targets.contains(&member) {
                targets.push(member);
            }
        }
    }
    targets
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
        let targets = update_targets(slf);
        let py = slf.py();
        let python_start = Instant::now();
        for target in targets {
            let Some(proxy) = live_proxy(py, slf, target) else {
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
        let python_ns = u64::try_from(python_start.elapsed().as_nanos()).unwrap_or(u64::MAX);
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
    #[pyo3(signature = (pairs, camera, run_time, rate_func, lag_ratio))]
    fn _play_transforms(
        slf: &Bound<'_, Self>,
        pairs: Vec<(Bound<'_, BridgeMobject>, Bound<'_, BridgeMobject>)>,
        camera: Option<(Bound<'_, PyCameraFrameCore>, Bound<'_, PyCameraFrameCore>)>,
        run_time: Option<f64>,
        rate_func: Option<&str>,
        lag_ratio: Option<f64>,
    ) -> PyResult<Vec<f64>> {
        let engine = Rc::clone(&slf.borrow().engine);
        let mut animations: Vec<Box<dyn fmn_anim::Animation>> = Vec::with_capacity(pairs.len());
        for (mobject, target) in &pairs {
            let (mob_engine, mob) = bound_parts(&mobject.borrow())?;
            let (target_engine, target_mob) = bound_parts(&target.borrow())?;
            if !same_engine(&engine, &mob_engine) || !same_engine(&engine, &target_engine) {
                return Err(ForeignStageError::new_err(
                    "play endpoints must belong to this Scene",
                ));
            }
            animations.push(Box::new(fmn_anim::Transform::new(mob, target_mob)));
        }
        let effective_run_time = run_time.unwrap_or(fmn_anim::DEFAULT_ANIMATION_RUN_TIME);
        let camera_lerp = camera
            .map(|(live, target)| -> PyResult<CameraLerp> {
                Ok(CameraLerp {
                    start: live.borrow().frame.clone(),
                    end: target.borrow().frame.clone(),
                    core: live.unbind(),
                    start_time: engine.borrow().stage().time(),
                    run_time: effective_run_time,
                    rate: rate_func
                        .map(named_rate_func)
                        .transpose()?
                        .unwrap_or_default(),
                })
            })
            .transpose()?;
        let overrides = fmn_scene::PlayOverrides {
            run_time,
            rate_func: rate_func.map(named_rate_func).transpose()?,
            lag_ratio,
        };
        let mut sink = AlphaProbeSink {
            camera: camera_lerp,
            ..AlphaProbeSink::default()
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
        } else {
            engine
                .borrow_mut()
                .play(animations, overrides, &mut sink)
                .map(|_| ())
                .map_err(|error| PyRuntimeError::new_err(error.to_string()))?;
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

    /// Adopt a detached mobject graph into this scene's arena WITHOUT
    /// adding it to the draw list — the `.animate` target seam.
    fn _adopt(slf: &Bound<'_, Self>, mobject: &Bound<'_, BridgeMobject>) -> PyResult<()> {
        bind_graph(slf.py(), slf, mobject)?;
        Ok(())
    }

    /// `Scene.wait(duration)` over the native wait segment (NullSceneSink;
    /// rendering is a later tranche).
    #[pyo3(signature = (duration = None))]
    fn _wait(slf: &Bound<'_, Self>, duration: Option<f64>) -> PyResult<()> {
        let engine = Rc::clone(&slf.borrow().engine);
        engine
            .borrow_mut()
            .wait(duration, &mut fmn_scene::NullSceneSink)
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
#[pymodule]
fn manimlib(py: Python<'_>, module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<BridgeMobject>()?;
    module.add_class::<PyScene>()?;
    module.add_class::<PyRecordView>()?;
    module.add_class::<PyGilProbe>()?;
    module.add_class::<PyCameraFrameCore>()?;
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
#[cfg(test)]
pub(crate) fn python_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|error| error.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::PyString;

    #[test]
    fn production_bridge_acceptance_suite() {
        let _lock = crate::python_test_lock();
        Python::initialize();
        Python::attach(|py| {
            let module = PyModule::new(py, "manimlib").expect("create test module");
            py.import("sys")
                .expect("sys")
                .getattr("modules")
                .expect("sys.modules")
                .set_item("manimlib", &module)
                .expect("install module");
            manimlib(py, &module).expect("initialize manimlib");
            let source = CString::new(include_str!("../tests/bridge.py"))
                .expect("test source contains no NUL");
            let globals = PyDict::new(py);
            globals
                .set_item("__name__", PyString::new(py, "__fmn_bridge_tests__"))
                .expect("test module name");
            py.run(source.as_c_str(), Some(&globals), Some(&globals))
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
}
