//! Texture constructors over Atlas records, Marionette resources, and Lumen sampling.
//! No new renderer, decoder, or snapshot side channel is introduced.
use super::*;
use fmn_mobject::RenderPrimitive;

const MAX_TEXTURE_BYTES: usize = 64 * 1024 * 1024;

fn image(payload: &Bound<'_, PyBytes>) -> PyResult<fmn_mobject::ImageResource> {
    if payload.as_bytes().len() > MAX_TEXTURE_BYTES {
        return Err(PyValueError::new_err(
            "texture input exceeds the 64 MiB encoded budget",
        ));
    }
    let mut decoded: Mobject = fmn_library::ImageMobject::from_bytes(payload.as_bytes())
        .map_err(|error| PyValueError::new_err(error.to_string()))?
        .into();
    decoded
        .image
        .take()
        .ok_or_else(|| PyRuntimeError::new_err("native image decoder returned no resource"))
}

fn detached(target: &Bound<'_, BridgeMobject>) -> PyResult<()> {
    if target.try_borrow()?.engine.is_some() {
        return Err(PyRuntimeError::new_err(
            "a texture constructor requires a detached target",
        ));
    }
    Ok(())
}

#[pyfunction]
fn _build_textured_surface<'py>(
    target: &Bound<'py, BridgeMobject>,
    source: &Bound<'py, BridgeMobject>,
    payload: &Bound<'py, PyBytes>,
    factory: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyList>> {
    detached(target)?;
    if target.is(source) {
        return Err(PyValueError::new_err(
            "TexturedSurface must copy a distinct source surface",
        ));
    }
    // Decode first: unreadable input cannot mutate source or target state.
    let image = image(payload)?;
    let mut tree = with_stage(source, |stage, mob| -> PyResult<Mobject> {
        stage.bake_placement(mob).map_err(stage_error)?;
        let entry = stage
            .get(mob)
            .ok_or_else(|| StaleHandleError::new_err("source surface is stale"))?;
        let RenderPrimitive::SurfaceGrid {
            resolution: (nu, nv),
        } = entry.render_primitive()
        else {
            return Err(PyTypeError::new_err(
                "TexturedSurface requires a native sampled UV surface",
            ));
        };
        let count = nu
            .checked_mul(nv)
            .filter(|&count| {
                nu >= 2
                    && nv >= 2
                    && count == entry.buffer.len()
                    && count <= fmn_library::SamplingBudget::DEFAULT.max_samples()
            })
            .ok_or_else(|| {
                PyValueError::new_err(
                    "TexturedSurface grid is inconsistent or exceeds the native sample budget",
                )
            })?;
        let mut buffer = RecordBuffer::new(fmn_library::solids::textured_surface_schema(), count)
            .map_err(native_error)?;
        for row in 0..count {
            for field in ["point", "d_normal_point"] {
                let values = entry
                    .buffer
                    .read(row, field)
                    .filter(|values| {
                        values.len() == 3 && values.iter().all(|value| value.is_finite())
                    })
                    .ok_or_else(|| {
                        PyValueError::new_err(
                            "surface point/normal records must be finite vec3 values",
                        )
                    })?;
                buffer.write(row, field, &values);
            }
            let opacity = entry
                .buffer
                .read(row, "opacity")
                .and_then(|value| value.first().copied())
                .or_else(|| {
                    entry
                        .buffer
                        .read(row, "rgba")
                        .and_then(|value| value.get(3).copied())
                })
                .filter(|value| value.is_finite())
                .ok_or_else(|| {
                    PyValueError::new_err("surface requires finite per-vertex opacity")
                })?;
            #[allow(clippy::cast_possible_truncation)]
            let uv = [
                (row / nv) as f32 / (nu - 1) as f32,
                1.0 - (row % nv) as f32 / (nv - 1) as f32,
            ];
            buffer.write(row, "im_coords", &uv);
            buffer.write(row, "opacity", &[opacity]);
        }
        Ok(Mobject::from_buffer(buffer)
            .with_uniforms(*entry.uniforms())
            .with_z_index(stage.z_index(mob))
            .with_render_primitive(entry.render_primitive()))
    })??;
    tree.image = Some(image);
    install_native_tree(target, factory, tree)
}

#[pyfunction]
fn _build_textured_geometry<'py>(
    target: &Bound<'py, BridgeMobject>,
    vertices: &Bound<'py, PyAny>,
    faces: &Bound<'py, PyAny>,
    uv: &Bound<'py, PyAny>,
    payload: &Bound<'py, PyBytes>,
    factory: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyList>> {
    detached(target)?;
    let limit = fmn_library::SamplingBudget::DEFAULT.max_samples();
    if vertices.len()? > limit || faces.len()? > limit * 3 || uv.len()? != vertices.len()? {
        return Err(PyValueError::new_err(
            "textured mesh exceeds its vertex/triangle budget or has mismatched UVs",
        ));
    }
    let vertices = vertices.extract::<Vec<[f64; 3]>>()?;
    let uv = uv.extract::<Vec<[f64; 2]>>()?;
    let faces = faces.extract::<Vec<u32>>()?;
    if !vertices
        .iter()
        .flatten()
        .chain(uv.iter().flatten())
        .all(|value| value.is_finite() && value.abs() <= f64::from(f32::MAX))
    {
        return Err(PyValueError::new_err(
            "textured mesh positions and UVs must be finite f32-representable values",
        ));
    }
    let image = image(payload)?;
    let mut tree: Mobject = fmn_library::TexturedGeometry::from_mesh(vertices, &faces, &uv, "")
        .map_err(|error| PyValueError::new_err(error.to_string()))?
        .into();
    tree.image = Some(image);
    // An extractor may execute arbitrary Python. Recheck target ownership
    // immediately before installing the complete, decoded native object.
    detached(target)?;
    install_native_tree(target, factory, tree)
}

pub(crate) fn install(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(_build_textured_surface, module)?)?;
    module.add_function(wrap_pyfunction!(_build_textured_geometry, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_textured_surface_and_mesh_acceptance() {
        crate::with_python_test_module("textured surface acceptance", |py, _module, globals| {
            let source =
                std::ffi::CString::new(include_str!("../tests/textured_surfaces.py")).unwrap();
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .unwrap();
            globals
                .get_item("verify_textured_surfaces")
                .unwrap()
                .unwrap()
                .call0()
                .inspect_err(|error| error.print(py))
                .unwrap();
            globals
                .get_item("verify_textured_rendering")
                .unwrap()
                .unwrap()
                .call0()
                .inspect_err(|error| error.print(py))
                .unwrap();
        });
    }
}

/// Run the native textured-surface pixel witness for the permanent Gauntlet.
///
/// # Errors
/// Returns the authored test failure if geometry, rendering or publication drifts.
#[cfg(feature = "gauntlet")]
pub fn run_portal_gauntlet_textures() -> Result<(Vec<u8>, Vec<u8>), String> {
    crate::with_python_test_module("texture gauntlet", |py, _module, globals| {
        let source = CString::new(include_str!("../tests/textured_surfaces.py"))
            .map_err(|error| error.to_string())?;
        py.run(source.as_c_str(), Some(globals), Some(globals))
            .map_err(|error| error.to_string())?;
        globals
            .get_item("verify_textured_rendering")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "texture rendering witness is missing".to_owned())?
            .call0()
            .and_then(|value| value.extract())
            .map_err(|error| error.to_string())
    })
}
