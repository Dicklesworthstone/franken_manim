//! An in-memory rendering handoff, not an undo/replay checkpoint.
//!
//! Only already-observed scene data crosses this boundary. Updater callables
//! and native updater state are deliberately absent from the representation,
//! so neither an Rc nor a callback destructor can migrate to a render worker.
//! Handles, generations, roots, DAG edges and rendering revisions are retained;
//! unlike durable render projection, this does not renumber the live chart.

use super::{
    ArcVec, Entry, IdBuildHasher, Mob, RcVec, Slot, Snapshot, SnapshotEntry, Stage, fresh_epoch,
};
use crate::bbox::BboxCache;
use crate::mobject::{ImageResource, RenderPrimitive};
use crate::placement::Placement;
use crate::record::RecordBuffer;
use crate::shape::ShapeSlot;
use crate::uniforms::Uniforms;
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

/// A callback-free, Send + Sync image of an already-observed scene.
///
/// Record and image storage follow the normal copy-on-write freeze protocol,
/// including eager isolation of exported views. This value can be moved to a
/// preparation worker and consumed there without another metadata clone.
/// It is NOT a rollback checkpoint or permission to evaluate animations on a
/// worker: materialization intentionally installs no updater registrations.
/// Use [`Snapshot`] for undo/replay, where retaining those callbacks matters.
#[derive(Clone)]
pub struct FrozenRenderState {
    stage_id: u64,
    next_updater_id: u64,
    frame_shape: [f64; 2],
    // Identity history stays dense, but an omitted object does not reserve an
    // entire FrozenRenderEntry. In-flight picture captures therefore retain
    // eight bytes per historical slot, not record buffers and entry metadata.
    slots: Vec<FrozenSlot>,
    entries: Vec<(u32, FrozenRenderEntry)>,
    free: ArcVec<u32>,
    roots: ArcVec<Mob>,
    time: f64,
}

#[derive(Clone, Copy)]
struct FrozenSlot {
    generation: u32,
    // An omitted live handle still belongs to this identity history. A later
    // restore/allocation on the materialized arena must never recreate it.
    occupied: bool,
}

struct VisibleMembers {
    order: Vec<Mob>,
    seen: HashSet<Mob, IdBuildHasher>,
}

impl VisibleMembers {
    fn discover<'a>(
        roots: &[Mob],
        auxiliary: &[Mob],
        children: impl Fn(Mob) -> Option<&'a [Mob]>,
    ) -> Self {
        let mut members = Self {
            order: Vec::new(),
            seen: HashSet::default(),
        };
        let mut pending: Vec<_> = roots.iter().chain(auxiliary).rev().copied().collect();
        while let Some(mob) = pending.pop() {
            if members.seen.contains(&mob) {
                continue;
            }
            let Some(children) = children(mob) else {
                continue;
            };
            members.seen.insert(mob);
            members.order.push(mob);
            pending.extend(children.iter().rev().copied());
        }
        members
    }

    fn retain_links(&self, entry: &mut FrozenRenderEntry) {
        // A shared child may also have an invisible parent. That incoming edge
        // is not part of the rooted picture; retain the shared visible edges
        // without changing child order, placements or any rendering revisions.
        if entry.parents.iter().any(|mob| !self.seen.contains(mob)) {
            entry.parents.retain(|mob| self.seen.contains(mob));
        }
        entry.target = entry.target.filter(|mob| self.seen.contains(mob));
        entry.saved_state = entry.saved_state.filter(|mob| self.seen.contains(mob));
        entry.padding_source = entry.padding_source.filter(|mob| self.seen.contains(mob));
    }
}

struct FrozenRenderEntry {
    buffer: RecordBuffer,
    placement: Placement,
    placement_revision: u64,
    submobjects: ArcVec<Mob>,
    parents: ArcVec<Mob>,
    updating_suspended: bool,
    is_animating: bool,
    tracker: Option<crate::dynamics::Tracker>,
    target: Option<Mob>,
    saved_state: Option<Mob>,
    padding_source: Option<Mob>,
    pins: usize,
    pending_delete: bool,
    uniforms: Uniforms,
    z_index: i32,
    shape: ShapeSlot,
    render_primitive: RenderPrimitive,
    image: Option<ImageResource>,
    image_revision: u64,
    family_cache: Option<Arc<Vec<Mob>>>,
}

