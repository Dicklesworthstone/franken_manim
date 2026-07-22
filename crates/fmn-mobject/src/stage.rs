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
use crate::mobject::Mobject;
use crate::record::RecordBuffer;

static NEXT_STAGE_ID: AtomicU64 = AtomicU64::new(1);

/// Generational, stage-scoped, `Copy` mobject handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mob {
    stage_id: u64,
    index: u32,
    generation: u32,
}

/// An updater closure: receives the stage, its own handle, and `dt`.
/// `Rc` because manim's `copy()` keeps updater callables by reference.
pub type Updater = Rc<RefCell<dyn FnMut(&mut Stage, Mob, f64)>>;

/// Arena entry: record data plus graph edges and lifetime state. Edges are
/// private so every structural mutation flows through [`Stage`] and the
/// family cache invalidates correctly.
pub struct Entry {
    /// The per-object record data (fm-cus layers the full RecordBuffer
    /// surface onto this).
    pub buffer: RecordBuffer,
    submobjects: Vec<Mob>,
    parents: Vec<Mob>,
    updaters: Vec<Updater>,
    pins: usize,
    pending_delete: bool,
    /// Cached family flattening (§1.1 API surface), invalidated on any
    /// structural change in the subtree.
    family_cache: RefCell<Option<Vec<Mob>>>,
}

impl Entry {
    fn from_data(buffer: RecordBuffer) -> Self {
        Self {
            buffer,
            submobjects: Vec::new(),
            parents: Vec::new(),
            updaters: Vec::new(),
            pins: 0,
            pending_delete: false,
            family_cache: RefCell::new(None),
        }
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
    updaters: Vec<Updater>,
    pins: usize,
    pending_delete: bool,
}

/// The arena.
pub struct Stage {
    id: u64,
    slots: Vec<Slot>,
    free: Vec<u32>,
    roots: Vec<Mob>,
    time: f64,
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
                pins: 0,
                pending_delete: false,
                family_cache: RefCell::new(None),
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

    /// Insertion-ordered updater registration. `call_now` runs the updater
    /// exactly once, immediately (the Reference's double-call is a bug we
    /// fix — Behavior Note, finalized with fm-yra).
    pub fn add_updater(
        &mut self,
        mob: Mob,
        updater: impl FnMut(&mut Stage, Mob, f64) + 'static,
        call_now: bool,
    ) -> Result<(), StageError> {
        let updater: Updater = Rc::new(RefCell::new(updater));
        let entry = self.get_mut(mob).ok_or(StageError::StaleHandle)?;
        entry.updaters.push(Rc::clone(&updater));
        if call_now {
            updater.borrow_mut()(self, mob, 0.0);
        }
        Ok(())
    }

    /// Run every rooted family's updaters in insertion order, then advance
    /// time. (The six-step frame order is Choreo's; the arena provides only
    /// the execution slot.)
    pub fn update(&mut self, dt: f64) {
        let targets: Vec<Mob> = self
            .roots
            .clone()
            .into_iter()
            .flat_map(|root| self.family(root))
            .collect();
        for target in targets {
            let updaters = match self.get(target) {
                Some(entry) => entry.updaters.clone(),
                None => continue,
            };
            for updater in updaters {
                updater.borrow_mut()(self, target, dt);
            }
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
                            pins: entry.pins,
                            pending_delete: entry.pending_delete,
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
                    pins: e.pins,
                    pending_delete: e.pending_delete,
                    family_cache: RefCell::new(None),
                }),
            })
            .collect();
        self.free = snapshot.free.clone();
        self.roots = snapshot.roots.clone();
    }
}
