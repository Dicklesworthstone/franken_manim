//! The Stage arena: generational handles, rooted lifetimes, cached family
//! flattening, CoW snapshots — §8.1 exactly as ratified by G0-1 (D-11,
//! `docs/g0/G0-1-object-model-ratification.md`).
//!
//! Lifetime rules (the ratified set):
//! - Entries are **arena-owned**; scene membership (`roots`) is a root set,
//!   not ownership. Removal from the scene never frees anything.
//! - `Mob` handles are `Copy`, generational, and **stage-scoped** (the
//!   two-scene policy): stale and foreign handles resolve to `None` /
//!   [`StageError::StaleHandle`] — a defined state, never a recycled
//!   stranger's data.
//! - Explicit [`Stage::delete`] is the only destructor and **defers while
//!   proxy pins are outstanding** — the fmn-python identity story.
//! - Updater callables are shared **by reference** on copy (§8.3); they
//!   receive `(&mut Stage, Mob, dt)` so closures capture plain handles.
//!   The full updater semantics (dt/non-dt split, suspend/resume,
//!   ValueTrackers, `.animate`) land with fm-yra; the arena owns only the
//!   insertion-ordered slot and the run-once `call_now` rule (the
//!   Reference's double-call is a bug we fix — Behavior Note).
//! - Snapshots are CoW under the view protocol's rule V5: O(touched)
//!   copies, verified by test.

use std::any::Any;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fmn_core::constants::{FRAME_HEIGHT, FRAME_WIDTH};

use crate::StageError;
use crate::bbox::BboxCache;
use crate::mobject::{ImageResource, Mobject, RenderPrimitive};
use crate::persist::{UpdaterIdentity, UpdaterKindTag};
use crate::placement::Placement;
use crate::record::RecordBuffer;
use crate::shape::ShapeSlot;
use crate::uniforms::Uniforms;

mod frozen_render;
pub use frozen_render::FrozenRenderState;

/// Copy-on-write `Arc`-backed vector that allocates nothing when empty and
/// clones in O(1) across [`Snapshot`]s and [`FrozenRenderState`]s.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ArcVec<T>(Option<Arc<Vec<T>>>);

impl<T> Clone for ArcVec<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Default for ArcVec<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> ArcVec<T> {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self(None)
    }

    #[must_use]
    pub(crate) fn storage_id(&self) -> usize {
        self.0.as_ref().map_or(0, |arc| Arc::as_ptr(arc) as usize)
    }
}

impl<T: Clone> ArcVec<T> {
    pub(crate) fn vec_mut(&mut self) -> &mut Vec<T> {
        let arc = self.0.get_or_insert_with(|| Arc::new(Vec::new()));
        Arc::make_mut(arc)
    }

    pub(crate) fn push(&mut self, value: T) {
        self.vec_mut().push(value);
    }

    pub(crate) fn pop(&mut self) -> Option<T> {
        self.0.as_mut().and_then(|arc| Arc::make_mut(arc).pop())
    }

    pub(crate) fn retain(&mut self, f: impl FnMut(&T) -> bool) {
        if let Some(arc) = self.0.as_mut() {
            Arc::make_mut(arc).retain(f);
        }
    }

    pub(crate) fn retain_mut(&mut self, f: impl FnMut(&mut T) -> bool) {
        if let Some(arc) = self.0.as_mut() {
            Arc::make_mut(arc).retain_mut(f);
        }
    }

    pub(crate) fn extend_from_slice(&mut self, other: &[T]) {
        if !other.is_empty() {
            self.vec_mut().extend_from_slice(other);
        }
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "fm-q2l6: only tests call it since the f6b54659 merge"
        )
    )]
    pub(crate) fn extend<I>(&mut self, iter: I)
    where
        I: IntoIterator<Item = T>,
    {
        let mut iter = iter.into_iter().peekable();
        if iter.peek().is_none() {
            return;
        }
        self.vec_mut().extend(iter);
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "fm-q2l6: only tests call it since the f6b54659 merge"
        )
    )]
    pub(crate) fn clear(&mut self) {
        if let Some(arc) = self.0.as_mut() {
            Arc::make_mut(arc).clear();
        }
    }

    pub(crate) fn splice<R, I>(&mut self, range: R, replace_with: I)
    where
        R: std::ops::RangeBounds<usize>,
        I: IntoIterator<Item = T>,
    {
        let _ = self.vec_mut().splice(range, replace_with);
    }

    pub(crate) fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        match self.0.as_mut() {
            Some(arc) => Arc::make_mut(arc).iter_mut(),
            None => [].iter_mut(),
        }
    }
}

impl<T: PartialEq, const N: usize> PartialEq<[T; N]> for ArcVec<T> {
    fn eq(&self, other: &[T; N]) -> bool {
        (**self) == other[..]
    }
}

impl<T: PartialEq> PartialEq<[T]> for ArcVec<T> {
    fn eq(&self, other: &[T]) -> bool {
        &(**self) == other
    }
}

impl<T: PartialEq> PartialEq<&[T]> for ArcVec<T> {
    fn eq(&self, other: &&[T]) -> bool {
        &(**self) == *other
    }
}

impl<T> std::ops::Deref for ArcVec<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.0.as_deref().map_or(&[], Vec::as_slice)
    }
}

impl<T> std::ops::Index<usize> for ArcVec<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        &(**self)[index]
    }
}

impl<T: Clone> std::ops::IndexMut<usize> for ArcVec<T> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.vec_mut()[index]
    }
}

impl<T> From<Vec<T>> for ArcVec<T> {
    fn from(value: Vec<T>) -> Self {
        if value.is_empty() {
            Self(None)
        } else {
            Self(Some(Arc::new(value)))
        }
    }
}

impl<'a, T> IntoIterator for &'a ArcVec<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, T: Clone> IntoIterator for &'a mut ArcVec<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<T: Clone> IntoIterator for ArcVec<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        match self.0 {
            Some(arc) => Arc::unwrap_or_clone(arc).into_iter(),
            None => Vec::new().into_iter(),
        }
    }
}

/// Copy-on-write `Rc`-backed vector for non-`Send` updater slots and states.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RcVec<T>(Option<Rc<Vec<T>>>);

impl<T> Clone for RcVec<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Default for RcVec<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> RcVec<T> {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self(None)
    }

    #[must_use]
    pub(crate) fn storage_id(&self) -> usize {
        self.0.as_ref().map_or(0, |rc| Rc::as_ptr(rc) as usize)
    }
}

impl<T: Clone> RcVec<T> {
    pub(crate) fn vec_mut(&mut self) -> &mut Vec<T> {
        let rc = self.0.get_or_insert_with(|| Rc::new(Vec::new()));
        Rc::make_mut(rc)
    }

    pub(crate) fn push(&mut self, value: T) {
        self.vec_mut().push(value);
    }

    pub(crate) fn insert(&mut self, index: usize, value: T) {
        self.vec_mut().insert(index, value);
    }

    pub(crate) fn retain(&mut self, f: impl FnMut(&T) -> bool) {
        if let Some(rc) = self.0.as_mut() {
            Rc::make_mut(rc).retain(f);
        }
    }

    pub(crate) fn clear(&mut self) {
        if let Some(rc) = self.0.as_mut() {
            Rc::make_mut(rc).clear();
        }
    }

    pub(crate) fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        match self.0.as_mut() {
            Some(rc) => Rc::make_mut(rc).iter_mut(),
            None => [].iter_mut(),
        }
    }
}

impl<T> std::ops::Deref for RcVec<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.0.as_deref().map_or(&[], Vec::as_slice)
    }
}

impl<'a, T> IntoIterator for &'a RcVec<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, T: Clone> IntoIterator for &'a mut RcVec<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<T: Clone> IntoIterator for RcVec<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        match self.0 {
            Some(rc) => Rc::unwrap_or_clone(rc).into_iter(),
            None => Vec::new().into_iter(),
        }
    }
}

static NEXT_STAGE_ID: AtomicU64 = AtomicU64::new(1);
/// Source of [`Stage::topology_epoch`] values, shared by every stage so that
/// equal epochs never name two different topologies, even across a
/// [`Snapshot::materialize`] that reuses a stage id.
static NEXT_TOPOLOGY_EPOCH: AtomicU64 = AtomicU64::new(1);

/// A value no stage topology or entry child list has held before.
fn fresh_epoch() -> u64 {
    NEXT_TOPOLOGY_EPOCH.fetch_add(1, Ordering::Relaxed)
}

/// Fibonacci-mixes integer keys ([`Mob`] handles, object addresses) for
/// per-call identity sets and maps. The keys are identities the engine
/// chose, not caller data, so the keyed SipHash default buys nothing here
/// and cost ~5% of a large scene's main thread (fm-5wq.31).
#[derive(Default)]
pub struct IdHasher(u64);

impl IdHasher {
    fn mix(&mut self, value: u64) {
        self.0 = (self.0.rotate_left(29) ^ value).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

impl std::hash::Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.mix(u64::from(byte));
        }
    }

    fn write_u32(&mut self, value: u32) {
        self.mix(u64::from(value));
    }

    fn write_u64(&mut self, value: u64) {
        self.mix(value);
    }

    fn write_usize(&mut self, value: usize) {
        self.mix(value as u64);
    }
}

/// [`IdHasher`] as a `HashMap`/`HashSet` hasher parameter.
pub type IdBuildHasher = std::hash::BuildHasherDefault<IdHasher>;

/// Generational, stage-scoped, `Copy` mobject handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mob {
    stage_id: u64,
    index: u32,
    generation: u32,
}

impl Mob {
    /// A stable per-handle bit pattern (slot index + generation) used to fold
    /// structural identity into the bounding-box subtree signature. Not an
    /// address and never serialized — an in-memory cache-keying aid only.
    pub(crate) fn bits(self) -> u64 {
        (u64::from(self.index) << 32) | u64::from(self.generation)
    }

    /// The durable identity a serialized snapshot records: (slot index,
    /// generation). The stage id is deliberately NOT part of it — it is a
    /// process-local mint, re-bound at decode (§8.7).
    pub(crate) fn parts(self) -> (u32, u32) {
        (self.index, self.generation)
    }

    /// Rebuild a handle for `stage_id` from its durable parts (the
    /// persistence layer's decode hook).
    pub(crate) fn from_parts(stage_id: u64, index: u32, generation: u32) -> Self {
        Self {
            stage_id,
            index,
            generation,
        }
    }
}

/// A non-dt updater closure: receives the stage and its own handle — the
/// Reference's `lambda m: ...` form, and the overwhelmingly common one.
/// `Rc` because manim's `copy()` keeps updater callables by reference.
pub type NonDtUpdater = Rc<RefCell<dyn FnMut(&mut Stage, Mob)>>;

/// A dt updater closure: additionally receives the frame's `dt` — the
/// Reference's `lambda m, dt: ...` form (detected there by signature
/// inspection; a typed registration here).
pub type DtUpdater = Rc<RefCell<dyn FnMut(&mut Stage, Mob, f64)>>;

/// The two updater kinds behind one insertion-ordered list (§8.6): the
/// Reference keeps a single `self.updaters` list mixing both and passes
/// `dt` only to the dt-kind; execution order is pure insertion order across
/// kinds, and that is exact semantics.
#[derive(Clone)]
pub enum UpdaterFn {
    /// Called as `f(stage, mob)`.
    NonDt(NonDtUpdater),
    /// Called as `f(stage, mob, dt)`.
    Dt(DtUpdater),
}

/// Identity of a registered updater — the removal token (closures have no
/// equality; the Reference removes by function identity, this is the typed
/// equivalent). Copies of a mobject share updaters *and* their ids, exactly
/// as Reference copies share function objects; removal always names the
/// mobject it acts on.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct UpdaterId(u64);

impl UpdaterId {
    /// The raw token — what a durable snapshot records as the updater's
    /// identity (§8.7: callables never serialize; identity + the §13.4
    /// effect model's hashes are the replay vocabulary).
    #[must_use]
    pub fn raw(self) -> u64 {
        self.0
    }

    pub(crate) fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

/// A returned failure from a native updater or redraw factory.
///
/// Ownership failures retain their typed cause. Other builders can attach
/// their own operation and diagnostic with [`Self::callback`], without
/// introducing dependencies from the arena to the mobject library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdaterError {
    /// The arena refused an ownership or family operation.
    Stage(StageError),
    /// A user-supplied operation refused the current update.
    Callback {
        /// The operation that failed, such as a graph or text rebuild.
        operation: &'static str,
        /// The original operation's diagnostic.
        detail: String,
    },
}

impl UpdaterError {
    /// Attach a named operation to a builder's error.
    #[must_use]
    pub fn callback(operation: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Callback {
            operation,
            detail: error.to_string(),
        }
    }
}

impl From<StageError> for UpdaterError {
    fn from(error: StageError) -> Self {
        Self::Stage(error)
    }
}

impl std::fmt::Display for UpdaterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stage(error) => error.fmt(f),
            Self::Callback { operation, detail } => write!(f, "{operation}: {detail}"),
        }
    }
}

impl std::error::Error for UpdaterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Stage(error) => Some(error),
            Self::Callback { .. } => None,
        }
    }
}

/// The first updater failure retained by one physical [`Stage`].
///
/// A copied mobject shares its callable and updater identity, but failures
/// name the actual receiving handle. This diagnostic is execution state;
/// snapshots and durable updater manifests do not carry it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdaterFailure {
    /// The receiving mobject, which may have been deleted by its callback.
    pub mobject: Mob,
    /// The failing callback's registration identity.
    pub updater: UpdaterId,
    /// The first returned error. Later errors never replace it.
    pub error: UpdaterError,
}

impl std::fmt::Display for UpdaterFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "updater {} on {:?} failed: {}",
            self.updater.raw(),
            self.mobject,
            self.error
        )
    }
}

impl std::error::Error for UpdaterFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// One registered updater: identity plus callable.
#[derive(Clone)]
pub struct UpdaterSlot {
    /// The removal token.
    pub id: UpdaterId,
    /// The callable, by kind.
    pub func: UpdaterFn,
}

/// Native mobject attributes used by a shared updater callable. Keeping these
/// apart from the callable is essential: `copy()` shares functions, but a
/// tracer's observations or a boundary's clock belong to the receiving object.
trait NativeUpdaterValue {
    fn clone_value(&self) -> Box<dyn NativeUpdaterValue>;
    fn remap_handles(&mut self, map: &CopyMap);
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl Clone for Box<dyn NativeUpdaterValue> {
    fn clone(&self) -> Self {
        self.clone_value()
    }
}

struct TypedUpdaterValue<T> {
    value: T,
    remap: fn(&mut T, &CopyMap),
}

impl<T: Clone + 'static> NativeUpdaterValue for TypedUpdaterValue<T> {
    fn clone_value(&self) -> Box<dyn NativeUpdaterValue> {
        Box::new(Self {
            value: self.value.clone(),
            remap: self.remap,
        })
    }

    fn remap_handles(&mut self, map: &CopyMap) {
        (self.remap)(&mut self.value, map);
    }

    fn as_any(&self) -> &dyn Any {
        &self.value
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        &mut self.value
    }
}

/// CoW keeps frame capture cheap while freezing native updater attributes.
/// Mutable access detaches before a write; callables remain shared by reference.
#[derive(Clone)]
pub(crate) struct NativeUpdaterState(Rc<Box<dyn NativeUpdaterValue>>);

impl NativeUpdaterState {
    fn new<T: Clone + 'static>(value: T, remap: fn(&mut T, &CopyMap)) -> Self {
        Self(Rc::new(Box::new(TypedUpdaterValue { value, remap })))
    }

    fn remap_handles(&mut self, map: &CopyMap) {
        Rc::make_mut(&mut self.0).remap_handles(map);
    }

    fn get<T: 'static>(&self) -> Option<&T> {
        self.0.as_any().downcast_ref()
    }

    fn get_mut<T: 'static>(&mut self) -> Option<&mut T> {
        Rc::make_mut(&mut self.0).as_any_mut().downcast_mut()
    }
}

