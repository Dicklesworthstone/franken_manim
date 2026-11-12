//! Bounded reuse of camera-bound projected draws.
//!
//! Cache entries own only derived rows and exact lightweight source identities.
//! A successful frame replaces the previous table atomically; no entry retains
//! a scene, texture binding, predecessor frame, or another cache generation.

use std::sync::{Arc, Mutex, PoisonError};

use super::{
    Camera, CompiledDraw, EdgeSampleLimit, PreparationBudget, PreparationRows,
    PreparedThreeDProjection, ProjectedDraw, ProjectedPrimitive, ProjectedRows, ThreeDDraw,
    ThreeDError, ThreeDJob, ThreeDPreparationLimits, TileScratchPool, Tiling, build_tile_draw_bins,
    build_triangle_tile_bins, compile_dot, compile_surface, compile_vector, same_projection_camera,
};
use crate::cache::CameraSourceKey;

/// Source of the rows in one immutable camera projection.
///
/// Replaying an already prepared frame does not prepare it again. These
/// counters describe the successful construction path, not each raster call;
/// discarded preparation attempts are excluded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CameraProjectionStats {
    /// Draws passed through the ordinary geometry/lighting/clipping compiler.
    pub compiled_draws: usize,
    /// Draws whose exact shared projection rows were reused after admission.
    pub reused_draws: usize,
}

/// The complete logical charge of one ordinary compile traversal.
///
/// Temporary allocations are intentionally included, just as in the cold
/// path. A reused draw cannot bypass any aggregate row or byte ceiling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CompileCharge {
    draws: u64,
    projected_curves: u64,
    fill_pieces: u64,
    raster_triangles: u64,
    working_bytes: u64,
}

impl CompileCharge {
    fn capture(budget: &PreparationBudget) -> Self {
        Self {
            draws: budget.draws,
            projected_curves: budget.projected_curves,
            fill_pieces: budget.fill_pieces,
            raster_triangles: budget.raster_triangles,
            working_bytes: budget.working_bytes,
        }
    }

    fn difference(self, before: Self) -> Result<Self, ThreeDError> {
        let subtract = |after: u64, before: u64| {
            after
                .checked_sub(before)
                .ok_or(ThreeDError::PreparationSizeOverflow {
                    resource: "retained projection charges",
                })
        };
        Ok(Self {
            draws: subtract(self.draws, before.draws)?,
            projected_curves: subtract(self.projected_curves, before.projected_curves)?,
            fill_pieces: subtract(self.fill_pieces, before.fill_pieces)?,
            raster_triangles: subtract(self.raster_triangles, before.raster_triangles)?,
            working_bytes: subtract(self.working_bytes, before.working_bytes)?,
        })
    }

    fn admit(self, budget: &mut PreparationBudget) -> Result<(), ThreeDError> {
        let mut totals = [0_u64; 4];
        for (index, (resource, additional)) in [
            (PreparationRows::Draws, self.draws),
            (PreparationRows::ProjectedCurves, self.projected_curves),
            (PreparationRows::FillPieces, self.fill_pieces),
            (PreparationRows::RasterTriangles, self.raster_triangles),
        ]
        .into_iter()
        .enumerate()
        {
            let requested = budget.rows(resource).checked_add(additional).ok_or(
                ThreeDError::PreparationSizeOverflow {
                    resource: resource.resource(),
                },
            )?;
            let limit = resource.limit(budget.limits);
            if requested > limit {
                return Err(ThreeDError::PreparationLimitExceeded {
                    resource: resource.resource(),
                    limit,
                    requested,
                });
            }
            if resource == PreparationRows::ProjectedCurves && requested > u64::from(u32::MAX) {
                return Err(ThreeDError::PreparationLimitExceeded {
                    resource: "projected curve indices",
                    limit: u64::from(u32::MAX),
                    requested,
                });
            }
            totals[index] = requested;
        }
        let working_bytes = budget.working_bytes.checked_add(self.working_bytes).ok_or(
            ThreeDError::PreparationSizeOverflow {
                resource: "preparation working bytes",
            },
        )?;
        if working_bytes > budget.limits.max_working_bytes {
            return Err(ThreeDError::PreparationLimitExceeded {
                resource: "preparation working bytes",
                limit: budget.limits.max_working_bytes,
                requested: working_bytes,
            });
        }
        // Admission is atomic: a failed cached receipt leaves the compiler's
        // original budget untouched for its ordinary fallback.
        budget.draws = totals[0];
        budget.projected_curves = totals[1];
        budget.fill_pieces = totals[2];
        budget.raster_triangles = totals[3];
        budget.working_bytes = working_bytes;
        Ok(())
    }
}

