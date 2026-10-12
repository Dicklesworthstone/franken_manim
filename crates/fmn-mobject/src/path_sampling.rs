//! Retained world-space path observations for native animation and authoring.
//! This cache is derived state; it never enters a Stage snapshot or FMTL.
//!
//! The mobject layer owns invalidation and consumes the existing Chisel kernel.
//! Callers can keep one sampler per active path without adding geometry state
//! to every Stage entry or changing the dependency graph of the animation layer.

use fmn_core::types::Vec3;
use fmn_geom::{ArcLengthTable, QuadPath};

use crate::record::StorageIdentity;
use crate::{Entry, Mob, Placement, Stage, StageError};

struct CachedPath {
    mob: Mob,
    storage: StorageIdentity,
    point_revision: Option<u64>,
    len: usize,
    placement: Placement,
    path: QuadPath,
    table: ArcLengthTable,
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

    /// Observe a point by true world-space arc length (BN-03).
    ///
    /// # Errors
    /// A stale handle or missing point field, an empty path, or malformed
    /// shared-anchor geometry retains the Stage query's named refusal. A
    /// refused observation never falls back to previously cached geometry.
    pub fn sample(&mut self, stage: &Stage, mob: Mob, alpha: f64) -> Result<Vec3, StageError> {
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
            let current = CachedPath {
                mob,
                storage: entry.buffer.storage_identity(),
                point_revision: entry.buffer.field_revision("point"),
                len: entry.buffer.len(),
                placement: entry.placement(),
                path,
                table,
            };
            self.current = Some(if let Some(mut slot) = old {
                *slot = current;
                slot
            } else {
                Box::new(current)
            });
            self.materializations = self.materializations.saturating_add(1);
        }
        let current = self.current.as_ref().expect("path cache initialized");
        current
            .path
            .point_from_proportion_with(&current.table, alpha)
            .ok_or(StageError::SchemaMismatch)
    }
}