/// Arena entry: record data plus graph edges and lifetime state. Edges are
/// private so every structural mutation flows through [`Stage`] and the
/// family cache invalidates correctly.
pub struct Entry {
    /// The per-object record data (fm-cus layers the full RecordBuffer
    /// surface onto this). Its `point` field is object-space geometry;
    /// [`Entry::placement`] carries object→world motion independently.
    pub buffer: RecordBuffer,
    placement: Placement,
    placement_revision: u64,
    submobjects: ArcVec<Mob>,
    /// [`Entry::children_epoch`]: moves whenever `submobjects` changes.
    children_epoch: u64,
    parents: ArcVec<Mob>,
    updaters: RcVec<UpdaterSlot>,
    updater_states: RcVec<(UpdaterId, NativeUpdaterState)>,
    /// `suspend_updating` state: while set, [`Stage::update`] prunes this
    /// entry's whole subtree (the Reference's early return).
    updating_suspended: bool,
    /// `set_animating_status` state (§9.1): set on the whole family *and*
    /// every ancestor while an animation plays; read through
    /// [`Stage::is_changing`] (the Reference's render-cache probe).
    is_animating: bool,
    /// ValueTracker state (§8.6), if this mobject is a tracker; plain
    /// mobjects carry `None`. Copied by value with the entry.
    pub(crate) tracker: Option<crate::dynamics::Tracker>,
    /// The `generate_target` copy (§8.3). Cleared on every copy — the
    /// Reference's `stash_mobject_pointers` list is exactly
    /// `["parents", "target", "saved_state"]` — and re-linked only by the
    /// target machinery.
    target: Option<Mob>,
    /// The `save_state` copy (§8.3) that `restore_mobject` becomes.
    /// Cleared on every copy, like `target`.
    saved_state: Option<Mob>,
    /// The family member this one is an invisible padding copy of, set by
    /// [`Stage::add_n_more_submobjects`] (the Reference's `invisible_copy`).
    /// The binding tier publishes such a member with its source's class and
    /// Python state (fm-b97z). Transient: cleared on every copy and move,
    /// and not persisted.
    padding_source: Option<Mob>,
    pins: usize,
    pending_delete: bool,
    /// Per-object typed uniform state (§8.4): scene code reads and writes this
    /// directly; Lumen's StyleTable synchronizes from it.
    uniforms: Uniforms,
    /// The Reference's `z_index` (§8.5): the scene list's sort key. Plain
    /// per-object data, not a uniform — only a *top-level* member's value
    /// orders anything, but a child may be promoted to a root later, so the
    /// value travels with the object (see [`crate::order`]).
    z_index: i32,
    /// The semantic shape a constructor built, with the point revision at
    /// which its geometry was true (§10.8, [`crate::shape`]).
    shape: ShapeSlot,
    /// Durable renderer program/topology identity.
    render_primitive: RenderPrimitive,
    /// Immutable raster bytes for an image primitive.
    image: Option<ImageResource>,
    /// Monotone replacement counter for the retained image axis.
    image_revision: u64,
    /// Cached family flattening (§1.1 API surface), invalidated on any
    /// structural change in the subtree.
    family_cache: RefCell<Option<Arc<Vec<Mob>>>>,
    /// Lazily recomputed bounding box, keyed by a subtree signature so any
    /// point write or structural change invalidates it automatically, through
    /// any channel (fm-jru).
    bbox: RefCell<BboxCache>,
}

impl Entry {
    fn from_data(buffer: RecordBuffer) -> Self {
        Self {
            buffer,
            placement: Placement::IDENTITY,
            placement_revision: 0,
            submobjects: ArcVec::new(),
            children_epoch: fresh_epoch(),
            parents: ArcVec::new(),
            updaters: RcVec::new(),
            updater_states: RcVec::new(),
            updating_suspended: false,
            is_animating: false,
            tracker: None,
            target: None,
            saved_state: None,
            padding_source: None,
            pins: 0,
            pending_delete: false,
            uniforms: Uniforms::default(),
            z_index: 0,
            shape: ShapeSlot::default(),
            render_primitive: RenderPrimitive::Vector,
            image: None,
            image_revision: 0,
            family_cache: RefCell::new(None),
            bbox: RefCell::new(BboxCache::default()),
        }
    }

    /// Materialize this entry under the §8.3 copy-category rules, retaining
    /// graph edges temporarily so the family installer can remap internal
    /// handles and discard external ones in one second pass.
    fn detached_copy(&self) -> Self {
        let buffer = self.buffer.deep_clone();
        // A copy has the same points, so it has the same semantic shape — but
        // a deep clone starts its own revision counters. Re-stamp a trusted
        // hint against the new buffer; never resurrect one invalidated by a
        // writable point view.
        let hint_was_live = self.shape.point_revision.is_some()
            && self.shape.point_revision == self.buffer.field_revision("point")
            && !self.buffer.writable_view_affects("point");
        let shape = ShapeSlot {
            tag: self.shape.tag,
            point_revision: hint_was_live
                .then(|| buffer.field_revision("point"))
                .flatten(),
        };
        Self {
            buffer,
            placement: self.placement,
            placement_revision: 0,
            submobjects: self.submobjects.clone(),
            children_epoch: fresh_epoch(),
            parents: self.parents.clone(),
            updaters: self.updaters.clone(),
            updater_states: self.updater_states.clone(),
            updating_suspended: self.updating_suspended,
            is_animating: self.is_animating,
            tracker: self.tracker,
            target: None,
            saved_state: None,
            padding_source: None,
            pins: 0,
            pending_delete: false,
            uniforms: self.uniforms,
            z_index: self.z_index,
            shape,
            render_primitive: self.render_primitive,
            image: self.image.clone(),
            image_revision: 0,
            family_cache: RefCell::new(None),
            bbox: RefCell::new(BboxCache::default()),
        }
    }

    /// The per-object uniform inventory (read access — §8.4 API surface).
    #[must_use]
    pub fn uniforms(&self) -> &Uniforms {
        &self.uniforms
    }

    /// Mutable access to the uniform inventory (scene code writes
    /// `mobject.uniforms` directly).
    pub fn uniforms_mut(&mut self) -> &mut Uniforms {
        &mut self.uniforms
    }

    /// This entry's object→world affine placement (§10.8).
    #[must_use]
    pub fn placement(&self) -> Placement {
        self.placement
    }

    /// Monotonic revision of [`Entry::placement`].
    #[must_use]
    pub fn placement_revision(&self) -> u64 {
        self.placement_revision
    }

    pub(crate) fn set_placement(&mut self, placement: Placement) -> bool {
        if self.placement.same_bits(placement) {
            return false;
        }
        self.placement = placement;
        self.placement_revision = self.placement_revision.wrapping_add(1);
        true
    }

    pub(crate) fn shape(&self) -> ShapeSlot {
        self.shape
    }

    pub(crate) fn set_shape(&mut self, slot: ShapeSlot) {
        self.shape = slot;
    }

    /// Durable renderer identity/topology metadata.
    #[must_use]
    pub const fn render_primitive(&self) -> RenderPrimitive {
        self.render_primitive
    }

    /// Whether a program can paint this entry's records. A vector record
    /// needs the vector paint fields. The Reference's base `Mobject`, and
    /// so `Group`, `Point` and a bare `PMobject`, has an empty
    /// `shader_folder` (`mobject.py:69`), so its wrapper compiles no
    /// program (`shader_wrapper.py:83-88`) and its points are data that
    /// never draw. Its dtype is [`RecordSchema::mobject`]: point and rgba,
    /// no stroke or fill.
    ///
    /// [`RecordSchema::mobject`]: crate::record::RecordSchema::mobject
    #[must_use]
    pub fn paints(&self) -> bool {
        self.render_primitive != RenderPrimitive::Vector
            || ["stroke_rgba", "fill_rgba"]
                .iter()
                .any(|field| self.buffer.schema().field_width(field).is_some())
    }

    /// Immutable image bytes, when this is an image primitive.
    #[must_use]
    pub fn image_resource(&self) -> Option<&ImageResource> {
        self.image.as_ref()
    }

    /// Monotone revision of the immutable image resource.
    #[must_use]
    pub const fn image_revision(&self) -> u64 {
        self.image_revision
    }

    fn replace_image_resource(&mut self, image: Option<ImageResource>) -> bool {
        if self.image == image {
            // Equal rendered pixels still admit the caller's immutable
            // allocation: host input receipts may follow its lifetime. Keep
            // cache/revision identity stable while preserving that ownership.
            self.image = image;
            return false;
        }
        self.image = image;
        self.image_revision = self.image_revision.wrapping_add(1);
        true
    }

    pub(crate) fn bbox_cell(&self) -> &RefCell<BboxCache> {
        &self.bbox
    }

    /// Direct children, in insertion order.
    #[must_use]
    pub fn submobjects(&self) -> &[Mob] {
        &self.submobjects
    }

    /// Changes whenever [`Entry::submobjects`] changes, and is new for every
    /// constructed, copied, moved or restored entry: an unchanged value means
    /// an unchanged child list (fm-5wq.31, the binding's incremental scene
    /// projection).
    #[must_use]
    pub fn children_epoch(&self) -> u64 {
        self.children_epoch
    }

    /// Parents (a submobject may have several — the family is a DAG).
    #[must_use]
    pub fn parents(&self) -> &[Mob] {
        &self.parents
    }

    /// Outstanding proxy pins.
    #[must_use]
    pub fn pins(&self) -> usize {
        self.pins
    }

    /// Identity of the CoW metadata storage backing `(submobjects, parents, updaters, updater_states)`.
    /// Equal values across a [`Snapshot`] prove that untouched entries share metadata storage.
    #[must_use]
    pub fn metadata_storage_id(&self) -> (usize, usize, usize, usize) {
        (
            self.submobjects.storage_id(),
            self.parents.storage_id(),
            self.updaters.storage_id(),
            self.updater_states.storage_id(),
        )
    }
}

struct Slot {
    generation: u32,
    entry: Option<Entry>,
}

/// A CoW snapshot of the whole stage: begin-states for §9.5's frame-parallel
/// pure segments, Studio undo, and replay barriers.
pub struct Snapshot {
    pub(crate) stage_id: u64,
    pub(crate) next_updater_id: u64,
    // The frame of the stage it was taken from: only `materialize` reads it;
    // `restore` keeps the restoring stage's frame.
    pub(crate) frame_shape: [f64; 2],
    pub(crate) slots: Vec<(u32, Option<SnapshotEntry>)>,
    pub(crate) free: ArcVec<u32>,
    pub(crate) roots: ArcVec<Mob>,
}

impl Snapshot {
    /// Materialize an independent arena in the same logical handle domain.
    ///
    /// A [`Snapshot`] is the immutable serial-front-end boundary carried by a
    /// `FramePacket`. Lumen/Reel adapters and pure-frame workers need to read
    /// it without mutating the live scene, so they restore into a physical
    /// arena that intentionally shares the snapshot's stage id. Handles remain
    /// meaningful because the two arenas are copies of one logical Stage, not
    /// unrelated scenes; writes remain isolated by the record buffers' CoW
    /// discipline.
    #[must_use]
    pub fn materialize(&self) -> Stage {
        let mut stage = Stage {
            id: self.stage_id,
            slots: Vec::new(),
            free: ArcVec::new(),
            roots: ArcVec::new(),
            time: 0.0,
            next_updater_id: self.next_updater_id,
            topology_epoch: fresh_epoch(),
            updater_failure: None,
            updater_callbacks: Rc::new(()),
            frame_shape: self.frame_shape,
        };
        stage.restore(self);
        stage
    }
}

pub(crate) struct SnapshotEntry {
    pub(crate) buffer: RecordBuffer,
    pub(crate) placement: Placement,
    pub(crate) placement_revision: u64,
    pub(crate) submobjects: ArcVec<Mob>,
    pub(crate) parents: ArcVec<Mob>,
    pub(crate) updaters: RcVec<UpdaterSlot>,
    pub(crate) updater_states: RcVec<(UpdaterId, NativeUpdaterState)>,
    pub(crate) updating_suspended: bool,
    pub(crate) is_animating: bool,
    pub(crate) tracker: Option<crate::dynamics::Tracker>,
    pub(crate) target: Option<Mob>,
    pub(crate) saved_state: Option<Mob>,
    pub(crate) padding_source: Option<Mob>,
    pub(crate) pins: usize,
    pub(crate) pending_delete: bool,
    pub(crate) uniforms: Uniforms,
    pub(crate) z_index: i32,
    pub(crate) shape: ShapeSlot,
    pub(crate) render_primitive: RenderPrimitive,
    pub(crate) image: Option<ImageResource>,
    pub(crate) image_revision: u64,
    pub(crate) family_cache: Option<Arc<Vec<Mob>>>,
}

/// The stable old-handle → new-handle map a family copy produces — the
/// engine-side hook fmn-python's `__dict__` remapping walks (§8.3). Pairs
/// are in family (depth-first) order and the copy preserves that order, so
/// `family(copy)[i]` is always the copy of `family(original)[i]` — exactly
/// the `family.index(value)` remap rule the Reference's `copy()` applies to
/// attribute aliases.
#[derive(Debug, Clone)]
pub struct CopyMap {
    pairs: Vec<(Mob, Mob)>,
}

impl CopyMap {
    /// The copy of the family root (the handle `copy_family` returns).
    #[must_use]
    pub fn root(&self) -> Mob {
        self.pairs[0].1
    }

    /// All `(original, copy)` pairs, in family (depth-first) order.
    #[must_use]
    pub fn pairs(&self) -> &[(Mob, Mob)] {
        &self.pairs
    }

    /// The copy of `original`, if `original` was in the copied family.
    #[must_use]
    pub fn get(&self, original: Mob) -> Option<Mob> {
        self.pairs
            .iter()
            .find(|(old, _)| *old == original)
            .map(|(_, new)| *new)
    }

    /// Number of copied family members.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }
}

/// The arena.
pub struct Stage {
    id: u64,
    slots: Vec<Slot>,
    free: ArcVec<u32>,
    roots: ArcVec<Mob>,
    time: f64,
    next_updater_id: u64,
    topology_epoch: u64,
    // Physical execution state, deliberately outside Snapshot/SnapshotEntry.
    // A stack clone of this token prevents acknowledgment during callbacks;
    // automatic Rc destruction also restores the guard when a callback panics.
    updater_failure: Option<UpdaterFailure>,
    updater_callbacks: Rc<()>,
    // The frame `to_edge`, `to_corner` and `shift_onto_screen` align against
    // (the Reference's FRAME_SHAPE, derived from the configured resolution).
    // Configuration, not scene content: snapshots neither save nor restore it.
    frame_shape: [f64; 2],
}

impl Default for Stage {
    fn default() -> Self {
        Self::new()
    }
}

