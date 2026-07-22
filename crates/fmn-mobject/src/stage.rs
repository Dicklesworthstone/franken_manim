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

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::StageError;
use crate::bbox::BboxCache;
use crate::mobject::Mobject;
use crate::record::RecordBuffer;
use crate::uniforms::Uniforms;

static NEXT_STAGE_ID: AtomicU64 = AtomicU64::new(1);

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

/// One registered updater: identity plus callable.
#[derive(Clone)]
pub struct UpdaterSlot {
    /// The removal token.
    pub id: UpdaterId,
    /// The callable, by kind.
    pub func: UpdaterFn,
}

/// Arena entry: record data plus graph edges and lifetime state. Edges are
/// private so every structural mutation flows through [`Stage`] and the
/// family cache invalidates correctly.
pub struct Entry {
    /// The per-object record data (fm-cus layers the full RecordBuffer
    /// surface onto this).
    pub buffer: RecordBuffer,
    submobjects: Vec<Mob>,
    parents: Vec<Mob>,
    updaters: Vec<UpdaterSlot>,
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
    pins: usize,
    pending_delete: bool,
    /// Per-object typed uniform state (§8.4): scene code reads and writes this
    /// directly; Lumen's StyleTable synchronizes from it.
    uniforms: Uniforms,
    /// Cached family flattening (§1.1 API surface), invalidated on any
    /// structural change in the subtree.
    family_cache: RefCell<Option<Vec<Mob>>>,
    /// Lazily recomputed bounding box, keyed by a subtree signature so any
    /// point write or structural change invalidates it automatically, through
    /// any channel (fm-jru).
    bbox: RefCell<BboxCache>,
}

impl Entry {
    fn from_data(buffer: RecordBuffer) -> Self {
        Self {
            buffer,
            submobjects: Vec::new(),
            parents: Vec::new(),
            updaters: Vec::new(),
            updating_suspended: false,
            is_animating: false,
            tracker: None,
            pins: 0,
            pending_delete: false,
            uniforms: Uniforms::default(),
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

    pub(crate) fn bbox_cell(&self) -> &RefCell<BboxCache> {
        &self.bbox
    }

    /// Direct children, in insertion order.
    #[must_use]
    pub fn submobjects(&self) -> &[Mob] {
        &self.submobjects
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
}

struct Slot {
    generation: u32,
    entry: Option<Entry>,
}

/// A CoW snapshot of the whole stage: begin-states for §9.5's frame-parallel
/// pure segments, Studio undo, and replay barriers.
pub struct Snapshot {
    slots: Vec<(u32, Option<SnapshotEntry>)>,
    free: Vec<u32>,
    roots: Vec<Mob>,
}

struct SnapshotEntry {
    buffer: RecordBuffer,
    submobjects: Vec<Mob>,
    parents: Vec<Mob>,
    updaters: Vec<UpdaterSlot>,
    updating_suspended: bool,
    is_animating: bool,
    tracker: Option<crate::dynamics::Tracker>,
    pins: usize,
    pending_delete: bool,
    uniforms: Uniforms,
}

/// The arena.
pub struct Stage {
    id: u64,
    slots: Vec<Slot>,
    free: Vec<u32>,
    roots: Vec<Mob>,
    time: f64,
    next_updater_id: u64,
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
            free: Vec::new(),
            roots: Vec::new(),
            time: 0.0,
            next_updater_id: 1,
        }
    }

    /// Scene time advanced by [`Stage::update`]. (The RationalFrameClock
    /// replaces this float accumulator at the Choreo boundary — fm-wuq;
    /// nothing here depends on its precision.)
    #[must_use]
    pub fn time(&self) -> f64 {
        self.time
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
            submobjects,
        } = mobject.into();
        let mob = self.alloc(Entry::from_data(buffer));
        for child in submobjects {
            let child_mob = self.add(child);
            self.attach(mob, child_mob)
                .expect("freshly allocated handles are live and acyclic");
        }
        mob
    }

    /// Add to the scene's draw list (idempotent). Rooting is membership,
    /// not ownership.
    pub fn add_to_scene(&mut self, mob: Mob) -> Result<(), StageError> {
        if !self.contains(mob) {
            return Err(StageError::StaleHandle);
        }
        if !self.roots.contains(&mob) {
            self.roots.push(mob);
        }
        Ok(())
    }

