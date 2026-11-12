//! Live surface geometry on the existing Stage/view protocol.
//! Sampling is Atlas's job. This seam validates and commits its complete result
//! without replacing the surface handle, family, material, or updater owner.
use super::*;

#[path = "portal_surface_init.rs"]
mod initialization;
#[cfg(feature = "gauntlet")]
pub use initialization::run_portal_gauntlet_surface_lifecycle;

fn grid(entry: &fmn_mobject::Entry) -> PyResult<Option<(usize, usize)>> {
    let RenderPrimitive::SurfaceGrid { resolution } = entry.render_primitive() else {
        return Ok(None);
    };
    let (nu, nv) = resolution;
    if nu < 2
        || nv < 2
        || nu.checked_mul(nv) != Some(entry.buffer.len())
        || entry.buffer.len() > fmn_library::SURFACE_SAMPLING_BUDGET.max_samples()
    {
        return Err(PyValueError::new_err(format!(
            "surface topology is inconsistent or exceeds the {}-point budget",
            fmn_library::SURFACE_SAMPLING_BUDGET.max_samples()
        )));
    }
    for key in ["point", "d_normal_point"] {
        if !entry
            .buffer
            .schema()
            .fields()
            .iter()
            .any(|f| f.name == key && f.width == 3)
        {
            return Err(PyValueError::new_err(
                "surface geometry requires point and normal-control vec3 fields",
            ));
        }
    }
    fmn_mobject::stage::surface_validity(&entry.buffer).map_err(native_error)?;
    Ok(Some(resolution))
}

#[pyfunction]
fn _surface_grid_resolution(target: &Bound<'_, BridgeMobject>) -> PyResult<Option<(usize, usize)>> {
    with_stage(target, |stage, mob| {
        grid(
            stage
                .get(mob)
                .ok_or_else(|| StaleHandleError::new_err("surface is stale"))?,
        )
    })?
}

/// Presence is owned by the native records, including intermediate morph
/// values. Python must not infer it from the finite inert geometry in holes.
#[pyfunction]
fn _surface_sample_validity(target: &Bound<'_, BridgeMobject>) -> PyResult<Vec<bool>> {
    with_stage(target, |stage, mob| -> PyResult<Vec<bool>> {
        let entry = stage
            .get(mob)
            .ok_or_else(|| StaleHandleError::new_err("surface is stale"))?;
        let count = entry.buffer.len();
        let limit = fmn_library::SURFACE_SAMPLING_BUDGET.max_samples();
        match entry.render_primitive() {
            RenderPrimitive::SurfaceGrid {
                resolution: (nu, nv),
            } if nu <= limit && nv <= limit && nu.checked_mul(nv) == Some(count) => {}
            // Empty Surface and SGroup roots deliberately have no drawable
            // topology; constructor-admitted strips still use SurfaceGrid.
            RenderPrimitive::Vector if count == 0 => {}
            _ => {
                return Err(PyTypeError::new_err(
                    "sample validity requires native UV surface topology",
                ));
            }
        }
        if count > limit {
            return Err(PyValueError::new_err(
                "surface exceeds the native sample budget",
            ));
        }
        let validity = fmn_mobject::stage::surface_validity(&entry.buffer).map_err(native_error)?;
        Ok((0..count)
            .map(|index| fmn_mobject::stage::surface_sample_is_present(validity.as_deref(), index))
            .collect())
    })?
}