impl Stage {
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: NEXT_STAGE_ID.fetch_add(1, Ordering::Relaxed),
            slots: Vec::new(),
            free: ArcVec::new(),
            roots: ArcVec::new(),
            time: 0.0,
            next_updater_id: 1,
            topology_epoch: fresh_epoch(),
            updater_failure: None,
            updater_callbacks: Rc::new(()),
            frame_shape: [FRAME_WIDTH, FRAME_HEIGHT],
        }
    }

    /// The frame `(width, height)` the border-alignment surface is defined
    /// against: the Reference's `FRAME_SHAPE`. It starts at the 16:9 default
    /// frame; the scene runtime sets the configured one (fm-8azr, BN-25).
    #[must_use]
    pub fn frame_shape(&self) -> [f64; 2] {
        self.frame_shape
    }

    /// Set the frame `(width, height)` that [`Stage::frame_shape`] reports.
    ///
    /// # Errors
    /// [`StageError::InvalidFrameShape`] unless both sides are finite and
    /// positive; the frame is left unchanged.
    pub fn set_frame_shape(&mut self, shape: [f64; 2]) -> Result<(), StageError> {
        if !shape.iter().all(|side| side.is_finite() && *side > 0.0) {
            return Err(StageError::InvalidFrameShape);
        }
        self.frame_shape = shape;
        Ok(())
    }

    /// Changes whenever the draw list or any parent/child edge changes:
    /// equal values mean every root's family has the same members in the
    /// same order. Detached families (copies not yet attached) do not move
    /// it; their attach or add does.
    #[must_use]
    pub fn topology_epoch(&self) -> u64 {
        self.topology_epoch
    }

    fn touch_topology(&mut self) {
        self.topology_epoch = fresh_epoch();
    }

    /// Scene time advanced by [`Stage::update`]. (The RationalFrameClock
    /// replaces this float accumulator at the Choreo boundary — fm-wuq;
    /// nothing here depends on its precision.)
    #[must_use]
    pub fn time(&self) -> f64 {
        self.time
    }

    /// Synchronize the stage's scene-time mirror to Choreo's rational clock.
    ///
    /// [`Stage::update`] advances this value before running scene updaters so
    /// they observe the new time. Choreo remains the source of truth, however:
    /// a segment boundary and a restored `SceneState` use this method to replace
    /// any accumulated floating-point roundoff with the clock's single
    /// `frames / fps` conversion. The caller must provide a finite,
    /// non-negative clock value.
    pub fn set_time_from_clock(&mut self, time: f64) {
        assert!(
            time.is_finite() && time >= 0.0,
            "the Stage clock mirror requires finite, non-negative time"
        );
        self.time = time;
    }

    /// Advance the scene-time mirror without running updaters. The
    /// Reference's `Scene.increment_time` is this bump; `Stage::update`
    /// still does bump-then-updaters for the combined frame step.
    pub fn increment_time(&mut self, dt: f64) {
        self.time += dt;
    }

    /// The process-local stage mint the persistence layer re-binds decoded
    /// handles to (§8.7). Never serialized.
    pub(crate) fn stage_id(&self) -> u64 {
        self.id
    }

    pub(crate) fn mob_at_slot(&self, index: u32) -> Option<Mob> {
        let slot = self.slots.get(index as usize)?;
        slot.entry.as_ref()?;
        Some(Mob {
            stage_id: self.id,
            index,
            generation: slot.generation,
        })
    }

    /// Whether an in-memory snapshot belongs to this logical arena.
    ///
    /// Durable decode explicitly rebinds handles to a target Stage; raw CoW
    /// snapshots do not. Higher-level `Result` APIs use this probe to return a
    /// typed refusal before [`Stage::restore`] enforces the invariant.
    #[must_use]
    pub fn can_restore(&self, snapshot: &Snapshot) -> bool {
        self.id == snapshot.stage_id
    }

    // ------------------------------------------------------------ handles

    fn alloc(&mut self, entry: Entry) -> Mob {
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.entry = Some(entry);
            Mob {
                stage_id: self.id,
                index,
                generation: slot.generation,
            }
        } else {
            let index = u32::try_from(self.slots.len()).expect("arena slot count exceeds u32");
            self.slots.push(Slot {
                generation: 0,
                entry: Some(entry),
            });
            Mob {
                stage_id: self.id,
                index,
                generation: 0,
            }
        }
    }

    /// Resolve a handle. `None` for stale, deleted, or foreign handles —
    /// a defined state, never UB, never a recycled slot's data.
    #[must_use]
    pub fn get(&self, mob: Mob) -> Option<&Entry> {
        if mob.stage_id != self.id {
            return None; // two-scene policy: foreign handles never resolve
        }
        let slot = self.slots.get(mob.index as usize)?;
        if slot.generation != mob.generation {
            return None;
        }
        slot.entry.as_ref()
    }

    #[must_use]
    pub fn get_mut(&mut self, mob: Mob) -> Option<&mut Entry> {
        if mob.stage_id != self.id {
            return None;
        }
        let slot = self.slots.get_mut(mob.index as usize)?;
        if slot.generation != mob.generation {
            return None;
        }
        slot.entry.as_mut()
    }

    /// [`Stage::get`] with a typed error for contexts that must report.
    pub fn try_get(&self, mob: Mob) -> Result<&Entry, StageError> {
        self.get(mob).ok_or(StageError::StaleHandle)
    }

    #[must_use]
    pub fn contains(&self, mob: Mob) -> bool {
        self.get(mob).is_some()
    }

    // ---------------------------------------------------- add and compose

    /// Move a detached mobject (and its detached children, recursively)
    /// into the arena. Builders enter through `Into<Mobject>` (§15.1).
    pub fn add(&mut self, mobject: impl Into<Mobject>) -> Mob {
        let Mobject {
            buffer,
            uniforms,
            z_index,
            shape,
            render_primitive,
            image,
            submobjects,
            defaults_row,
        } = mobject.into();
        // The portal consumes a point-free node's defaults row when it
        // wraps the builder output; the arena's own resize path re-derives
        // rows from live records, so the carried row ends here.
        drop(defaults_row);
        let mut entry = Entry::from_data(buffer);
        entry.uniforms = uniforms;
        entry.z_index = z_index;
        // The tag describes the points that just arrived, so it is stamped
        // against this buffer's current revision (crate::shape).
        entry.shape = ShapeSlot {
            tag: shape,
            point_revision: entry.buffer.field_revision("point"),
        };
        entry.render_primitive = render_primitive;
        entry.image_revision = u64::from(image.is_some());
        entry.image = image;
        let mob = self.alloc(entry);
        for child in submobjects {
            let child_mob = self.add(child);
            self.attach(mob, child_mob)
                .expect("freshly allocated handles are live and acyclic");
        }
        mob
    }

    /// Add one mobject to the scene's draw list.
    ///
    /// This is the singleton form of [`Stage::add_many_to_scene`].
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] for a dead handle.
    pub fn add_to_scene(&mut self, mob: Mob) -> Result<(), StageError> {
        self.add_many_to_scene(&[mob])
    }

    /// Add a batch to the scene's draw list — the Reference's `Scene.add`
    /// (`scene.py:327`) exactly: remove the union of all new families from
    /// the *existing* list, append the requested roots together, then
    /// **stable-sort by `(z_index, position)`**, so equal z_index keeps
    /// insertion order (§8.5). Rooting is membership, not ownership.
    ///
    /// The batch boundary is semantic. Two new roots may share a child and
    /// both remain placements; adding the second root in a later call removes
    /// that child from the first placement before appending it.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] if any requested root is dead.
    pub fn add_many_to_scene(&mut self, mobs: &[Mob]) -> Result<(), StageError> {
        if mobs.iter().any(|&mob| !self.contains(mob)) {
            return Err(StageError::StaleHandle);
        }
        self.remove_many_from_scene(mobs);
        self.roots.extend_from_slice(mobs);
        self.sort_scene_by_z_index();
        self.touch_topology();
        Ok(())
    }

    /// The Reference's z-reorder, run on every `add` and nowhere else
    /// (`scene.py:337`): a stable sort by `z_index` over the *current*
    /// positions.
    ///
    /// That "and nowhere else" is semantics, not an oversight to tidy:
    /// [`Stage::bring_to_back`] deliberately puts a member behind
    /// everything **regardless** of its z_index, and re-sorting there would
    /// make the call a silent no-op for any member whose z_index is high.
    /// The list is therefore z-ordered *as of the last add*, and the next
    /// add re-normalizes it. `docs/RENDER_ORDER.md` states the rule; the
    /// ordering-trace corpus pins it.
    fn sort_scene_by_z_index(&mut self) {
        let keys: Vec<i32> = self.roots.iter().map(|&m| self.z_index(m)).collect();
        let mut order: Vec<usize> = (0..self.roots.len()).collect();
        // Stable by construction: ties keep their position index.
        order.sort_by_key(|&i| (keys[i], i));
        self.roots = order
            .into_iter()
            .map(|i| self.roots[i])
            .collect::<Vec<_>>()
            .into();
    }

    /// Remove from the draw list — the Reference's `Scene.remove`
    /// (`scene.py:371` over `extract_mobject_family_members` and
    /// `recursive_mobject_remove`, `utils/family_ops.py:11,23`): the removal
    /// set is the complete family of `mob`. Removing a member that lives
    /// *inside* a rooted family replaces its ancestor in the list with that
    /// ancestor's **other** children, spliced in place. Removing one element
    /// of a rooted group therefore leaves the rest on stage, ungrouped, in
    /// order — which is what makes a remover animation on a submobject
    /// behave.
    ///
    /// The entry and every handle stay valid (rooted-lifetime rule); this
    /// is a draw-list edit, never a deletion. The scene is *not* re-sorted
    /// (see [`Stage::add_to_scene`]).
    pub fn remove_from_scene(&mut self, mob: Mob) {
        self.remove_many_from_scene(&[mob]);
    }

    /// Remove the union of several complete families in one recursive pass,
    /// matching one `Scene.remove(*mobs)` call.
    pub fn remove_many_from_scene(&mut self, mobs: &[Mob]) {
        let targets: HashSet<Mob, IdBuildHasher> =
            mobs.iter().flat_map(|&mob| self.family(mob)).collect();
        // Only a target's ancestors can change. `recursive_remove` finds a
        // target below a member exactly when the member is one, since
        // `parents` mirrors `submobjects`; every other member keeps itself,
        // so it is not descended into. Every `add` removes first, so this
        // walk used to visit the whole scene on each add (fm-5wq.31).
        let mut ancestors: HashSet<Mob, IdBuildHasher> = HashSet::default();
        let mut stack: Vec<Mob> = targets.iter().copied().collect();
        while let Some(current) = stack.pop() {
            if let Some(entry) = self.get(current) {
                for &parent in &entry.parents {
                    if ancestors.insert(parent) {
                        stack.push(parent);
                    }
                }
            }
        }
        let roots = std::mem::take(&mut self.roots);
        let (kept, _) = self.recursive_remove(&roots, &targets, &ancestors);
        self.roots = kept.into();
        self.touch_topology();
    }

    /// `recursive_mobject_remove`, structure for structure: returns the
    /// surviving list and whether anything was removed from it. A member
    /// outside `ancestors` has no target below it and is kept unvisited.
    fn recursive_remove(
        &self,
        members: &[Mob],
        targets: &HashSet<Mob, IdBuildHasher>,
        ancestors: &HashSet<Mob, IdBuildHasher>,
    ) -> (Vec<Mob>, bool) {
        let mut kept = Vec::with_capacity(members.len());
        let mut found = false;
        for &member in members {
            if targets.contains(&member) {
                found = true;
                continue;
            }
            if !ancestors.contains(&member) {
                kept.push(member);
                continue;
            }
            let children = self
                .get(member)
                .map(|entry| entry.submobjects.clone())
                .unwrap_or_default();
            let (sub, found_below) = self.recursive_remove(&children, targets, ancestors);
            if found_below {
                kept.extend(sub);
                found = true;
            } else {
                kept.push(member);
            }
        }
        (kept, found)
    }

    /// The Reference's `bring_to_front` (`scene.py:389`): re-adding *is*
    /// promotion — with the z-sort that `add` always runs.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] for a dead handle.
    pub fn bring_to_front(&mut self, mob: Mob) -> Result<(), StageError> {
        self.add_to_scene(mob)
    }

    /// The Reference's `bring_to_back` (`scene.py:394`): remove, then
    /// prepend — **without** a z-sort, so the demotion holds until the next
    /// [`Stage::add_to_scene`] call.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] for a dead handle.
    pub fn bring_to_back(&mut self, mob: Mob) -> Result<(), StageError> {
        self.bring_many_to_back(&[mob])
    }

    /// Batch form of the Reference's `bring_to_back`: remove all requested
    /// families together, then prepend the requested roots in argument order,
    /// without sorting.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] if any requested root is dead.
    pub fn bring_many_to_back(&mut self, mobs: &[Mob]) -> Result<(), StageError> {
        if mobs.iter().any(|&mob| !self.contains(mob)) {
            return Err(StageError::StaleHandle);
        }
        self.remove_many_from_scene(mobs);
        self.roots.splice(0..0, mobs.iter().copied());
        self.touch_topology();
        Ok(())
    }

    /// The Reference's `Scene.replace` (`scene.py:360`): splice
    /// `replacements` into `mob`'s position in the draw list, in order. A
    /// member that is not on stage is left alone (the Reference's `if
    /// mobject in self.mobjects`). No z-sort, like `bring_to_back`.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] if any handle is dead.
    pub fn replace_in_scene(&mut self, mob: Mob, replacements: &[Mob]) -> Result<(), StageError> {
        for &m in replacements {
            if !self.contains(m) {
                return Err(StageError::StaleHandle);
            }
        }
        if let Some(at) = self.roots.iter().position(|&m| m == mob) {
            self.roots.splice(at..=at, replacements.iter().copied());
            self.touch_topology();
        }
        Ok(())
    }

    /// The scene-list sort key (§8.5). Zero unless set.
    #[must_use]
    pub fn z_index(&self, mob: Mob) -> i32 {
        self.get(mob).map_or(0, |entry| entry.z_index)
    }

    /// The Reference's `set_z_index` (`mobject.py:1238`): write the value,
    /// recursing over the family by default.
    ///
    /// Only a *top-level* member's value orders anything — the scene sort
    /// reads the draw list, not families — but the write recurses because a
    /// child may be promoted to a root later, and because the Reference
    /// does. Setting it does **not** re-sort a scene that already contains
    /// the mobject; `add_to_scene`/`bring_to_front` is what re-normalizes.
    pub fn set_z_index(&mut self, mob: Mob, z_index: i32, recurse: bool) {
        let targets = if recurse {
            self.family(mob)
        } else if self.contains(mob) {
            vec![mob]
        } else {
            Vec::new()
        };
        for target in targets {
            if let Some(entry) = self.get_mut(target) {
                entry.z_index = z_index;
            }
        }
    }

    /// Replace one immutable image resource and advance only its revision axis.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] for a dead or foreign handle.
    pub fn set_image_resource(
        &mut self,
        mob: Mob,
        image: Option<ImageResource>,
    ) -> Result<bool, StageError> {
        Ok(self
            .get_mut(mob)
            .ok_or(StageError::StaleHandle)?
            .replace_image_resource(image))
    }

    /// The scene draw list, in insertion order.
    #[must_use]
    pub fn roots(&self) -> &[Mob] {
        &self.roots
    }

    /// Attach `child` under `parent`. A child may have any number of
    /// parents; the edge is deduplicated. Attaching an ancestor under its
    /// own descendant is a defined error (the Reference would recurse
    /// forever — Behavior-Noted with the family work).
    pub fn attach(&mut self, parent: Mob, child: Mob) -> Result<(), StageError> {
        if !self.contains(parent) || !self.contains(child) {
            return Err(StageError::StaleHandle);
        }
        if parent == child || self.with_family_slice(child, |f| f.contains(&parent)) {
            return Err(StageError::CycleDetected);
        }
        {
            let entry = self.get_mut(parent).expect("checked above");
            if entry.submobjects.contains(&child) {
                return Ok(());
            }
            entry.submobjects.push(child);
            entry.children_epoch = fresh_epoch();
        }
        {
            let entry = self.get_mut(child).expect("checked above");
            if !entry.parents.contains(&parent) {
                entry.parents.push(parent);
            }
        }
        self.invalidate_family_caches(parent);
        self.touch_topology();
        Ok(())
    }

    /// Replace `parent`'s children with `children`, in order: the graph that
    /// detaching every current child and then attaching each new one leaves,
    /// including a kept child's `parent` edge moving to the end of its
    /// `parents` (where a re-attach puts it) and a repeated child counting
    /// once. One pass, instead of one scan of the child list per edge: a
    /// 2,900-child subset reveal made each frame quadratic (fm-5wq.31).
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] or [`StageError::CycleDetected`] for the
    /// first child [`Stage::attach`] would refuse, with the graph unchanged.
    pub fn replace_children(&mut self, parent: Mob, children: &[Mob]) -> Result<(), StageError> {
        if !self.contains(parent) {
            return Err(StageError::StaleHandle);
        }
        let mut unique = Vec::with_capacity(children.len());
        let mut seen: HashSet<Mob, IdBuildHasher> =
            HashSet::with_capacity_and_hasher(children.len(), IdBuildHasher::default());
        for &child in children {
            if !self.contains(child) {
                return Err(StageError::StaleHandle);
            }
            // Removing the parent's own edges cannot open or close a path
            // from `child` back to it, so this is attach's check as is.
            if parent == child || self.with_family_slice(child, |f| f.contains(&parent)) {
                return Err(StageError::CycleDetected);
            }
            if seen.insert(child) {
                unique.push(child);
            }
        }
        let old = {
            let entry = self.get_mut(parent).expect("checked above");
            entry.children_epoch = fresh_epoch();
            std::mem::replace(&mut entry.submobjects, unique.clone().into())
        };
        for &child in &old {
            if let Some(entry) = self.get_mut(child) {
                entry.parents.retain(|m| *m != parent);
            }
        }
        for child in unique {
            let entry = self.get_mut(child).expect("checked above");
            if !entry.parents.contains(&parent) {
                entry.parents.push(parent);
            }
        }
        self.invalidate_family_caches(parent);
        self.touch_topology();
        Ok(())
    }

    /// Detach `child` from `parent`; the entry stays alive.
    pub fn detach(&mut self, parent: Mob, child: Mob) {
        if let Some(entry) = self.get_mut(parent) {
            entry.submobjects.retain(|m| *m != child);
            entry.children_epoch = fresh_epoch();
        }
        if let Some(entry) = self.get_mut(child) {
            entry.parents.retain(|m| *m != parent);
        }
        self.invalidate_family_caches(parent);
        self.touch_topology();
    }

    /// Visit the deduplicated depth-first family slice under `mob` without
    /// cloning a `Vec<Mob>` on every probe.
    pub(crate) fn with_family_slice<R>(&self, mob: Mob, f: impl FnOnce(&[Mob]) -> R) -> R {
        let Some(entry) = self.get(mob) else {
            return f(&[]);
        };
        if entry.submobjects.is_empty() {
            return f(std::slice::from_ref(&mob));
        }
        if let Some(cached) = entry.family_cache.borrow().as_ref() {
            return f(cached.as_slice());
        }
        let arc = Arc::new(self.compute_family_vec(mob));
        *entry.family_cache.borrow_mut() = Some(Arc::clone(&arc));
        f(arc.as_slice())
    }

    /// Return the cached `Arc<Vec<Mob>>` for a non-leaf family without cloning
    /// the underlying `Vec<Mob>`.
    pub(crate) fn family_arc(&self, mob: Mob) -> Option<Arc<Vec<Mob>>> {
        let entry = self.get(mob)?;
        if let Some(cached) = entry.family_cache.borrow().as_ref() {
            return Some(Arc::clone(cached));
        }
        let arc = Arc::new(self.compute_family_vec(mob));
        *entry.family_cache.borrow_mut() = Some(Arc::clone(&arc));
        Some(arc)
    }

    fn compute_family_vec(&self, mob: Mob) -> Vec<Mob> {
        let Some(entry) = self.get(mob) else {
            return Vec::new();
        };
        if entry.submobjects.is_empty() {
            return vec![mob];
        }
        // Membership through a set: a linear `out.contains` made each cache
        // miss quadratic, ~60 ms for a 19K-member family (fm-5wq.31).
        let mut out = Vec::new();
        let mut seen: HashSet<Mob, IdBuildHasher> = HashSet::default();
        let mut stack = vec![mob];
        while let Some(current) = stack.pop() {
            if !seen.insert(current) {
                continue;
            }
            if let Some(e) = self.get(current) {
                out.push(current);
                for &child in e.submobjects.iter().rev() {
                    stack.push(child);
                }
            }
        }
        out
    }

    /// The family under `mob` in depth-first order, each member exactly
    /// once even through diamond composition. Cached per entry; any
    /// structural change in the subtree invalidates every ancestor's cache.
    #[must_use]
    pub fn family(&self, mob: Mob) -> Vec<Mob> {
        self.with_family_slice(mob, <[Mob]>::to_vec)
    }

    /// Clear the family cache of `start` and every (transitive) ancestor.
    fn invalidate_family_caches(&self, start: Mob) {
        let mut stack = vec![start];
        let mut visited: Vec<Mob> = Vec::new();
        while let Some(current) = stack.pop() {
            if visited.contains(&current) {
                continue;
            }
            visited.push(current);
            if let Some(entry) = self.get(current) {
                *entry.family_cache.borrow_mut() = None;
                stack.extend(entry.parents.iter().copied());
            }
        }
    }

    // ----------------------------------------------------------- lifetime

    /// Pin an entry (the Python proxy holds one pin for its lifetime).
    ///
    /// Refuses exhaustion before mutation so a live proxy count can never
    /// wrap to zero and invalidate deferred-deletion safety.
    pub fn pin(&mut self, mob: Mob) -> Result<(), StageError> {
        let entry = self.get_mut(mob).ok_or(StageError::StaleHandle)?;
        entry.pins = entry
            .pins
            .checked_add(1)
            .ok_or(StageError::PinCountExhausted)?;
        Ok(())
    }

    /// Release a pin; a deferred delete completes at the last unpin.
    pub fn unpin(&mut self, mob: Mob) {
        let finalize = match self.get_mut(mob) {
            Some(entry) => {
                entry.pins = entry.pins.saturating_sub(1);
                entry.pins == 0 && entry.pending_delete
            }
            None => false,
        };
        if finalize {
            self.finalize_delete(mob);
        }
    }

    /// Explicit destruction — the only way an entry dies. Defers while
    /// pins are outstanding (the handle stays live for the proxies) and
    /// completes on the last unpin.
    pub fn delete(&mut self, mob: Mob) -> Result<(), StageError> {
        let entry = self.get_mut(mob).ok_or(StageError::StaleHandle)?;
        if entry.pins > 0 {
            entry.pending_delete = true;
        } else {
            self.finalize_delete(mob);
        }
        Ok(())
    }

    fn finalize_delete(&mut self, mob: Mob) {
        let Some(entry) = self.get(mob) else {
            return;
        };
        let parents = entry.parents.clone();
        let children = entry.submobjects.clone();
        // Ancestors' cached families mention this entry.
        self.invalidate_family_caches(mob);
        for &parent in &parents {
            if let Some(p) = self.get_mut(parent) {
                p.submobjects.retain(|m| *m != mob);
                p.children_epoch = fresh_epoch();
            }
        }
        for &child in &children {
            if let Some(c) = self.get_mut(child) {
                c.parents.retain(|m| *m != mob);
            }
        }
        self.remove_from_scene(mob);
        let slot = &mut self.slots[mob.index as usize];
        slot.entry = None;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(mob.index);
        self.touch_topology();
    }

    // --------------------------------------------------------------- copy
    //
    // manim `copy()` (§8.3) — the category rules, to the letter
    // (`mobject.py::copy` + the `stash_mobject_pointers` list, which is
    // exactly `["parents", "target", "saved_state"]`):
    //
    // | Reference attribute          | Rule              | Here                          |
    // |------------------------------|-------------------|-------------------------------|
    // | `data` (ndarray records)     | deep copy         | `RecordBuffer::deep_clone`    |
    // | `uniforms`                   | deep copy         | `Uniforms` copied by value    |
    // | `submobjects` (family)       | recursive copy,   | whole family copied; edges    |
    // |                              | edges remapped    | remapped through the map      |
    // | family-internal attr aliases | remap by family   | [`CopyMap`] preserves family  |
    // | (`self.arrow is family[i]`)  | index             | order; the binding tier walks |
    // |                              |                   | it for `__dict__` remapping   |
    // | updater callables            | by reference      | `Rc` clone, shared `UpdaterId`|
    // | `parents` (external edges)   | cleared           | dropped — a detached family   |
    // | scene membership             | cleared           | the copy is never rooted      |
    // | `target` / `saved_state`     | cleared           | `None` on every copied member |
    // | render caches / live views   | reset             | fresh storage, zero views     |
    //
    // The Reference's `deep` flag (`deepcopy`) differs only for arbitrary
    // Python `__dict__` attributes — a binding-tier (fmn-python)
    // distinction; the engine has exactly one copy operation (Python
    // `deepcopy` of a function object returns the same object, so even
    // `deepcopy` keeps updater callables by reference). Diamond-shared
    // members follow the ratified family model (G0-1/D-11): each family
    // member is copied exactly once and the sharing is preserved, where the
    // Reference's per-child recursion would silently duplicate them.

    /// Build every detached entry before changing the destination. Besides
    /// keeping stale-source refusal atomic, this separates source traversal
    /// from destination allocation so cross-stage copies never recurse.
    fn detached_family_entries(&self, mob: Mob) -> Result<Vec<(Mob, Entry)>, StageError> {
        self.try_get(mob)?;
        self.family(mob)
            .into_iter()
            .map(|old| self.try_get(old).map(|entry| (old, entry.detached_copy())))
            .collect()
    }

    /// Allocate a prebuilt detached family, then remap every internal edge.
    /// No fallible operation remains after allocation begins.
    fn install_detached_family(target: &mut Self, entries: Vec<(Mob, Entry)>) -> CopyMap {
        // Imported updater identities share callable identity just like a
        // same-stage copy. Keep future destination registrations above every
        // imported token so the stage-local allocator cannot collide.
        if let Some(next_imported_id) = entries
            .iter()
            .flat_map(|(_, entry)| {
                entry
                    .updaters
                    .iter()
                    .map(|slot| slot.id)
                    .chain(entry.updater_states.iter().map(|(id, _)| *id))
            })
            .map(|id| id.raw().saturating_add(1))
            .max()
        {
            target.next_updater_id = target.next_updater_id.max(next_imported_id);
        }

        let mut pairs: Vec<(Mob, Mob)> = Vec::with_capacity(entries.len());
        let mut map: HashMap<Mob, Mob> = HashMap::with_capacity(entries.len());
        for (old, entry) in entries {
            let new = target.alloc(entry);
            pairs.push((old, new));
            map.insert(old, new);
        }
        let copied = CopyMap { pairs };
        for &(_, new) in copied.pairs() {
            let entry = target.get_mut(new).expect("just allocated");
            for edges in [&mut entry.submobjects, &mut entry.parents] {
                let mut seen: Vec<Mob> = Vec::new();
                edges.retain_mut(|edge| match map.get(edge) {
                    Some(mapped) => {
                        *edge = *mapped;
                        if seen.contains(mapped) {
                            false
                        } else {
                            seen.push(*mapped);
                            true
                        }
                    }
                    None => false,
                });
            }
            // A moved entry arrives with its old stage's value.
            entry.children_epoch = fresh_epoch();
            for (_, state) in &mut entry.updater_states {
                state.remap_handles(&copied);
            }
        }
        copied
    }

    /// manim `copy()` (§8.3): deep-copy the family subtree; family-internal
    /// references remap; family-external edges drop (the copy is a detached
    /// family); updater callables shared by reference; record data
    /// independent. See [`Stage::copy_family_mapped`] for the remap hook.
    pub fn copy_family(&mut self, mob: Mob) -> Result<Mob, StageError> {
        Ok(self.copy_family_mapped(mob)?.root())
    }

    /// [`Stage::copy_family`], returning the stable old-handle → new-handle
    /// [`CopyMap`] — the engine-side hook the binding tier (fmn-python,
    /// fm-aqv) walks to remap `__dict__` attribute aliases exactly as the
    /// Reference's `copy()` does with `family.index(value)`.
    pub fn copy_family_mapped(&mut self, mob: Mob) -> Result<CopyMap, StageError> {
        let entries = self.detached_family_entries(mob)?;
        Ok(Self::install_detached_family(self, entries))
    }

    // ------------------------------------------- target / saved state (§8.3)

    /// Reference `generate_target`: `self.target = self.copy()`, with the
    /// fresh target's `saved_state` pointing at the **same** saved state as
    /// the original (`target.saved_state = self.saved_state` — a shared
    /// link, not a copy). A previous target is unlinked but stays alive
    /// (explicit [`Stage::delete`] is the only destructor — exactly the
    /// Reference, where a replaced target survives as long as user code
    /// holds it).
    ///
    /// # Errors
    /// [`StageError::StaleHandle`].
    pub fn generate_target(&mut self, mob: Mob) -> Result<Mob, StageError> {
        let target = self.copy_family(mob)?;
        let saved = self.try_get(mob)?.saved_state;
        self.get_mut(target).expect("just copied").saved_state = saved;
        self.get_mut(mob).expect("copy checked liveness").target = Some(target);
        Ok(target)
    }

    /// The family member `mob` is an invisible padding copy of, if
    /// [`Stage::add_n_more_submobjects`] made it one (fm-b97z). A plain
    /// handle: the source may since have been deleted.
    #[must_use]
    pub fn padding_source(&self, mob: Mob) -> Option<Mob> {
        self.get(mob).and_then(|e| e.padding_source)
    }

    /// Record each member of a padding copy as a copy of its source member.
    pub(crate) fn mark_padding(&mut self, copy: &CopyMap) {
        for &(source, member) in copy.pairs() {
            if let Some(entry) = self.get_mut(member) {
                entry.padding_source = Some(source);
            }
        }
    }

    /// The current `generate_target` copy, if one was generated. The link
    /// is a plain handle: deleting the target leaves it stale, a defined
    /// state.
    #[must_use]
    pub fn target(&self, mob: Mob) -> Option<Mob> {
        self.get(mob).and_then(|e| e.target)
    }

    /// Reference `save_state`: `self.saved_state = self.copy()`, with the
    /// fresh copy's `target` pointing at the **same** target as the
    /// original (`saved_state.target = self.target`). Returns the
    /// saved-state handle (the Reference returns `self`; the handle is the
    /// useful value under the arena).
    ///
    /// # Errors
    /// [`StageError::StaleHandle`].
    pub fn save_state(&mut self, mob: Mob) -> Result<Mob, StageError> {
        let saved = self.copy_family(mob)?;
        let target = self.try_get(mob)?.target;
        self.get_mut(saved).expect("just copied").target = target;
        self.get_mut(mob)
            .expect("copy checked liveness")
            .saved_state = Some(saved);
        Ok(saved)
    }

    /// The current `save_state` copy, if one was saved.
    #[must_use]
    pub fn saved_state(&self, mob: Mob) -> Option<Mob> {
        self.get(mob).and_then(|e| e.saved_state)
    }

    /// Link an existing same-stage family as `mob`'s saved state.
    ///
    /// Binding tiers use this when the Reference saved a detached Python
    /// family before either graph entered a scene: both graphs are adopted
    /// independently, then this recreates the ordinary `saved_state`
    /// pointer without taking a late snapshot of already-mutated data.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] if either handle is not live in this
    /// stage.
    pub fn link_saved_state(&mut self, mob: Mob, saved: Mob) -> Result<(), StageError> {
        self.try_get(mob)?;
        self.try_get(saved)?;
        self.get_mut(mob)
            .expect("liveness checked above")
            .saved_state = Some(saved);
        Ok(())
    }

    /// Reference `Mobject.restore` (named for the per-mobject slot — the
    /// whole-stage [`Stage::restore`] is the snapshot path): `become` the
    /// saved state. The saved link survives, so repeated restores work,
    /// exactly as in the Reference.
    ///
    /// # Errors
    /// [`StageError::NoSavedState`] without a prior [`Stage::save_state`]
    /// (the Reference's "Trying to restore without having saved");
    /// [`StageError::StaleHandle`] if the saved copy was deleted; any
    /// [`Stage::become_mobject`] error.
    pub fn restore_mobject(&mut self, mob: Mob) -> Result<(), StageError> {
        let saved = self
            .try_get(mob)?
            .saved_state
            .ok_or(StageError::NoSavedState)?;
        self.become_mobject(mob, saved, false)
    }

    /// Reference `become` (§8.3) — named `become_mobject` because `become`
    /// is a Rust 2024 reserved keyword, following the
    /// [`Stage::update_mobject`] convention: edit `mob`'s data to be
    /// identical to `other`'s. The Reference aligns families first
    /// (`align_family`); callers must therefore run [`Stage::align_family`]
    /// before this lower-level copy. The two families must arrive with equal
    /// member counts and member-for-member child counts. Per zipped member: record data
    /// (schema-checked — the Reference's `set_data` asserts dtype
    /// equality), uniforms, and tracker state copy across; outstanding live
    /// views on `mob` detach as under resize (V6). Updater lists are
    /// untouched unless `match_updaters`, which then shares the root's
    /// list by reference (the Reference's `match_updaters` call — root
    /// only, not the family). Animating/suspension flags, scene membership,
    /// and the target/saved-state links stay `mob`'s own.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`], [`StageError::FamilyShapeMismatch`],
    /// [`StageError::SchemaMismatch`].
    pub fn become_mobject(
        &mut self,
        mob: Mob,
        other: Mob,
        match_updaters: bool,
    ) -> Result<(), StageError> {
        let family1 = self.family(mob);
        let family2 = self.family(other);
        if family1.is_empty() || family2.is_empty() {
            return Err(StageError::StaleHandle);
        }
        if family1.len() != family2.len() {
            return Err(StageError::FamilyShapeMismatch);
        }
        // Precheck the whole zip so a failure never leaves a half-become.
        for (&a, &b) in family1.iter().zip(family2.iter()) {
            let e1 = self.try_get(a)?;
            let e2 = self.try_get(b)?;
            if e1.submobjects.len() != e2.submobjects.len() {
                return Err(StageError::FamilyShapeMismatch);
            }
            if e1.buffer.schema() != e2.buffer.schema() {
                return Err(StageError::SchemaMismatch);
            }
        }
        for (&a, &b) in family1.iter().zip(family2.iter()) {
            if a == b {
                continue;
            }
            let (src, placement, uniforms, z_index, tracker, render_primitive, image) = {
                let e2 = self.get(b).expect("prechecked");
                (
                    e2.buffer.snapshot_clone(),
                    e2.placement,
                    e2.uniforms,
                    e2.z_index,
                    e2.tracker,
                    e2.render_primitive,
                    e2.image.clone(),
                )
            };
            let e1 = self.get_mut(a).expect("prechecked");
            if !e1.buffer.assign_from(&src) {
                return Err(StageError::SchemaMismatch); // unreachable: prechecked
            }
            e1.set_placement(placement);
            e1.uniforms = uniforms;
            e1.z_index = z_index;
            e1.tracker = tracker;
            e1.render_primitive = render_primitive;
            e1.replace_image_resource(image);
        }
        if match_updaters {
            self.match_updaters(mob, other)?;
        }
        Ok(())
    }

    /// Install a value mobject over an existing handle.
    ///
    /// The handle survives — scene-list membership, updaters, suspension,
    /// animating status, tracker, target, and pins are identity-linked
    /// runtime state this method deliberately preserves — while everything
    /// content-shaped is replaced from the value: buffer, uniforms,
    /// z-index, semantic shape slot, render primitive, image, and the child
    /// subtree. Old members are reclaimed unless they remain scene roots
    /// or have parents outside the replaced family; retained members keep
    /// their complete descendants. Cached raw handles alone do not retain
    /// retired content, and pinned retired members follow [`Stage::delete`]'s
    /// deferred destruction rules. The value's children are added and
    /// attached, mirroring [`Stage::add`]. The shape-keyed `save_state` copy
    /// is cleared as stale, and placement resets to identity because the
    /// value's geometry is authoritative.
    ///
    /// This is the primitive structurally-changing rebuilds need (the
    /// numbers family's `set_value` crossing a digit count, fm-jh7):
    /// [`Stage::become_mobject`] requires both families to match shape,
    /// which a value rebuild does not guarantee.
    ///
    /// # Errors
    ///
    /// [`StageError::StaleHandle`] for a dead `mob`, or the attach errors
    /// reported while wiring the value's children.
    pub fn install(&mut self, mob: Mob, mobject: impl Into<Mobject>) -> Result<(), StageError> {
        let Mobject {
            buffer,
            uniforms,
            z_index,
            shape,
            render_primitive,
            image,
            submobjects,
            defaults_row,
        } = mobject.into();
        drop(defaults_row);
        // Content-only installs (notably animated numbers) need no scene
        // scan or topology change when there is no old child family.
        if !self.try_get(mob)?.submobjects.is_empty() {
            let old: Vec<_> = self.family(mob).into_iter().skip(1).collect();
            let old_members: HashSet<Mob, IdBuildHasher> = old.iter().copied().collect();
            self.replace_children(mob, &[])?;

            // Mark after detaching the rebuilt root: its old edges must
            // not count as outside ownership of every replaced child.
            let mut pending: Vec<_> = self
                .roots
                .iter()
                .copied()
                .filter(|root| old_members.contains(root))
                .collect();
            for &member in &old {
                if self.get(member).is_some_and(|entry| {
                    entry
                        .parents
                        .iter()
                        .any(|parent| !old_members.contains(parent))
                }) {
                    pending.push(member);
                }
            }
            let mut retained: HashSet<Mob, IdBuildHasher> = HashSet::default();
            while let Some(member) = pending.pop() {
                if !retained.insert(member) {
                    continue;
                }
                if let Some(entry) = self.get(member) {
                    pending.extend_from_slice(&entry.submobjects);
                }
            }

            // Fixed candidate/retention sets reclaim private descendants
            // through diamonds and pinned parents. Clear outgoing edges
            // before deletion's family-based scene removal can reach a
            // retained descendant, including on a later unpin.
            for member in old {
                if !retained.contains(&member) && self.contains(member) {
                    self.replace_children(member, &[])?;
                    self.delete(member)?;
                }
            }
        }
        let entry = self.get_mut(mob).ok_or(StageError::StaleHandle)?;
        entry.buffer = buffer;
        entry.uniforms = uniforms;
        entry.z_index = z_index;
        entry.shape = ShapeSlot {
            tag: shape,
            point_revision: entry.buffer.field_revision("point"),
        };
        entry.render_primitive = render_primitive;
        entry.image_revision = u64::from(image.is_some());
        entry.image = image;
        entry.placement = Placement::IDENTITY;
        entry.placement_revision = 0;
        entry.saved_state = None;
        self.invalidate_family_caches(mob);
        for child in submobjects {
            let child_mob = self.add(child);
            self.attach(mob, child_mob)?;
        }
        Ok(())
    }

    /// Cross-stage transfer under the two-scene policy: content moves by the
    /// same copy-category rules as [`Stage::copy_family_mapped`], never by
    /// handle. The family is traversed iteratively and each DAG member is
    /// installed exactly once, preserving internal sharing.
    pub fn copy_into(&self, mob: Mob, target: &mut Stage) -> Result<Mob, StageError> {
        let entries = self.detached_family_entries(mob)?;
        Ok(Self::install_detached_family(target, entries).root())
    }

    /// Cross-stage move of a whole family: this stage gives up every member
    /// and the destination receives the same entries. Each keeps its record
    /// storage, so NumPy views exported before the move stay live (§8.2:
    /// reallocation under a live view is impossible by construction), and
    /// no record is copied. Otherwise the result is [`Stage::copy_into`]'s:
    /// family-internal edges remap, external edges drop, and `target`,
    /// `saved_state`, pins and derived caches reset.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] if `mob` is not live in this stage.
    pub fn move_into(&mut self, mob: Mob, target: &mut Stage) -> Result<Mob, StageError> {
        self.try_get(mob)?;
        let members = self.family(mob);
        self.remove_many_from_scene(&[mob]);
        // Drop edges from outside the family before the slots empty, so no
        // remaining entry here names a moved member.
        for &member in &members {
            let parents = self.try_get(member)?.parents.clone();
            for parent in parents {
                if !members.contains(&parent) {
                    self.detach(parent, member);
                }
            }
        }
        let mut entries = Vec::with_capacity(members.len());
        for old in members {
            let slot = &mut self.slots[old.index as usize];
            let mut entry = slot.entry.take().expect("family members are live");
            slot.generation = slot.generation.wrapping_add(1);
            self.free.push(old.index);
            entry.target = None;
            entry.saved_state = None;
            entry.padding_source = None;
            entry.pins = 0;
            entry.pending_delete = false;
            entry.family_cache = RefCell::new(None);
            entry.bbox = RefCell::new(BboxCache::default());
            entries.push((old, entry));
        }
        Ok(Self::install_detached_family(target, entries).root())
    }

    /// Cross-stage transfer of exactly one arena entry, without its family.
    ///
    /// Binding and serialization tiers whose object graph lives outside
    /// Marionette use this to preserve the entry's complete native payload
    /// while letting their own graph memo reconnect children. Internal
    /// parent/child links are cleared by the same detached-family installer
    /// as [`Stage::copy_into`]; renderer metadata, placement, uniforms,
    /// tracker state, and updater identities follow the ordinary copy rules.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] if `mob` is not live in this stage.
    pub fn copy_entry_into(&self, mob: Mob, target: &mut Stage) -> Result<Mob, StageError> {
        let entry = self.try_get(mob)?.detached_copy();
        Ok(Self::install_detached_family(target, vec![(mob, entry)]).root())
    }

    // ----------------------------------------------------------- updaters
    //
    // The §8.6 dynamic-behavior surface (fm-yra). Exact semantics, mirroring
    // the pinned Reference:
    // - ONE insertion-ordered list per mobject mixing dt and non-dt kinds;
    //   execution is pure insertion order (the Reference distinguishes kinds
    //   by signature inspection; registration is typed here).
    // - `update(dt)` runs each rooted family child-first (the Reference
    //   recurses submobjects before running its own updaters) and prunes a
    //   subtree at any suspended node or any node with no updaters anywhere
    //   in its family.
    // - The updater list is snapshotted per node per tick, so add/remove
    //   during iteration has a defined outcome: changes take effect next
    //   tick (§8.6's "snapshot the list per tick").
    // - `add_updater(call = true)` runs `update(dt = 0)` exactly ONCE — the
    //   Reference calls it twice (C-5), a fixed bug (Behavior Note
    //   BN-07-updater-and-group-fixes).

    /// The first returned updater failure, retained until explicitly taken.
    ///
    /// While a failure is pending, no native updater callbacks run. Restoring
    /// a snapshot or aborting an animation leaves the diagnostic intact so
    /// cleanup cannot retry a failed callback. The failure belongs to this
    /// physical Stage; [`Snapshot::materialize`] starts with no failure.
    #[must_use]
    pub fn updater_error(&self) -> Option<&UpdaterFailure> {
        self.updater_failure.as_ref()
    }

    /// Acknowledge a failure after repairing or removing the offending updater.
    ///
    /// This is the only operation that clears the retained diagnostic and
    /// allows callbacks to run again. It never rolls back callback mutations.
    ///
    /// # Errors
    /// A callback or initial redraw factory cannot acknowledge an error while
    /// it is running: nested updates must not resume the rest of a failed pass.
    pub fn take_updater_error(&mut self) -> Result<Option<UpdaterFailure>, UpdaterError> {
        if Rc::strong_count(&self.updater_callbacks) > 1 {
            return Err(UpdaterError::callback(
                "Stage::take_updater_error",
                "cannot acknowledge an updater failure while a callback is running",
            ));
        }
        Ok(self.updater_failure.take())
    }

    /// Preflight fallible registration without invoking a factory or minting
    /// an identity. Redraw factories must repeat this after running user code.
    pub(crate) fn check_updater_registration(&self) -> Result<(), UpdaterError> {
        if let Some(failure) = &self.updater_failure {
            return Err(failure.error.clone());
        }
        self.next_updater_id
            .checked_add(1)
            .ok_or(StageError::UpdaterIdExhausted)?;
        Ok(())
    }

    /// Guard a redraw's initial factory, before it has a registered identity.
    /// An ignored failure from a nested updater still prevents publication.
    pub(crate) fn invoke_updater_factory<T>(
        &mut self,
        factory: impl FnOnce(&mut Stage) -> Result<T, UpdaterError>,
    ) -> Result<T, UpdaterError> {
        if let Some(failure) = &self.updater_failure {
            return Err(failure.error.clone());
        }
        let _callback = Rc::clone(&self.updater_callbacks);
        let result = factory(self);
        match &self.updater_failure {
            Some(failure) => Err(failure.error.clone()),
            None => result,
        }
    }

    fn record_updater_error(&mut self, mobject: Mob, updater: UpdaterId, error: UpdaterError) {
        if self.updater_failure.is_none() {
            self.updater_failure = Some(UpdaterFailure {
                mobject,
                updater,
                error,
            });
        }
    }

    fn register_try_updater(
        &mut self,
        mob: Mob,
        func: UpdaterFn,
        call: bool,
    ) -> Result<UpdaterId, UpdaterError> {
        let id = self.register_updater(mob, func, None, false)?;
        if call {
            self.update_mob(mob, 0.0);
            if let Some(error) = self.updater_error().map(|failure| failure.error.clone()) {
                // Return no undisclosed registration after an immediate error.
                // User mutations and any other registrations remain intact;
                // issued identities are never rewound or reused.
                self.remove_updater(mob, id);
                return Err(error);
            }
        }
        Ok(id)
    }

    fn register_updater(
        &mut self,
        mob: Mob,
        func: UpdaterFn,
        index: Option<usize>,
        call: bool,
    ) -> Result<UpdaterId, StageError> {
        if !self.contains(mob) {
            return Err(StageError::StaleHandle);
        }
        let id = UpdaterId(self.next_updater_id);
        self.next_updater_id = self
            .next_updater_id
            .checked_add(1)
            .ok_or(StageError::UpdaterIdExhausted)?;
        let entry = self.get_mut(mob).expect("handle was checked above");
        let slot = UpdaterSlot { id, func };
        match index {
            Some(i) => {
                let i = i.min(entry.updaters.len());
                entry.updaters.insert(i, slot);
            }
            None => entry.updaters.push(slot),
        }
        if call {
            // C-5 correction: exactly one update pass (the Reference runs
            // `self.update(dt=0)` and then unconditionally `self.update()`
            // again — a double call).
            self.update_mob(mob, 0.0);
        }
        Ok(id)
    }

    /// Register a non-dt updater (the Reference's `lambda m: ...`),
    /// appended in insertion order. `call` runs an immediate `update(0)`
    /// pass over this mobject's family — exactly once (C-5 fixed).
    ///
    /// # Errors
    /// [`StageError::StaleHandle`].
    pub fn add_updater(
        &mut self,
        mob: Mob,
        updater: impl FnMut(&mut Stage, Mob) + 'static,
        call: bool,
    ) -> Result<UpdaterId, StageError> {
        self.register_updater(
            mob,
            UpdaterFn::NonDt(Rc::new(RefCell::new(updater))),
            None,
            call,
        )
    }

    /// Register a dt updater (the Reference's `lambda m, dt: ...`).
    ///
    /// # Errors
    /// [`StageError::StaleHandle`].
    pub fn add_dt_updater(
        &mut self,
        mob: Mob,
        updater: impl FnMut(&mut Stage, Mob, f64) + 'static,
        call: bool,
    ) -> Result<UpdaterId, StageError> {
        self.register_updater(
            mob,
            UpdaterFn::Dt(Rc::new(RefCell::new(updater))),
            None,
            call,
        )
    }

    /// Register a non-dt updater that can refuse a graph, text, or other rebuild.
    ///
    /// The first returned error is retained by [`Self::updater_error`] and
    /// stops all remaining callbacks until [`Self::take_updater_error`].
    /// Copies share the callable and its identity, but failures name the
    /// receiving mobject and stay on the physical Stage that ran the callback.
    /// Insertion order, suspension, and per-tick list snapshots are unchanged.
    ///
    /// With `call`, run exactly one zero-dt family pass. An immediate failure
    /// removes only this new registration; callback mutations are not undone.
    ///
    /// # Errors
    /// A stale target, exhausted identity space, an already-pending failure,
    /// or a callback failure during the immediate pass.
    pub fn add_try_updater(
        &mut self,
        mob: Mob,
        mut updater: impl FnMut(&mut Stage, Mob) -> Result<(), UpdaterError> + 'static,
        call: bool,
    ) -> Result<UpdaterId, UpdaterError> {
        self.check_updater_registration()?;
        self.try_get(mob)?;
        let id = UpdaterId(self.next_updater_id);
        self.register_try_updater(
            mob,
            UpdaterFn::NonDt(Rc::new(RefCell::new(move |stage: &mut Stage, me| {
                if stage.updater_error().is_some() {
                    return;
                }
                let result = if stage.contains(me) {
                    updater(stage, me)
                } else {
                    Err(StageError::StaleHandle.into())
                };
                if let Err(error) = result {
                    stage.record_updater_error(me, id, error);
                }
            }))),
            call,
        )
    }

    /// Register a fallible dt updater, with [`Self::add_try_updater`]'s error
    /// retention and immediate-call rules. The callback receives the same
    /// frame delta as an ordinary [`Self::add_dt_updater`] callback.
    ///
    /// # Errors
    /// A stale target, exhausted identity space, an already-pending failure,
    /// or a callback failure during the immediate pass.
    pub fn add_try_dt_updater(
        &mut self,
        mob: Mob,
        mut updater: impl FnMut(&mut Stage, Mob, f64) -> Result<(), UpdaterError> + 'static,
        call: bool,
    ) -> Result<UpdaterId, UpdaterError> {
        self.check_updater_registration()?;
        self.try_get(mob)?;
        let id = UpdaterId(self.next_updater_id);
        self.register_try_updater(
            mob,
            UpdaterFn::Dt(Rc::new(RefCell::new(move |stage: &mut Stage, me, dt| {
                if stage.updater_error().is_some() {
                    return;
                }
                let result = if stage.contains(me) {
                    updater(stage, me, dt)
                } else {
                    Err(StageError::StaleHandle.into())
                };
                if let Err(error) = result {
                    stage.record_updater_error(me, id, error);
                }
            }))),
            call,
        )
    }

    /// Register a dt updater whose native object attributes are separate from
    /// its callable. Copies share `updater` and its identity, while `state`
    /// follows the receiving mobject through family copies and in-memory
    /// snapshots. `remap` rewrites family-internal handles through the copy map;
    /// references outside the copied family should remain unchanged.
    ///
    /// The callable reads or edits its attributes with [`Stage::updater_state`]
    /// and [`Stage::updater_state_mut`], using the supplied identity. Finish any
    /// mutable state borrow before calling other Stage methods. `T::clone` must
    /// give mutable object attributes independent value semantics; shared
    /// callback objects themselves should stay in the callable.
    ///
    /// Attributes survive removing or clearing the updater, just like ordinary
    /// mobject attributes. This also preserves snapshot-per-tick execution when
    /// an earlier callback removes a later callback from the same list.
    /// Durable snapshots omit executable native attributes along with callable
    /// bodies: replay must rebuild them, or reinstall them explicitly with
    /// [`Stage::set_updater_state`] after binding the callable identity.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] or [`StageError::UpdaterIdExhausted`].
    pub fn add_dt_updater_with_state<T: Clone + 'static>(
        &mut self,
        mob: Mob,
        state: T,
        remap: fn(&mut T, &CopyMap),
        mut updater: impl FnMut(&mut Stage, Mob, UpdaterId, f64) + 'static,
        call: bool,
    ) -> Result<UpdaterId, StageError> {
        let id = UpdaterId(self.next_updater_id);
        self.register_updater(
            mob,
            UpdaterFn::Dt(Rc::new(RefCell::new(move |stage: &mut Stage, mob, dt| {
                updater(stage, mob, id, dt);
            }))),
            None,
            false,
        )?;
        self.set_updater_state(mob, id, state, remap)?;
        if call {
            self.update_mob(mob, 0.0);
        }
        Ok(id)
    }

    /// Register a fallible dt updater with mobject-owned native attributes.
    ///
    /// State has [`Self::add_dt_updater_with_state`]'s copy, handle-remapping,
    /// and snapshot semantics. It is installed before the optional immediate
    /// zero-dt family pass, so the callback can read it on its first invocation.
    /// Errors have [`Self::add_try_updater`]'s retention and ordering semantics:
    /// the first failure stops the pass and names the actual receiving copy.
    ///
    /// An immediate failure removes only the new callable registration. Its
    /// attributes remain ordinary mobject state, as when removing any updater;
    /// callback mutations are not rolled back. Prepare fallible work before
    /// publishing new attributes when the last valid state must be preserved.
    ///
    /// # Errors
    /// A stale target, exhausted identity space, an already-pending failure,
    /// or a callback failure during the immediate pass.
    pub fn add_try_dt_updater_with_state<T: Clone + 'static>(
        &mut self,
        mob: Mob,
        state: T,
        remap: fn(&mut T, &CopyMap),
        mut updater: impl FnMut(&mut Stage, Mob, UpdaterId, f64) -> Result<(), UpdaterError> + 'static,
        call: bool,
    ) -> Result<UpdaterId, UpdaterError> {
        // No user code runs between reserving the next identity and installing
        // the state. The ordinary fallible registration performs every guard.
        let id = UpdaterId(self.next_updater_id);
        self.add_try_dt_updater(
            mob,
            move |stage, receiver, dt| updater(stage, receiver, id, dt),
            false,
        )?;
        if let Err(error) = self.set_updater_state(mob, id, state, remap) {
            self.remove_updater(mob, id);
            return Err(error.into());
        }
        if call {
            self.update_mob(mob, 0.0);
            if let Some(error) = self.updater_error().map(|failure| failure.error.clone()) {
                self.remove_updater(mob, id);
                return Err(error);
            }
        }
        Ok(id)
    }

    /// Install native attributes for an existing updater identity. This is
    /// also the explicit state-rebinding seam after a durable snapshot's
    /// callable manifest has been resolved. No native state is inferred from
    /// a serialized updater identity.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] for a dead mobject or
    /// [`StageError::UpdaterBindingMismatch`] for an unregistered identity.
    pub fn set_updater_state<T: Clone + 'static>(
        &mut self,
        mob: Mob,
        id: UpdaterId,
        state: T,
        remap: fn(&mut T, &CopyMap),
    ) -> Result<(), StageError> {
        let entry = self.get_mut(mob).ok_or(StageError::StaleHandle)?;
        if !entry.updaters.iter().any(|slot| slot.id == id) {
            return Err(StageError::UpdaterBindingMismatch);
        }
        let state = NativeUpdaterState::new(state, remap);
        if let Some((_, previous)) = entry.updater_states.iter_mut().find(|(key, _)| *key == id) {
            *previous = state;
        } else {
            entry.updater_states.push((id, state));
        }
        Ok(())
    }

    /// Read a native updater's object-owned attributes. Returns `None` for a
    /// stale handle, missing binding, or mismatched state type.
    #[must_use]
    pub fn updater_state<T: 'static>(&self, mob: Mob, id: UpdaterId) -> Option<&T> {
        self.get(mob)?
            .updater_states
            .iter()
            .find(|(key, _)| *key == id)?
            .1
            .get()
    }

    /// Edit native updater attributes, detaching any shared in-memory
    /// snapshot or copied value before the write.
    pub fn updater_state_mut<T: 'static>(&mut self, mob: Mob, id: UpdaterId) -> Option<&mut T> {
        self.get_mut(mob)?
            .updater_states
            .iter_mut()
            .find(|(key, _)| *key == id)?
            .1
            .get_mut()
    }

    /// Insert a non-dt updater at `index` in the list (Reference
    /// `insert_updater`; no immediate call, matching it).
    ///
    /// # Errors
    /// [`StageError::StaleHandle`].
    pub fn insert_updater(
        &mut self,
        mob: Mob,
        index: usize,
        updater: impl FnMut(&mut Stage, Mob) + 'static,
    ) -> Result<UpdaterId, StageError> {
        self.register_updater(
            mob,
            UpdaterFn::NonDt(Rc::new(RefCell::new(updater))),
            Some(index),
            false,
        )
    }

    /// Remove every occurrence of `id` from `mob`'s updater list (the
    /// Reference removes by function identity, all occurrences).
    pub fn remove_updater(&mut self, mob: Mob, id: UpdaterId) {
        if let Some(entry) = self.get_mut(mob) {
            entry.updaters.retain(|slot| slot.id != id);
        }
    }

    /// Clear updaters on `mob` (and, with `recurse`, its whole family).
    pub fn clear_updaters(&mut self, mob: Mob, recurse: bool) {
        let targets = if recurse {
            self.family(mob)
        } else if self.contains(mob) {
            vec![mob]
        } else {
            Vec::new()
        };
        for target in targets {
            if let Some(entry) = self.get_mut(target) {
                entry.updaters.clear();
            }
        }
    }

    /// Copy `source`'s updater list onto `mob` (Reference `match_updaters`:
    /// callables shared by reference). Native updater attributes copy by value
    /// and rebind across aligned families; other target attributes remain.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] if either handle is dead, or
    /// [`StageError::FamilyShapeMismatch`] if native attributes need rebinding
    /// across families with different shapes.
    pub fn match_updaters(&mut self, mob: Mob, source: Mob) -> Result<(), StageError> {
        self.try_get(mob)?;
        let source_entry = self.try_get(source)?;
        let updaters = source_entry.updaters.clone();
        let mut states: Vec<_> = source_entry
            .updater_states
            .iter()
            .filter(|(id, _)| updaters.iter().any(|slot| slot.id == *id))
            .cloned()
            .collect();
        if !states.is_empty() {
            let source_family = self.family(source);
            let target_family = self.family(mob);
            if source_family.len() != target_family.len()
                || source_family.iter().zip(&target_family).any(|(&a, &b)| {
                    self.get(a).map(|entry| entry.submobjects.len())
                        != self.get(b).map(|entry| entry.submobjects.len())
                })
            {
                return Err(StageError::FamilyShapeMismatch);
            }
            let map = CopyMap {
                pairs: source_family.into_iter().zip(target_family).collect(),
            };
            for (_, state) in &mut states {
                state.remap_handles(&map);
            }
        }
        let entry = self.get_mut(mob).ok_or(StageError::StaleHandle)?;
        entry.updaters = updaters;
        for (id, state) in states {
            if let Some((_, previous)) = entry.updater_states.iter_mut().find(|(key, _)| *key == id)
            {
                *previous = state;
            } else {
                entry.updater_states.push((id, state));
            }
        }
        Ok(())
    }

    /// The registered updater ids on `mob`, in execution order.
    #[must_use]
    pub fn updater_ids(&self, mob: Mob) -> Vec<UpdaterId> {
        self.get(mob)
            .map(|e| e.updaters.iter().map(|s| s.id).collect())
            .unwrap_or_default()
    }

    /// Atomically reinstall updater callables under identities recovered from
    /// a durable manifest.
    ///
    /// Durable snapshots retain updater identity and kind but never executable
    /// code. A replay owner resolves every callback first, then supplies the
    /// complete binding set here. All targets and identities are validated
    /// before any list changes; a mismatch cannot leave a partially rebound
    /// scene. Bindings for one mobject must appear in execution order.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] if a target is not live, or
    /// [`StageError::UpdaterBindingMismatch`] if an identity was never issued,
    /// is duplicated on one target, or that target has already acquired
    /// another updater list.
    pub fn restore_updater_bindings(
        &mut self,
        bindings: Vec<(UpdaterIdentity, UpdaterFn)>,
    ) -> Result<(), StageError> {
        let mut targets = Vec::new();
        let mut identities = Vec::new();
        for (identity, func) in &bindings {
            let entry = self.get(identity.mob).ok_or(StageError::StaleHandle)?;
            if identity.id.0 == 0 || identity.id.0 >= self.next_updater_id {
                return Err(StageError::UpdaterBindingMismatch);
            }
            let kind_matches = matches!(
                (identity.kind, func),
                (UpdaterKindTag::NonDt, UpdaterFn::NonDt(_))
                    | (UpdaterKindTag::Dt, UpdaterFn::Dt(_))
            );
            if !kind_matches {
                return Err(StageError::UpdaterBindingMismatch);
            }
            if !targets.contains(&identity.mob) {
                if !entry.updaters.is_empty() {
                    return Err(StageError::UpdaterBindingMismatch);
                }
                targets.push(identity.mob);
            }
            if identities.contains(&(identity.mob, identity.id)) {
                return Err(StageError::UpdaterBindingMismatch);
            }
            identities.push((identity.mob, identity.id));
        }
        for (identity, func) in bindings {
            self.get_mut(identity.mob)
                .expect("binding targets were prevalidated")
                .updaters
                .push(UpdaterSlot {
                    id: identity.id,
                    func,
                });
        }
        Ok(())
    }

    /// Whether `mob` or anything in its family has updaters.
    #[must_use]
    pub fn has_updaters_in_family(&self, mob: Mob) -> bool {
        self.with_family_slice(mob, |family| {
            family
                .iter()
                .any(|m| self.get(*m).is_some_and(|e| !e.updaters.is_empty()))
        })
    }

    /// `(non_dt, dt)` updater counts across `mob`'s whole family — the
    /// §9.5 purity classifier's probe (a dt-updater and a non-dt updater
    /// demote a segment for different recorded reasons).
    #[must_use]
    pub fn family_updater_kinds(&self, mob: Mob) -> (usize, usize) {
        self.with_family_slice(mob, |family| {
            let mut non_dt = 0;
            let mut dt = 0;
            for &member in family {
                if let Some(entry) = self.get(member) {
                    for slot in &entry.updaters {
                        match slot.func {
                            UpdaterFn::NonDt(_) => non_dt += 1,
                            UpdaterFn::Dt(_) => dt += 1,
                        }
                    }
                }
            }
            (non_dt, dt)
        })
    }

    /// Whether updating is suspended on `mob` itself.
    #[must_use]
    pub fn is_updating_suspended(&self, mob: Mob) -> bool {
        self.get(mob).is_some_and(|e| e.updating_suspended)
    }

    /// Reference `set_animating_status` (§9.1): mark the family under `mob`
    /// (all of it when `recurse`, else `mob` alone) **and every transitive
    /// ancestor** — the Reference iterates `(*get_family(recurse),
    /// *get_ancestors())`, and `get_ancestors` always walks the full parent
    /// closure regardless of `recurse`.
    pub fn set_animating_status(&mut self, mob: Mob, is_animating: bool, recurse: bool) {
        let mut targets = if recurse {
            self.family(mob)
        } else if self.contains(mob) {
            vec![mob]
        } else {
            Vec::new()
        };
        // Ancestor closure: parents, grandparents, … (dedup; order is
        // irrelevant for a flag write).
        let mut pending = self
            .get(mob)
            .map(|e| e.parents().to_vec())
            .unwrap_or_default();
        while let Some(parent) = pending.pop() {
            if targets.contains(&parent) {
                continue;
            }
            targets.push(parent);
            if let Some(entry) = self.get(parent) {
                pending.extend_from_slice(entry.parents());
            }
        }
        for target in targets {
            if let Some(entry) = self.get_mut(target) {
                entry.is_animating = is_animating;
            }
        }
    }

    /// Whether `mob` itself is currently marked animating.
    #[must_use]
    pub fn is_animating(&self, mob: Mob) -> bool {
        self.get(mob).is_some_and(|e| e.is_animating)
    }

    /// Reference `is_changing`: animating, or carrying updaters of its own
    /// (self-only, not the family) — the render-cache invalidation probe.
    #[must_use]
    pub fn is_changing(&self, mob: Mob) -> bool {
        self.get(mob)
            .is_some_and(|e| e.is_animating || !e.updaters.is_empty())
    }

    /// Reference `Mobject.update(dt)`: one mobject's family update pass
    /// (children first, insertion order, suspension-pruned) — the per-target
    /// slot §9.1's `Animation::update_mobjects` drives for starting/target
    /// copies, while [`Stage::update`] remains the whole-scene pass.
    pub fn update_mobject(&mut self, mob: Mob, dt: f64) {
        self.update_mobject_with_recurse(mob, dt, true);
    }

    /// Reference `Mobject.update(dt, recurse)`: choose between the whole
    /// child-first family pass and this node's own updater list. Front doors
    /// use the explicit flag rather than approximating `recurse = false` by
    /// temporarily mutating the family graph.
    pub fn update_mobject_with_recurse(&mut self, mob: Mob, dt: f64, recurse: bool) {
        if recurse {
            self.update_mob(mob, dt);
        } else {
            self.update_mob_self(mob, dt);
        }
    }

    /// Suspend updating on `mob` (and, with `recurse`, its children,
    /// transitively) — Reference `suspend_updating`. A suspended node
    /// prunes its whole subtree in [`Stage::update`], which is exactly how
    /// an animated mobject's updaters pause during a play (§9.1's
    /// `suspend_mobject_updating` hooks in here).
    pub fn suspend_updating(&mut self, mob: Mob, recurse: bool) {
        let targets = if recurse {
            self.family(mob)
        } else if self.contains(mob) {
            vec![mob]
        } else {
            Vec::new()
        };
        for target in targets {
            if let Some(entry) = self.get_mut(target) {
                entry.updating_suspended = true;
            }
        }
    }

    /// Resume updating — Reference `resume_updating`, rule for rule: clears
    /// the flag on `mob` (and, with `recurse`, its children), clears it on
    /// the whole ancestor chain *without* recursing into their subtrees or
    /// calling their updaters (each Reference parent resumes its own
    /// parents in turn — the clear is transitive upward), then (with
    /// `call_updater`) runs one `update(0)` pass over `mob`.
    pub fn resume_updating(&mut self, mob: Mob, recurse: bool, call_updater: bool) {
        let targets = if recurse {
            self.family(mob)
        } else if self.contains(mob) {
            vec![mob]
        } else {
            Vec::new()
        };
        if targets.is_empty() {
            return;
        }
        for target in &targets {
            if let Some(entry) = self.get_mut(*target) {
                entry.updating_suspended = false;
            }
        }
        // Transitive ancestor clear (no subtree recursion, no updater call).
        let mut pending = self
            .get(mob)
            .map(|e| e.parents().to_vec())
            .unwrap_or_default();
        let mut seen: Vec<Mob> = Vec::new();
        while let Some(parent) = pending.pop() {
            if seen.contains(&parent) {
                continue;
            }
            seen.push(parent);
            if let Some(entry) = self.get_mut(parent) {
                entry.updating_suspended = false;
            }
            if let Some(entry) = self.get(parent) {
                pending.extend_from_slice(entry.parents());
            }
        }
        if call_updater {
            self.update_mob(mob, 0.0);
        }
    }

    /// One node's update pass, in the Reference's exact order: prune if the
    /// node is suspended or its family has no updaters; recurse children
    /// first; then run this node's own updaters in insertion order over a
    /// per-tick snapshot of the list.
    fn update_mob(&mut self, mob: Mob, dt: f64) {
        if self.updater_failure.is_some() {
            return;
        }
        let Some(entry) = self.get(mob) else {
            return;
        };
        if entry.updating_suspended || !self.has_updaters_in_family(mob) {
            return;
        }
        let children = self
            .get(mob)
            .map(|e| e.submobjects.clone())
            .unwrap_or_default();
        for &child in &children {
            self.update_mob(child, dt);
            if self.updater_failure.is_some() {
                return;
            }
        }
        self.update_mob_self(mob, dt);
    }

    /// Run only one node's updater snapshot. The caller owns family order;
    /// this helper is also the exact `recurse = false` path.
    fn update_mob_self(&mut self, mob: Mob, dt: f64) {
        if self.updater_failure.is_some()
            || self.get(mob).is_none_or(|entry| entry.updating_suspended)
        {
            return;
        }
        let updaters = self
            .get(mob)
            .map(|e| e.updaters.clone())
            .unwrap_or_default();
        for slot in updaters {
            if self.updater_failure.is_some() {
                break;
            }
            let _callback = Rc::clone(&self.updater_callbacks);
            let result = match slot.func {
                UpdaterFn::NonDt(f) => f.try_borrow_mut().map(|mut callback| callback(self, mob)),
                UpdaterFn::Dt(f) => f
                    .try_borrow_mut()
                    .map(|mut callback| callback(self, mob, dt)),
            };
            if result.is_err() {
                self.record_updater_error(
                    mob,
                    slot.id,
                    UpdaterError::callback(
                        "Stage::update_mobject",
                        "updater callback is already running",
                    ),
                );
            }
        }
    }

    /// The scene-updater step of the frame order (§9.3 steps 3–4): time
    /// advances FIRST (the Reference's `increment_time` precedes
    /// `update_mobjects`, so an updater reading scene time observes the
    /// post-advance value), then every rooted family's updaters run
    /// (child-first, insertion order, suspension-pruned). The full
    /// six-step order is Choreo's; the arena provides the execution slot.
    pub fn update(&mut self, dt: f64) {
        if self.updater_failure.is_some() {
            return;
        }
        self.time += dt;
        let roots = self.roots.clone();
        for &root in &roots {
            self.update_mob(root, dt);
            if self.updater_failure.is_some() {
                break;
            }
        }
    }

    /// Run the scene-updater pass at an exact front-end clock value.
    ///
    /// Choreo advances its rational frame clock before step 4, then supplies
    /// the single `frames / fps` conversion here. This avoids exposing a
    /// repeatedly accumulated float to updaters while retaining `dt` as their
    /// per-frame delta. Lower-level callers without a rational clock continue
    /// to use [`Stage::update`].
    pub fn update_at_time(&mut self, dt: f64, time: f64) {
        if self.updater_failure.is_some() {
            return;
        }
        self.set_time_from_clock(time);
        let roots = self.roots.clone();
        for &root in &roots {
            self.update_mob(root, dt);
            if self.updater_failure.is_some() {
                break;
            }
        }
    }

    // ---------------------------------------------------------- snapshots

    /// CoW snapshot of the whole stage: record storages share until someone
    /// writes; entries with live views copy eagerly (view-protocol rule
    /// V5). Cost is O(touched + live-viewed), verified by test.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            stage_id: self.id,
            next_updater_id: self.next_updater_id,
            frame_shape: self.frame_shape,
            slots: self
                .slots
                .iter()
                .map(|slot| {
                    (
                        slot.generation,
                        slot.entry.as_ref().map(|entry| {
                            let buffer = entry.buffer.snapshot_clone();
                            let hint_was_live = entry.shape.point_revision.is_some()
                                && entry.shape.point_revision
                                    == entry.buffer.field_revision("point")
                                && !entry.buffer.writable_view_affects("point");
                            SnapshotEntry {
                                shape: ShapeSlot {
                                    tag: entry.shape.tag,
                                    point_revision: hint_was_live
                                        .then(|| buffer.field_revision("point"))
                                        .flatten(),
                                },
                                buffer,
                                placement: entry.placement,
                                placement_revision: entry.placement_revision,
                                submobjects: entry.submobjects.clone(),
                                parents: entry.parents.clone(),
                                updaters: entry.updaters.clone(),
                                updater_states: entry.updater_states.clone(),
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
                                image: entry.image.clone(),
                                image_revision: entry.image_revision,
                                family_cache: entry.family_cache.borrow().clone(),
                            }
                        }),
                    )
                })
                .collect(),
            free: self.free.clone(),
            roots: self.roots.clone(),
        }
    }

    /// Restore a snapshot. Handles minted after the snapshot go stale
    /// (generation discipline); outstanding views detach exactly as under
    /// resize (view-protocol rule V6).
    pub fn restore(&mut self, snapshot: &Snapshot) {
        assert_eq!(
            self.id, snapshot.stage_id,
            "an in-memory snapshot must restore into its logical Stage; \
             use Snapshot::materialize for an independent copy or durable \
             decode to rebind handles"
        );
        self.slots = snapshot
            .slots
            .iter()
            .map(|(generation, entry)| Slot {
                generation: *generation,
                entry: entry.as_ref().map(|e| Entry {
                    buffer: e.buffer.snapshot_clone(),
                    placement: e.placement,
                    placement_revision: e.placement_revision,
                    submobjects: e.submobjects.clone(),
                    children_epoch: fresh_epoch(),
                    parents: e.parents.clone(),
                    updaters: e.updaters.clone(),
                    updater_states: e.updater_states.clone(),
                    updating_suspended: e.updating_suspended,
                    is_animating: e.is_animating,
                    tracker: e.tracker,
                    target: e.target,
                    saved_state: e.saved_state,
                    padding_source: e.padding_source,
                    pins: e.pins,
                    pending_delete: e.pending_delete,
                    uniforms: e.uniforms,
                    z_index: e.z_index,
                    shape: e.shape,
                    render_primitive: e.render_primitive,
                    image: e.image.clone(),
                    image_revision: e.image_revision,
                    family_cache: RefCell::new(e.family_cache.clone()),
                    bbox: RefCell::new(BboxCache::default()),
                }),
            })
            .collect();
        self.free = snapshot.free.clone();
        self.roots = snapshot.roots.clone();
        self.next_updater_id = snapshot.next_updater_id;
        self.touch_topology();
    }
}

