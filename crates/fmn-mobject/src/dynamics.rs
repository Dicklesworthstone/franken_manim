//! ValueTrackers, `always_redraw`/`f_always` closure binding, and the C-6
//! group-addition correction (§8.6, fm-yra).
//!
//! # ValueTrackers
//!
//! The Reference's `ValueTracker` is "a mobject not meant to be displayed":
//! the value rides the mobject machinery so it can be animated like anything
//! else. It stores the encoded value in `uniforms["value"]` as float64 /
//! complex128 — full double precision, deliberately *not* the f32 record
//! plane. Here that is the entry's typed [`Tracker`] state ([`TrackerKind`]
//! plus up to two f64 lanes), with the same encodings:
//!
//! - [`TrackerKind::Plain`] — one lane, the value itself;
//! - [`TrackerKind::Exponential`] — one lane holding `ln(value)`, so
//!   interpolation of the lane is geometric interpolation of the value
//!   (`ExponentialValueTracker`);
//! - [`TrackerKind::Complex`] — two lanes, re/im (`ComplexValueTracker`).
//!
//! `increment` is `set(get + d)` through the encoding, exactly as the
//! Reference implements it (so incrementing an exponential tracker
//! multiplies the stored exponential's argument correctly).
//!
//! # `always_redraw` / `f_always`
//!
//! `always_redraw(f)` binds a rebuild closure into the clock: every
//! [`Stage::update`] tick replaces the returned mobject's content with a
//! fresh `f()` result. The returned handle is a stable container whose
//! children are swapped per tick, allowing the rebuilt family to change
//! shape. Replaced private content is reclaimed after the new family is
//! installed; shared, rooted, and reused content stays alive. `f_always` is the
//! named-parity form of a non-dt updater: in Rust a closure already closes
//! over its argument generators.
//!
//! # C-6: group addition is a value operation
//!
//! The Reference's `Mobject.__add__` builds a NEW group from its operands,
//! but `Group.__add__` mutates the left operand in place and returns it —
//! two different semantics under one operator (Appendix C, C-6).
//! [`Stage::group_add`] always builds a new group (Behavior Note
//! BN-07-updater-and-group-fixes).

use crate::StageError;
use crate::mobject::Mobject;
use crate::stage::{IdBuildHasher, Mob, Stage, UpdaterError, UpdaterId};
use std::collections::HashSet;

/// Which tracker encoding a mobject carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrackerKind {
    /// `ValueTracker`: one lane, value stored directly.
    Plain,
    /// `ExponentialValueTracker`: one lane, `ln(value)` stored.
    Exponential,
    /// `ComplexValueTracker`: two lanes, re/im.
    Complex,
}

/// ValueTracker state: the encoding kind plus the stored f64 lanes.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Tracker {
    /// The encoding.
    pub kind: TrackerKind,
    /// Stored lanes (`[value, 0]`, `[ln(value), 0]`, or `[re, im]`).
    pub lanes: [f64; 2],
}

impl Stage {
    // ----------------------------------------------------------- trackers

    fn add_tracker(&mut self, kind: TrackerKind, lanes: [f64; 2]) -> Mob {
        let mob = self.add(Mobject::new());
        if let Some(entry) = self.get_mut(mob) {
            entry.tracker = Some(Tracker { kind, lanes });
        }
        mob
    }

    /// A `ValueTracker` holding `value`.
    pub fn add_value_tracker(&mut self, value: f64) -> Mob {
        self.add_tracker(TrackerKind::Plain, [value, 0.0])
    }

    /// An `ExponentialValueTracker` holding `value` (stored as `ln(value)`,
    /// so lane interpolation is geometric).
    pub fn add_exponential_value_tracker(&mut self, value: f64) -> Mob {
        self.add_tracker(TrackerKind::Exponential, [fmn_dmath::ln(value), 0.0])
    }

