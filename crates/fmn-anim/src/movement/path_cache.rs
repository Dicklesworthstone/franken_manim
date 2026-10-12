//! One animation's retained world-space Chisel path and inverse-length table.
//! This cache is derived state; it never enters a Stage snapshot or FMTL.

use fmn_core::types::Vec3;
use fmn_geom::{ArcLengthTable, QuadPath};
use fmn_mobject::record::StorageIdentity;
use fmn_mobject::{Entry, Mob, Placement, Stage, StageError};

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

#[derive(Default)]
pub(super) struct PathCache {
    current: Option<Box<CachedPath>>,
    materializations: u64,
}

// Cloning an animation copies its authored configuration, not a cache of the
// old arena. Reset the diagnostic too so the clone reports its own work.
impl Clone for PathCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for PathCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PathCache")
            .field("populated", &self.current.is_some())
            .field("materializations", &self.materializations)
            .finish()
    }
}

impl PathCache {
    pub(super) fn materializations(&self) -> u64 {
        self.materializations
    }

    pub(super) fn clear(&mut self) {
        self.current = None;
    }

    pub(super) fn sample(
        &mut self,
        stage: &Stage,
        mob: Mob,
        alpha: f64,
    ) -> Result<Vec3, StageError> {
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
