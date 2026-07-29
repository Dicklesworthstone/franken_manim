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
//! CPython buffer slots on [`PyRecordView`]; every other operation is safe
//! Rust and no engine borrow crosses a Python callback.
#![deny(unsafe_op_in_unsafe_fn)]

use std::cell::{Ref, RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::ffi::{CString, c_int, c_void};
use std::ptr;
use std::rc::Rc;

use fmn_mobject::{
    JointType, Mob, Mobject, RecordBuffer, RecordSchema, RecordView, StageError, Uniforms,
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

/// Subclassable Python proxy over either detached builder state or one
/// Stage-scoped Marionette handle.
#[pyclass(subclass, weakref, dict, unsendable, name = "_BridgeMobject")]
struct BridgeMobject {
    detached: Option<Mobject>,
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
    let detached = cell
        .detached
        .as_mut()
        .ok_or_else(|| StaleHandleError::new_err("mobject has no detached or bound state"))?;
    Ok(operation(&mut detached.buffer))
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
    let cell = proxy.borrow();
    let detached = cell
        .detached
        .as_ref()
        .ok_or_else(|| StaleHandleError::new_err("mobject has no detached or bound state"))?;
    Ok(operation(&detached.buffer))
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
    let detached = cell
        .detached
        .as_mut()
        .ok_or_else(|| StaleHandleError::new_err("mobject has no detached or bound state"))?;
    operation(&mut detached.uniforms)
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
    cell.detached
        .as_ref()
        .map(|detached| detached.uniforms)
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
    Ok(RecordSchema::new(&fields, &aligned_refs, &pointlike_refs))
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
        if cell.engine.is_none() && (!cell.initialized || cell.detached.is_none()) {
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
        let detached = proxy
            .borrow_mut()
            .detached
            .take()
            .expect("validated detached state");
        let mob = {
            let mut runtime = engine.borrow_mut();
            let mob = runtime.stage_mut().add(detached);
            runtime.stage_mut().pin(mob).map_err(stage_error)?;
            mob
        };
        {
            let mut cell = proxy.borrow_mut();
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
            cell.detached.as_ref().map_or(0, |mobject| mobject.z_index)
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
    let schema = RecordSchema::new(&field_refs, &aligned_refs, &pointlike_refs);
    let expected = len
        .checked_mul(stride)
        .ok_or_else(|| PyOverflowError::new_err("restored record shape overflows usize"))?;
    if records.len() != expected {
        return Err(PyValueError::new_err(format!(
            "restored record state has {} lanes, expected {expected}",
            records.len()
        )));
    }
    let mut buffer = RecordBuffer::new(schema, len);
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
    cell.detached = Some(detached);
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
        cell.detached = None;
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
            detached: Some(Mobject::new()),
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
            cell.detached = Some(Mobject::from_buffer(RecordBuffer::new(schema, 0)));
            cell.initialized = true;
        }
        // No engine or proxy borrow is live across these calls.
        slf.call_method0("init_data")?;
        slf.call_method0("init_points")?;
        slf.call_method0("init_uniforms")?;
        Ok(())
    }

    fn init_data(_slf: &Bound<'_, Self>) {}

    fn init_points(_slf: &Bound<'_, Self>) {}

    fn init_uniforms(_slf: &Bound<'_, Self>) {}

    fn resize(slf: &Bound<'_, Self>, len: usize) -> PyResult<()> {
        let stride = with_buffer_ref(slf, |buffer| buffer.schema().stride())?;
        len.checked_mul(stride)
            .ok_or_else(|| PyOverflowError::new_err("RecordBuffer resize overflows usize"))?;
        with_buffer(slf, |buffer| buffer.resize(len))
    }

    fn n_records(slf: &Bound<'_, Self>) -> PyResult<usize> {
        with_buffer_ref(slf, RecordBuffer::len)
    }

    fn revision(slf: &Bound<'_, Self>) -> PyResult<u64> {
        with_buffer_ref(slf, RecordBuffer::revision)
    }

    fn field_names(slf: &Bound<'_, Self>) -> PyResult<Vec<String>> {
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
        with_buffer_ref(slf, |buffer| buffer.read(index, field))?
            .ok_or_else(|| PyKeyError::new_err(format!("no `{field}` record at index {index}")))
    }

    fn set_field(
        slf: &Bound<'_, Self>,
        field: &str,
        index: usize,
        values: Vec<f32>,
    ) -> PyResult<()> {
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
        numpy_array(slf.py(), slf, writable)
    }

    #[staticmethod]
    fn _uniform_names() -> Vec<&'static str> {
        UNIFORM_NAMES.to_vec()
    }

    fn _get_uniform<'py>(slf: &Bound<'py, Self>, name: &str) -> PyResult<Bound<'py, PyAny>> {
        uniform_value(slf.py(), uniforms_snapshot(slf)?, name)
    }

    fn _set_uniform(slf: &Bound<'_, Self>, name: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        with_uniforms(slf, |uniforms| set_uniform(uniforms, name, value))
    }

    fn _replace_submobjects(
        slf: &Bound<'_, Self>,
        children: Vec<Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let mut seen = HashSet::new();
        let mut child_locations = Vec::with_capacity(children.len());
        for child in children {
            let child = child
                .cast::<BridgeMobject>()
                .map_err(|_| PyTypeError::new_err("submobjects must be Mobject instances"))?;
            let marker = child.as_ptr() as usize;
            if !seen.insert(marker) {
                return Err(PyValueError::new_err(
                    "one submobject cannot appear twice under the same parent",
                ));
            }
            let cell = child.borrow();
            child_locations.push((
                cell.engine.as_ref().map(Rc::clone),
                cell.mob,
                cell.initialized && cell.detached.is_some(),
            ));
        }

        let parent_location = {
            let cell = slf.borrow();
            (
                cell.engine.as_ref().map(Rc::clone),
                cell.mob,
                cell.initialized && cell.detached.is_some(),
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
        with_buffer(slf, |buffer| {
            let mut cursor = 0usize;
            for index in 0..buffer.len() {
                let fields: Vec<(String, usize)> = buffer
                    .schema()
                    .fields()
                    .iter()
                    .map(|field| (field.name.clone(), field.width))
                    .collect();
                for (field, width) in fields {
                    let mixed: Vec<f32> = (0..width)
                        .map(|lane| {
                            let from = start_records[cursor + lane];
                            let to = target_records[cursor + lane];
                            from + (to - from) * alpha
                        })
                        .collect();
                    let wrote = buffer.write(index, &field, &mixed);
                    debug_assert!(wrote, "schema and loop are identical");
                    cursor += width;
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
        let cell = slf.borrow();
        match (&cell.engine, cell.mob) {
            (Some(engine), Some(mob)) => engine.borrow().stage().contains(mob),
            _ => cell.detached.is_some(),
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
        self.engine.borrow().stage().roots().len()
    }

    fn time(&self) -> f64 {
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

    /// Python updater callbacks run with no Scene/Stage borrow live. After
    /// they finish, Marionette advances time and runs native updaters.
    fn update(slf: &Bound<'_, Self>, dt: f64) -> PyResult<()> {
        let targets = {
            let scene = slf.borrow();
            let runtime = scene.engine.borrow();
            let mut targets = Vec::new();
            for &root in runtime.stage().roots() {
                for member in runtime.stage().family(root) {
                    if !targets.contains(&member) {
                        targets.push(member);
                    }
                }
            }
            targets
        };
        let py = slf.py();
        for target in targets {
            let Some(proxy) = live_proxy(py, slf, target) else {
                continue;
            };
            let updaters = proxy.getattr("updaters")?;
            let snapshot: Vec<Py<PyAny>> = updaters
                .try_iter()?
                .map(|item| item.map(Bound::unbind))
                .collect::<PyResult<_>>()?;
            for updater in snapshot {
                proxy.call_method1("_dispatch_updater", (updater, dt))?;
            }
        }
        slf.borrow().engine.borrow_mut().stage_mut().update(dt);
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
        let copy = mobject.call_method0("__copy__")?;
        for step in 0..=steps {
            let alpha = if steps == 0 {
                1.0
            } else {
                step as f64 / steps as f64
            };
            mobject.call_method1("interpolate", (&copy, target, alpha))?;
        }
        Ok(())
    }

    /// setup → construct → tear_down through Python MRO. tear_down runs even
    /// when construct raises; the original construct exception remains primary.
    fn _run_lifecycle(slf: &Bound<'_, Self>) -> PyResult<()> {
        slf.call_method0("setup")?;
        let construct = slf.call_method0("construct");
        let teardown = slf.call_method0("tear_down");
        match (construct, teardown) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(_), Ok(_)) => Ok(()),
        }
    }

    fn _checkpoint_bytes(&self) -> PyResult<Vec<u8>> {
        self.engine
            .borrow_mut()
            .state_bytes()
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::PyString;

    #[test]
    fn production_bridge_acceptance_suite() {
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