/// An identity-preserving checkpoint of one family's content and animation
/// lifecycle flags. Unlike [`Snapshot`], this is not an arena rollback: it
/// retains no roots, clock, allocation state, pins, target/saved-state links,
/// updater registrations, or updater closure state.
///
/// The captured graph is descendant-closed and preserves every shared child.
/// Storage is proportional to that family, with record/image data shared under
/// the ordinary snapshot CoW rules. No frame history or detached mobjects are
/// allocated. Every original handle must remain live when restoring.
pub struct FamilyCheckpoint {
    entries: Vec<(Mob, FamilyCheckpointEntry)>,
}

struct FamilyCheckpointEntry {
    buffer: RecordBuffer,
    placement: Placement,
    submobjects: ArcVec<Mob>,
    updating_suspended: bool,
    is_animating: bool,
    tracker: Option<crate::dynamics::Tracker>,
    uniforms: Uniforms,
    z_index: i32,
    shape: ShapeSlot,
    render_primitive: RenderPrimitive,
    image: Option<ImageResource>,
}

impl Stage {
    /// Capture only `mob`'s current family for a composition's reversible
    /// interpolation. Capturing does not run or copy executable updater state.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] for a dead or foreign root.
    pub fn checkpoint_family(&self, mob: Mob) -> Result<FamilyCheckpoint, StageError> {
        self.try_get(mob)?;
        let entries = self
            .family(mob)
            .into_iter()
            .map(|member| {
                let entry = self.get(member).expect("a live family's member is live");
                let buffer = entry.buffer.snapshot_clone();
                let hint_was_live = entry.shape.point_revision.is_some()
                    && entry.shape.point_revision == entry.buffer.field_revision("point")
                    && !entry.buffer.writable_view_affects("point");
                (
                    member,
                    FamilyCheckpointEntry {
                        shape: ShapeSlot {
                            tag: entry.shape.tag,
                            point_revision: hint_was_live
                                .then(|| buffer.field_revision("point"))
                                .flatten(),
                        },
                        buffer,
                        placement: entry.placement,
                        submobjects: entry.submobjects.clone(),
                        updating_suspended: entry.updating_suspended,
                        is_animating: entry.is_animating,
                        tracker: entry.tracker,
                        uniforms: entry.uniforms,
                        z_index: entry.z_index,
                        render_primitive: entry.render_primitive,
                        image: entry.image.clone(),
                    },
                )
            })
            .collect();
        Ok(FamilyCheckpoint { entries })
    }

