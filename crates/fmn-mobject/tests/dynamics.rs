//! The §8.6 dynamic-behavior corpus (fm-yra acceptance): mixed dt/non-dt
//! ordering, the suspend/resume matrix, snapshot-per-tick add/remove, the
//! C-5 once-only correction, ValueTrackers, `always_redraw`/`f_always`, and
//! the C-6 group-addition correction.

use fmn_mobject::{Mob, Mobject, Stage, StageError, TrackerKind};
use std::cell::RefCell;
use std::rc::Rc;

fn square() -> Mobject {
    Mobject::from_points(&[
        [-0.5, -0.5, 0.0],
        [0.5, -0.5, 0.0],
        [0.5, 0.5, 0.0],
        [-0.5, 0.5, 0.0],
    ])
}

type Log = Rc<RefCell<Vec<String>>>;

fn logger(log: &Log, tag: &str) -> impl FnMut(&mut Stage, Mob) + 'static {
    let log = Rc::clone(log);
    let tag = tag.to_string();
    move |_, _| log.borrow_mut().push(tag.clone())
}

#[test]
fn execution_is_insertion_order_across_mixed_kinds() {
    let mut stage = Stage::new();
    let mob = stage.add(square());
    stage.add_to_scene(mob).unwrap();
    let log: Log = Rc::default();

    stage.add_updater(mob, logger(&log, "a"), false).unwrap();
    {
        let log = Rc::clone(&log);
        stage
            .add_dt_updater(
                mob,
                move |_, _, dt| log.borrow_mut().push(format!("b(dt={dt})")),
                false,
            )
            .unwrap();
    }
    stage.add_updater(mob, logger(&log, "c"), false).unwrap();

    stage.update(0.25);
    assert_eq!(
        log.borrow().as_slice(),
        ["a", "b(dt=0.25)", "c"],
        "one list, insertion order, dt only to the dt kind"
    );
}

#[test]
fn children_update_before_parents() {
    let mut stage = Stage::new();
    let parent = stage.add(Mobject::new());
    let child = stage.add(square());
    let grandchild = stage.add(square());
    stage.attach(parent, child).unwrap();
    stage.attach(child, grandchild).unwrap();
    stage.add_to_scene(parent).unwrap();
    let log: Log = Rc::default();

    stage
        .add_updater(parent, logger(&log, "parent"), false)
        .unwrap();
    stage
        .add_updater(child, logger(&log, "child"), false)
        .unwrap();
    stage
        .add_updater(grandchild, logger(&log, "grandchild"), false)
        .unwrap();

    stage.update(0.1);
    assert_eq!(
        log.borrow().as_slice(),
        ["grandchild", "child", "parent"],
        "the Reference recurses submobjects before running its own updaters"
    );
}

#[test]
fn insert_and_remove_shape_the_list() {
    let mut stage = Stage::new();
    let mob = stage.add(square());
    stage.add_to_scene(mob).unwrap();
    let log: Log = Rc::default();

    let a = stage.add_updater(mob, logger(&log, "a"), false).unwrap();
    let _b = stage.insert_updater(mob, 0, logger(&log, "b")).unwrap();
    let c = stage.add_updater(mob, logger(&log, "c"), false).unwrap();
    assert_eq!(stage.updater_ids(mob), vec![_b, a, c]);

    stage.update(0.0);
    assert_eq!(log.borrow().as_slice(), ["b", "a", "c"]);

    log.borrow_mut().clear();
    stage.remove_updater(mob, a);
    stage.update(0.0);
    assert_eq!(log.borrow().as_slice(), ["b", "c"]);

    stage.clear_updaters(mob, true);
    assert!(stage.updater_ids(mob).is_empty());
    assert!(!stage.has_updaters_in_family(mob));
}

#[test]
fn suspend_resume_matrix() {
    let mut stage = Stage::new();
    let parent = stage.add(Mobject::new());
    let child = stage.add(square());
    stage.attach(parent, child).unwrap();
    stage.add_to_scene(parent).unwrap();
    let log: Log = Rc::default();
    stage
        .add_updater(child, logger(&log, "child"), false)
        .unwrap();

    // Suspending the PARENT (even non-recursively) prunes the subtree: the
    // child's updater cannot run through a suspended ancestor.
    stage.suspend_updating(parent, false);
    assert!(!stage.is_updating_suspended(child));
    stage.update(0.1);
    assert!(
        log.borrow().is_empty(),
        "suspended parent prunes the subtree"
    );

    // Recursive suspension marks the child too.
    stage.suspend_updating(parent, true);
    assert!(stage.is_updating_suspended(child));

    // resume_updating with call_updater runs one immediate update(0) pass.
    stage.resume_updating(parent, true, true);
    assert_eq!(
        log.borrow().as_slice(),
        ["child"],
        "resume ran update(0) once"
    );

    log.borrow_mut().clear();
    stage.update(0.1);
    assert_eq!(
        log.borrow().as_slice(),
        ["child"],
        "updates flow after resume"
    );
}