const SOURCE_OWNER_BYTES: usize = std::mem::size_of::<[usize; 2]>();

#[derive(Debug)]
struct CachedDraw {
    index: usize,
    source: CameraSourceKey,
    projected: Option<ProjectedDraw<()>>,
    charge: CompileCharge,
    payload_bytes: usize,
}

#[derive(Debug)]
struct CacheState {
    camera: Option<Camera>,
    entries: Vec<CachedDraw>,
    retained_bytes: usize,
    max_bytes: usize,
    epoch: Arc<()>,
}

/// Optional cache shared by the captures of one retained renderer.
///
/// The cap includes actual projected-row/profile/buffer capacities, shared
/// owners, source owners, and entry-table capacity. Shared allocations are
/// conservatively charged once per entry; allocator bookkeeping and this
/// fixed control object are excluded. Old frame permits remain responsible
/// for their own admitted rows after a cache entry is released.
///
/// Publication planning shares the current frame's already-admitted rows and
/// may temporarily allocate one additional bounded entry table. Callers must
/// provide that metadata/allocator headroom in addition to the retained cap.
/// No compilation or rasterization runs while this cache's lock is held.
#[derive(Debug)]
pub(crate) struct CameraProjectionCache {
    state: Mutex<CacheState>,
}

impl CameraProjectionCache {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            state: Mutex::new(CacheState {
                camera: None,
                entries: Vec::new(),
                retained_bytes: 0,
                max_bytes,
                epoch: Arc::new(()),
            }),
        }
    }

    pub(crate) fn set_budget(&self, max_bytes: usize) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.max_bytes == max_bytes {
            return;
        }
        state.max_bytes = max_bytes;
        state.retained_bytes = 0;
        state.camera = None;
        state.epoch = Arc::new(());
        let retired = std::mem::take(&mut state.entries);
        drop(state);
        drop(retired);
    }

    pub(crate) fn clear(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.retained_bytes = 0;
        state.camera = None;
        state.epoch = Arc::new(());
        let retired = std::mem::take(&mut state.entries);
        drop(state);
        drop(retired);
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retained_bytes
    }

    fn begin(&self, camera: &Camera) -> Publication {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut retired = Vec::new();
        if !state
            .camera
            .as_ref()
            .is_some_and(|before| same_projection_camera(before, camera))
        {
            // Incompatible projections cease to be cache-owned before the
            // ordinary compiler allocates a replacement camera generation.
            retired = std::mem::take(&mut state.entries);
            state.retained_bytes = 0;
            state.camera = Some(camera.clone());
            state.epoch = Arc::new(());
        }
        let publication = Publication {
            epoch: Arc::clone(&state.epoch),
            max_bytes: state.max_bytes,
            entries: Vec::new(),
            payload_bytes: 0,
            final_growth: false,
        };
        drop(state);
        drop(retired);
        publication
    }

    fn reuse(
        &self,
        publication: &Publication,
        index: usize,
        source: &CameraSourceKey,
        budget: &mut PreparationBudget,
    ) -> Option<(ProjectedDraw<()>, CompileCharge)> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !Arc::ptr_eq(&state.epoch, &publication.epoch) {
            return None;
        }
        let slot = state
            .entries
            .binary_search_by_key(&index, |entry| entry.index)
            .ok()?;
        let entry = &mut state.entries[slot];
        if let Some(projected) = entry.projected.as_ref()
            && entry.source.matches(source)
            && entry.charge.admit(budget).is_ok()
        {
            return Some((projected.clone(), entry.charge));
        }

        // Do not retain an incompatible or unaffordable historical payload
        // alongside the fallback compile. Keep only the sorted metadata slot;
        // successful publication replaces the table, without per-draw shifts.
        let retired = entry.projected.take();
        let bytes = std::mem::take(&mut entry.payload_bytes);
        state.retained_bytes = state.retained_bytes.saturating_sub(bytes);
        drop(state);
        drop(retired);
        None
    }

    fn discard_epoch(&self, epoch: &Arc<()>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !Arc::ptr_eq(&state.epoch, epoch) {
            return;
        }
        state.retained_bytes = 0;
        state.camera = None;
        state.epoch = Arc::new(());
        let retired = std::mem::take(&mut state.entries);
        drop(state);
        drop(retired);
    }

    fn publish(&self, publication: Publication) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !Arc::ptr_eq(&state.epoch, &publication.epoch) {
            return;
        }
        let bytes = publication.retained_bytes();
        debug_assert!(bytes <= state.max_bytes);
        state.retained_bytes = bytes;
        let retired = std::mem::replace(&mut state.entries, publication.entries);
        drop(state);
        drop(retired);
    }
}

