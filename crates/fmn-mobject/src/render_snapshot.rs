//! Render-only persistence for already-observed frames.
//!
//! An undo/replay snapshot must retain the complete arena and its identities.
//! A recorded picture must not: unreachable animation copies, saved states,
//! proxy pins and updater registrations are not inputs to drawing that frame.
//! This projection retains the rooted family DAG, all record columns, shape
//! hints, placements, uniforms and image resources. It uses the existing FMNA
//! snapshot encoder, not a second wire format or a second renderer.
//!
//! Handles are renumbered in first-encounter, root/child order. Shared children
//! remain shared, and roots are neither flattened nor re-sorted: separate root
//! placements must still composite a shared drawable once per root. The result
//! is deliberately exposed only as durable bytes. It is NOT an undo state and
//! must never replace the live arena behind existing handles or Python proxies.
//!
//! A live Stage uses the same bounded projection directly at the owner-thread
//! capture boundary. It visits only rooted entries, without first snapshotting
//! detached history or copying its writable views and callback registrations.

use std::collections::HashMap;

use fmn_hash::{Limits, SerialError};

use crate::shape::ShapeSlot;
use crate::stage::{Mob, Snapshot, SnapshotEntry, SnapshotEntryData};
use crate::{ImageResource, Placement, RecordBuffer, RenderPrimitive, Stage, StageError, Uniforms};

/// Graph workspace admitted while projecting a recorded frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderSnapshotLimits {
    /// Cumulative logical storage for traversal, identity and destination graph
    /// tables. Record/image payloads retain the existing snapshot CoW policy;
    /// encoding is separately bounded by [`Limits::DEFAULT`]. This is not a
    /// bound on the live scene, allocator bookkeeping or encoder scratch space.
    pub max_graph_bytes: usize,
}

impl RenderSnapshotLimits {
    /// The same graph-work headroom as the ordinary snapshot decoder: four
    /// times the canonical document ceiling, independent of arena high-water.
    pub const DEFAULT: Self = Self {
        max_graph_bytes: 4 * Limits::DEFAULT.max_total,
    };
}

impl Default for RenderSnapshotLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A projection refusal leaves the input snapshot and live scene untouched.
#[derive(Debug)]
pub enum RenderSnapshotError {
    /// A rooted edge is stale/foreign, or the selected graph contains a cycle.
    Graph(StageError),
    /// Duplicate roots or child edges cannot be represented canonically.
    InvalidGraph(&'static str),
    /// Graph work was refused before the corresponding reservation.
    GraphLimit {
        /// Graph budget in force.
        limit: usize,
        /// Cumulative logical bytes requested.
        needed: usize,
        /// Destination or traversal table that exhausted it.
        context: &'static str,
    },
    /// The allocator refused graph storage already admitted by the budget.
    AllocationFailed {
        /// Destination or traversal table.
        context: &'static str,
    },
    /// The shared canonical snapshot writer refused the projected state.
    Serial(SerialError),
}

impl std::fmt::Display for RenderSnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Graph(error) => write!(f, "render snapshot graph: {error}"),
            Self::InvalidGraph(reason) => write!(f, "render snapshot graph: {reason}"),
            Self::GraphLimit {
                limit,
                needed,
                context,
            } => write!(
                f,
                "render snapshot {context} needs {needed} graph bytes; limit is {limit}"
            ),
            Self::AllocationFailed { context } => {
                write!(f, "render snapshot could not allocate {context}")
            }
            Self::Serial(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RenderSnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Graph(error) => Some(error),
            Self::Serial(error) => Some(error),
            Self::GraphLimit { .. } | Self::AllocationFailed { .. } | Self::InvalidGraph(_) => None,
        }
    }
}

struct Budget {
    limit: usize,
    charged: usize,
}

impl Budget {
    fn charge<T>(
        &mut self,
        count: usize,
        context: &'static str,
    ) -> Result<(), RenderSnapshotError> {
        let needed = count
            .checked_mul(std::mem::size_of::<T>())
            .and_then(|bytes| self.charged.checked_add(bytes));
        if needed.is_none_or(|needed| needed > self.limit) {
            return Err(RenderSnapshotError::GraphLimit {
                limit: self.limit,
                needed: needed.unwrap_or(usize::MAX),
                context,
            });
        }
        self.charged = needed.unwrap_or(self.limit);
        Ok(())
    }

    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
        context: &'static str,
    ) -> Result<(), RenderSnapshotError> {
        self.charge::<T>(additional, context)?;
        values
            .try_reserve(additional)
            .map_err(|_| RenderSnapshotError::AllocationFailed { context })
    }
}