    /// Restore a family's captured content in place without changing other
    /// families, scene membership, time, or updater state. Live views detach
    /// from restored records under the same V6 rule as an arena restore.
    ///
    /// Current children absent from the checkpoint are detached, never deleted.
    /// External parents and their sibling edges survive. The snapshot's graph
    /// is descendant-closed, so restoring its own outgoing edges cannot create
    /// a cycle through an external node. Removing changed edges first also
    /// avoids temporary cycles between two different valid family layouts.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] if any original member has been deleted or
    /// the checkpoint belongs to a different Stage. All handles are checked
    /// before any content or topology changes; deleted identities are never
    /// resurrected or rebound to a recycled slot.
    pub fn restore_family_checkpoint(
        &mut self,
        checkpoint: &FamilyCheckpoint,
    ) -> Result<(), StageError> {
        for (member, _) in &checkpoint.entries {
            self.try_get(*member)?;
        }
        let changed: Vec<_> = checkpoint
            .entries
            .iter()
            .filter(|(member, saved)| {
                self.get(*member).expect("prechecked").submobjects != saved.submobjects
            })
            .map(|(member, saved)| (*member, &saved.submobjects))
            .collect();
        for &(member, _) in &changed {
            self.replace_children(member, &[])?;
        }
        for &(member, children) in &changed {
            self.replace_children(member, children)?;
        }
        for (member, saved) in &checkpoint.entries {
            let entry = self.get_mut(*member).expect("prechecked");
            entry.buffer = saved.buffer.snapshot_clone();
            entry.set_placement(saved.placement);
            entry.updating_suspended = saved.updating_suspended;
            entry.is_animating = saved.is_animating;
            entry.tracker = saved.tracker;
            entry.uniforms = saved.uniforms;
            entry.z_index = saved.z_index;
            entry.shape = saved.shape;
            entry.render_primitive = saved.render_primitive;
            entry.replace_image_resource(saved.image.clone());
            *entry.bbox.borrow_mut() = BboxCache::default();
        }
        Ok(())
    }
}

