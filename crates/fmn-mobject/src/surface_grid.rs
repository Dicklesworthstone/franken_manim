//! UV-aware record alignment for sampled surfaces (§12.4).
//!
//! Resolution is topology, not merely a flat record count. Resampling uses
//! normalized UV coordinates and bilinear interpolation of every record lane,
//! including normal control points, paint and texture coordinates. No authored
//! sampler runs during alignment. This module is a child of `stage` so it can
//! publish a buffer and its durable topology together without replacing identity.

use super::{Entry, Mob, Stage};
use crate::{RecordBuffer, RenderPrimitive, StageError};

/// Maximum number of records in an aligned UV surface.
pub const MAX_SURFACE_GRID_POINTS: usize = 65_536;

/// A validated, prepared surface update. Preparation never changes a live entry.
/// Application preserves placement, materials, family, updaters and saved state.
pub struct SurfaceGridUpdate {
    before: RecordBuffer,
    old_resolution: (usize, usize),
    resolution: (usize, usize),
    buffer: RecordBuffer,
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
        old_resolution: old,
        resolution: shape,
        buffer,
    }))
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
    /// Publish prepared records and UV topology together. Old views detach via
    /// the normal RecordBuffer generation protocol. Refuse a stale preparation
    /// rather than overwrite record edits made since it was captured.
    pub fn apply_surface_grid_update(
        &mut self,
        mob: Mob,
        update: SurfaceGridUpdate,
    ) -> Result<(), StageError> {
        let entry = self.get_mut(mob).ok_or(StageError::StaleHandle)?;
        if resolution(entry)? != update.old_resolution
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
        if !entry.buffer.assign_from(&update.buffer) {
            return Err(StageError::SchemaMismatch);
        }
        entry.render_primitive = RenderPrimitive::SurfaceGrid {
            resolution: update.resolution,
        };
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