#[derive(Clone, Copy)]
struct Visit {
    index: u32,
    active: bool,
    root_seen: bool,
}

fn snapshot_entry(snapshot: &Snapshot, mob: Mob) -> Result<&SnapshotEntry, RenderSnapshotError> {
    let (index, generation) = mob.parts();
    if mob != Mob::from_parts(snapshot.stage_id, index, generation) {
        return Err(RenderSnapshotError::Graph(StageError::StaleHandle));
    }
    snapshot
        .slots
        .get(index as usize)
        .filter(|(stored, _)| *stored == generation)
        .and_then(|(_, entry)| entry.as_ref())
        .ok_or(RenderSnapshotError::Graph(StageError::StaleHandle))
}

/// Borrow just the inputs to an observed picture. Neither source adapter
/// clones records or executable metadata while traversing the family graph.
struct RenderEntry<'a> {
    buffer: &'a RecordBuffer,
    submobjects: &'a [Mob],
    placement: Placement,
    uniforms: Uniforms,
    z_index: i32,
    shape: ShapeSlot,
    render_primitive: RenderPrimitive,
    image: Option<&'a ImageResource>,
}

#[derive(Clone, Copy)]
enum Source<'a> {
    Snapshot(&'a Snapshot),
    Stage(&'a Stage),
}

impl<'a> From<&'a Snapshot> for Source<'a> {
    fn from(snapshot: &'a Snapshot) -> Self {
        Self::Snapshot(snapshot)
    }
}

impl<'a> From<&'a Stage> for Source<'a> {
    fn from(stage: &'a Stage) -> Self {
        Self::Stage(stage)
    }
}

impl<'a> Source<'a> {
    fn roots(self) -> &'a [Mob] {
        match self {
            Self::Snapshot(snapshot) => &snapshot.roots,
            Self::Stage(stage) => stage.roots(),
        }
    }

    fn stage_id(self) -> u64 {
        match self {
            Self::Snapshot(snapshot) => snapshot.stage_id,
            Self::Stage(stage) => stage.stage_id(),
        }
    }

    fn frame_shape(self) -> [f64; 2] {
        match self {
            Self::Snapshot(snapshot) => snapshot.frame_shape,
            Self::Stage(stage) => stage.frame_shape(),
        }
    }

    fn entry(self, mob: Mob) -> Result<RenderEntry<'a>, RenderSnapshotError> {
        match self {
            Self::Snapshot(snapshot) => {
                let entry = snapshot_entry(snapshot, mob)?;
                Ok(RenderEntry {
                    buffer: &entry.buffer,
                    submobjects: &entry.submobjects,
                    placement: entry.placement,
                    uniforms: entry.uniforms,
                    z_index: entry.z_index,
                    shape: entry.shape,
                    render_primitive: entry.render_primitive,
                    image: entry.image.as_ref(),
                })
            }
            Self::Stage(stage) => {
                let entry = stage
                    .get(mob)
                    .ok_or(RenderSnapshotError::Graph(StageError::StaleHandle))?;
                Ok(RenderEntry {
                    buffer: &entry.buffer,
                    submobjects: entry.submobjects(),
                    placement: entry.placement(),
                    uniforms: *entry.uniforms(),
                    z_index: stage.z_index(mob),
                    shape: entry.shape(),
                    render_primitive: entry.render_primitive(),
                    image: entry.image_resource(),
                })
            }
        }
    }
}

fn discover(
    input: Source<'_>,
    mob: Mob,
    visits: &mut HashMap<Mob, Visit>,
    order: &mut Vec<Mob>,
    stack: &mut Vec<(Mob, usize)>,
    budget: &mut Budget,
) -> Result<(), RenderSnapshotError> {
    input.entry(mob)?;
    let index = u32::try_from(order.len()).map_err(|_| RenderSnapshotError::GraphLimit {
        limit: budget.limit,
        needed: usize::MAX,
        context: "addressable objects",
    })?;
    budget.charge::<(Mob, Visit)>(1, "identity table")?;
    visits
        .try_reserve(1)
        .map_err(|_| RenderSnapshotError::AllocationFailed {
            context: "identity table",
        })?;
    budget.reserve(order, 1, "object order")?;
    budget.reserve(stack, 1, "traversal stack")?;
    visits.insert(
        mob,
        Visit {
            index,
            active: true,
            root_seen: false,
        },
    );
    order.push(mob);
    stack.push((mob, 0));
    Ok(())
}

