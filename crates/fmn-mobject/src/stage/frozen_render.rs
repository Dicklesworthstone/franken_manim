//! An in-memory rendering handoff, not an undo/replay checkpoint.
//!
//! Only already-observed scene data crosses this boundary. Updater callables
//! and native updater state are deliberately absent from the representation,
//! so neither an Rc nor a callback destructor can migrate to a render worker.
//! Handles, generations, roots, DAG edges and rendering revisions are retained;
//! unlike durable render projection, this does not renumber the live chart.

use super::{Entry, Mob, Slot, Snapshot, SnapshotEntry, Stage, fresh_epoch};
use crate::bbox::BboxCache;
use crate::mobject::{ImageResource, RenderPrimitive};
use crate::placement::Placement;
use crate::record::RecordBuffer;
use crate::shape::ShapeSlot;
use crate::uniforms::Uniforms;
use std::cell::RefCell;

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
    slots: Vec<(u32, Option<FrozenRenderEntry>)>,
    free: Vec<u32>,
    roots: Vec<Mob>,
    time: f64,
}

struct FrozenRenderEntry {
    buffer: RecordBuffer,
    placement: Placement,
    placement_revision: u64,
    submobjects: Vec<Mob>,
    parents: Vec<Mob>,
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
        }
    }

    fn take_snapshot(entry: SnapshotEntry) -> Self {
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
            updaters: Vec::new(),
            updater_states: Vec::new(),
            children_epoch: fresh_epoch(),
            family_cache: RefCell::new(None),
            bbox: RefCell::new(BboxCache::default()),
        }
    }
}

impl FrozenRenderState {
    /// Consume the handoff into a worker-local arena in the same handle domain.
    ///
    /// No callback runs here, and no updater callable or native updater state
    /// is installed. Record buffers and graph vectors move rather than clone;
    /// writes in either arena still obey the usual copy-on-write protocol.
    #[must_use]
    pub fn materialize(self) -> Stage {
        Stage {
            id: self.stage_id,
            slots: self
                .slots
                .into_iter()
                .map(|(generation, entry)| Slot {
                    generation,
                    entry: entry.map(FrozenRenderEntry::into_entry),
                })
                .collect(),
            free: self.free,
            roots: self.roots,
            time: self.time,
            next_updater_id: self.next_updater_id,
            topology_epoch: fresh_epoch(),
            updater_failure: None,
            updater_callbacks: std::rc::Rc::new(()),
        }
    }
}

impl Stage {
    /// Freeze already-observed rendering data without cloning any callbacks.
    ///
    /// Must be called on the scene owner after its updater/capture boundary.
    /// The returned value is thread-transferable even when this stage has
    /// arbitrary non-Send updaters; rendering never executes them again.
    #[must_use]
    pub fn freeze_render_state(&self) -> FrozenRenderState {
        FrozenRenderState {
            stage_id: self.id,
            next_updater_id: self.next_updater_id,
            slots: self
                .slots
                .iter()
                .map(|slot| {
                    (
                        slot.generation,
                        slot.entry.as_ref().map(FrozenRenderEntry::capture),
                    )
                })
                .collect(),
            free: self.free.clone(),
            roots: self.roots.clone(),
            time: self.time,
        }
    }
}

impl Snapshot {
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
        assert!(
            time.is_finite() && time >= 0.0,
            "invalid render capture time"
        );
        FrozenRenderState {
            stage_id: self.stage_id,
            next_updater_id: self.next_updater_id,
            slots: self
                .slots
                .iter()
                .map(|(generation, entry)| {
                    (
                        *generation,
                        entry.as_ref().map(FrozenRenderEntry::copy_snapshot),
                    )
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
        assert!(
            time.is_finite() && time >= 0.0,
            "invalid render capture time"
        );
        FrozenRenderState {
            stage_id: self.stage_id,
            next_updater_id: self.next_updater_id,
            slots: self
                .slots
                .into_iter()
                .map(|(generation, entry)| {
                    (generation, entry.map(FrozenRenderEntry::take_snapshot))
                })
                .collect(),
            free: self.free,
            roots: self.roots,
            time,
        }
    }
}
