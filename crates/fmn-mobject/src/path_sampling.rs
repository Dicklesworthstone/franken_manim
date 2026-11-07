//! Retained world-space path observations for native animation and authoring.
//! This cache is derived state; it never enters a Stage snapshot or FMTL.
//!
//! The mobject layer owns invalidation and consumes the existing Chisel kernel.
//! Callers can keep one sampler per active path. Ordinary Stage queries share
//! a bounded cache, without adding geometry state to every arena entry or
//! changing the dependency graph of the animation layer.

use std::collections::VecDeque;
use std::sync::Arc;

use fmn_core::types::Vec3;
use fmn_geom::{ArcLengthTable, QuadPath};

use crate::record::StorageIdentity;
use crate::{Entry, Mob, Placement, Stage, StageError};

/// An immutable, owned observation of one path in world space.
///
/// Clones share the path and its true-arclength table without retaining a
/// Stage, mobject, record buffer or live view. The observation can be sampled
/// on another thread and remains valid after its source changes or is dropped.
/// Use [`PathSampler::observe`] again to observe the current source geometry.
#[derive(Debug, Clone)]
pub struct PathObservation {
    geometry: Arc<PathGeometry>,
}

#[derive(Debug)]
struct PathGeometry {
    path: QuadPath,
    table: ArcLengthTable,
}

impl PathObservation {
    /// Sample the captured path by true world-space arc length (BN-03).
    ///
    /// This uses the same kernel and clamping as [`PathSampler::sample`].
    /// Sampling never consults the source Stage or changes the observation.
    ///
    /// # Errors
    /// [`StageError::SchemaMismatch`] if the retained path cannot supply a
    /// point, matching the ordinary Stage path query.
    pub fn sample(&self, alpha: f64) -> Result<Vec3, StageError> {
        self.geometry
            .path
            .point_from_proportion_with(&self.geometry.table, alpha)
            .ok_or(StageError::SchemaMismatch)
    }

    /// The captured path's total true world-space arc length.
    #[must_use]
    pub fn total_length(&self) -> f64 {
        self.geometry.table.total()
    }

    /// Number of captured shared-anchor point records.
    #[must_use]
    pub fn point_count(&self) -> usize {
        self.geometry.path.points().len()
    }
}

struct CachedPath {
    mob: Mob,
    storage: StorageIdentity,
    point_revision: Option<u64>,
    len: usize,
    placement: Placement,
    observation: PathObservation,
}

impl CachedPath {
    fn matches(&self, mob: Mob, entry: &Entry) -> bool {
        self.mob == mob
            && !entry.buffer.writable_view_affects("point")
            && self.storage.matches(&entry.buffer)
            && self.point_revision == entry.buffer.field_revision("point")
            && self.len == entry.buffer.len()
            && self.placement.same_bits(entry.placement())
    }
}

/// A reusable, revision-aware observation of one world-space path.
///
/// Samples use the existing true-arclength kernel. Point writes, changed
/// placements and storage replacement invalidate the table; writable point
/// views force refresh at every observation. Cloning starts with an empty
/// cache, and the sampler holds neither a Stage nor a live record view.
#[derive(Default)]
pub struct PathSampler {
    current: Option<Box<CachedPath>>,
    materializations: u64,
}

// Clones never carry an observation of the previous owner's arena. Reset the
// diagnostic too so each sampler reports only its own work.
impl Clone for PathSampler {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for PathSampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PathSampler")
            .field("populated", &self.current.is_some())
            .field("materializations", &self.materializations)
            .finish()
    }
}

impl PathSampler {
    /// Successful world-space path/table builds, not the number of samples.
    #[must_use]
    pub fn materializations(&self) -> u64 {
        self.materializations
    }

    /// Release derived geometry while retaining the materialization count.
    pub fn clear(&mut self) {
        self.current = None;
    }

    fn retained_points(&self) -> usize {
        self.current
            .as_ref()
            .map_or(0, |entry| entry.observation.point_count())
    }

    /// Observe a point by true world-space arc length (BN-03).
    ///
    /// # Errors
    /// A stale handle or missing point field, an empty path, or malformed
    /// shared-anchor geometry retains the Stage query's named refusal. A
    /// refused observation never falls back to previously cached geometry.
    pub fn sample(&mut self, stage: &Stage, mob: Mob, alpha: f64) -> Result<Vec3, StageError> {
        // Ordinary sampling borrows the retained geometry: only observe()
        // needs to clone the Arc for an independently owned observation.
        self.ensure_cached(stage, mob)?.sample(alpha)
    }

    /// Capture an immutable, shareable observation of the current world path.
    ///
    /// Unchanged geometry shares the retained path and table. Source edits
    /// invalidate later observations without changing any already returned
    /// value; writable point views force a fresh capture on every call.
    ///
    /// # Errors
    /// The same stale-handle, missing-point, empty-path and malformed-geometry
    /// refusals as [`Self::sample`]. Failure clears the live cache, while any
    /// previously returned observations remain valid.
    pub fn observe(&mut self, stage: &Stage, mob: Mob) -> Result<PathObservation, StageError> {
        Ok(self.ensure_cached(stage, mob)?.clone())
    }