fn project<'a>(
    source: impl Into<Source<'a>>,
    limits: RenderSnapshotLimits,
) -> Result<Snapshot, RenderSnapshotError> {
    let input = source.into();
    let mut budget = Budget {
        limit: limits.max_graph_bytes,
        charged: 0,
    };
    let mut visits = HashMap::<Mob, Visit>::new();
    let mut order = Vec::new();
    let mut stack = Vec::new();
    // Iterative DFS uses stack space proportional to depth, not the number of
    // paths through a shared DAG. HashMap is used only for lookup; its random
    // iteration order cannot assign an identity or change serialized order.
    for &root in input.roots() {
        if let Some(visit) = visits.get_mut(&root) {
            if visit.root_seen {
                return Err(RenderSnapshotError::InvalidGraph("duplicate scene root"));
            }
            visit.root_seen = true;
            continue;
        }
        discover(
            input,
            root,
            &mut visits,
            &mut order,
            &mut stack,
            &mut budget,
        )?;
        visits
            .get_mut(&root)
            .ok_or(RenderSnapshotError::Graph(StageError::StaleHandle))?
            .root_seen = true;
        while let Some((mob, next)) = stack.last_mut() {
            let source = input.entry(*mob)?;
            if let Some(&child) = source.submobjects.get(*next) {
                *next += 1;
                match visits.get(&child) {
                    Some(visit) if visit.active => {
                        return Err(RenderSnapshotError::Graph(StageError::CycleDetected));
                    }
                    Some(_) => {}
                    None => discover(
                        input,
                        child,
                        &mut visits,
                        &mut order,
                        &mut stack,
                        &mut budget,
                    )?,
                }
            } else {
                let done = *mob;
                stack.pop();
                if let Some(visit) = visits.get_mut(&done) {
                    visit.active = false;
                }
            }
        }
    }
    let remap = |mob: Mob| -> Result<Mob, RenderSnapshotError> {
        visits
            .get(&mob)
            .map(|visit| Mob::from_parts(input.stage_id(), visit.index, 0))
            .ok_or(RenderSnapshotError::Graph(StageError::StaleHandle))
    };
    let mut slots = Vec::new();
    budget.reserve(&mut slots, order.len(), "projected slots")?;
    budget.charge::<[u8; SnapshotEntry::ALLOCATION_BYTES]>(order.len(), "projected metadata")?;
    for &mob in &order {
        let source = input.entry(mob)?;
        let mut children = Vec::new();
        budget.reserve(&mut children, source.submobjects.len(), "child links")?;
        for &child in source.submobjects {
            children.push(remap(child)?);
        }
        let mut identities = Vec::new();
        budget.reserve(&mut identities, children.len(), "child uniqueness")?;
        identities.extend(children.iter().map(|child| child.parts().0));
        identities.sort_unstable();
        if identities.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(RenderSnapshotError::InvalidGraph("duplicate child edge"));
        }
        let mut buffer = source.buffer.snapshot_clone();
        // Interpolation locks govern future writes, not the already-observed
        // record values. Removing them also avoids serializing execution history.
        buffer.unlock_data();
        // Preserve exactly the validity decision made for the frozen frame;
        // never revive a hint invalidated by point edits or writable views.
        let hint_is_live = source.shape.point_revision.is_some()
            && source.shape.point_revision == source.buffer.field_revision("point")
            && !source.buffer.writable_view_affects("point");
        let shape = ShapeSlot {
            tag: source.shape.tag,
            point_revision: hint_is_live
                .then(|| buffer.field_revision("point"))
                .flatten(),
        };
        slots.push((
            0,
            Some(SnapshotEntry::new(SnapshotEntryData {
                buffer,
                placement: source.placement,
                placement_revision: 0,
                submobjects: children.into(),
                parents: crate::stage::ArcVec::new(),
                updaters: crate::stage::RcVec::new(),
                updater_states: crate::stage::RcVec::new(),
                updating_suspended: false,
                is_animating: false,
                tracker: None,
                target: None,
                saved_state: None,
                padding_source: None,
                pins: 0,
                pending_delete: false,
                uniforms: source.uniforms,
                z_index: source.z_index,
                shape,
                render_primitive: source.render_primitive,
                image: source.image.cloned(),
                image_revision: u64::from(source.image.is_some()),
                family_cache: None,
            })),
        ));
    }
    // Rebuild reciprocal parents in canonical source/child order. Filtering
    // the original parent list would leak unrooted owners and attachment order.
    for &parent in &order {
        for &child in input.entry(parent)?.submobjects {
            let index = visits[&child].index as usize;
            let target = slots[index]
                .1
                .as_mut()
                .ok_or(RenderSnapshotError::Graph(StageError::StaleHandle))?;
            budget.reserve(target.parents.vec_mut(), 1, "parent links")?;
            target.parents.push(remap(parent)?);
        }
    }
    let mut roots = Vec::new();
    budget.reserve(&mut roots, input.roots().len(), "scene roots")?;
    for &root in input.roots() {
        roots.push(remap(root)?);
    }
    Ok(Snapshot {
        stage_id: input.stage_id(),
        next_updater_id: 1,
        frame_shape: input.frame_shape(),
        slots,
        free: crate::stage::ArcVec::new(),
        roots: roots.into(),
    })
}

