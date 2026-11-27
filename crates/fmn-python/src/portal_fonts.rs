//! Interpreter-thread font catalog with scoped, content-identified user faces.
use fmn_hash::{Digest, sha256};
use fmn_library::FontBook;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyModule;
use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

const MAX_FONT_BYTES: usize = 64 * 1024 * 1024;

struct Catalog {
    book: Arc<FontBook>,
    active: BTreeMap<Digest, (String, usize)>,
}

type SharedCatalog = Arc<Mutex<Catalog>>;

thread_local! {
    static CATALOG: OnceCell<SharedCatalog> = const { OnceCell::new() };
}

fn catalog() -> PyResult<SharedCatalog> {
    CATALOG.with(|cell| {
        if let Some(catalog) = cell.get() {
            return Ok(Arc::clone(catalog));
        }
        let book = FontBook::bundled().map_err(|error| {
            PyRuntimeError::new_err(format!("bundled FontBook unavailable: {error}"))
        })?;
        Ok(Arc::clone(cell.get_or_init(|| {
            Arc::new(Mutex::new(Catalog {
                book: Arc::new(book),
                active: BTreeMap::new(),
            }))
        })))
    })
}

pub(super) fn with_font_book<T>(operation: impl FnOnce(&FontBook) -> PyResult<T>) -> PyResult<T> {
    // Release the catalog lock before any layout builder or Python callback.
    // Snapshots share face outlines and survive changes to the active scopes.
    let catalog = catalog()?;
    let book = Arc::clone(&catalog.lock().unwrap_or_else(PoisonError::into_inner).book);
    operation(&book)
}

#[pyclass(module = "manimlib")]
struct FontRegistration {
    catalog: Option<SharedCatalog>,
    digest: Digest,
}

#[pymethods]
impl FontRegistration {
    #[getter]
    fn sha256(&self) -> String {
        self.digest.to_hex()
    }

    fn close(&mut self) {
        let Some(catalog) = self.catalog.take() else {
            return;
        };
        let mut state = catalog.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, users)) = state.active.get_mut(&self.digest) {
            *users -= 1;
            if *users == 0 {
                let (name, _) = state.active.remove(&self.digest).expect("entry just found");
                Arc::make_mut(&mut state.book).remove_user_family(&name);
            }
        }
    }
}

impl Drop for FontRegistration {
    fn drop(&mut self) {
        self.close();
    }
}

#[pyfunction]
fn _register_font_bytes(bytes: &[u8]) -> PyResult<FontRegistration> {
    if bytes.len() > MAX_FONT_BYTES {
        return Err(PyValueError::new_err("font input exceeds 64 MiB"));
    }
    let digest = sha256(bytes);
    let catalog = catalog()?;
    {
        let mut state = catalog.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, users)) = state.active.get_mut(&digest) {
            *users += 1;
        } else {
            let name = FontBook::embedded_family_name(bytes)
                .ok_or_else(|| PyValueError::new_err("font has no usable OpenType family name"))?;
            Arc::make_mut(&mut state.book)
                .add_family(&name, bytes.to_vec())
                .map_err(super::native_error)?;
            state.active.insert(digest, (name, 1));
        }
    }
    Ok(FontRegistration {
        catalog: Some(catalog),
        digest,
    })
}

pub(super) fn install(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(_register_font_bytes, module)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::PyBytes;

    #[test]
    fn register_font_drives_native_geometry_and_scope() {
        crate::with_python_test_module("scoped font registration", |py, _, globals| {
            let bytes = fmn_library::bundled_faces()
                .iter()
                .find(|(name, _)| *name == "plex-regular")
                .unwrap()
                .1;
            globals
                .set_item("original_font", PyBytes::new(py, bytes))
                .unwrap();
            let source =
                std::ffi::CString::new(include_str!("../tests/registered_font.py")).unwrap();
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .unwrap();
        });
    }
}