    fn ensure_cached(&mut self, stage: &Stage, mob: Mob) -> Result<&PathObservation, StageError> {
        let Some(entry) = stage.get(mob) else {
            self.clear();
            return Err(StageError::StaleHandle);
        };
        if !self
            .current
            .as_ref()
            .is_some_and(|path| path.matches(mob, entry))
        {
            // Discard the previous observation before any fallible work: an
            // invalid live path must not silently fall back to stale geometry.
            let old = self.current.take();
            let points = stage.get_points(mob).ok_or(StageError::StaleHandle)?;
            if points.is_empty() {
                return Err(StageError::SchemaMismatch);
            }
            // Preserve the uncached API's exact world-space rounding. Reusing
            // object-space lengths under an arbitrary affine transform would
            // be incorrect, especially under nonuniform scale or shear.
            let path = QuadPath::from_points(points).map_err(StageError::Geometry)?;
            let table = ArcLengthTable::for_path(&path);
            let geometry = PathGeometry { path, table };
            self.current = Some(if let Some(mut slot) = old {
                // Retain both allocations when no owned observation shares
                // this geometry. An outstanding observation keeps its exact
                // old path and table; only the live cache receives a new Arc.
                if let Some(previous) = Arc::get_mut(&mut slot.observation.geometry) {
                    *previous = geometry;
                } else {
                    slot.observation.geometry = Arc::new(geometry);
                }
                slot.mob = mob;
                slot.storage = entry.buffer.storage_identity();
                slot.point_revision = entry.buffer.field_revision("point");
                slot.len = entry.buffer.len();
                slot.placement = entry.placement();
                slot
            } else {
                Box::new(CachedPath {
                    mob,
                    storage: entry.buffer.storage_identity(),
                    point_revision: entry.buffer.field_revision("point"),
                    len: entry.buffer.len(),
                    placement: entry.placement(),
                    observation: PathObservation {
                        geometry: Arc::new(geometry),
                    },
                })
            });
            self.materializations = self.materializations.saturating_add(1);
        }
        Ok(&self
            .current
            .as_ref()
            .expect("path cache initialized")
            .observation)
    }
}

/// Maximum paths retained by a Stage's ordinary `point_from_proportion` calls.
/// Explicit [`PathSampler`] instances have independent lifetimes.
pub const MAX_QUERY_PATHS: usize = 64;
/// Maximum total point records retained by the Stage query cache. Larger
/// paths still sample correctly, but their derived geometry is not retained.
/// The points and two arc-length columns consume about four MiB at this limit.
pub const MAX_QUERY_PATH_POINTS: usize = 131_072;

/// Work and current storage of a Stage's derived path-query cache.
/// These diagnostics are not scene state and never enter snapshots or FMTL.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PathQueryStats {
    /// Successful world-space path/table builds since creation or restore.
    pub materializations: u64,
    /// Number of paths currently retained, at most [`MAX_QUERY_PATHS`].
    pub retained_paths: usize,
    /// Retained point records, at most [`MAX_QUERY_PATH_POINTS`].
    pub retained_points: usize,
}

/// LRU observations owned by one physical Stage, not by its persistent arena.
#[derive(Default)]
pub(crate) struct PathQueryCache {
    entries: VecDeque<(Mob, PathSampler)>,
    points: usize,
    materializations: u64,
}

impl PathQueryCache {
    pub(crate) fn stats(&self) -> PathQueryStats {
        PathQueryStats {
            materializations: self.materializations,
            retained_paths: self.entries.len(),
            retained_points: self.points,
        }
    }

    pub(crate) fn sample(
        &mut self,
        stage: &Stage,
        mob: Mob,
        alpha: f64,
    ) -> Result<Vec3, StageError> {
        // Removing an observation does not allocate. Keeping recently used
        // paths at the front makes a single repeatedly sampled path O(1).
        let mut sampler = self
            .entries
            .iter()
            .position(|(cached, _)| *cached == mob)
            .and_then(|index| self.entries.remove(index))
            .map_or_else(PathSampler::default, |(_, sampler)| sampler);
        self.points -= sampler.retained_points();
        let before = sampler.materializations();
        let result = sampler.sample(stage, mob, alpha);
        self.materializations = self
            .materializations
            .saturating_add(sampler.materializations() - before);

        // A failed or live-view observation must not retain geometry for later
        // use. A foreign write does not necessarily move the record revision;
        // after the last view drops, the next query must observe its final edit.
        let cacheable = result.is_ok()
            && stage
                .get(mob)
                .is_some_and(|entry| !entry.buffer.writable_view_affects("point"));
        let points = sampler.retained_points();
        if cacheable && points <= MAX_QUERY_PATH_POINTS {
            while self.entries.len() >= MAX_QUERY_PATHS
                || self.points + points > MAX_QUERY_PATH_POINTS
            {
                if let Some((_, old)) = self.entries.pop_back() {
                    self.points -= old.retained_points();
                } else {
                    break;
                }
            }
            self.points += points;
            self.entries.push_front((mob, sampler));
        }
        result
    }
}

impl Stage {
    /// Observe work performed by ordinary true-arclength path queries.
    /// Cache eviction or a restore may make a later query rebuild; neither
    /// affects its answer. Restore resets these diagnostics along with the
    /// cache. Explicit animation samplers count their own work.
    #[must_use]
    pub fn path_query_stats(&self) -> PathQueryStats {
        self.path_queries.borrow().stats()
    }
}
