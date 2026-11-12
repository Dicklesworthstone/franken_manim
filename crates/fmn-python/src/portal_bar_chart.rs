//! One bounded geometry crossing for stock Python bar-chart updates.
//!
//! The Python adapter retains authored descriptors and point-editing hooks.
//! This seam receives already admitted proxies and numbers, plans through
//! Atlas's shared bar-chart kernel, and publishes only after every target is
//! valid. Detached nurseries and scene entries keep their original identities.

use fmn_library::BarChartBinding;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyModule, PyTuple};

use crate::{
    BridgeMobject, Mob, Mobject, RecordBuffer, RecordSchema, Stage, StaleHandleError, crossing,
    record_error_to_py, stage_error, with_stage,
};

const MAX_BARS: usize = 4096;
const MAX_PARENTS: usize = 65_536;

/// Copy only bounded stock geometry. Authored schemas/topologies keep their
/// public Python path; no updater, child or unrelated renderer payload needs
/// to enter the scratch arena for a numeric bar-height calculation.
fn copy_geometry(
    proxy: &Bound<'_, BridgeMobject>,
    candidate: &mut Stage,
    schema: &RecordSchema,
    count: usize,
) -> PyResult<Option<Mob>> {
    with_stage(proxy, |source, mob| {
        let entry = source
            .get(mob)
            .ok_or_else(|| StaleHandleError::new_err("bar-chart handle no longer resolves"))?;
        if entry.buffer.len() != count || entry.buffer.schema() != schema {
            return Ok(None);
        }
        let points = entry
            .buffer
            .read_column("point")
            .ok_or_else(|| PyValueError::new_err("bar-chart point field is missing"))?;
        let mut buffer = RecordBuffer::new(schema.clone(), count).map_err(record_error_to_py)?;
        buffer.write_range("point", 0, &points);
        let copied = candidate.add(Mobject::from_buffer(buffer));
        candidate
            .set_placement(copied, entry.placement())
            .map_err(stage_error)?;
        Ok(Some(copied))
    })?
}

#[pyfunction]
fn _change_bar_values(
    axis: &Bound<'_, BridgeMobject>,
    bars: &Bound<'_, PyTuple>,
    values: &Bound<'_, PyTuple>,
    parents: &Bound<'_, PyTuple>,
    max_value: f64,
    height_fraction: f64,
) -> PyResult<bool> {
    crossing::record(crossing::CrossingClass::FieldWrite);
    if bars.is_empty() || bars.len() > MAX_BARS || bars.len() != values.len() {
        return Err(PyValueError::new_err(
            "native bar updates require 1..4096 bars and one value per bar",
        ));
    }
    if parents.len() > MAX_PARENTS {
        return Err(PyValueError::new_err(
            "native bar update exceeds its ancestor budget",
        ));
    }

    // Extraction can invoke Python numeric conversions. Complete all of it
    // before borrowing any nursery or Scene, including private direct calls.
    let values = values
        .iter()
        .map(|value| value.extract::<f64>())
        .collect::<PyResult<Vec<_>>>()?;
    let bars = bars
        .iter()
        .map(|bar| bar.cast_into::<BridgeMobject>().map_err(Into::into))
        .collect::<PyResult<Vec<_>>>()?;
    let parents = parents
        .iter()
        .map(|parent| parent.cast_into::<BridgeMobject>().map_err(Into::into))
        .collect::<PyResult<Vec<_>>>()?;

    // The public Python graph may span several detached nurseries or a
    // partially bound family. Copy geometry, never bind or copy their children.
    // The scratch root is only a topology for the native kernel's admission.
    let mut candidate = Stage::new();
    let root = candidate.add(Mobject::new());
    let schema = RecordSchema::vmobject();
    let Some(y_axis) = copy_geometry(axis, &mut candidate, &schema, 3)? else {
        return Ok(false);
    };
    candidate.attach(root, y_axis).map_err(stage_error)?;
    let mut handles = Vec::with_capacity(bars.len());
    for bar in &bars {
        let Some(handle) = copy_geometry(bar, &mut candidate, &schema, 9)? else {
            return Ok(false);
        };
        // Stock bar.get_points exposes world-space f32 records, whereas the
        // axis's get_start/get_end reads f64 placement directly. Preserve both
        // observation rules; the live source is not modified during planning.
        candidate.bake_placement(handle).map_err(stage_error)?;
        candidate.attach(root, handle).map_err(stage_error)?;
        handles.push(handle);
    }
    for parent in &parents {
        if !with_stage(parent, |stage, mob| stage.get(mob).is_some())? {
            return Err(StaleHandleError::new_err(
                "bar-chart ancestor handle no longer resolves",
            ));
        }
    }
    let Ok(mut binding) = BarChartBinding::from_handles(
        &candidate,
        root,
        y_axis,
        &handles,
        max_value,
        height_fraction,
    ) else {
        // The native binding deliberately refuses degenerate bases. Python's
        // established point-editing path also supports authored collapsed
        // charts, so an unsupported candidate falls back before publication.
        return Ok(false);
    };
    if binding.change_bar_values(&mut candidate, &values).is_err() {
        return Ok(false);
    }
    let targets = handles
        .iter()
        .map(|&mob| {
            candidate.get_points(mob).ok_or_else(|| {
                StaleHandleError::new_err("planned bar-chart entry no longer resolves")
            })
        })
        .collect::<PyResult<Vec<_>>>()?;

    // There has been no Python call since extraction and no source mutation.
    // Every source has the exact schema/cardinality just validated on its
    // copy. These fixed-size writes preserve exported views and style lanes.
    // Publish the rounded point rows, not the candidate's derived angles:
    // stock VMobject.set_points writes f32 data first, then refreshes its
    // joint angles from those rounded rows. Stage::set_points does that same
    // final metadata computation here, including for lazy affine placements.
    for (bar, target) in bars.iter().zip(targets) {
        with_stage(bar, |stage, mob| stage.set_points(mob, &target))?.map_err(stage_error)?;
    }
    // Stage knows bound ancestors. Detached ancestor back-edges live only in
    // Python, so the adapter supplies the admitted, deduplicated parent walk.
    for parent in &parents {
        with_stage(parent, |stage, mob| {
            stage.refresh_bounding_box(mob, false, false);
        })?;
    }
    Ok(true)
}

pub(crate) fn install(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(_change_bar_values, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use pyo3::prelude::*;

    #[test]
    fn native_bar_chart_batch() {
        crate::with_python_test_module("native bar-chart batch", |py, _module, globals| {
            let code = std::ffi::CString::new(include_str!("../tests/live_bar_chart.py")).unwrap();
            py.run(code.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .unwrap();
            let run = std::ffi::CString::new(
                "suite = unittest.defaultTestLoader.loadTestsFromTestCase(LiveBarChartTests)\n\
                 result = unittest.TextTestRunner(verbosity=2).run(suite)\n\
                 assert result.wasSuccessful()\n",
            )
            .unwrap();
            py.run(run.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .unwrap();
        });
    }
}