#[path = "surface_grid.rs"]
mod surface_grid;
pub use surface_grid::{
    MAX_SURFACE_GRID_POINTS, SurfaceGridUpdate, prepare_surface_grid_alignment,
};

#[cfg(test)]
mod updater_tests {
    use super::*;
    use std::cell::Cell;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    #[test]
    fn fallible_stateful_updaters_remap_copies_and_keep_snapshot_state_independent() {
        #[derive(Clone)]
        struct Clock {
            tracker: Mob,
            samples: Vec<f64>,
        }

        let mut stage = Stage::new();
        let root = stage.add(Mobject::new());
        let tracker = stage.add_value_tracker(10.0);
        stage.attach(root, tracker).unwrap();
        let updater = stage
            .add_try_dt_updater_with_state(
                root,
                Clock {
                    tracker,
                    samples: Vec::new(),
                },
                |clock, map| clock.tracker = map.get(clock.tracker).unwrap_or(clock.tracker),
                |stage, receiver, id, dt| {
                    if dt > 1.0 {
                        return Err(UpdaterError::callback("owned clock", "step refused"));
                    }
                    let tracker = stage.updater_state::<Clock>(receiver, id).unwrap().tracker;
                    stage.increment_tracker_value(tracker, dt)?;
                    stage
                        .updater_state_mut::<Clock>(receiver, id)
                        .unwrap()
                        .samples
                        .push(dt);
                    Ok(())
                },
                true,
            )
            .unwrap();
        assert_eq!(
            stage.updater_state::<Clock>(root, updater).unwrap().samples,
            [0.0]
        );
        let map = stage.copy_family_mapped(root).unwrap();
        let copied = map.get(root).unwrap();
        let copied_tracker = map.get(tracker).unwrap();
        let snapshot = stage.snapshot();

        stage.update_mobject(copied, 0.25);
        assert_eq!(stage.tracker_value(tracker), Some(10.0));
        assert_eq!(stage.tracker_value(copied_tracker), Some(10.25));
        assert_eq!(
            stage.updater_state::<Clock>(root, updater).unwrap().samples,
            [0.0]
        );
        assert_eq!(
            stage
                .updater_state::<Clock>(copied, updater)
                .unwrap()
                .samples,
            [0.0, 0.25]
        );

        let mut independent = snapshot.materialize();
        independent.update_mobject(copied, 0.5);
        assert_eq!(independent.tracker_value(copied_tracker), Some(10.5));
        assert_eq!(stage.tracker_value(copied_tracker), Some(10.25));
        assert_eq!(
            independent
                .updater_state::<Clock>(copied, updater)
                .unwrap()
                .samples,
            [0.0, 0.5]
        );

        stage.update_mobject(copied, 2.0);
        let failure = stage.updater_error().unwrap();
        assert_eq!(failure.mobject, copied);
        assert_eq!(failure.updater, updater);
        assert_eq!(stage.tracker_value(copied_tracker), Some(10.25));
        assert_eq!(
            stage
                .updater_state::<Clock>(copied, updater)
                .unwrap()
                .samples,
            [0.0, 0.25]
        );
        assert!(independent.updater_error().is_none());
    }