impl Clone for FrozenRenderEntry {
    fn clone(&self) -> Self {
        Self {
            buffer: self.buffer.snapshot_clone(),
            placement: self.placement,
            placement_revision: self.placement_revision,
            submobjects: self.submobjects.clone(),
            parents: self.parents.clone(),
            updating_suspended: self.updating_suspended,
            is_animating: self.is_animating,
            tracker: self.tracker,
            target: self.target,
            saved_state: self.saved_state,
            padding_source: self.padding_source,
            pins: self.pins,
            pending_delete: self.pending_delete,
            uniforms: self.uniforms,
            z_index: self.z_index,
            shape: self.shape,
            render_primitive: self.render_primitive,
            image: self.image.clone(),
            image_revision: self.image_revision,
            family_cache: self.family_cache.clone(),
        }
    }
}

impl FrozenRenderEntry {
    fn capture(entry: &Entry) -> Self {
        let buffer = entry.buffer.snapshot_clone();
        let hint_was_live = entry.shape.point_revision.is_some()
            && entry.shape.point_revision == entry.buffer.field_revision("point")
            && !entry.buffer.writable_view_affects("point");
        Self {
            shape: ShapeSlot {
                tag: entry.shape.tag,
                point_revision: hint_was_live
                    .then(|| buffer.field_revision("point"))
                    .flatten(),
            },
            buffer,
            submobjects: entry.submobjects.clone(),
            parents: entry.parents.clone(),
            image: entry.image.clone(),
            placement: entry.placement,
            placement_revision: entry.placement_revision,
            updating_suspended: entry.updating_suspended,
            is_animating: entry.is_animating,
            tracker: entry.tracker,
            target: entry.target,
            saved_state: entry.saved_state,
            padding_source: entry.padding_source,
            pins: entry.pins,
            pending_delete: entry.pending_delete,
            uniforms: entry.uniforms,
            z_index: entry.z_index,
            render_primitive: entry.render_primitive,
            image_revision: entry.image_revision,
            family_cache: entry.family_cache.borrow().clone(),
        }
    }

    fn copy_snapshot(entry: &SnapshotEntry) -> Self {
        Self {
            buffer: entry.buffer.snapshot_clone(),
            submobjects: entry.submobjects.clone(),
            parents: entry.parents.clone(),
            image: entry.image.clone(),
            shape: entry.shape,
            placement: entry.placement,
            placement_revision: entry.placement_revision,
            updating_suspended: entry.updating_suspended,
            is_animating: entry.is_animating,
            tracker: entry.tracker,
            target: entry.target,
            saved_state: entry.saved_state,
            padding_source: entry.padding_source,
            pins: entry.pins,
            pending_delete: entry.pending_delete,
            uniforms: entry.uniforms,
            z_index: entry.z_index,
            render_primitive: entry.render_primitive,
            image_revision: entry.image_revision,
            family_cache: entry.family_cache.clone(),
        }
    }

    fn take_snapshot(entry: SnapshotEntry) -> Self {
        let entry = entry.into_data();
        // The fields not moved below (updaters and updater_states) are dropped
        // HERE on the scene owner, never in materialize on another thread.
        Self {
            buffer: entry.buffer,
            placement: entry.placement,
            placement_revision: entry.placement_revision,
            submobjects: entry.submobjects,
            parents: entry.parents,
            updating_suspended: entry.updating_suspended,
            is_animating: entry.is_animating,
            tracker: entry.tracker,
            target: entry.target,
            saved_state: entry.saved_state,
            padding_source: entry.padding_source,
            pins: entry.pins,
            pending_delete: entry.pending_delete,
            uniforms: entry.uniforms,
            z_index: entry.z_index,
            shape: entry.shape,
            render_primitive: entry.render_primitive,
            image: entry.image,
            image_revision: entry.image_revision,
            family_cache: entry.family_cache,
        }
    }

    fn into_entry(self) -> Entry {
        Entry {
            buffer: self.buffer,
            placement: self.placement,
            placement_revision: self.placement_revision,
            submobjects: self.submobjects,
            parents: self.parents,
            updating_suspended: self.updating_suspended,
            is_animating: self.is_animating,
            tracker: self.tracker,
            target: self.target,
            saved_state: self.saved_state,
            padding_source: self.padding_source,
            pins: self.pins,
            pending_delete: self.pending_delete,
            uniforms: self.uniforms,
            z_index: self.z_index,
            shape: self.shape,
            render_primitive: self.render_primitive,
            image: self.image,
            image_revision: self.image_revision,
            updaters: RcVec::new(),
            updater_states: RcVec::new(),
            children_epoch: fresh_epoch(),
            family_cache: RefCell::new(self.family_cache),
            bbox: RefCell::new(BboxCache::default()),
            snapshot_cache: RefCell::new(None),
        }
    }
}