    /// A `ComplexValueTracker` holding `re + im·i`.
    pub fn add_complex_value_tracker(&mut self, re: f64, im: f64) -> Mob {
        self.add_tracker(TrackerKind::Complex, [re, im])
    }

    /// The raw tracker state, if `mob` is a tracker.
    #[must_use]
    pub fn tracker(&self, mob: Mob) -> Option<Tracker> {
        self.get(mob).and_then(|e| e.tracker)
    }

    /// Replace the raw typed tracker payload attached to `mob`.
    ///
    /// This is the state-transfer seam used by binding-tier detached-copy
    /// restoration and by Choreo's Transform interpolation.  Ordinary users
    /// should prefer the decoded scalar/complex accessors below; this method
    /// deliberately preserves the encoded lane representation (notably the
    /// logarithmic lane of [`TrackerKind::Exponential`]).
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] if `mob` is not live.
    pub fn set_tracker_state(
        &mut self,
        mob: Mob,
        tracker: Option<Tracker>,
    ) -> Result<(), StageError> {
        self.get_mut(mob).ok_or(StageError::StaleHandle)?.tracker = tracker;
        Ok(())
    }

    /// `get_value` for scalar trackers (`Plain` decodes directly,
    /// `Exponential` decodes through `exp`); `None` for non-trackers and
    /// complex trackers (use [`Stage::tracker_complex_value`]).
    #[must_use]
    pub fn tracker_value(&self, mob: Mob) -> Option<f64> {
        match self.tracker(mob)? {
            Tracker {
                kind: TrackerKind::Plain,
                lanes,
            } => Some(lanes[0]),
            Tracker {
                kind: TrackerKind::Exponential,
                lanes,
            } => Some(fmn_dmath::exp(lanes[0])),
            Tracker {
                kind: TrackerKind::Complex,
                ..
            } => None,
        }
    }

    /// `get_value` for complex trackers.
    #[must_use]
    pub fn tracker_complex_value(&self, mob: Mob) -> Option<(f64, f64)> {
        match self.tracker(mob)? {
            Tracker {
                kind: TrackerKind::Complex,
                lanes,
            } => Some((lanes[0], lanes[1])),
            _ => None,
        }
    }

    /// `set_value` for scalar trackers, through the encoding.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] if `mob` is dead or not a scalar tracker.
    pub fn set_tracker_value(&mut self, mob: Mob, value: f64) -> Result<(), StageError> {
        let entry = self.get_mut(mob).ok_or(StageError::StaleHandle)?;
        match &mut entry.tracker {
            Some(t) if t.kind == TrackerKind::Plain => {
                t.lanes[0] = value;
                Ok(())
            }
            Some(t) if t.kind == TrackerKind::Exponential => {
                t.lanes[0] = fmn_dmath::ln(value);
                Ok(())
            }
            _ => Err(StageError::StaleHandle),
        }
    }

    /// `set_value` for complex trackers.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] if `mob` is dead or not a complex tracker.
    pub fn set_tracker_complex_value(
        &mut self,
        mob: Mob,
        re: f64,
        im: f64,
    ) -> Result<(), StageError> {
        let entry = self.get_mut(mob).ok_or(StageError::StaleHandle)?;
        match &mut entry.tracker {
            Some(t) if t.kind == TrackerKind::Complex => {
                t.lanes = [re, im];
                Ok(())
            }
            _ => Err(StageError::StaleHandle),
        }
    }

    /// `increment_value`: `set(get + d)` through the encoding (the
    /// Reference's exact composition, so exponential trackers increment the
    /// decoded value, not the stored log).
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] if `mob` is dead or not a scalar tracker.
    pub fn increment_tracker_value(&mut self, mob: Mob, d_value: f64) -> Result<(), StageError> {
        let current = self.tracker_value(mob).ok_or(StageError::StaleHandle)?;
        self.set_tracker_value(mob, current + d_value)
    }

    /// Increment a complex tracker's decoded real and imaginary components.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] for a dead or non-complex tracker.
    pub fn increment_tracker_complex_value(
        &mut self,
        mob: Mob,
        real: f64,
        imaginary: f64,
    ) -> Result<(), StageError> {
        let (re, im) = self
            .tracker_complex_value(mob)
            .ok_or(StageError::StaleHandle)?;
        self.set_tracker_complex_value(mob, re + real, im + imaginary)
    }

    // ---------------------------------------------- closure-clock binding

    /// `always_redraw`: build a mobject from `f` now, and rebuild it on
    /// every update tick. The returned handle is stable across rebuilds
    /// (it is the container whose content is swapped); attach it, position
    /// it, and play against it freely.
    ///
    /// The factory runs while the previous content is alive. After a
    /// successful replacement, old members are reclaimed unless they belong
    /// to the new family, remain scene roots, or have parents outside the
    /// replaced family. Retained members keep their complete descendants.
    /// A cached raw [`Mob`] alone does not retain retired content; pinned
    /// retired members follow [`Stage::delete`]'s deferred destruction rules.
    ///
    /// # Panics
    /// Panics with a [`StageError`] if the factory returns a stale handle or
    /// a replacement would create a cycle, or updater registration fails.
    /// A refused replacement leaves the published children unchanged; the
    /// factory's own mutations are not rolled back.
    pub fn always_redraw(&mut self, f: impl Fn(&mut Stage) -> Mob + 'static) -> Mob {
        let first = f(self);
        self.try_get(first)
            .unwrap_or_else(|error| std::panic::panic_any(error));
        let container = self.add(Mobject::new());
        self.attach(container, first)
            .unwrap_or_else(|error| std::panic::panic_any(error));
        self.add_updater(
            container,
            move |stage, me| {
                // Capture before the factory: it may reuse descendants and
                // remove their old wrapper while building the replacement.
                let old: Vec<_> = stage.family(me).into_iter().skip(1).collect();
                let fresh = f(stage);
                stage
                    .replace_redraw_content(me, old, fresh)
                    .unwrap_or_else(|error| std::panic::panic_any(error));
            },
            false,
        )
        .unwrap_or_else(|error| std::panic::panic_any(error));
        container
    }

    /// Fallible factory variant of [`Stage::always_redraw`].
    ///
    /// The factory runs once now and once per update tick. Successful
    /// rebuilds preserve the returned container's identity and reclaim only
    /// retired private content, just like `always_redraw`. A failed factory
    /// or invalid replacement leaves the published family in place; the
    /// factory's own mutations are not rolled back.
    ///
    /// Initial errors are returned directly. Later errors are retained by
    /// [`Stage::updater_error`] with the container and updater identities.
    /// Further callbacks stop until the caller acknowledges that failure
    /// through [`Stage::take_updater_error`]; the same factory can then retry.
    /// An error raised by a nested updater is honored before publishing the
    /// factory's result, even if the factory itself returns `Ok`.
    ///
    /// # Errors
    ///
    /// Returns a pending updater failure or exhausted updater identity
    /// before invoking the initial factory. Initial callback errors and
    /// stale or foreign results are refused before allocating a container.
    /// Registration is checked again after the factory, since the factory
    /// may itself register updaters.
    pub fn try_always_redraw(
        &mut self,
        mut f: impl FnMut(&mut Stage) -> Result<Mob, UpdaterError> + 'static,
    ) -> Result<Mob, UpdaterError> {
        self.check_updater_registration()?;
        let first = self.invoke_updater_factory(&mut f)?;
        self.try_get(first)?;
        self.check_updater_registration()?;
        let container = self.add(Mobject::new());
        self.attach(container, first)?;
        self.add_try_updater(
            container,
            move |stage, me| {
                let old: Vec<_> = stage.family(me).into_iter().skip(1).collect();
                let fresh = f(stage)?;
                if let Some(failure) = stage.updater_error() {
                    return Err(failure.error.clone());
                }
                stage.replace_redraw_content(me, old, fresh)?;
                Ok(())
            },
            false,
        )?;
        Ok(container)
    }

    fn replace_redraw_content(
        &mut self,
        container: Mob,
        old: Vec<Mob>,
        fresh: Mob,
    ) -> Result<(), StageError> {
        let old_members: HashSet<Mob, IdBuildHasher> = old.iter().copied().collect();
        self.replace_children(container, &[fresh])?;

        let mut pending: Vec<_> = self
            .roots()
            .iter()
            .copied()
            .filter(|root| old_members.contains(root))
            .collect();
        pending.push(fresh);
        for &member in &old {
            if self.get(member).is_some_and(|entry| {
                entry
                    .parents()
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
                pending.extend_from_slice(entry.submobjects());
            }
        }

        // Decide retention before deleting anything: a diamond or a
        // pinned, doomed parent must not retain private descendants merely
        // because its other edge still exists.
        for member in old {
            if !retained.contains(&member) && self.contains(member) {
                // Sever outgoing edges first so deleting a retired wrapper
                // cannot hide a retained shared descendant from the scene.
                self.replace_children(member, &[])?;
                self.delete(member)?;
            }
        }
        Ok(())
    }

    /// `f_always`: run `f(stage, mob)` on every update tick — the
    /// named-parity form of [`Stage::add_updater`] (in the Reference,
    /// `f_always` exists to bind argument *generators*; a Rust closure
    /// already closes over them).
    ///
    /// # Errors
    /// [`StageError::StaleHandle`].
    pub fn f_always(
        &mut self,
        mob: Mob,
        f: impl FnMut(&mut Stage, Mob) + 'static,
    ) -> Result<UpdaterId, StageError> {
        self.add_updater(mob, f, false)
    }

    // ------------------------------------------------------ C-6: grouping

    /// Group addition as a value operation: ALWAYS builds a new group
    /// containing `a` and `b` — including when `a` is itself a group
    /// (where the Reference's `Group.__add__` mutates `a` in place and
    /// returns it, diverging from `Mobject.__add__`; C-6, Behavior Note).
    ///
    /// # Errors
    /// [`StageError::StaleHandle`] if either operand is dead.
    pub fn group_add(&mut self, a: Mob, b: Mob) -> Result<Mob, StageError> {
        if !self.contains(a) || !self.contains(b) {
            return Err(StageError::StaleHandle);
        }
        let group = self.add(Mobject::new());
        self.attach(group, a)?;
        self.attach(group, b)?;
        Ok(group)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn try_always_redraw_initial_errors_do_not_allocate_a_container() {
        for stale_result in [false, true] {
            let mut stage = Stage::new();
            let stale = stage.add(Mobject::new());
            stage.delete(stale).unwrap();
            let before = stage.snapshot_bytes().unwrap();
            let calls = Rc::new(Cell::new(0));
            let factory_calls = Rc::clone(&calls);
            let expected = if stale_result {
                UpdaterError::Stage(StageError::StaleHandle)
            } else {
                UpdaterError::callback("redraw factory", "initial geometry refused")
            };
            let factory_error = expected.clone();

            let result = stage.try_always_redraw(move |_| {
                factory_calls.set(factory_calls.get() + 1);
                if stale_result {
                    Ok(stale)
                } else {
                    Err(factory_error.clone())
                }
            });

            assert_eq!(result, Err(expected));
            assert_eq!(calls.get(), 1);
            assert!(stage.updater_error().is_none());
            assert_eq!(stage.snapshot_bytes().unwrap(), before);
        }
    }

    #[test]
    fn try_always_redraw_refuses_pending_errors_and_exhausted_ids_before_the_factory() {
        for exhausted in [false, true] {
            let mut stage = Stage::new();
            let existing = stage.add(Mobject::new());
            let expected = if exhausted {
                let mut snapshot = stage.snapshot();
                snapshot.next_updater_id = u64::MAX;
                stage.restore(&snapshot);
                UpdaterError::Stage(StageError::UpdaterIdExhausted)
            } else {
                let error = UpdaterError::callback("existing updater", "repair me first");
                let callback_error = error.clone();
                stage
                    .add_try_updater(existing, move |_, _| Err(callback_error.clone()), false)
                    .unwrap();
                stage.update_mobject(existing, 0.0);
                error
            };
            let before = stage.snapshot_bytes().unwrap();
            let calls = Rc::new(Cell::new(0));
            let factory_calls = Rc::clone(&calls);

            let result = stage.try_always_redraw(move |_| {
                factory_calls.set(factory_calls.get() + 1);
                Ok(existing)
            });

            assert_eq!(result, Err(expected.clone()));
            assert_eq!(calls.get(), 0);
            assert_eq!(stage.snapshot_bytes().unwrap(), before);
            if !exhausted {
                assert_eq!(stage.updater_error().unwrap().error, expected);
            }
        }
    }

    #[test]
    fn try_always_redraw_rechecks_identity_capacity_after_the_initial_factory() {
        let mut stage = Stage::new();
        let existing = stage.add(Mobject::new());
        let mut snapshot = stage.snapshot();
        snapshot.next_updater_id = u64::MAX - 1;
        stage.restore(&snapshot);
        let slots_before = stage.snapshot().slots.len();

        let result = stage.try_always_redraw(move |stage| {
            stage.add_updater(existing, |_, _| {}, false)?;
            Ok(existing)
        });

        assert_eq!(
            result,
            Err(UpdaterError::Stage(StageError::UpdaterIdExhausted))
        );
        assert_eq!(
            stage.updater_ids(existing).len(),
            1,
            "the factory's own mutation remains"
        );
        assert_eq!(
            stage.snapshot().slots.len(),
            slots_before,
            "no redraw wrapper was allocated"
        );
        assert!(stage.get(existing).unwrap().parents().is_empty());
    }

    #[test]
    fn try_always_redraw_keeps_the_last_family_until_acknowledged_retry() {
        let mut stage = Stage::new();
        let original = stage.add(Mobject::from_points(&[[1.0, 2.0, 0.0]]));
        let fresh = stage.add(Mobject::from_points(&[[3.0, 4.0, 0.0]]));
        let next = Rc::new(Cell::new(original));
        let fail = Rc::new(Cell::new(false));
        let calls = Rc::new(Cell::new(0));
        let factory_next = Rc::clone(&next);
        let factory_fail = Rc::clone(&fail);
        let factory_calls = Rc::clone(&calls);
        let expected = UpdaterError::callback("redraw factory", "geometry is not ready");
        let factory_error = expected.clone();
        let drawn = stage
            .try_always_redraw(move |_| {
                factory_calls.set(factory_calls.get() + 1);
                if factory_fail.get() {
                    Err(factory_error.clone())
                } else {
                    Ok(factory_next.get())
                }
            })
            .unwrap();
        let updater = stage.updater_ids(drawn)[0];
        let points = stage.get_points(original).unwrap();
        let before = stage.snapshot_bytes().unwrap();
        fail.set(true);

        stage.update_mobject(drawn, 0.1);

        let failure = stage.updater_error().unwrap();
        assert_eq!(failure.mobject, drawn);
        assert_eq!(failure.updater, updater);
        assert_eq!(failure.error, expected);
        assert_eq!(stage.snapshot_bytes().unwrap(), before);
        assert_eq!(stage.get(drawn).unwrap().submobjects(), &[original]);
        assert_eq!(stage.get_points(original).unwrap(), points);
        next.set(fresh);
        fail.set(false);
        stage.update_mobject(drawn, 0.1);
        assert_eq!(
            calls.get(),
            2,
            "a pending failure suppresses later callbacks"
        );
        assert_eq!(stage.get(drawn).unwrap().submobjects(), &[original]);

        let acknowledged = stage.take_updater_error().unwrap().unwrap();
        assert_eq!(acknowledged.error, expected);
        stage.update_mobject(drawn, 0.1);

        assert!(stage.updater_error().is_none());
        assert_eq!(calls.get(), 3);
        assert_eq!(stage.get(drawn).unwrap().submobjects(), &[fresh]);
        assert!(
            !stage.contains(original),
            "successful retry retires private old content"
        );
        assert_eq!(stage.updater_ids(drawn), vec![updater]);
        stage.pin(fresh).unwrap();
        for _ in 0..3 {
            stage.update_mobject(drawn, 0.1);
        }
        stage.unpin(fresh);
        assert!(
            stage.contains(fresh),
            "reused content must not be marked for deletion"
        );
        assert_eq!(stage.family(drawn), vec![drawn, fresh]);
    }

    #[test]
    fn try_always_redraw_invalid_replacements_preserve_the_published_graph() {
        for cyclic in [false, true] {
            let mut stage = Stage::new();
            let original = stage.add(Mobject::from_points(&[[2.0, 0.0, 0.0]]));
            let stale = stage.add(Mobject::new());
            stage.delete(stale).unwrap();
            let next = Rc::new(Cell::new(original));
            let factory_next = Rc::clone(&next);
            let drawn = stage
                .try_always_redraw(move |_| Ok(factory_next.get()))
                .unwrap();
            next.set(if cyclic { drawn } else { stale });
            let before = stage.snapshot_bytes().unwrap();

            stage.update_mobject(drawn, 0.1);

            let expected = if cyclic {
                StageError::CycleDetected
            } else {
                StageError::StaleHandle
            };
            assert_eq!(
                stage.updater_error().unwrap().error,
                UpdaterError::Stage(expected)
            );
            assert_eq!(stage.snapshot_bytes().unwrap(), before);
            assert_eq!(stage.get(drawn).unwrap().submobjects(), &[original]);
            assert_eq!(stage.get(original).unwrap().parents(), &[drawn]);

            next.set(original);
            stage.take_updater_error().unwrap().unwrap();
            stage.update_mobject(drawn, 0.1);
            assert!(stage.updater_error().is_none());
            assert_eq!(stage.family(drawn), vec![drawn, original]);
        }
    }

    #[test]
    fn try_always_redraw_preserves_shared_and_rooted_descendants() {
        for independently_rooted in [false, true] {
            let mut stage = Stage::new();
            let old = stage.add(Mobject::new());
            let shared = stage.add(Mobject::new());
            let leaf = stage.add(Mobject::from_points(&[[5.0, 6.0, 0.0]]));
            let private = stage.add(Mobject::new());
            let outside = stage.add(Mobject::new());
            let fresh = stage.add(Mobject::new());
            stage.attach(old, shared).unwrap();
            stage.attach(old, private).unwrap();
            stage.attach(shared, leaf).unwrap();
            stage.pin(old).unwrap();
            if independently_rooted {
                stage.add_to_scene(shared).unwrap();
            } else {
                stage.attach(outside, shared).unwrap();
                stage.add_to_scene(outside).unwrap();
            }
            let next = Rc::new(Cell::new(old));
            let factory_next = Rc::clone(&next);
            let drawn = stage
                .try_always_redraw(move |_| Ok(factory_next.get()))
                .unwrap();
            next.set(fresh);

            stage.update_mobject(drawn, 0.1);

            assert!(stage.updater_error().is_none());
            assert_eq!(stage.get(drawn).unwrap().submobjects(), &[fresh]);
            assert!(stage.contains(old), "the explicit pin defers destruction");
            assert!(stage.get(old).unwrap().submobjects().is_empty());
            assert!(!stage.contains(private));
            assert_eq!(stage.family(shared), vec![shared, leaf]);
            assert_eq!(stage.get_points(leaf).unwrap(), vec![[5.0, 6.0, 0.0]]);
            stage.unpin(old);
            assert!(!stage.contains(old));
            if independently_rooted {
                assert_eq!(stage.roots(), &[shared]);
            } else {
                assert_eq!(stage.roots(), &[outside]);
                assert_eq!(stage.get(outside).unwrap().submobjects(), &[shared]);
            }
        }
    }

    #[test]
    fn try_always_redraw_honors_nested_failures_before_publishing() {
        let mut stage = Stage::new();
        let old = stage.add(Mobject::new());
        let fresh = stage.add(Mobject::new());
        let source = stage.add(Mobject::new());
        let expected = UpdaterError::callback("nested updater", "first failure wins");
        let callback_error = expected.clone();
        let source_updater = stage
            .add_try_updater(source, move |_, _| Err(callback_error.clone()), false)
            .unwrap();
        let mut first = true;
        let drawn = stage
            .try_always_redraw(move |stage| {
                if first {
                    first = false;
                    return Ok(old);
                }
                stage.update_mobject(source, 0.0);
                Ok(fresh)
            })
            .unwrap();

        stage.update_mobject(drawn, 0.1);

        let failure = stage.updater_error().unwrap();
        assert_eq!(failure.mobject, source);
        assert_eq!(failure.updater, source_updater);
        assert_eq!(failure.error, expected);
        assert_eq!(stage.get(drawn).unwrap().submobjects(), &[old]);
        assert!(stage.contains(old));
        assert!(stage.contains(fresh));
        stage.clear_updaters(source, false);
        stage.take_updater_error().unwrap().unwrap();
        stage.update_mobject(drawn, 0.1);
        assert!(stage.updater_error().is_none());
        assert_eq!(stage.get(drawn).unwrap().submobjects(), &[fresh]);
        assert!(!stage.contains(old));
    }

    #[test]
    fn try_always_redraw_copies_share_the_factory_and_report_the_failing_copy() {
        let mut stage = Stage::new();
        let calls = Rc::new(Cell::new(0_u32));
        let fail = Rc::new(Cell::new(false));
        let factory_calls = Rc::clone(&calls);
        let factory_fail = Rc::clone(&fail);
        let original = stage
            .try_always_redraw(move |stage| {
                let call = factory_calls.get() + 1;
                factory_calls.set(call);
                if factory_fail.get() {
                    return Err(UpdaterError::callback("copied redraw", "retry required"));
                }
                Ok(stage.add(Mobject::from_points(&[[f64::from(call), 0.0, 0.0]])))
            })
            .unwrap();
        let copy = stage.copy_family(original).unwrap();
        let old_original_child = stage.get(original).unwrap().submobjects()[0];
        let old_copy_child = stage.get(copy).unwrap().submobjects()[0];
        fail.set(true);

        stage.update_mobject(copy, 0.1);

        assert_eq!(stage.updater_error().unwrap().mobject, copy);
        assert_eq!(
            stage.get(original).unwrap().submobjects(),
            &[old_original_child]
        );
        assert_eq!(stage.get(copy).unwrap().submobjects(), &[old_copy_child]);
        fail.set(false);
        stage.take_updater_error().unwrap().unwrap();
        stage.update_mobject(copy, 0.1);
        let new_copy_child = stage.get(copy).unwrap().submobjects()[0];
        assert_eq!(stage.get_x(copy), 3.0);
        assert_eq!(stage.get_x(original), 1.0);
        assert!(!stage.contains(old_copy_child));
        assert!(stage.contains(old_original_child));
        stage.update_mobject(original, 0.1);
        assert_eq!(stage.get_x(original), 4.0);
        assert_eq!(stage.get_x(copy), 3.0);
        assert!(!stage.contains(old_original_child));
        assert!(stage.contains(new_copy_child));
        assert_eq!(calls.get(), 4);
    }
}