impl Stage {
    /// Canonical picture bytes captured directly from this owner arena.
    ///
    /// Produces the same bytes as `self.snapshot().to_render_bytes()` while
    /// visiting only the rooted family DAG. Detached objects, their writable
    /// record views and updater registrations are never captured. Retained
    /// views use the ordinary snapshot isolation policy before encoding.
    ///
    /// This is a render-only artifact with remapped handles, not editable
    /// scene state. Live objects and their handles remain unchanged; undo and
    /// replay consumers must continue to use [`Stage::snapshot`]. No animation,
    /// updater or clock transition runs during this observation.
    ///
    /// # Errors
    /// As [`Snapshot::to_render_bytes`].
    pub fn to_render_bytes(&self) -> Result<Vec<u8>, RenderSnapshotError> {
        self.to_render_bytes_with_limits(RenderSnapshotLimits::DEFAULT)
    }

    /// Direct picture encoding with an explicit graph workspace budget.
    ///
    /// # Errors
    /// As [`Self::to_render_bytes`].
    pub fn to_render_bytes_with_limits(
        &self,
        limits: RenderSnapshotLimits,
    ) -> Result<Vec<u8>, RenderSnapshotError> {
        self.to_render_bytes_with_encoding_limits(limits, Limits::DEFAULT)
    }

    /// Direct picture encoding with independent graph and byte budgets.
    ///
    /// Shares the snapshot projector and canonical FMNA writer. Both limits
    /// are enforced before the corresponding allocation, independent of the
    /// live arena's allocation history and detached record sizes.
    ///
    /// # Errors
    /// As [`Self::to_render_bytes`], including a narrowed encoding size limit.
    pub fn to_render_bytes_with_encoding_limits(
        &self,
        graph_limits: RenderSnapshotLimits,
        encoding_limits: Limits,
    ) -> Result<Vec<u8>, RenderSnapshotError> {
        project(self, graph_limits)?
            .to_bytes_with_limits(encoding_limits)
            .map_err(RenderSnapshotError::Serial)
    }
}

impl Snapshot {
    /// Canonical bytes of this frame's renderable state, independent of
    /// unrooted arena history, handle generations and updater registration.
    ///
    /// The existing [`Snapshot::from_bytes`] decodes these bytes. This is a
    /// render-only artifact: handles are remapped and executable/restoration
    /// state is intentionally absent. For undo, scene state, pure animation
    /// endpoints and replay barriers, keep using [`Snapshot::to_bytes`].
    ///
    /// # Errors
    /// A malformed rooted graph, graph workspace refusal, allocation failure,
    /// or a refusal from the standard snapshot encoder.
    pub fn to_render_bytes(&self) -> Result<Vec<u8>, RenderSnapshotError> {
        self.to_render_bytes_with_limits(RenderSnapshotLimits::DEFAULT)
    }

    /// Render-only encoding with an explicit graph workspace budget.
    ///
    /// # Errors
    /// As [`Snapshot::to_render_bytes`].
    pub fn to_render_bytes_with_limits(
        &self,
        limits: RenderSnapshotLimits,
    ) -> Result<Vec<u8>, RenderSnapshotError> {
        self.to_render_bytes_with_encoding_limits(limits, Limits::DEFAULT)
    }

    /// Render-only encoding with independent graph and canonical byte budgets.
    ///
    /// The byte budget is enforced before each encoder growth. The graph
    /// budget still bounds projection workspace; neither changes the source
    /// snapshot or the bytes emitted for a successful encoding.
    ///
    /// # Errors
    /// As [`Self::to_render_bytes`], including a narrowed encoding size limit.
    pub fn to_render_bytes_with_encoding_limits(
        &self,
        graph_limits: RenderSnapshotLimits,
        encoding_limits: Limits,
    ) -> Result<Vec<u8>, RenderSnapshotError> {
        project(self, graph_limits)?
            .to_bytes_with_limits(encoding_limits)
            .map_err(RenderSnapshotError::Serial)
    }
}

#[cfg(test)]
#[path = "render_snapshot_tests.rs"]
mod tests;