/// Detached families publish through separate nursery roots. Preflight every
/// pair using Marionette's schema rule before Python changes the first root.
#[pyfunction]
fn _become_schema_compatible(
    left: &Bound<'_, BridgeMobject>,
    right: &Bound<'_, BridgeMobject>,
) -> PyResult<bool> {
    let describe = |target| {
        with_stage(target, |stage, mob| {
            let entry = stage
                .get(mob)
                .ok_or_else(|| StaleHandleError::new_err("become endpoint is stale"))?;
            Ok::<_, PyErr>((
                entry.buffer.schema().clone(),
                matches!(
                    entry.render_primitive(),
                    RenderPrimitive::SurfaceGrid { .. }
                ),
            ))
        })?
    };
    let (left_schema, left_surface) = describe(left)?;
    let (right_schema, right_surface) = describe(right)?;
    let surface_pair = left_surface || right_surface;
    if surface_pair {
        for target in [left, right] {
            with_stage(target, |stage, mob| {
                let entry = stage
                    .get(mob)
                    .ok_or_else(|| StaleHandleError::new_err("become endpoint is stale"))?;
                fmn_mobject::stage::surface_validity(&entry.buffer).map_err(native_error)?;
                Ok::<_, PyErr>(())
            })??;
        }
    }
    Ok(left_schema == right_schema
        || (surface_pair
            && fmn_mobject::stage::compatible_surface_schemas(&left_schema, &right_schema)))
}

struct Geometry {
    resolution: (usize, usize),
    points: Vec<f32>,
    normal_points: Vec<f32>,
    validity: Option<Vec<f32>>,
}

fn world_column(entry: &fmn_mobject::Entry, key: &str) -> PyResult<Vec<f32>> {
    let input = entry
        .buffer
        .read_column(key)
        .ok_or_else(|| PyValueError::new_err("surface field is missing"))?;
    let mut output = Vec::with_capacity(input.len());
    // An identity placement is a no-op, as in `Stage::bake_placement`.
    // Evaluating it anyway turns every -0.0 into +0.0 and breaks the
    // native-bit contract of sampled surfaces.
    let placement = entry.placement();
    let identity = placement.is_identity();
    for point in input.as_chunks::<3>().0 {
        let local = point.map(f64::from);
        let world = if identity {
            local
        } else {
            placement.apply_point(local)
        };
        if world
            .iter()
            .any(|v| !v.is_finite() || v.abs() > f64::from(f32::MAX))
        {
            return Err(PyValueError::new_err(
                "surface geometry must be finite and f32-representable",
            ));
        }
        #[allow(clippy::cast_possible_truncation)]
        output.extend(world.map(|v| v as f32));
    }
    Ok(output)
}

#[pyfunction]
fn _copy_surface_geometry(
    target: &Bound<'_, BridgeMobject>,
    source: &Bound<'_, BridgeMobject>,
) -> PyResult<()> {
    // Read the complete source first, releasing its engine before borrowing the
    // target. Source and target may share an engine or be in different nurseries.
    let geometry = with_stage(source, |stage, mob| -> PyResult<Geometry> {
        let entry = stage
            .get(mob)
            .ok_or_else(|| StaleHandleError::new_err("source surface is stale"))?;
        Ok(Geometry {
            resolution: grid(entry)?
                .ok_or_else(|| PyTypeError::new_err("source must be a native UV surface"))?,
            points: world_column(entry, "point")?,
            normal_points: world_column(entry, "d_normal_point")?,
            validity: fmn_mobject::stage::surface_validity(&entry.buffer).map_err(native_error)?,
        })
    })??;
    with_stage(target, |stage, mob| -> PyResult<()> {
        let entry = stage
            .get(mob)
            .ok_or_else(|| StaleHandleError::new_err("target surface is stale"))?;
        if grid(entry)? != Some(geometry.resolution) {
            return Err(PyValueError::new_err(
                "live surface regeneration requires matching native UV topology",
            ));
        }
        let has_validity = entry
            .buffer
            .schema()
            .field_width(fmn_mobject::stage::SURFACE_VALIDITY_FIELD)
            .is_some();
        if geometry.validity.is_some() && !has_validity {
            // Promoting the optional mask changes storage schema. Prepare the
            // complete native update before publishing that new generation.
            let update = fmn_mobject::stage::SurfaceGridUpdate::for_geometry_with_validity(
                entry,
                geometry.resolution,
                &geometry.points,
                &geometry.normal_points,
                geometry.validity.as_deref(),
            )
            .map_err(native_error)?;
            return stage
                .apply_surface_grid_update(mob, update)
                .map_err(native_error);
        }
        // Existing schemas keep the precise geometry writes and attached
        // views of ordinary live regeneration. All source values and target
        // fields have been validated; no host code runs during publication.
        stage.bake_placement(mob).map_err(stage_error)?;
        let entry = stage
            .get_mut(mob)
            .ok_or_else(|| StaleHandleError::new_err("target surface is stale"))?;
        entry.buffer.write_range("point", 0, &geometry.points);
        entry
            .buffer
            .write_range("d_normal_point", 0, &geometry.normal_points);
        if has_validity {
            let validity = geometry
                .validity
                .unwrap_or_else(|| vec![1.0; entry.buffer.len()]);
            entry
                .buffer
                .write_range(fmn_mobject::stage::SURFACE_VALIDITY_FIELD, 0, &validity);
        }
        Ok(())
    })?
}

