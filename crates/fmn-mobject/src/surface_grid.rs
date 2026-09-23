//! UV-aware record alignment for sampled surfaces (§12.4).
//!
//! Resolution is topology, not merely a flat record count. Resampling uses
//! normalized UV coordinates and bilinear interpolation of every record lane,
//! including normal control points, paint and texture coordinates. No authored
//! sampler runs during alignment. This module is a child of `stage` so it can
//! publish a buffer and its durable topology together without replacing identity.

use super::{Entry, Mob, Placement, Stage};
use crate::{RecordBuffer, RenderPrimitive, StageError};

/// Maximum number of records in an aligned UV surface.
pub const MAX_SURFACE_GRID_POINTS: usize = 65_536;

/// A validated, prepared surface update. Preparation never changes a live entry.
/// Alignment preserves placement; geometry replacement bakes it. Materials,
/// family, updaters and saved state retain their existing owners in both cases.
pub struct SurfaceGridUpdate {
    before: RecordBuffer,
    before_placement: Placement,
    old_resolution: (usize, usize),
    resolution: (usize, usize),
    buffer: RecordBuffer,
    placement: Placement,
}

fn grid_count(resolution: (usize, usize)) -> Result<usize, StageError> {
    let (u, v) = resolution;
    if u < 2 || v < 2 {
        return Err(StageError::SurfaceGrid(
            "both UV dimensions must be at least two",
        ));
    }
    u.checked_mul(v)
        .filter(|&count| count <= MAX_SURFACE_GRID_POINTS)
        .ok_or(StageError::SurfaceGrid(
            "UV grid exceeds the 65536-point budget",
        ))
}

fn resolution(entry: &Entry) -> Result<(usize, usize), StageError> {
    let RenderPrimitive::SurfaceGrid { resolution } = entry.render_primitive() else {
        return Err(StageError::SurfaceGrid(
            "alignment requires two UV-grid surfaces",
        ));
    };
    if grid_count(resolution)? != entry.buffer.len() {
        return Err(StageError::SurfaceGrid(
            "UV topology does not match the record count",
        ));
    }
    for key in ["point", "d_normal_point"] {
        if entry.buffer.schema().field_width(key) != Some(3) {
            return Err(StageError::SurfaceGrid(
                "surface point and normal-control fields must be vec3",
            ));
        }
    }
    Ok(resolution)
}

// Exact integer stations preserve existing knots and endpoints. The budget
// above bounds these products, independently of pointer width.
#[allow(clippy::cast_precision_loss)]
fn station(index: usize, old: usize, new: usize) -> (usize, usize, f64) {
    let numerator = index * (old - 1);
    let denominator = new - 1;
    let low = numerator / denominator;
    (
        low,
        (low + 1).min(old - 1),
        (numerator % denominator) as f64 / denominator as f64,
    )
}

fn lerp(a: f64, b: f64, alpha: f64) -> f64 {
    if alpha == 0.0 || a == b {
        a
    } else if alpha == 1.0 {
        b
    } else {
        (1.0 - alpha) * a + alpha * b
    }
}

fn prepare(entry: &Entry, shape: (usize, usize)) -> Result<Option<SurfaceGridUpdate>, StageError> {
    let old = resolution(entry)?;
    let count = grid_count(shape)?;
    // Validate even equal-topology pairs, before the peer can be changed.
    let columns: Vec<_> = entry
        .buffer
        .schema()
        .fields()
        .iter()
        .map(|field| {
            (
                field.name.clone(),
                field.width,
                entry.buffer.read_column(&field.name).unwrap_or_default(),
            )
        })
        .collect();
    if columns
        .iter()
        .any(|(_, _, data)| data.iter().any(|v| !v.is_finite()))
    {
        return Err(StageError::SurfaceGrid(
            "surface records must be finite before alignment",
        ));
    }
    if shape == old {
        return Ok(None);
    }
    let mut buffer =
        RecordBuffer::new(entry.buffer.schema().clone(), count).map_err(StageError::Record)?;
    for (name, width, data) in columns {
        let mut values = Vec::with_capacity(count * width);
        for u in 0..shape.0 {
            let (u0, u1, fu) = station(u, old.0, shape.0);
            for v in 0..shape.1 {
                let (v0, v1, fv) = station(v, old.1, shape.1);
                for lane in 0..width {
                    let sample = |ui, vi| f64::from(data[(ui * old.1 + vi) * width + lane]);
                    let a = lerp(sample(u0, v0), sample(u0, v1), fv);
                    let b = lerp(sample(u1, v0), sample(u1, v1), fv);
                    #[allow(clippy::cast_possible_truncation)]
                    let value = lerp(a, b, fu) as f32;
                    if !value.is_finite() {
                        return Err(StageError::SurfaceGrid(
                            "resampled surface records must be finite",
                        ));
                    }
                    values.push(value);
                }
            }
        }
        buffer.write_range(&name, 0, &values);
    }
    Ok(Some(SurfaceGridUpdate {
        before: entry.buffer.snapshot_clone(),
        before_placement: entry.placement(),
        old_resolution: old,
        resolution: shape,
        buffer,
        placement: entry.placement(),
    }))
}