#[derive(Debug)]
struct Publication {
    epoch: Arc<()>,
    max_bytes: usize,
    entries: Vec<CachedDraw>,
    // Actual shared row capacities plus the charged source identity owners.
    payload_bytes: usize,
    final_growth: bool,
}

impl Publication {
    fn retained_bytes(&self) -> usize {
        self.payload_bytes.saturating_add(
            self.entries
                .capacity()
                .saturating_mul(std::mem::size_of::<CachedDraw>()),
        )
    }

    fn enabled(&self) -> bool {
        self.max_bytes >= std::mem::size_of::<CachedDraw>() + SOURCE_OWNER_BYTES
    }

    fn retain(
        &mut self,
        index: usize,
        source: &CameraSourceKey,
        projected: &ProjectedDraw<()>,
        charge: CompileCharge,
        draw_count: usize,
    ) {
        if source.is_volatile() {
            return;
        }
        let Some(payload_bytes) = projected_storage_bytes(projected) else {
            return;
        };
        let Some(additional) = payload_bytes.checked_add(SOURCE_OWNER_BYTES) else {
            return;
        };
        let used = self.retained_bytes();
        if additional > self.max_bytes.saturating_sub(used) {
            return;
        }
        if self.entries.len() == self.entries.capacity() {
            if self.final_growth {
                return;
            }
            let before = self.entries.capacity();
            let desired_growth = before.max(1);
            let affordable_growth = self
                .max_bytes
                .saturating_sub(used)
                .saturating_sub(additional)
                / std::mem::size_of::<CachedDraw>();
            let growth = desired_growth
                .min(affordable_growth)
                .min(draw_count.saturating_sub(before));
            self.final_growth = growth < desired_growth;
            if growth == 0 || self.entries.try_reserve_exact(growth).is_err() {
                self.final_growth = true;
                return;
            }
            // The allocator may expose more capacity than requested. Account
            // for that capacity before keeping even one payload reference.
            if self.retained_bytes() > self.max_bytes {
                self.entries = Vec::new();
                self.payload_bytes = 0;
                self.final_growth = true;
                return;
            }
            if additional > self.max_bytes.saturating_sub(self.retained_bytes()) {
                self.final_growth = true;
                return;
            }
        }
        self.entries.push(CachedDraw {
            index,
            source: source.clone(),
            projected: Some(projected.clone()),
            charge,
            payload_bytes,
        });
        self.payload_bytes += additional;
    }
}

fn vector_bytes<T>(rows: &Vec<T>) -> Option<usize> {
    rows.capacity().checked_mul(std::mem::size_of::<T>())
}

fn shared_rows_bytes<T>(rows: &ProjectedRows<T>) -> Option<usize> {
    let ProjectedRows::Shared(rows) = rows else {
        // Cache admission must never turn a clone into an implicit deep copy.
        return None;
    };
    vector_bytes(rows.as_ref())?.checked_add(std::mem::size_of::<(Vec<T>, [usize; 2])>())
}

fn projected_storage_bytes(draw: &ProjectedDraw<()>) -> Option<usize> {
    match &draw.primitive {
        ProjectedPrimitive::Triangles { triangles, .. } => shared_rows_bytes(triangles),
        ProjectedPrimitive::Vector(slot) => {
            let mut bytes = shared_rows_bytes(&slot.0)?;
            for vector in &slot.0 {
                bytes = bytes
                    .checked_add(vector_bytes(&vector.fill)?)?
                    .checked_add(vector_bytes(&vector.curves)?)?
                    .checked_add(vector_bytes(&vector.joins)?)?;
                if let Some(field) = &vector.field {
                    bytes = bytes
                        .checked_add(vector_bytes(&field.points)?)?
                        .checked_add(vector_bytes(&field.params)?)?
                        .checked_add(vector_bytes(&field.next)?)?
                        .checked_add(vector_bytes(&field.edge_params)?)?;
                }
                if let Some(bins) = &vector.bins {
                    bytes = bytes
                        .checked_add(vector_bytes(&bins.starts)?)?
                        .checked_add(vector_bytes(&bins.entries)?)?
                        .checked_add(vector_bytes(&bins.ranges)?)?;
                }
                if let Some(profile) = &vector.style.stroke_profile {
                    bytes = bytes.checked_add(profile.shared_storage_bytes()?)?;
                }
                if let Some(profile) = &vector.style.fill_profile {
                    bytes = bytes.checked_add(profile.shared_storage_bytes()?)?;
                }
            }
            Some(bytes)
        }
    }
}