impl FrozenRenderState {
    /// Number of objects whose metadata and record/image storage this handoff
    /// owns. Visible-only captures count each shared family member once and
    /// exclude detached history. Compact allocation identities are separate.
    #[must_use]
    pub fn retained_object_count(&self) -> usize {
        self.entries.len()
    }

    /// Immutable images retained by this handoff, including prepared animation
    /// endpoints. Hosts can validate input capabilities before worker dispatch.
    pub fn image_resources(&self) -> impl Iterator<Item = &ImageResource> {
        self.entries
            .iter()
            .filter_map(|(_, entry)| entry.image.as_ref())
    }

    /// Consume the handoff into a worker-local arena in the same handle domain.
    ///
    /// No callback runs here, and no updater callable or native updater state
    /// is installed. Record buffers and graph vectors move rather than clone;
    /// writes in either arena still obey the usual copy-on-write protocol.
    /// Allocation cursors belong to this new physical arena and begin after
    /// every identity recorded in the handoff, including deleted slots.
    #[must_use]
    pub fn materialize(self) -> Stage {
        let mut generation_high_water = Vec::with_capacity(self.slots.len());
        let mut slots: Vec<_> = self
            .slots
            .iter()
            .map(|slot| {
                generation_high_water
                    .push(slot.generation.saturating_add(u32::from(slot.occupied)));
                Slot {
                    generation: slot.generation,
                    entry: None,
                }
            })
            .collect();
        for (index, entry) in self.entries {
            slots[index as usize].entry = Some(entry.into_entry());
        }
        let mut free = self.free;
        for (index, (slot, identity)) in slots.iter_mut().zip(self.slots).enumerate() {
            if identity.occupied && slot.entry.is_none() {
                // Projection releases the hidden object only in this private
                // arena. Keep the ordinary free-slot ledger valid and retire
                // its old handle both in memory and in any serialized copy.
                slot.generation = identity.generation.saturating_add(1);
                free.push(index as u32);
            }
        }
        Stage {
            id: self.stage_id,
            slots,
            generation_high_water,
            free,
            roots: self.roots,
            time: self.time,
            next_updater_id: self.next_updater_id,
            updater_id_high_water: self.next_updater_id,
            topology_epoch: fresh_epoch(),
            updater_failure: None,
            updater_callbacks: std::rc::Rc::new(()),
            frame_shape: self.frame_shape,
            path_queries: RefCell::default(),
            linear_bounds: RefCell::default(),
        }
    }
}

impl Stage {
    /// Immutable images retained by this arena, including saved states and
    /// animation endpoints that may become visible during the next capture.
    pub fn image_resources(&self) -> impl Iterator<Item = &ImageResource> {
        self.slots
            .iter()
            .filter_map(|slot| slot.entry.as_ref()?.image.as_ref())
    }

    /// Freeze already-observed rendering data without cloning any callbacks.
    ///
    /// Must be called on the scene owner after its updater/capture boundary.
    /// The returned value is thread-transferable even when this stage has
    /// arbitrary non-Send updaters; rendering never executes them again.
    #[must_use]
    pub fn freeze_render_state(&self) -> FrozenRenderState {
        FrozenRenderState {
            stage_id: self.id,
            frame_shape: self.frame_shape,
            next_updater_id: self.next_updater_id,
            slots: self
                .slots
                .iter()
                .map(|slot| FrozenSlot {
                    generation: slot.generation,
                    occupied: slot.entry.is_some(),
                })
                .collect(),
            entries: self
                .slots
                .iter()
                .enumerate()
                .filter_map(|(index, slot)| {
                    Some((
                        index as u32,
                        FrozenRenderEntry::capture(slot.entry.as_ref()?),
                    ))
                })
                .collect(),
            free: self.free.clone(),
            roots: self.roots.clone(),
            time: self.time,
        }
    }