#[test]
fn resume_clears_the_ancestor_chain_transitively() {
    let mut stage = Stage::new();
    let grandparent = stage.add(Mobject::new());
    let parent = stage.add(Mobject::new());
    let child = stage.add(square());
    stage.attach(grandparent, parent).unwrap();
    stage.attach(parent, child).unwrap();
    stage.add_to_scene(grandparent).unwrap();

    stage.suspend_updating(grandparent, true);
    // Resuming the CHILD clears its whole ancestor chain (each Reference
    // parent resumes its own parents), without recursing into subtrees.
    stage.resume_updating(child, false, false);
    assert!(!stage.is_updating_suspended(child));
    assert!(!stage.is_updating_suspended(parent));
    assert!(!stage.is_updating_suspended(grandparent));
}

#[test]
fn list_is_snapshotted_per_tick() {
    let mut stage = Stage::new();
    let mob = stage.add(square());
    stage.add_to_scene(mob).unwrap();
    let log: Log = Rc::default();

    // Updater "a" removes itself and registers "d" while iterating: neither
    // change affects THIS tick (the list was snapshotted); both hold next
    // tick.
    let a_id: Rc<RefCell<Option<fmn_mobject::UpdaterId>>> = Rc::default();
    {
        let log = Rc::clone(&log);
        let a_id_inner = Rc::clone(&a_id);
        let id = stage
            .add_updater(
                mob,
                move |stage, me| {
                    log.borrow_mut().push("a".into());
                    if let Some(id) = *a_id_inner.borrow() {
                        stage.remove_updater(me, id);
                    }
                    let log2 = Rc::clone(&log);
                    stage
                        .add_updater(me, move |_, _| log2.borrow_mut().push("d".into()), false)
                        .unwrap();
                },
                false,
            )
            .unwrap();
        *a_id.borrow_mut() = Some(id);
    }
    stage.add_updater(mob, logger(&log, "b"), false).unwrap();

    stage.update(0.1);
    assert_eq!(
        log.borrow().as_slice(),
        ["a", "b"],
        "mid-tick add/remove takes effect next tick"
    );

    log.borrow_mut().clear();
    stage.update(0.1);
    assert_eq!(
        log.borrow().as_slice(),
        ["b", "d"],
        "a removed itself; d joined at the tail"
    );
}

#[test]
fn c5_call_runs_the_update_pass_exactly_once() {
    let mut stage = Stage::new();
    let mob = stage.add(square());
    stage.add_to_scene(mob).unwrap();
    let log: Log = Rc::default();

    // A pre-existing updater sees exactly ONE update(0) pass when a new
    // updater registers with call=true (the Reference runs update(0) and
    // then update() again — the C-5 double call, fixed; Behavior Note).
    stage
        .add_updater(mob, logger(&log, "existing"), false)
        .unwrap();
    stage.add_updater(mob, logger(&log, "new"), true).unwrap();
    assert_eq!(
        log.borrow().as_slice(),
        ["existing", "new"],
        "one update(0) pass, not two"
    );
}

#[test]
fn value_trackers_encode_and_increment() {
    let mut stage = Stage::new();

    let plain = stage.add_value_tracker(2.5);
    assert_eq!(stage.tracker_value(plain), Some(2.5));
    stage.set_tracker_value(plain, -1.0).unwrap();
    stage.increment_tracker_value(plain, 0.25).unwrap();
    assert_eq!(stage.tracker_value(plain), Some(-0.75));

    // Exponential: lane holds ln(value); get decodes; increment acts on the
    // decoded value (set(get + d)), exactly the Reference's composition.
    let exp = stage.add_exponential_value_tracker(4.0);
    let stored = stage.tracker(exp).unwrap();
    assert_eq!(stored.kind, TrackerKind::Exponential);
    assert!((stored.lanes[0] - 4.0f64.ln()).abs() < 1e-12);
    assert!((stage.tracker_value(exp).unwrap() - 4.0).abs() < 1e-12);
    stage.increment_tracker_value(exp, 1.0).unwrap();
    assert!((stage.tracker_value(exp).unwrap() - 5.0).abs() < 1e-12);

    // Complex: two lanes; scalar accessors refuse it.
    let complex = stage.add_complex_value_tracker(1.0, -2.0);
    assert_eq!(stage.tracker_complex_value(complex), Some((1.0, -2.0)));
    assert_eq!(stage.tracker_value(complex), None);
    assert_eq!(
        stage.set_tracker_value(complex, 3.0),
        Err(StageError::StaleHandle)
    );
    stage.set_tracker_complex_value(complex, 0.5, 0.5).unwrap();
    assert_eq!(stage.tracker_complex_value(complex), Some((0.5, 0.5)));

    // Non-trackers refuse tracker operations.
    let mob = stage.add(square());
    assert_eq!(stage.tracker_value(mob), None);
    assert!(stage.set_tracker_value(mob, 1.0).is_err());
}