/// Prepare a new geometric sample of an existing surface, at any admitted UV
/// resolution. Only positions and normal-control points come from the sampler;
/// all other record fields retain their normalized-UV interpolants. Materials,
/// uniforms and graph ownership are not part of this operation.
///
/// Input points are world-space. Auxiliary pointlike fields are baked through
/// the old placement before that placement is reset, matching geometry writes.
/// No entry is mutated until [`Stage::apply_surface_grid_update`] succeeds.
///
/// # Errors
/// Rejects malformed grids, nonfinite records, unrepresentable world-space
/// auxiliary points and inconsistent sample lengths before publication.
fn prepare_surface_grid_geometry(
    entry: &Entry,
    shape: (usize, usize),
    points: &[f32],
    normal_points: &[f32],
) -> Result<SurfaceGridUpdate, StageError> {
    let count = grid_count(shape)?;
    if points.len() != 3 * count || normal_points.len() != 3 * count {
        return Err(StageError::SurfaceGrid(
            "surface sample lengths do not match the UV grid",
        ));
    }
    if points
        .iter()
        .chain(normal_points)
        .any(|value| !value.is_finite())
    {
        return Err(StageError::SurfaceGrid(
            "surface geometry samples must be finite",
        ));
    }
    let mut update = match prepare(entry, shape)? {
        Some(update) => update,
        None => SurfaceGridUpdate {
            before: entry.buffer.snapshot_clone(),
            before_placement: entry.placement(),
            old_resolution: shape,
            resolution: shape,
            buffer: entry.buffer.deep_clone(),
            placement: entry.placement(),
        },
    };
    // The geometry sampler replaces these two fields. Every other declared
    // vec3 pointlike lane still belongs to the destination and must stay in
    // world space when its retained placement is discarded.
    if !update.placement.is_identity() {
        let keys = update.buffer.schema().pointlike_keys().to_vec();
        for key in keys {
            if key == "point"
                || key == "d_normal_point"
                || update.buffer.schema().field_width(&key) != Some(3)
            {
                continue;
            }
            let mut column = update.buffer.read_column(&key).unwrap_or_default();
            for point in column.as_chunks_mut::<3>().0 {
                let world = update.placement.apply_point(point.map(f64::from));
                if world
                    .iter()
                    .any(|value| !value.is_finite() || value.abs() > f64::from(f32::MAX))
                {
                    return Err(StageError::SurfaceGrid(
                        "surface auxiliary points exceed finite f32 storage",
                    ));
                }
                #[allow(clippy::cast_possible_truncation)]
                {
                    *point = world.map(|value| value as f32);
                }
            }
            update.buffer.write_range(&key, 0, &column);
        }
    }
    update.buffer.write_range("point", 0, points);
    update
        .buffer
        .write_range("d_normal_point", 0, normal_points);
    update.placement = Placement::IDENTITY;
    Ok(update)
}