    /// Freeze only the rooted family DAG of this already-observed picture.
    ///
    /// Detached animation endpoints, saved states and removed objects are not
    /// raster inputs and their records/images are not retained. Shared children,
    /// root order, live handles and allocation history are preserved. A writable
    /// view on an omitted object does not cause a copy of its record storage.
    ///
    /// Use this only after separately freezing camera/light state. Native
    /// interpolation, succession preparation and handle-based camera readback
    /// require [`Self::freeze_render_state`]'s complete arena instead.
    #[must_use]
    pub fn freeze_visible_render_state(&self) -> FrozenRenderState {
        let members = VisibleMembers::discover(&self.roots, &[], |mob| {
            self.get(mob).map(|entry| &*entry.submobjects)
        });
        let entries = members
            .order
            .iter()
            .filter_map(|&mob| {
                let mut entry = FrozenRenderEntry::capture(self.get(mob)?);
                members.retain_links(&mut entry);
                Some((mob.parts().0, entry))
            })
            .collect();
        FrozenRenderState {
            stage_id: self.id,
            frame_shape: self.frame_shape,
            next_updater_id: self.next_updater_id,
            slots: self
                .slots
                .iter()
                .map(|slot| FrozenSlot {
                    generation: slot.generation,
                    occupied: slot.entry.is_some(),
                })
                .collect(),
            entries,
            free: self.free.clone(),
            roots: self.roots.clone(),
            time: self.time,
        }
    }
}

impl Snapshot {
    /// All immutable images in this checkpoint, including detached endpoints
    /// and saved states. Provenance/capability checks can observe the complete
    /// scene input before a picture-only projection releases hidden history.
    pub fn image_resources(&self) -> impl Iterator<Item = &ImageResource> {
        self.slots
            .iter()
            .filter_map(|(_, entry)| entry.as_ref()?.image.as_ref())
    }

    /// Project an existing checkpoint without changing its replay callbacks.
    ///
    /// `time` is the capture clock supplied by FramePacket; ordinary Snapshot
    /// does not itself carry scene time. Graph metadata is copied, record/image
    /// storage remains copy-on-write, and no callback reference is copied.
    ///
    /// # Panics
    /// As Stage::set_time_from_clock, a non-finite or negative time is refused.
    #[must_use]
    pub fn render_state(&self, time: f64) -> FrozenRenderState {
        self.render_state_selected(time, None)
    }

    /// Project just the rooted picture without modifying this checkpoint.
    ///
    /// As [`Stage::freeze_visible_render_state`], camera/light handles and
    /// detached interpolation inputs must be resolved before using this path.
    /// Replay retains this snapshot's entire arena and updater registrations.
    ///
    /// # Panics
    /// As [`Self::render_state`].
    #[must_use]
    pub fn visible_render_state(&self, time: f64) -> FrozenRenderState {
        self.visible_render_state_with_auxiliary(time, &[])
    }

    /// Project the rooted picture plus explicitly needed auxiliary families.
    ///
    /// Auxiliary handles and their descendants remain available for frozen
    /// camera/light queries, but do not become scene roots or add draw calls.
    /// Invalid or stale auxiliary handles remain absent; they never resolve to
    /// a later occupant of the same slot. This still cannot replay animations
    /// whose detached inputs were not explicitly retained.
    ///
    /// # Panics
    /// As [`Self::render_state`].
    #[must_use]
    pub fn visible_render_state_with_auxiliary(
        &self,
        time: f64,
        auxiliary: &[Mob],
    ) -> FrozenRenderState {
        let members = self.visible_render_members(auxiliary);
        self.render_state_selected(time, Some(&members))
    }

    fn visible_render_members(&self, auxiliary: &[Mob]) -> VisibleMembers {
        VisibleMembers::discover(&self.roots, auxiliary, |mob| {
            let (index, generation) = mob.parts();
            if mob != Mob::from_parts(self.stage_id, index, generation) {
                return None;
            }
            self.slots
                .get(index as usize)
                .filter(|(stored, _)| *stored == generation)
                .and_then(|(_, entry)| entry.as_ref())
                .map(|entry| &*entry.submobjects)
        })
    }

    fn render_state_selected(
        &self,
        time: f64,
        members: Option<&VisibleMembers>,
    ) -> FrozenRenderState {
        assert!(
            time.is_finite() && time >= 0.0,
            "invalid render capture time"
        );
        FrozenRenderState {
            stage_id: self.stage_id,
            frame_shape: self.frame_shape,
            next_updater_id: self.next_updater_id,
            slots: self
                .slots
                .iter()
                .map(|(generation, entry)| FrozenSlot {
                    generation: *generation,
                    occupied: entry.is_some(),
                })
                .collect(),
            entries: self
                .slots
                .iter()
                .enumerate()
                .filter_map(|(index, (generation, source))| {
                    let source = source.as_ref()?;
                    if let Some(members) = members
                        && !members.seen.contains(&Mob::from_parts(
                            self.stage_id,
                            index as u32,
                            *generation,
                        ))
                    {
                        return None;
                    }
                    let mut entry = FrozenRenderEntry::copy_snapshot(source);
                    if let Some(members) = members {
                        members.retain_links(&mut entry);
                    }
                    Some((index as u32, entry))
                })
                .collect(),
            free: self.free.clone(),
            roots: self.roots.clone(),
            time,
        }
    }