impl PreparedThreeDProjection {
    pub(crate) fn preparation_stats(&self) -> CameraProjectionStats {
        self.preparation_stats
    }

    pub(crate) fn prepare_retained(
        camera: &Camera,
        draws: &[ThreeDDraw<'_>],
        sources: &[CameraSourceKey],
        tiling: Tiling,
        limits: ThreeDPreparationLimits,
        cache: &CameraProjectionCache,
    ) -> Result<Self, ThreeDError> {
        if draws.len() != sources.len() {
            return Err(ThreeDError::ProjectionMismatch);
        }
        let publication = cache.begin(camera);
        if !publication.enabled() {
            // Keep the ordinary compiler as the independent zero-budget path,
            // with no per-draw cache lookups or publication-table allocation.
            return ThreeDJob::new_with_limits(camera, draws, tiling, limits)?.into_projection();
        }
        let epoch = Arc::clone(&publication.epoch);
        match Self::try_prepare_retained(camera, draws, sources, tiling, limits, cache, publication)
        {
            Ok(projection) => Ok(projection),
            Err(
                ThreeDError::PreparationLimitExceeded { .. }
                | ThreeDError::PreparationSizeOverflow { .. }
                | ThreeDError::PreparationAllocationFailed { .. },
            ) => {
                // A roomy cached receipt may include optional indexes whose
                // charge fits now but crowds out a later mandatory draw.
                // Drop every pending row/table before trying the unchanged
                // full cold traversal, which can choose different optional
                // indexes under this frame's exact remaining allowance.
                // Clearing only our epoch cannot evict another camera's work.
                cache.discard_epoch(&epoch);
                ThreeDJob::new_with_limits(camera, draws, tiling, limits)?.into_projection()
            }
            Err(error) => Err(error),
        }
    }

    fn try_prepare_retained(
        camera: &Camera,
        draws: &[ThreeDDraw<'_>],
        sources: &[CameraSourceKey],
        tiling: Tiling,
        limits: ThreeDPreparationLimits,
        cache: &CameraProjectionCache,
        mut publication: Publication,
    ) -> Result<Self, ThreeDError> {
        let mut budget = PreparationBudget::new(limits);
        // Future direct rendering binds one ordinary draw row per command.
        // The owned table and binding table carry the same conservative charge
        // as cold new_with_limits followed by into_projection.
        budget.charge_rows::<CompiledDraw<'_>>(PreparationRows::Draws, draws.len())?;
        let mut projected = Vec::new();
        budget.reserve_retained(&mut projected, "retained projection rows", draws.len())?;
        let mut stats = CameraProjectionStats::default();
        // The camera's constants, once for the projection rather than per point.
        let projection = camera.projection();
        let location = camera.location();
        for (index, (&draw, source)) in draws.iter().zip(sources).enumerate() {
            let (row, charge) =
                if let Some(reused) = cache.reuse(&publication, index, source, &mut budget) {
                    stats.reused_draws += 1;
                    reused
                } else {
                    let before = CompileCharge::capture(&budget);
                    let row = match draw {
                        ThreeDDraw::Vector(vector) => compile_vector(camera, vector, &mut budget)?,
                        ThreeDDraw::Surface(surface) => {
                            compile_surface(camera, &projection, surface, &mut budget)?
                        }
                        ThreeDDraw::TrueDot(dot) => {
                            compile_dot(camera, &projection, location, dot, &mut budget)?
                        }
                    }
                    .share_rows(&mut budget)?
                    .map_material(|_| ());
                    let charge = CompileCharge::capture(&budget).difference(before)?;
                    stats.compiled_draws += 1;
                    (row, charge)
                };
            publication.retain(index, source, &row, charge, draws.len());
            projected.push(row);
        }
        let draw_bins = build_tile_draw_bins(
            &projected,
            camera.pixel_shape(),
            tiling.fine_tile,
            &mut budget,
        );
        let triangle_bins = build_triangle_tile_bins(
            &projected,
            camera.pixel_shape(),
            tiling.fine_tile,
            &mut budget,
        );
        let sample_grid = match camera.edge_sample_limit() {
            EdgeSampleLimit::Native => 1,
            EdgeSampleLimit::TwoByTwo => 2,
            EdgeSampleLimit::FourByFour => 4,
        };
        let result = Self {
            camera: camera.clone(),
            draws: projected,
            draw_bins,
            triangle_bins,
            tiling,
            sample_grid,
            preparation_bytes: budget.working_bytes,
            preparation_limits: limits,
            scratch_pool: Arc::new(TileScratchPool::new()),
            preparation_stats: stats,
        };
        // A failed compile never publishes a partial table. If another frame
        // changed camera/budget in flight, its epoch also rejects this table.
        cache.publish(publication);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::CameraConfig;
    use crate::three_d::{
        CompiledVector, CompiledVectorSlot, SurfaceDraw, SurfaceMesh, SurfaceVertex, TrueDotDraw,
        VectorDraw,
    };
    use crate::{FillKnot, FillProfile, RenderPlan, StrokeKnot, StrokeProfile, Style};
    use fmn_core::color::LinearRgba;
    use fmn_mobject::{Mobject, RecordBuffer, RecordSchema, Stage};

    fn camera() -> Camera {
        let mut config = CameraConfig {
            resolution: (48, 36),
            samples: 2,
            ..CameraConfig::default()
        };
        config.frame.set_shape([10.0, 7.5]).unwrap();
        Camera::new(config).unwrap()
    }

    fn tiling() -> Tiling {
        Tiling {
            macro_tile: 16,
            fine_tile: 8,
        }
    }

    fn color(r: f64, g: f64, b: f64) -> LinearRgba {
        LinearRgba { r, g, b, a: 0.8 }
    }

    fn sources(count: usize) -> Vec<CameraSourceKey> {
        (0..count)
            .map(|_| CameraSourceKey::new(&Arc::new(()), 0, false))
            .collect()
    }

    fn vector_plan() -> RenderPlan {
        let points = [
            [-3.0, -1.0, 0.0],
            [-2.0, -1.0, 0.0],
            [-1.0, -1.0, 0.0],
            [-1.0, 0.0, 0.0],
            [-1.0, 1.0, 0.0],
            [-2.0, 1.0, 0.0],
            [-3.0, 1.0, 0.0],
            [-3.0, 0.0, 0.0],
            [-3.0, -1.0, 0.0],
        ];
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len()).unwrap();
        for (index, point) in points.iter().enumerate() {
            let variation = (index % 3) as f32 * 0.15;
            assert!(buffer.write(index, "point", point));
            assert!(buffer.write(index, "fill_rgba", &[0.1 + variation, 0.6, 0.9, 0.65]));
            assert!(buffer.write(index, "stroke_rgba", &[0.9, 0.2 + variation, 0.1, 0.8]));
            assert!(buffer.write(index, "stroke_width", &[0.1 + variation]));
        }
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_buffer(buffer));
        stage.add_to_scene(mob).unwrap();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0).unwrap();
        plan
    }

