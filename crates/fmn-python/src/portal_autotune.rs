//! Typed Python configuration and receipts for the shared raster planner.
//!
//! The portal reads only the live fields that affect raster calibration.
//! Filesystem ownership, cache identity, admission and fallback stay in fmn.

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyDictMethods, PyInt, PyModuleMethods, PyString, PyStringMethods};

pub(super) fn file_snapshot(py: Python<'_>, certified: bool) -> PyResult<fmn_config::Config> {
    if certified {
        return fmn_config::Config::resolve(&[], None)
            .map(|resolved| resolved.config)
            .map_err(|error| PyValueError::new_err(error.to_string()));
    }
    from_live(py, true)
}

/// The camera path executes the certified CPU renderer in a standard artifact
/// mode. Its AA remains configured, but FastCpu cache settings are irrelevant.
pub(super) fn camera_snapshot(py: Python<'_>) -> PyResult<fmn_config::Config> {
    from_live(py, false)
}

fn from_live(py: Python<'_>, include_cache: bool) -> PyResult<fmn_config::Config> {
    use fmn_config::yaml::Value;

    fn section<'py>(root: &Bound<'py, PyDict>, name: &str) -> PyResult<Option<Bound<'py, PyDict>>> {
        root.get_item(name)?
            .map(|value| {
                value.cast_into::<PyDict>().map_err(|_| {
                    PyTypeError::new_err(format!("manim_config.{name} must be a dictionary"))
                })
            })
            .transpose()
    }

    fn string_setting(
        values: &Bound<'_, PyDict>,
        name: &str,
        path: &str,
        max_bytes: usize,
    ) -> PyResult<Option<String>> {
        let Some(value) = values.get_item(name)? else {
            return Ok(None);
        };
        if !value.is_exact_instance_of::<PyString>() {
            return Err(PyTypeError::new_err(format!(
                "manim_config.{path} must be a string"
            )));
        }
        // Exact str prevents __len__ hooks. Bound characters before UTF-8,
        // whose conversion can allocate up to four bytes per character.
        if value.len()? > max_bytes {
            return Err(PyValueError::new_err(format!(
                "manim_config.{path} exceeds the {max_bytes}-byte limit"
            )));
        }
        let string = value.cast::<PyString>()?;
        let text = string.to_str().map_err(|error| {
            PyValueError::new_err(format!("manim_config.{path} is not valid UTF-8: {error}"))
        })?;
        if text.len() > max_bytes || text.contains('\0') {
            return Err(PyValueError::new_err(format!(
                "manim_config.{path} must be at most {max_bytes} UTF-8 bytes without NUL"
            )));
        }
        Ok(Some(text.to_owned()))
    }

    let module = py.import("manimlib")?;
    let root = module
        .dict()
        .get_item("manim_config")?
        .ok_or_else(|| PyValueError::new_err("manimlib.manim_config is absent"))?
        .cast_into::<PyDict>()
        .map_err(|_| PyTypeError::new_err("manimlib.manim_config must be a dictionary"))?;
    let mut overrides: Vec<(&'static str, Value)> = Vec::with_capacity(3);
    if include_cache {
        if let Some(values) = section(&root, "directories")?
            && let Some(cache) = string_setting(&values, "cache", "directories.cache", 4096)?
        {
            overrides.push(("directories.cache", Value::Str(cache)));
        }
        if let Some(values) = section(&root, "cache")?
            && let Some(value) = values.get_item("max_bytes")?
        {
            if !value.is_exact_instance_of::<PyInt>() {
                return Err(PyTypeError::new_err(
                    "manim_config.cache.max_bytes must be an integer, not a boolean or coercible object",
                ));
            }
            let bytes = value.extract::<i64>().map_err(|error| {
                PyValueError::new_err(format!(
                    "manim_config.cache.max_bytes must fit the typed signed integer range: {error}"
                ))
            })?;
            overrides.push(("cache.max_bytes", Value::Int(bytes)));
        }
    }
    // Missing AA uses typed defaults without invoking _ConfigDict's
    // auto-vivifying attribute lookup.
    if let Some(values) = section(&root, "render")?
        && let Some(aa) = string_setting(&values, "aa", "render.aa", 64)?
    {
        overrides.push(("render.aa", Value::Str(aa)));
    }
    fmn_config::Config::resolve(&[], Some(fmn_config::config::overlay(overrides)))
        .map(|resolved| resolved.config)
        .map_err(|error| PyValueError::new_err(error.to_string()))
}

/// Diagnostic evidence only; never an addition to the certified input closure.
#[derive(Clone, Debug)]
pub(super) struct Report {
    source: &'static str,
    engine: &'static str,
    fine_tile: u32,
    macro_tile: u32,
    diagnostic: Option<String>,
    bypass_reason: Option<&'static str>,
    planar_frames: Option<u64>,
    camera_frames: Option<u64>,
    camera_tiling: Option<fmn_render::Tiling>,
}

impl Report {
    pub(super) fn from_plan(plan: &fmn_runtime::ExecutionPlan) -> Self {
        use fmn_runtime::{Determinism, ExecutionEngine, TuningSource};
        Self {
            source: match plan.tuning_source {
                TuningSource::CertifiedProfile => "certified-profile",
                TuningSource::StandardBaseline => "standard-baseline",
                TuningSource::StandardAutotuneCache => "standard-autotune-cache",
            },
            engine: match plan.engine {
                ExecutionEngine::CertifiedCpu => "certified-cpu",
                ExecutionEngine::FastCpu => "fast-cpu",
                ExecutionEngine::Metal => "metal",
                ExecutionEngine::Cuda => "cuda",
            },
            fine_tile: plan.fine_tile,
            macro_tile: plan.macro_tile,
            diagnostic: plan.tuning_diagnostic.clone(),
            bypass_reason: if plan.determinism == Determinism::Certified {
                Some("certified rendering does not read the autotune cache")
            } else if plan.engine != ExecutionEngine::FastCpu {
                Some("FastCpu raster calibration does not apply to this renderer engine")
            } else {
                None
            },
            planar_frames: None,
            camera_frames: None,
            camera_tiling: None,
        }
    }

    pub(super) fn with_file_routes(
        mut self,
        planar: u64,
        camera: u64,
        camera_tiling: fmn_render::Tiling,
    ) -> Self {
        self.planar_frames = Some(planar);
        self.camera_frames = Some(camera);
        self.camera_tiling = Some(camera_tiling);
        self
    }

    pub(super) fn to_python(&self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        let out = PyDict::new(py);
        out.set_item("schema", "fmn-python.raster-autotune")?;
        out.set_item("version", 1)?;
        out.set_item("source", self.source)?;
        out.set_item("applies_to", "retained-planar-2d")?;
        out.set_item("engine", self.engine)?;
        out.set_item("fine_tile", self.fine_tile)?;
        out.set_item("macro_tile", self.macro_tile)?;
        out.set_item("diagnostic", self.diagnostic.as_deref())?;
        out.set_item("bypass_reason", self.bypass_reason)?;
        out.set_item("planar_frames", self.planar_frames)?;
        out.set_item("camera_frames", self.camera_frames)?;
        out.set_item(
            "camera_fine_tile",
            self.camera_tiling.map(|tiling| tiling.fine_tile),
        )?;
        out.set_item(
            "camera_macro_tile",
            self.camera_tiling.map(|tiling| tiling.macro_tile),
        )?;
        Ok(out.unbind())
    }
}

#[cfg(test)]
mod tests;
