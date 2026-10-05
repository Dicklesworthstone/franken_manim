//! Production typesetting services over the SAME engines used by Tex, native
//! matrices, and markdown. No Python serialization or parallel layout kernel.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};

use super::with_tex_template;

const TEMPLATES: [&str; 3] = ["default", "basic", "empty"];

fn stats<'py>(py: Python<'py>, engine: &fmn_library::TexEngine) -> PyResult<Bound<'py, PyDict>> {
    let memory = engine.memory_cache_stats();
    let row = PyDict::new(py);
    row.set_item("persistent", engine.persistent_cache_enabled())?;
    row.set_item("memory_hits", memory.hits)?;
    row.set_item("memory_misses", memory.misses)?;
    row.set_item("memory_entries", memory.entries)?;
    row.set_item("memory_bytes", memory.bytes)?;
    row.set_item("disk_hits", engine.persistent_cache_hits())?;
    row.set_item("layout_computations", engine.layout_computations())?;
    row.set_item("pack", engine.pack_content_id())?;
    Ok(row)
}

/// A storage refusal is a report, not a typesetting failure. Engine/font
/// construction failures still surface normally rather than masquerading as
/// a cache miss. `None` explicitly detaches all three persistent namespaces.
#[pyfunction]
#[pyo3(signature = (directory=None))]
fn _configure_tex_cache<'py>(
    py: Python<'py>,
    directory: Option<&str>,
) -> PyResult<Bound<'py, PyDict>> {
    if directory.is_some_and(|value| value.len() > 4096 || value.contains('\0')) {
        return Err(PyValueError::new_err(
            "typeset cache directory must contain at most 4096 UTF-8 bytes and no NUL",
        ));
    }
    let result = PyDict::new(py);
    result.set_item("schema", "fmn-python.typeset-cache")?;
    result.set_item("version", 1)?;
    result.set_item("requested", directory.is_some())?;
    result.set_item("directory", directory)?;
    let templates = PyDict::new(py);
    for name in TEMPLATES {
        let row = with_tex_template(name, |engine| {
            let error = engine
                .configure_host_cache(directory)
                .err()
                .map(|error| error.to_string().chars().take(2048).collect::<String>());
            let row = stats(py, engine)?;
            row.set_item("error", error)?;
            Ok(row)
        })?;
        templates.set_item(name, row)?;
    }
    result.set_item("templates", templates)?;
    Ok(result)
}

#[pyfunction]
fn _tex_cache_info(py: Python<'_>) -> PyResult<Bound<'_, PyDict>> {
    let result = PyDict::new(py);
    for name in TEMPLATES {
        result.set_item(name, with_tex_template(name, |engine| stats(py, engine))?)?;
    }
    Ok(result)
}

pub(super) fn install(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(_configure_tex_cache, module)?)?;
    module.add_function(wrap_pyfunction!(_tex_cache_info, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_tex_constructors_use_the_persistent_cache() {
        crate::with_python_test_module("typeset cache", |py, module, globals| {
            let source = std::ffi::CString::new(include_str!("../tests/typeset_cache.py"))
                .expect("typeset cache tests contain no NUL");
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .unwrap();
            globals
                .get_item("run_typeset_cache")
                .unwrap()
                .unwrap()
                .call1((module,))
                .inspect_err(|error| error.print(py))
                .unwrap();
        });
    }
}