    #[test]
    fn fallible_stateful_immediate_failure_removes_only_its_callable() {
        let mut stage = Stage::new();
        let mob = stage.add_value_tracker(0.0);
        let older = stage
            .add_updater(
                mob,
                |stage, me| stage.increment_tracker_value(me, 1.0).unwrap(),
                false,
            )
            .unwrap();
        let id = UpdaterId(stage.next_updater_id);
        let error = UpdaterError::callback("owned state", "initial sample refused");
        let callback_error = error.clone();
        let result = stage.add_try_dt_updater_with_state(
            mob,
            vec![4usize],
            |_, _| {},
            move |stage, receiver, id, dt| {
                assert_eq!(dt, 0.0);
                stage
                    .updater_state_mut::<Vec<usize>>(receiver, id)
                    .unwrap()
                    .push(5);
                Err(callback_error.clone())
            },
            true,
        );
        assert_eq!(result, Err(error.clone()));
        assert_eq!(
            stage.tracker_value(mob),
            Some(1.0),
            "one immediate family pass"
        );
        assert_eq!(stage.updater_ids(mob), [older]);
        assert_eq!(stage.updater_state::<Vec<usize>>(mob, id).unwrap(), &[4, 5]);
        assert_eq!(stage.updater_error().unwrap().updater, id);
        assert_eq!(stage.updater_error().unwrap().error, error);

        let next = stage.next_updater_id;
        let result = stage.add_try_dt_updater_with_state(
            mob,
            0usize,
            |_, _| {},
            |_, _, _, _| panic!("a pending error must prevent registration"),
            true,
        );
        assert!(result.is_err());
        assert_eq!(stage.next_updater_id, next);
        assert_eq!(stage.updater_ids(mob), [older]);
        assert!(stage.updater_state::<usize>(mob, UpdaterId(next)).is_none());
    }