    /// Consume an unshared checkpoint, moving its data into a render handoff.
    ///
    /// Updater references and native updater states are dropped by this call
    /// on the scene owner. They never become part of the Send representation.
    /// `time` must be the checkpoint's capture time, as supplied by FramePacket.
    ///
    /// # Panics
    /// As Stage::set_time_from_clock, a non-finite or negative time is refused.
    #[must_use]
    pub fn into_render_state(self, time: f64) -> FrozenRenderState {
        self.into_render_state_selected(time, None)
    }

    /// Consume a checkpoint into only its rooted, callback-free picture.
    ///
    /// Omitted entries and all updater registrations are released here, on the
    /// scene owner. Visible storage moves into the handoff without first
    /// materializing a full arena. As [`Self::visible_render_state`], this is
    /// unsuitable for detached animation targets or camera handle readback.
    ///
    /// # Panics
    /// As [`Self::into_render_state`].
    #[must_use]
    pub fn into_visible_render_state(self, time: f64) -> FrozenRenderState {
        self.into_visible_render_state_with_auxiliary(time, &[])
    }

    /// Move the rooted picture and explicit auxiliary families into a handoff.
    ///
    /// As [`Self::visible_render_state_with_auxiliary`], auxiliary objects stay
    /// unrooted. Omitted payloads and all callback references are dropped on
    /// this calling scene-owner thread, as in [`Self::into_render_state`].
    ///
    /// # Panics
    /// As [`Self::into_render_state`].
    #[must_use]
    pub fn into_visible_render_state_with_auxiliary(
        self,
        time: f64,
        auxiliary: &[Mob],
    ) -> FrozenRenderState {
        let members = self.visible_render_members(auxiliary);
        self.into_render_state_selected(time, Some(&members))
    }

    fn into_render_state_selected(
        self,
        time: f64,
        members: Option<&VisibleMembers>,
    ) -> FrozenRenderState {
        assert!(
            time.is_finite() && time >= 0.0,
            "invalid render capture time"
        );
        let mut slots = Vec::with_capacity(self.slots.len());
        let entry_count = members.map_or_else(
            || {
                self.slots
                    .iter()
                    .filter(|(_, entry)| entry.is_some())
                    .count()
            },
            |members| members.order.len(),
        );
        let mut entries = Vec::with_capacity(entry_count);
        for (index, (generation, source)) in self.slots.into_iter().enumerate() {
            slots.push(FrozenSlot {
                generation,
                occupied: source.is_some(),
            });
            let Some(source) = source else { continue };
            if let Some(members) = members
                && !members
                    .seen
                    .contains(&Mob::from_parts(self.stage_id, index as u32, generation))
            {
                continue;
            }
            let mut entry = FrozenRenderEntry::take_snapshot(source);
            if let Some(members) = members {
                members.retain_links(&mut entry);
            }
            entries.push((index as u32, entry));
        }
        FrozenRenderState {
            stage_id: self.stage_id,
            frame_shape: self.frame_shape,
            next_updater_id: self.next_updater_id,
            slots,
            entries,
            free: self.free,
            roots: self.roots,
            time,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mobject;

    #[test]
    fn omitting_the_last_live_generation_retires_its_slot() {
        let mut stage = Stage::new();
        let hidden = stage.add(Mobject::new());
        let visible = stage.add(Mobject::new());
        stage.add_to_scene(visible).unwrap();
        let mut checkpoint = stage.snapshot();
        checkpoint.slots[hidden.parts().0 as usize].0 = u32::MAX - 1;
        stage.restore(&checkpoint);
        let hidden = Mob::from_parts(stage.id, hidden.parts().0, u32::MAX - 1);
        assert!(stage.contains(hidden));
        for state in [
            stage.freeze_visible_render_state(),
            stage.snapshot().visible_render_state(0.0),
            stage.snapshot().into_visible_render_state(0.0),
        ] {
            let mut frame = state.materialize();
            assert_eq!(frame.slots[hidden.parts().0 as usize].generation, u32::MAX);
            assert!(!frame.contains(hidden));
            assert!(frame.contains(visible));
            let added = frame.add(Mobject::new());
            assert_ne!(added.parts().0, hidden.parts().0);
            assert!(frame.free.contains(&hidden.parts().0));
            Snapshot::from_bytes(&frame.snapshot_bytes().unwrap(), &frame).unwrap();
        }
        assert!(
            stage.contains(hidden),
            "projection cannot delete authored state"
        );
    }
}