    /// Remove from the draw list only; the entry and every handle stay
    /// valid (rooted-lifetime rule).
    pub fn remove_from_scene(&mut self, mob: Mob) {
        self.roots.retain(|m| *m != mob);
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
        if parent == child || self.family(child).contains(&parent) {
            return Err(StageError::CycleDetected);
        }
        {
            let entry = self.get_mut(parent).expect("checked above");
            if entry.submobjects.contains(&child) {
                return Ok(());
            }
            entry.submobjects.push(child);
        }
        {
            let entry = self.get_mut(child).expect("checked above");
            if !entry.parents.contains(&parent) {
                entry.parents.push(parent);
            }
        }
        self.invalidate_family_caches(parent);
        Ok(())
    }

    /// Detach `child` from `parent`; the entry stays alive.
    pub fn detach(&mut self, parent: Mob, child: Mob) {
        if let Some(entry) = self.get_mut(parent) {
            entry.submobjects.retain(|m| *m != child);
        }
        if let Some(entry) = self.get_mut(child) {
            entry.parents.retain(|m| *m != parent);
        }
        self.invalidate_family_caches(parent);
    }

    /// The family under `mob` in depth-first order, each member exactly
    /// once even through diamond composition. Cached per entry; any
    /// structural change in the subtree invalidates every ancestor's cache.
    #[must_use]
    pub fn family(&self, mob: Mob) -> Vec<Mob> {
        let Some(entry) = self.get(mob) else {
            return Vec::new();
        };
        if let Some(cached) = entry.family_cache.borrow().as_ref() {
            return cached.clone();
        }
        let mut out = Vec::new();
        let mut stack = vec![mob];
        while let Some(current) = stack.pop() {
            if out.contains(&current) {
                continue;
            }
            if let Some(e) = self.get(current) {
                out.push(current);
                for &child in e.submobjects.iter().rev() {
                    stack.push(child);
                }
            }
        }
        *entry.family_cache.borrow_mut() = Some(out.clone());
        out
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
    pub fn pin(&mut self, mob: Mob) -> Result<(), StageError> {
        let entry = self.get_mut(mob).ok_or(StageError::StaleHandle)?;
        entry.pins += 1;
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
        for parent in parents {
            if let Some(p) = self.get_mut(parent) {
                p.submobjects.retain(|m| *m != mob);
            }
        }
        for child in children {
            if let Some(c) = self.get_mut(child) {
                c.parents.retain(|m| *m != mob);
            }
        }
        self.remove_from_scene(mob);
        let slot = &mut self.slots[mob.index as usize];
        slot.entry = None;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(mob.index);
    }

    // --------------------------------------------------------------- copy

    /// manim `copy()` (§8.3): deep-copy the family subtree; family-internal
    /// references remap; family-external edges drop (the copy is a detached
    /// family); updater callables shared by reference; record data
    /// independent.
    pub fn copy_family(&mut self, mob: Mob) -> Result<Mob, StageError> {
        if !self.contains(mob) {
            return Err(StageError::StaleHandle);
        }
        let family = self.family(mob);
        let mut map: HashMap<Mob, Mob> = HashMap::new();
        for &old in &family {
            let entry = self.get(old).expect("family members resolve");
            let new_entry = Entry {
                buffer: entry.buffer.deep_clone(),
                submobjects: entry.submobjects.clone(), // remapped below
                parents: entry.parents.clone(),         // remapped below
                updaters: entry.updaters.clone(),       // by reference
                updating_suspended: entry.updating_suspended,
                is_animating: entry.is_animating,
                tracker: entry.tracker,
                pins: 0,
                pending_delete: false,
                uniforms: entry.uniforms, // copy semantics: independent state
                family_cache: RefCell::new(None),
                bbox: RefCell::new(BboxCache::default()),
            };
            let new = self.alloc(new_entry);
            map.insert(old, new);
        }
        for new in map.values() {
            let entry = self.get_mut(*new).expect("just allocated");
            for edges in [&mut entry.submobjects, &mut entry.parents] {
                let mut seen: Vec<Mob> = Vec::new();
                edges.retain_mut(|m| match map.get(m) {
                    Some(mapped) => {
                        *m = *mapped;
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
        }
        Ok(map[&mob])
    }

    /// Cross-stage transfer under the two-scene policy: content moves by
    /// copy, never by handle.
    pub fn copy_into(&self, mob: Mob, target: &mut Stage) -> Result<Mob, StageError> {
        let entry = self.try_get(mob)?;
        let children = entry.submobjects.clone();
        let new = target.alloc(Entry::from_data(entry.buffer.deep_clone()));
        for child in children {
            let new_child = self.copy_into(child, target)?;
            target
                .attach(new, new_child)
                .expect("fresh handles are live and acyclic");
        }
        Ok(new)
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

    fn register_updater(
        &mut self,
        mob: Mob,
        func: UpdaterFn,
        index: Option<usize>,
        call: bool,
    ) -> Result<UpdaterId, StageError> {
        let id = UpdaterId(self.next_updater_id);
        self.next_updater_id += 1;
        let entry = self.get_mut(mob).ok_or(StageError::StaleHandle)?;
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
    /// callables shared by reference).
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] if either handle is dead.
    pub fn match_updaters(&mut self, mob: Mob, source: Mob) -> Result<(), StageError> {
        let updaters = self.try_get(source)?.updaters.clone();
        let entry = self.get_mut(mob).ok_or(StageError::StaleHandle)?;
        entry.updaters = updaters;
        Ok(())
    }

    /// The registered updater ids on `mob`, in execution order.
    #[must_use]
    pub fn updater_ids(&self, mob: Mob) -> Vec<UpdaterId> {
        self.get(mob)
            .map(|e| e.updaters.iter().map(|s| s.id).collect())
            .unwrap_or_default()
    }

    /// Whether `mob` or anything in its family has updaters.
    #[must_use]
    pub fn has_updaters_in_family(&self, mob: Mob) -> bool {
        self.family(mob)
            .iter()
            .any(|m| self.get(*m).is_some_and(|e| !e.updaters.is_empty()))
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
            pending.extend(
                self.get(parent)
                    .map(|e| e.parents().to_vec())
                    .unwrap_or_default(),
            );
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
        self.update_mob(mob, dt);
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
            pending.extend(
                self.get(parent)
                    .map(|e| e.parents().to_vec())
                    .unwrap_or_default(),
            );
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
        let Some(entry) = self.get(mob) else {
            return;
        };
        if entry.updating_suspended || !self.has_updaters_in_family(mob) {
            return;
        }
        let children = self
            .get(mob)
            .map(|e| e.submobjects().to_vec())
            .unwrap_or_default();
        for child in children {
            self.update_mob(child, dt);
        }
        let updaters = self
            .get(mob)
            .map(|e| e.updaters.clone())
            .unwrap_or_default();
        for slot in updaters {
            match slot.func {
                UpdaterFn::NonDt(f) => f.borrow_mut()(self, mob),
                UpdaterFn::Dt(f) => f.borrow_mut()(self, mob, dt),
            }
        }
    }

    /// Run every rooted family's updaters (child-first, insertion order,
    /// suspension-pruned), then advance time. (The six-step frame order is
    /// Choreo's; the arena provides the execution slot.)
    pub fn update(&mut self, dt: f64) {
        for root in self.roots.clone() {
            self.update_mob(root, dt);
        }
        self.time += dt;
    }

    // ---------------------------------------------------------- snapshots

    /// CoW snapshot of the whole stage: record storages share until someone
    /// writes; entries with live views copy eagerly (view-protocol rule
    /// V5). Cost is O(touched + live-viewed), verified by test.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            slots: self
                .slots
                .iter()
                .map(|slot| {
                    (
                        slot.generation,
                        slot.entry.as_ref().map(|entry| SnapshotEntry {
                            buffer: entry.buffer.snapshot_clone(),
                            submobjects: entry.submobjects.clone(),
                            parents: entry.parents.clone(),
                            updaters: entry.updaters.clone(),
                            updating_suspended: entry.updating_suspended,
                            is_animating: entry.is_animating,
                            tracker: entry.tracker,
                            pins: entry.pins,
                            pending_delete: entry.pending_delete,
                            uniforms: entry.uniforms,
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
        self.slots = snapshot
            .slots
            .iter()
            .map(|(generation, entry)| Slot {
                generation: *generation,
                entry: entry.as_ref().map(|e| Entry {
                    buffer: e.buffer.snapshot_clone(),
                    submobjects: e.submobjects.clone(),
                    parents: e.parents.clone(),
                    updaters: e.updaters.clone(),
                    updating_suspended: e.updating_suspended,
                    is_animating: e.is_animating,
                    tracker: e.tracker,
                    pins: e.pins,
                    pending_delete: e.pending_delete,
                    uniforms: e.uniforms,
                    family_cache: RefCell::new(None),
                    bbox: RefCell::new(BboxCache::default()),
                }),
            })
            .collect();
        self.free = snapshot.free.clone();
        self.roots = snapshot.roots.clone();
    }
}