    #[test]
    fn fallible_updaters_stop_the_whole_ordered_pass_until_acknowledged() {
        let mut stage = Stage::new();
        let parent = stage.add(Mobject::new());
        let first = stage.add(Mobject::new());
        let second = stage.add(Mobject::new());
        let later_root = stage.add(Mobject::new());
        stage.attach(parent, first).unwrap();
        stage.attach(parent, second).unwrap();
        stage.add_many_to_scene(&[parent, later_root]).unwrap();
        let log = Rc::new(RefCell::new(Vec::new()));
        let times = Rc::new(RefCell::new(Vec::new()));
        let callback_log = Rc::clone(&log);
        let callback_times = Rc::clone(&times);
        stage
            .add_dt_updater(
                first,
                move |stage, _, dt| {
                    callback_log.borrow_mut().push("first dt");
                    callback_times.borrow_mut().push((stage.time(), dt));
                },
                false,
            )
            .unwrap();
        let callback_log = Rc::clone(&log);
        let expected = UpdaterError::callback("geometry", "sample refused");
        let callback_error = expected.clone();
        let failing = stage
            .add_try_updater(
                first,
                move |_, _| {
                    callback_log.borrow_mut().push("first failure");
                    Err(callback_error.clone())
                },
                false,
            )
            .unwrap();
        for (mob, label) in [
            (first, "first tail"),
            (second, "second"),
            (parent, "parent"),
            (later_root, "later root"),
        ] {
            let callback_log = Rc::clone(&log);
            stage
                .add_updater(
                    mob,
                    move |_, _| callback_log.borrow_mut().push(label),
                    false,
                )
                .unwrap();
        }

        stage.update(0.25);

        assert_eq!(*log.borrow(), vec!["first dt", "first failure"]);
        assert_eq!(*times.borrow(), vec![(0.25, 0.25)]);
        assert_eq!(
            stage.updater_error(),
            Some(&UpdaterFailure {
                mobject: first,
                updater: failing,
                error: expected,
            })
        );
        stage.suspend_updating(first, false);
        stage.resume_updating(first, false, true);
        assert!(!stage.get(first).unwrap().updating_suspended);
        stage.update(1.0);
        stage.update_at_time(1.0, 9.0);
        stage.update_mobject_with_recurse(later_root, 1.0, false);
        assert_eq!(stage.time(), 0.25);
        assert_eq!(*log.borrow(), vec!["first dt", "first failure"]);

        stage.remove_updater(first, failing);
        assert!(stage.take_updater_error().unwrap().is_some());
        log.borrow_mut().clear();
        stage.update(0.5);

        assert!(stage.updater_error().is_none());
        assert_eq!(
            *log.borrow(),
            vec!["first dt", "first tail", "second", "parent", "later root"]
        );
        assert_eq!(*times.borrow(), vec![(0.25, 0.25), (0.75, 0.5)]);
    }

    #[test]
    fn immediate_failure_removes_only_the_new_slot_and_preserves_user_mutations() {
        let mut stage = Stage::new();
        let mob = stage.add_value_tracker(0.0);
        let older = stage
            .add_updater(
                mob,
                |stage, me| stage.increment_tracker_value(me, 1.0).unwrap(),
                false,
            )
            .unwrap();
        let next = stage.next_updater_id;
        let expected = UpdaterError::callback("number rebuild", "invalid text");
        let callback_error = expected.clone();

        let result = stage.add_try_dt_updater(
            mob,
            move |stage, me, dt| {
                assert_eq!(dt, 0.0);
                stage.set_tracker_value(me, 42.0)?;
                Err(callback_error.clone())
            },
            true,
        );

        assert_eq!(result, Err(expected.clone()));
        assert_eq!(stage.tracker_value(mob), Some(42.0));
        assert_eq!(stage.time(), 0.0);
        assert_eq!(stage.updater_ids(mob), vec![older]);
        assert_eq!(stage.next_updater_id, next + 1);
        assert_eq!(
            stage.updater_error(),
            Some(&UpdaterFailure {
                mobject: mob,
                updater: UpdaterId(next),
                error: expected,
            })
        );
        stage.take_updater_error().unwrap().unwrap();
        let replacement = stage.add_try_updater(mob, |_, _| Ok(()), false).unwrap();
        assert_eq!(replacement.raw(), next + 1);
    }

    #[test]
    fn immediate_pass_retains_an_older_failure_and_never_invokes_the_new_callback() {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::new());
        let expected = UpdaterError::callback("older updater", "first failure");
        let callback_error = expected.clone();
        let older = stage
            .add_try_updater(mob, move |_, _| Err(callback_error.clone()), false)
            .unwrap();
        let calls = Rc::new(Cell::new(0));
        let callback_calls = Rc::clone(&calls);

        let result = stage.add_try_updater(
            mob,
            move |_, _| {
                callback_calls.set(callback_calls.get() + 1);
                Ok(())
            },
            true,
        );

        assert_eq!(result, Err(expected));
        assert_eq!(calls.get(), 0);
        assert_eq!(stage.updater_ids(mob), vec![older]);
        assert_eq!(stage.updater_error().unwrap().updater, older);
    }

    #[test]
    fn updater_failure_is_physical_execution_state_outside_snapshots() {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::new());
        let fail = Rc::new(Cell::new(true));
        let calls = Rc::new(Cell::new(0));
        let callback_fail = Rc::clone(&fail);
        let callback_calls = Rc::clone(&calls);
        let updater = stage
            .add_try_updater(
                mob,
                move |_, _| {
                    callback_calls.set(callback_calls.get() + 1);
                    if callback_fail.get() {
                        Err(UpdaterError::callback("snapshot callback", "retry later"))
                    } else {
                        Ok(())
                    }
                },
                false,
            )
            .unwrap();
        let original = stage.snapshot();
        let bytes = stage.snapshot_bytes().unwrap();

        stage.update_mobject(mob, 0.1);

        let failure = stage.updater_error().unwrap().clone();
        let failed_snapshot = stage.snapshot();
        let render_copy = stage.freeze_render_state().materialize();
        assert!(render_copy.updater_error().is_none());
        assert!(render_copy.updater_ids(mob).is_empty());
        assert_eq!(stage.snapshot_bytes().unwrap(), bytes);
        stage.remove_updater(mob, updater);
        stage.restore(&original);
        assert_eq!(stage.updater_error(), Some(&failure));
        assert_eq!(stage.updater_ids(mob), vec![updater]);

        let mut independent = failed_snapshot.materialize();
        assert!(independent.updater_error().is_none());
        fail.set(false);
        independent.update_mobject(mob, 0.1);
        assert_eq!(calls.get(), 2);
        assert!(independent.updater_error().is_none());
        stage.update_mobject(mob, 0.1);
        assert_eq!(calls.get(), 2);
        assert_eq!(stage.take_updater_error().unwrap(), Some(failure));
        stage.update_mobject(mob, 0.1);
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn copied_fallible_callbacks_retain_identity_and_report_the_actual_receiver() {
        let mut stage = Stage::new();
        let original = stage.add_value_tracker(1.0);
        let updater = stage
            .add_try_dt_updater(
                original,
                |stage, me, dt| {
                    stage.increment_tracker_value(me, dt)?;
                    Err(UpdaterError::callback("copied updater", "sample refused"))
                },
                false,
            )
            .unwrap();
        let copy = stage.copy_family(original).unwrap();

        stage.update_mobject(copy, 0.5);

        let failure = stage.updater_error().unwrap();
        assert_eq!(failure.mobject, copy);
        assert_eq!(failure.updater, updater);
        assert_eq!(stage.updater_ids(copy), vec![updater]);
        assert_eq!(stage.tracker_value(copy), Some(1.5));
        assert_eq!(stage.tracker_value(original), Some(1.0));
    }

    #[test]
    fn nested_failures_cannot_be_acknowledged_or_replaced_inside_callbacks() {
        let mut stage = Stage::new();
        let inner = stage.add(Mobject::new());
        let outer = stage.add(Mobject::new());
        let expected = UpdaterError::callback("inner updater", "first error");
        let callback_error = expected.clone();
        let inner_updater = stage
            .add_try_updater(inner, move |_, _| Err(callback_error.clone()), false)
            .unwrap();
        let tail_calls = Rc::new(Cell::new(0));
        stage
            .add_try_updater(
                outer,
                move |stage, _| {
                    stage.update_mobject(inner, 0.0);
                    assert!(stage.take_updater_error().is_err());
                    Err(UpdaterError::callback("outer updater", "later error"))
                },
                false,
            )
            .unwrap();
        let callback_calls = Rc::clone(&tail_calls);
        stage
            .add_updater(
                outer,
                move |_, _| callback_calls.set(callback_calls.get() + 1),
                false,
            )
            .unwrap();

        stage.update_mobject(outer, 0.0);

        assert_eq!(tail_calls.get(), 0);
        assert_eq!(
            stage.take_updater_error().unwrap(),
            Some(UpdaterFailure {
                mobject: inner,
                updater: inner_updater,
                error: expected.clone(),
            })
        );
        let result = stage.invoke_updater_factory(|stage| {
            stage.update_mobject(inner, 0.0);
            assert!(stage.take_updater_error().is_err());
            Ok(42)
        });
        assert_eq!(result, Err(expected));
        assert_eq!(stage.updater_error().unwrap().updater, inner_updater);
        assert!(stage.take_updater_error().unwrap().is_some());
    }

    #[test]
    fn recursive_callback_borrow_is_a_retained_error_instead_of_a_refcell_panic() {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::new());
        let calls = Rc::new(Cell::new(0));
        let callback_calls = Rc::clone(&calls);
        let updater = stage
            .add_try_updater(
                mob,
                move |stage, me| {
                    callback_calls.set(callback_calls.get() + 1);
                    stage.update_mobject(me, 0.0);
                    Ok(())
                },
                false,
            )
            .unwrap();

        stage.update_mobject(mob, 0.1);

        assert_eq!(calls.get(), 1);
        assert_eq!(
            stage.updater_error(),
            Some(&UpdaterFailure {
                mobject: mob,
                updater,
                error: UpdaterError::callback(
                    "Stage::update_mobject",
                    "updater callback is already running",
                ),
            })
        );
    }

    #[test]
    fn callback_guard_unwinds_after_a_user_panic() {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::new());
        let panicking = stage
            .add_updater(mob, |_, _| panic!("user callback panic"), false)
            .unwrap();

        let caught = catch_unwind(AssertUnwindSafe(|| stage.update_mobject(mob, 0.0)));

        assert!(caught.is_err());
        stage.remove_updater(mob, panicking);
        assert!(stage.take_updater_error().unwrap().is_none());
        stage
            .add_try_updater(
                mob,
                |stage, _| {
                    assert!(stage.take_updater_error().is_err());
                    Ok(())
                },
                true,
            )
            .unwrap();
        assert!(stage.take_updater_error().unwrap().is_none());
    }

    #[test]
    fn fallible_registration_preflights_target_capacity_and_pending_failure() {
        let mut stage = Stage::new();
        let stale = stage.add(Mobject::new());
        stage.delete(stale).unwrap();
        let mob = stage.add(Mobject::new());
        let before = stage.snapshot_bytes().unwrap();
        assert_eq!(
            stage.add_try_updater(stale, |_, _| panic!("stale callback must not run"), true),
            Err(UpdaterError::Stage(StageError::StaleHandle))
        );
        assert_eq!(stage.snapshot_bytes().unwrap(), before);

        let next = stage.next_updater_id;
        stage.next_updater_id = u64::MAX;
        let before = stage.snapshot_bytes().unwrap();
        assert_eq!(
            stage.add_try_dt_updater(
                mob,
                |_, _, _| panic!("exhausted callback must not run"),
                true
            ),
            Err(UpdaterError::Stage(StageError::UpdaterIdExhausted))
        );
        assert_eq!(stage.snapshot_bytes().unwrap(), before);
        assert!(stage.updater_error().is_none());

        stage.next_updater_id = next;
        let expected = UpdaterError::callback("existing updater", "repair first");
        let callback_error = expected.clone();
        stage
            .add_try_updater(mob, move |_, _| Err(callback_error.clone()), false)
            .unwrap();
        stage.update_mobject(mob, 0.0);
        let failure = stage.updater_error().unwrap().clone();
        let before = stage.snapshot_bytes().unwrap();
        assert_eq!(
            stage.add_try_updater(mob, |_, _| panic!("pending callback must not run"), true),
            Err(expected)
        );
        assert_eq!(stage.snapshot_bytes().unwrap(), before);
        assert_eq!(stage.updater_error(), Some(&failure));
    }

    #[test]
    fn a_deleted_receiver_is_refused_before_the_fallible_callback_runs() {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::new());
        stage
            .add_updater(mob, |stage, me| stage.delete(me).unwrap(), false)
            .unwrap();
        let updater = stage
            .add_try_updater(
                mob,
                |_, _| panic!("deleted receiver must not reach user code"),
                false,
            )
            .unwrap();

        stage.update_mobject(mob, 0.1);

        assert!(!stage.contains(mob));
        assert_eq!(
            stage.updater_error(),
            Some(&UpdaterFailure {
                mobject: mob,
                updater,
                error: UpdaterError::Stage(StageError::StaleHandle),
            })
        );
    }
}