/// Publish newly sampled positions at a different UV resolution. The shared
/// Marionette owner retains and regrids destination paint/UV/custom records;
/// this bridge only reads the source's world-space geometry.
#[pyfunction]
fn _regrid_surface_geometry(
    target: &Bound<'_, BridgeMobject>,
    source: &Bound<'_, BridgeMobject>,
) -> PyResult<()> {
    let geometry = with_stage(source, |stage, mob| -> PyResult<Geometry> {
        let entry = stage
            .get(mob)
            .ok_or_else(|| StaleHandleError::new_err("source surface is stale"))?;
        Ok(Geometry {
            resolution: grid(entry)?
                .ok_or_else(|| PyTypeError::new_err("source must be a native UV surface"))?,
            points: world_column(entry, "point")?,
            normal_points: world_column(entry, "d_normal_point")?,
            validity: fmn_mobject::stage::surface_validity(&entry.buffer).map_err(native_error)?,
        })
    })??;
    with_stage(target, |stage, mob| -> PyResult<()> {
        let entry = stage
            .get(mob)
            .ok_or_else(|| StaleHandleError::new_err("target surface is stale"))?;
        let update = fmn_mobject::stage::SurfaceGridUpdate::for_geometry_with_validity(
            entry,
            geometry.resolution,
            &geometry.points,
            &geometry.normal_points,
            geometry.validity.as_deref(),
        )
        .map_err(native_error)?;
        stage
            .apply_surface_grid_update(mob, update)
            .map_err(native_error)
    })?
}

/// Align proxies even when one is scene-bound and the other is detached.
/// Prepare both immutable entry copies before changing either actual owner.
/// The native Stage operation remains the only UV resampling implementation.
#[pyfunction]
fn _align_surface_grids(
    left: &Bound<'_, BridgeMobject>,
    right: &Bound<'_, BridgeMobject>,
) -> PyResult<()> {
    let mut preparation = Stage::new();
    let a = copy_proxy_entry_into(left, &mut preparation)?;
    let b = copy_proxy_entry_into(right, &mut preparation)?;
    let (a_update, b_update) = fmn_mobject::stage::prepare_surface_grid_alignment(
        preparation
            .get(a)
            .ok_or_else(|| StaleHandleError::new_err("left surface is stale"))?,
        preparation
            .get(b)
            .ok_or_else(|| StaleHandleError::new_err("right surface is stale"))?,
    )
    .map_err(native_error)?;
    // No authored code runs between preparation and publication. The Stage
    // also checks the captured record contents before accepting each update.
    if let Some(update) = a_update {
        with_stage(left, |stage, mob| {
            stage
                .apply_surface_grid_update(mob, update)
                .map_err(native_error)
        })??;
    }
    if let Some(update) = b_update {
        with_stage(right, |stage, mob| {
            stage
                .apply_surface_grid_update(mob, update)
                .map_err(native_error)
        })??;
    }
    Ok(())
}