#[test]
fn trackers_drive_updaters_through_the_clock() {
    // The canonical composition: a tracker animated (here: incremented by a
    // dt updater) drives a dependent mobject through a non-dt updater.
    let mut stage = Stage::new();
    let tracker = stage.add_value_tracker(0.0);
    let mob = stage.add(square());
    stage.add_to_scene(tracker).unwrap();
    stage.add_to_scene(mob).unwrap();

    stage
        .add_dt_updater(
            tracker,
            move |stage, me, dt| {
                stage.increment_tracker_value(me, dt).unwrap();
            },
            false,
        )
        .unwrap();
    stage
        .add_updater(
            mob,
            move |stage, me| {
                let x = stage.tracker_value(tracker).unwrap();
                stage.set_x(me, x);
            },
            false,
        )
        .unwrap();

    stage.update(0.5);
    stage.update(0.5);
    assert!((stage.tracker_value(tracker).unwrap() - 1.0).abs() < 1e-12);
    assert!((stage.get_x(mob) - 1.0).abs() < 1e-6);
}

#[test]
fn always_redraw_rebuilds_per_tick_without_arena_growth() {
    let mut stage = Stage::new();
    let phase = Rc::new(RefCell::new(0.0_f64));
    let phase_for_closure = Rc::clone(&phase);
    let drawn = stage.always_redraw(move |stage| {
        let x = *phase_for_closure.borrow();
        stage.add(Mobject::from_points(&[[x, 0.0, 0.0]]))
    });
    stage.add_to_scene(drawn).unwrap();

    assert!((stage.get_x(drawn) - 0.0).abs() < 1e-6);
    *phase.borrow_mut() = 3.0;
    stage.update(0.1);
    assert!(
        (stage.get_x(drawn) - 3.0).abs() < 1e-6,
        "content rebuilt from the closure on tick"
    );

    // The handle stays stable and the arena stays bounded: replaced content
    // is deleted and its slots recycled.
    let after_two = stage.family(drawn).len();
    for _ in 0..10 {
        stage.update(0.1);
    }
    assert_eq!(stage.family(drawn).len(), after_two);
}

#[test]
fn f_always_binds_the_closure_into_the_clock() {
    let mut stage = Stage::new();
    let mob = stage.add(square());
    stage.add_to_scene(mob).unwrap();
    stage
        .f_always(mob, |stage, me| {
            stage.shift(me, [1.0, 0.0, 0.0]);
        })
        .unwrap();
    let x0 = stage.get_x(mob);
    stage.update(0.1);
    stage.update(0.1);
    assert!((stage.get_x(mob) - x0 - 2.0).abs() < 1e-6);
}

#[test]
fn c6_group_add_is_a_value_operation() {
    let mut stage = Stage::new();
    let a1 = stage.add(square());
    let a2 = stage.add(square());
    let group = stage.group_add(a1, a2).unwrap();
    assert_ne!(group, a1);
    assert_eq!(stage.get(group).unwrap().submobjects(), &[a1, a2]);

    // The C-6 case: the left operand IS a group. The Reference's
    // Group.__add__ would mutate `group` in place; the correction builds a
    // new group and leaves the operand untouched (Behavior Note).
    let b = stage.add(square());
    let combined = stage.group_add(group, b).unwrap();
    assert_ne!(combined, group, "a new group, not the mutated left operand");
    assert_eq!(
        stage.get(group).unwrap().submobjects(),
        &[a1, a2],
        "the existing group is untouched"
    );
    assert_eq!(stage.get(combined).unwrap().submobjects(), &[group, b]);

    // Dead operands are refused.
    let dead = stage.add(square());
    stage.delete(dead).unwrap();
    assert_eq!(stage.group_add(a1, dead), Err(StageError::StaleHandle));
}
