//! Translation-invariant bounds over the exact linear-placement results.
//!
//! Retain only per-member extrema, never record storage or point arrays. The
//! ordinary subtree box still owns invalidation, caller-installed boxes, and
//! materialization counts; these observations only avoid rescanning unchanged
//! geometry after a translation.

use std::collections::VecDeque;

use fmn_core::types::Vec3;

use crate::record::StorageIdentity;
use crate::stage::{SURFACE_VALIDITY_FIELD, surface_sample_is_present};
use crate::{Entry, Mob};

/// Maximum member observations retained by one physical Stage.
pub const MAX_LINEAR_BOUND_MEMBERS: usize = 128;

/// Work and retained metadata of a Stage's linear bounding-box observations.
///
/// These counters are derived execution state, outside snapshots and FMTL.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinearBoundsStats {
    /// Member point-column scans, including observations refused by the fast path.
    pub geometry_materializations: u64,
    /// Reused member observations, including remembered refusals.
    pub cache_hits: u64,
    /// Whole-family queries delegated to the original point-by-point fold.
    pub fallback_queries: u64,
    /// Compact member observations currently retained, at most the cache limit.
    pub cached_members: usize,
}

/// Bounds of present records only. Empty members never contribute an origin.
#[derive(Clone, Copy)]
pub(crate) enum MemberBounds {
    Empty,
    Extrema { min: Vec3, max: Vec3 },
}

impl MemberBounds {
    fn translated(self, translation: Vec3) -> Option<Self> {
        match self {
            Self::Empty => Some(Self::Empty),
            Self::Extrema { mut min, mut max } => {
                for ((min, max), shift) in min.iter_mut().zip(max.iter_mut()).zip(translation) {
                    // Keep Placement::apply_point's final addition unchanged.
                    *min += shift;
                    *max += shift;
                    if !min.is_finite() || !max.is_finite() {
                        return None;
                    }
                }
                Some(Self::Extrema { min, max })
            }
        }
    }
}

struct CachedMember {
    mob: Mob,
    storage: StorageIdentity,
    point_revision: Option<u64>,
    validity_revision: Option<u64>,
    len: usize,
    linear: [[u64; 3]; 3],
    // None remembers geometry whose original fold must remain authoritative.
    bounds: Option<MemberBounds>,
}

impl CachedMember {
    fn matches(&self, mob: Mob, entry: &Entry) -> bool {
        self.mob == mob
            && self.storage.matches(&entry.buffer)
            && self.point_revision == entry.buffer.field_revision("point")
            && self.validity_revision == entry.buffer.field_revision(SURFACE_VALIDITY_FIELD)
            && self.len == entry.buffer.len()
            && self.linear == entry.placement().linear().map(|row| row.map(f64::to_bits))
    }
}

/// A bounded LRU of non-owning member observations, most recently used first.
#[derive(Default)]
pub(crate) struct LinearBoundsCache {
    members: VecDeque<CachedMember>,
    geometry_materializations: u64,
    cache_hits: u64,
    fallback_queries: u64,
}

impl LinearBoundsCache {
    pub(crate) fn stats(&self) -> LinearBoundsStats {
        LinearBoundsStats {
            geometry_materializations: self.geometry_materializations,
            cache_hits: self.cache_hits,
            fallback_queries: self.fallback_queries,
            cached_members: self.members.len(),
        }
    }

    pub(crate) fn note_fallback(&mut self) {
        self.fallback_queries = self.fallback_queries.saturating_add(1);
    }

    /// Observe current world-space extrema without changing the placement law.
    pub(crate) fn observe(&mut self, mob: Mob, entry: &Entry) -> Option<MemberBounds> {
        let translation = entry.placement().translation();
        if entry.buffer.writable_view_affects("point")
            || entry.buffer.writable_view_affects(SURFACE_VALIDITY_FIELD)
            || translation.iter().any(|value| !value.is_finite())
        {
            return None;
        }

        if let Some(index) = self.members.iter().position(|member| member.mob == mob) {
            let member = self.members.remove(index).expect("member index exists");
            if member.matches(mob, entry) {
                self.cache_hits = self.cache_hits.saturating_add(1);
                let bounds = member.bounds;
                self.members.push_front(member);
                return bounds.and_then(|bounds| bounds.translated(translation));
            }
        }

        let bounds = self.materialize(entry);
        let member = CachedMember {
            mob,
            storage: entry.buffer.storage_identity(),
            point_revision: entry.buffer.field_revision("point"),
            validity_revision: entry.buffer.field_revision(SURFACE_VALIDITY_FIELD),
            len: entry.buffer.len(),
            linear: entry.placement().linear().map(|row| row.map(f64::to_bits)),
            bounds,
        };
        if self.members.len() == MAX_LINEAR_BOUND_MEMBERS {
            let _ = self.members.pop_back();
        }
        self.members.push_front(member);
        bounds.and_then(|bounds| bounds.translated(translation))
    }

    fn materialize(&mut self, entry: &Entry) -> Option<MemberBounds> {
        let Some(points) = entry.buffer.read_column("point") else {
            return Some(MemberBounds::Empty);
        };
        self.geometry_materializations = self.geometry_materializations.saturating_add(1);
        let validity = entry.buffer.read_column(SURFACE_VALIDITY_FIELD);
        let placement = entry.placement();
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        let mut any = false;
        for (index, point) in points.as_chunks::<3>().0.iter().enumerate() {
            if !surface_sample_is_present(validity.as_deref(), index) {
                continue;
            }
            let linear = placement.apply_vector([
                f64::from(point[0]),
                f64::from(point[1]),
                f64::from(point[2]),
            ]);
            for (axis, value) in linear.into_iter().enumerate() {
                // min/max permit either sign for mixed zero ties. Admit only
                // finite, non-negative-zero linear components so regrouping
                // cannot alter a signed-zero choice in the original fold.
                if !value.is_finite() || (value == 0.0 && value.is_sign_negative()) {
                    return None;
                }
                min[axis] = min[axis].min(value);
                max[axis] = max[axis].max(value);
            }
            any = true;
        }
        Some(if any {
            MemberBounds::Extrema { min, max }
        } else {
            MemberBounds::Empty
        })
    }
}