pub(super) fn install(module: &Bound<'_, PyModule>) -> PyResult<()> {
    initialization::install(module)?;
    module.add_function(wrap_pyfunction!(_surface_grid_resolution, module)?)?;
    module.add_function(wrap_pyfunction!(_surface_sample_validity, module)?)?;
    module.add_function(wrap_pyfunction!(_become_schema_compatible, module)?)?;
    module.add_function(wrap_pyfunction!(_copy_surface_geometry, module)?)?;
    module.add_function(wrap_pyfunction!(_regrid_surface_geometry, module)?)?;
    module.add_function(wrap_pyfunction!(_align_surface_grids, module)?)?;
    Ok(())
}

/// Explicit undefined samples through Python construction, live geometry,
/// derived meshes/textures and native rendering.
///
/// # Errors
/// Returns the actual authored or native acceptance failure.
#[cfg(any(test, feature = "gauntlet"))]
pub fn run_portal_gauntlet_surface_holes() -> Result<u64, String> {
    crate::with_python_test_module("explicit surface holes", |py, _module, globals| {
        let code = std::ffi::CString::new(include_str!("../tests/surface_holes.py"))
            .map_err(|error| error.to_string())?;
        py.run(code.as_c_str(), Some(globals), Some(globals))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        globals
            .get_item("run_surface_holes")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "surface holes entry point is absent".to_owned())?
            .call0()
            .inspect_err(|error| error.print(py))
            .and_then(|value| value.extract::<u64>())
            .map_err(|error| error.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_surface_holes_and_native_rendering() {
        assert_eq!(run_portal_gauntlet_surface_holes().unwrap(), 17);
    }

    #[test]
    fn live_surface_regridding_and_native_output() {
        crate::with_python_test_module("live surface regridding", |py, _module, globals| {
            let code =
                std::ffi::CString::new(include_str!("../tests/surface_regridding.py")).unwrap();
            py.run(code.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .unwrap();
            globals
                .get_item("run_surface_regridding_acceptance")
                .unwrap()
                .unwrap()
                .call0()
                .inspect_err(|error| error.print(py))
                .unwrap();
        });
    }

    #[test]
    fn fallible_native_surface_sampling() {
        crate::with_python_test_module("fallible surface sampling", |py, _module, globals| {
            let code = std::ffi::CString::new(include_str!("../tests/native_surface_sampling.py"))
                .unwrap();
            py.run(code.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .unwrap();
            globals
                .get_item("run_native_surface_sampling")
                .unwrap()
                .unwrap()
                .call0()
                .inspect_err(|error| error.print(py))
                .unwrap();
        });
    }

    #[test]
    fn native_surface_alignment_and_morph_output() {
        crate::with_python_test_module("UV surface alignment", |py, _module, globals| {
            let code =
                std::ffi::CString::new(include_str!("../tests/surface_alignment.py")).unwrap();
            py.run(code.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .unwrap();
            globals
                .get_item("run_surface_alignment_acceptance")
                .unwrap()
                .unwrap()
                .call0()
                .inspect_err(|error| error.print(py))
                .unwrap();
        });
    }

    #[test]
    fn live_surface_wireframes_and_native_output() {
        crate::with_python_test_module("live surface wireframe", |py, _module, globals| {
            let code =
                std::ffi::CString::new(include_str!("../tests/live_surface_mesh.py")).unwrap();
            py.run(code.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .unwrap();
            globals
                .get_item("run_live_surface_mesh_acceptance")
                .unwrap()
                .unwrap()
                .call0()
                .inspect_err(|error| error.print(py))
                .unwrap();
        });
    }

    #[test]
    fn live_surface_regeneration_and_native_output() {
        crate::with_python_test_module("live surface geometry", |py, _module, globals| {
            let code =
                std::ffi::CString::new(include_str!("../tests/surface_geometry.py")).unwrap();
            py.run(code.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .unwrap();
            globals
                .get_item("run_surface_geometry_acceptance")
                .unwrap()
                .unwrap()
                .call0()
                .inspect_err(|error| error.print(py))
                .unwrap();
        });
    }
}