    fn long_stroke_plan() -> RenderPlan {
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), 129).unwrap();
        for index in 0..129 {
            let x = index as f32 / 128.0 * 6.0 - 3.0;
            let y = if index % 4 < 2 { -1.0 } else { 1.0 };
            assert!(buffer.write(index, "point", &[x, y, 0.0]));
            assert!(buffer.write(index, "fill_rgba", &[0.0; 4]));
            assert!(buffer.write(index, "stroke_rgba", &[0.2, 0.7, 0.9, 1.0]));
            assert!(buffer.write(index, "stroke_width", &[0.1]));
        }
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_buffer(buffer));
        stage.add_to_scene(mob).unwrap();
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0).unwrap();
        plan
    }

    fn dense_mesh() -> SurfaceMesh {
        let mut vertices = Vec::new();
        for u in 0..8 {
            for v in 0..8 {
                let x = f64::from(u) / 7.0;
                let y = f64::from(v) / 7.0;
                vertices.push(SurfaceVertex::colored(
                    [x * 1.5 - 0.75, y * 2.0 - 1.0, 0.25],
                    [0.0, 0.0, 1.0],
                    color(0.3 + x * 0.5, 0.2 + y * 0.4, 0.1),
                ));
            }
        }
        SurfaceMesh::from_uv_grid(vertices, (8, 8)).unwrap()
    }

    fn mesh() -> SurfaceMesh {
        let paint = color(0.9, 0.6, 0.1);
        SurfaceMesh::new(
            [
                [-0.75, -1.0, 0.25],
                [0.75, -1.0, 0.25],
                [0.75, 1.0, 0.25],
                [-0.75, 1.0, 0.25],
            ]
            .map(|point| SurfaceVertex::colored(point, [0.0, 0.0, 1.0], paint))
            .to_vec(),
            vec![0, 1, 2, 0, 2, 3],
        )
        .unwrap()
    }

    fn mixed<'a>(plan: &'a RenderPlan, mesh: &'a SurfaceMesh, dot_y: f64) -> [ThreeDDraw<'a>; 3] {
        [
            ThreeDDraw::Vector(VectorDraw::new(plan, 0)),
            ThreeDDraw::Surface(SurfaceDraw::new(mesh)),
            ThreeDDraw::TrueDot(TrueDotDraw::new(
                [2.5, dot_y, 0.0],
                0.6,
                color(0.8, 0.1, 0.3),
            )),
        ]
    }

    #[test]
    fn retained_projection_reuses_unchanged_vectors_and_surfaces_but_not_changed_dots() {
        let camera = camera();
        let plan = vector_plan();
        let mesh = mesh();
        let mut sources = sources(3);
        let cache = CameraProjectionCache::new(4 * 1024 * 1024);
        let mut previous_pixels: Option<Vec<u8>> = None;
        for (step, y) in [0.0, 0.75, -0.5].into_iter().enumerate() {
            if step != 0 {
                sources[2] = CameraSourceKey::new(&Arc::new(()), 0, false);
            }
            let draws = mixed(&plan, &mesh, y);
            let projection = PreparedThreeDProjection::prepare_retained(
                &camera,
                &draws,
                &sources,
                tiling(),
                ThreeDPreparationLimits::default(),
                &cache,
            )
            .unwrap();
            assert_eq!(
                projection.preparation_stats(),
                if step == 0 {
                    CameraProjectionStats {
                        compiled_draws: 3,
                        reused_draws: 0,
                    }
                } else {
                    CameraProjectionStats {
                        compiled_draws: 1,
                        reused_draws: 2,
                    }
                },
            );
            let cold = ThreeDJob::new(&camera, &draws, tiling())
                .unwrap()
                .render(1)
                .unwrap();
            for threads in [1, 4, 16] {
                let observed = projection
                    .bind(&camera, &draws)
                    .unwrap()
                    .render(threads)
                    .unwrap();
                assert_eq!(observed.as_bytes(), cold.as_bytes());
            }
            if let Some(previous) = &previous_pixels {
                assert_ne!(
                    cold.as_bytes(),
                    previous.as_slice(),
                    "the changed dot must remain visible"
                );
            }
            previous_pixels = Some(cold.as_bytes().to_vec());
            assert!(cache.retained_bytes() <= 4 * 1024 * 1024);
        }
    }

    #[test]
    fn retained_projection_rechecks_all_warm_row_limits_and_preserves_cold_errors() {
        let camera = camera();
        let plan = vector_plan();
        let mesh = mesh();
        let draws = mixed(&plan, &mesh, 0.0);
        let sources = sources(draws.len());
        let defaults = ThreeDPreparationLimits::default();
        for limits in [
            ThreeDPreparationLimits {
                max_draws: 2,
                ..defaults
            },
            ThreeDPreparationLimits {
                max_projected_curves: 0,
                ..defaults
            },
            ThreeDPreparationLimits {
                max_fill_pieces: 0,
                ..defaults
            },
            ThreeDPreparationLimits {
                max_raster_triangles: 0,
                ..defaults
            },
            ThreeDPreparationLimits {
                max_working_bytes: 0,
                ..defaults
            },
        ] {
            let cache = CameraProjectionCache::new(4 * 1024 * 1024);
            PreparedThreeDProjection::prepare_retained(
                &camera,
                &draws,
                &sources,
                tiling(),
                defaults,
                &cache,
            )
            .unwrap();
            assert!(
                cache.retained_bytes() > 0,
                "each limit must exercise a warm cache"
            );
            let cold = ThreeDJob::new_with_limits(&camera, &draws, tiling(), limits)
                .and_then(ThreeDJob::into_projection)
                .unwrap_err();
            let warm = PreparedThreeDProjection::prepare_retained(
                &camera,
                &draws,
                &sources,
                tiling(),
                limits,
                &cache,
            )
            .unwrap_err();
            assert_eq!(
                warm, cold,
                "warm reuse must preserve the cold refusal: {limits:?}"
            );
        }
    }

    #[test]
    fn retained_projection_tight_mixed_frames_keep_every_cold_admission() {
        let camera = camera();
        let plan = long_stroke_plan();
        let mesh = mesh();
        let dense = dense_mesh();
        let initial = mixed(&plan, &mesh, 0.0);
        let changed = mixed(&plan, &dense, 0.0);
        let keys = sources(3);
        let mut changed_keys = keys.clone();
        changed_keys[1] = CameraSourceKey::new(&Arc::new(()), 0, false);
        let defaults = ThreeDPreparationLimits::default();
        let full = ThreeDJob::new(&camera, &changed, tiling())
            .unwrap()
            .into_projection()
            .unwrap()
            .preparation_bytes;
        let mut admitted = 0;
        let mut refused = 0;
        // Roomy-cache receipts must not prevent the unchanged cold compiler
        // from making its own optional-index choices under tighter limits.
        let mut allowances = vec![0, full, full.saturating_add(1), full.saturating_mul(2)];
        allowances.extend((1..16).map(|part| full.saturating_mul(part) / 16));
        for max_working_bytes in allowances {
            let cache = CameraProjectionCache::new(4 * 1024 * 1024);
            let roomy = PreparedThreeDProjection::prepare_retained(
                &camera,
                &initial,
                &keys,
                tiling(),
                defaults,
                &cache,
            )
            .unwrap();
            let ProjectedPrimitive::Vector(vector) = &roomy.draws[0].primitive else {
                panic!("the first draw is the long vector");
            };
            assert!(
                vector.get().bins.is_some(),
                "the warm source must carry optional curve bins"
            );
            let limits = ThreeDPreparationLimits {
                max_working_bytes,
                ..defaults
            };
            let cold = ThreeDJob::new_with_limits(&camera, &changed, tiling(), limits)
                .and_then(ThreeDJob::into_projection);
            let warm = PreparedThreeDProjection::prepare_retained(
                &camera,
                &changed,
                &changed_keys,
                tiling(),
                limits,
                &cache,
            );
            match cold {
                Ok(cold) => {
                    admitted += 1;
                    let warm =
                        warm.expect("a valid cold frame must remain admitted with a warm cache");
                    let expected = cold.bind(&camera, &changed).unwrap().render(1).unwrap();
                    let actual = warm.bind(&camera, &changed).unwrap().render(1).unwrap();
                    assert_eq!(actual.as_bytes(), expected.as_bytes());
                    assert!(warm.preparation_bytes <= max_working_bytes);
                }
                Err(cold) => {
                    refused += 1;
                    if let Err(warm) = warm {
                        assert_eq!(warm, cold, "cold retry must preserve its named refusal");
                    }
                }
            }
        }
        assert!(admitted > 0 && refused > 0);
    }

    #[test]
    fn retained_projection_failure_never_publishes_a_partial_source_table() {
        let camera = camera();
        let limits = ThreeDPreparationLimits::default();
        let cache = CameraProjectionCache::new(1024 * 1024);
        let keys = sources(2);
        let first = TrueDotDraw::new([-1.0, 0.0, 0.0], 0.5, color(0.8, 0.2, 0.1));
        let second = TrueDotDraw::new([1.0, 0.0, 0.0], 0.5, color(0.1, 0.2, 0.8));
        let original = [ThreeDDraw::TrueDot(first), ThreeDDraw::TrueDot(second)];
        let frozen = PreparedThreeDProjection::prepare_retained(
            &camera,
            &original,
            &keys,
            tiling(),
            limits,
            &cache,
        )
        .unwrap();
        let before = frozen.bind(&camera, &original).unwrap().render(1).unwrap();

        let changed_keys = sources(2);
        let changed = TrueDotDraw {
            center: [-1.0, 0.75, 0.0],
            ..first
        };
        let invalid = TrueDotDraw {
            radius: -0.5,
            ..second
        };
        let error = PreparedThreeDProjection::prepare_retained(
            &camera,
            &[ThreeDDraw::TrueDot(changed), ThreeDDraw::TrueDot(invalid)],
            &changed_keys,
            tiling(),
            limits,
            &cache,
        )
        .unwrap_err();
        assert_eq!(error, ThreeDError::InvalidRadius);
        {
            let state = cache.state.lock().unwrap();
            assert!(state.entries.iter().all(|entry| entry.projected.is_none()));
        }
        let repaired = [ThreeDDraw::TrueDot(changed), ThreeDDraw::TrueDot(second)];
        let prepared = PreparedThreeDProjection::prepare_retained(
            &camera,
            &repaired,
            &changed_keys,
            tiling(),
            limits,
            &cache,
        )
        .unwrap();
        assert_eq!(prepared.preparation_stats().compiled_draws, 2);
        assert_eq!(prepared.preparation_stats().reused_draws, 0);
        let after = frozen.bind(&camera, &original).unwrap().render(1).unwrap();
        assert_eq!(
            after.as_bytes(),
            before.as_bytes(),
            "old captures retain their own rows"
        );
    }

    #[test]
    fn stale_projection_publication_cannot_restore_a_cleared_or_changed_cache() {
        let camera = camera();
        let cache = CameraProjectionCache::new(1024 * 1024);
        let key = sources(1);
        let dot = ThreeDDraw::TrueDot(TrueDotDraw::new([0.0; 3], 0.5, color(0.7, 0.1, 0.3)));
        let projected = PreparedThreeDProjection::prepare_retained(
            &camera,
            &[dot],
            &key,
            tiling(),
            ThreeDPreparationLimits::default(),
            &cache,
        )
        .unwrap();
        let charge = cache.state.lock().unwrap().entries[0].charge;
        let mut old = cache.begin(&camera);
        old.retain(0, &key[0], &projected.draws[0], charge, 1);
        assert!(old.retained_bytes() > 0);
        cache.set_budget(0);
        cache.publish(old);
        assert_eq!(cache.retained_bytes(), 0);

        cache.set_budget(1024 * 1024);
        let mut old = cache.begin(&camera);
        old.retain(0, &key[0], &projected.draws[0], charge, 1);
        let mut changed = camera.clone();
        changed.set_light_source_position([5.0, -4.0, 3.0]).unwrap();
        let _current = cache.begin(&changed);
        cache.publish(old);
        assert_eq!(cache.retained_bytes(), 0);
        assert!(same_projection_camera(
            cache.state.lock().unwrap().camera.as_ref().unwrap(),
            &changed
        ));
    }

    #[test]
    fn projection_cache_counts_profile_spare_capacity_and_releases_shared_owners() {
        let mut stroke_knots = Vec::with_capacity(4096);
        stroke_knots.extend([
            StrokeKnot {
                s: 0.0,
                width: 0.1,
                rgba: [1.0; 4],
            },
            StrokeKnot {
                s: 1.0,
                width: 0.2,
                rgba: [0.5; 4],
            },
        ]);
        let mut fill_knots = Vec::with_capacity(4096);
        fill_knots.extend([
            FillKnot {
                s: 0.0,
                rgba: [0.1, 0.2, 0.3, 1.0],
            },
            FillKnot {
                s: 1.0,
                rgba: [0.8, 0.7, 0.6, 1.0],
            },
        ]);
        let spare_bytes = stroke_knots.capacity() * std::mem::size_of::<StrokeKnot>()
            + fill_knots.capacity() * std::mem::size_of::<FillKnot>();
        let stroke = Arc::new(StrokeProfile::new(stroke_knots).unwrap());
        let fill = Arc::new(FillProfile::new(fill_knots).unwrap());
        let stroke_weak = Arc::downgrade(&stroke);
        let fill_weak = Arc::downgrade(&fill);
        let vector = CompiledVector {
            fill: Vec::new(),
            curves: Vec::new(),
            joins: Vec::new(),
            field: None,
            style: Style {
                stroke_profile: Some(stroke),
                fill_profile: Some(fill),
                ..Style::default()
            },
            normal: [0.0, 0.0, 1.0],
            fill_plane: None,
            draws_fill: false,
            draws_stroke: false,
            bins: None,
        };
        let row: ProjectedDraw<()> = ProjectedDraw {
            primitive: ProjectedPrimitive::Vector(CompiledVectorSlot(ProjectedRows::Shared(
                Arc::new(vec![vector]),
            ))),
            depth_test: false,
            bounds: None,
        };
        assert!(projected_storage_bytes(&row).unwrap() >= spare_bytes);
        let camera = camera();
        let key = sources(1);
        let cache = CameraProjectionCache::new(spare_bytes - 1);
        let mut tiny = cache.begin(&camera);
        tiny.retain(0, &key[0], &row, CompileCharge::default(), 1);
        cache.publish(tiny);
        assert_eq!(
            cache.retained_bytes(),
            0,
            "unused profile capacity is still owned memory"
        );

        let allowance = projected_storage_bytes(&row).unwrap()
            + SOURCE_OWNER_BYTES
            + std::mem::size_of::<CachedDraw>();
        cache.set_budget(allowance);
        let mut full = cache.begin(&camera);
        full.retain(0, &key[0], &row, CompileCharge::default(), 1);
        cache.publish(full);
        assert!(cache.retained_bytes() > 0 && cache.retained_bytes() <= allowance);
        drop(row);
        assert!(stroke_weak.upgrade().is_some() && fill_weak.upgrade().is_some());
        cache.clear();
        assert_eq!(cache.retained_bytes(), 0);
        assert!(stroke_weak.upgrade().is_none() && fill_weak.upgrade().is_none());
    }
}