impl SurfaceGridUpdate {
    /// Prepare an atomic geometry replacement, resampling all other record
    /// fields onto `shape` and baking auxiliary pointlike fields to world space.
    /// The input point and normal-control columns are already world-space.
    ///
    /// # Errors
    /// Invalid dimensions, schemas, numeric values or sample lengths refuse
    /// before any live records, placement or topology can be changed.
    pub fn for_geometry(
        entry: &Entry,
        shape: (usize, usize),
        points: &[f32],
        normal_points: &[f32],
    ) -> Result<Self, StageError> {
        prepare_surface_grid_geometry(entry, shape, points, normal_points)
    }
}

/// Prepare both sides on the component-wise maximum UV resolution. Neither
/// side is published if either topology, schema, numeric data or budget fails.
/// Equal record counts with different UV shapes still require alignment.
pub fn prepare_surface_grid_alignment(
    left: &Entry,
    right: &Entry,
) -> Result<(Option<SurfaceGridUpdate>, Option<SurfaceGridUpdate>), StageError> {
    let a = resolution(left)?;
    let b = resolution(right)?;
    if left.buffer.schema() != right.buffer.schema() {
        return Err(StageError::SchemaMismatch);
    }
    let shape = (a.0.max(b.0), a.1.max(b.1));
    grid_count(shape)?;
    Ok((prepare(left, shape)?, prepare(right, shape)?))
}

impl Stage {
    /// Publish prepared records, placement and UV topology together. Preserve
    /// live views when record count is unchanged; resized generations detach.
    /// Refuse stale preparation rather than overwrite later record/placement edits.
    pub fn apply_surface_grid_update(
        &mut self,
        mob: Mob,
        update: SurfaceGridUpdate,
    ) -> Result<(), StageError> {
        let entry = self.get_mut(mob).ok_or(StageError::StaleHandle)?;
        if resolution(entry)? != update.old_resolution
            || !entry.placement().same_bits(update.before_placement)
            || entry.buffer.schema() != update.before.schema()
            || entry
                .buffer
                .schema()
                .fields()
                .iter()
                .any(|field| !entry.buffer.column_eq(&update.before, &field.name))
        {
            return Err(StageError::SurfaceGrid(
                "surface changed after alignment was prepared",
            ));
        }
        if entry.buffer.len() == update.buffer.len() {
            // set_data/become intentionally detaches even equal-size views.
            // Regridding is an in-place edit instead: prepare all field copies
            // before writing, then use the existing snapshot-aware write path.
            let columns: Vec<_> = update
                .buffer
                .schema()
                .fields()
                .iter()
                .map(|field| {
                    (
                        field.name.clone(),
                        update.buffer.read_column(&field.name).unwrap_or_default(),
                    )
                })
                .collect();
            for (name, values) in columns {
                let written = entry.buffer.write_range(&name, 0, &values);
                debug_assert!(written, "prepared surface column has the validated layout");
            }
        } else if !entry.buffer.assign_from(&update.buffer) {
            return Err(StageError::SchemaMismatch);
        }
        entry.render_primitive = RenderPrimitive::SurfaceGrid {
            resolution: update.resolution,
        };
        entry.set_placement(update.placement);
        Ok(())
    }

    /// Regrid stored surface records without evaluating a geometry recipe.
    /// Supports refinement, coarsening and equal-count UV reshaping with the
    /// same bilinear record owner used by Transform alignment. Placement and
    /// every non-record owner are retained; equal shapes are true no-ops.
    ///
    /// # Errors
    /// A stale handle, invalid grid or nonfinite record refuses before mutation.
    pub fn resample_surface_grid(
        &mut self,
        mob: Mob,
        shape: (usize, usize),
    ) -> Result<(), StageError> {
        if let Some(update) = prepare(self.try_get(mob)?, shape)? {
            self.apply_surface_grid_update(mob, update)?;
        }
        Ok(())
    }

    /// Align only one pair of sampled UV entries. This is the same owner used
    /// by native Transform and by the optional host-Python portal.
    pub fn align_surface_points(&mut self, a: Mob, b: Mob) -> Result<(), StageError> {
        let (left, right) = prepare_surface_grid_alignment(self.try_get(a)?, self.try_get(b)?)?;
        if let Some(update) = left {
            self.apply_surface_grid_update(a, update)?;
        }
        if let Some(update) = right {
            self.apply_surface_grid_update(b, update)?;
        }
        Ok(())
    }
}
