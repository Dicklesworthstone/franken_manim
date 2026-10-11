//! Ownership, lifetime and view-protocol witnesses for the render handoff.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Barrier, Mutex};

use fmn_mobject::{FrozenRenderState, Mobject, Stage};

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn frozen_render_state_is_send_sync_even_with_non_send_updater_state() {
    assert_send_sync::<FrozenRenderState>();
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_points(&[[1.0, 2.0, 3.0]]));
    stage.add_to_scene(mob).unwrap();
    let calls = Rc::new(Cell::new(0));
    let callback_calls = Rc::clone(&calls);
    let state = Rc::new(Cell::new(19));
    let updater = stage
        .add_dt_updater_with_state(
            mob,
            Rc::clone(&state),
            |_, _| {},
            move |_, _, _, _| callback_calls.set(callback_calls.get() + 1),
            false,
        )
        .unwrap();
    stage.set_time_from_clock(1.25);
    let frozen = stage.freeze_render_state();
    assert_eq!(
        Rc::strong_count(&calls),
        2,
        "freeze must not clone callables"
    );
    assert_eq!(
        Rc::strong_count(&state),
        2,
        "freeze must not clone native state"
    );
    std::thread::spawn(move || {
        let mut frame = frozen.materialize();
        assert_eq!(frame.time(), 1.25);
        assert_eq!(
            frame.get(mob).unwrap().buffer.read(0, "point").unwrap(),
            [1.0, 2.0, 3.0]
        );
        assert!(frame.updater_ids(mob).is_empty());
        assert!(frame.updater_state::<Rc<Cell<i32>>>(mob, updater).is_none());
        frame.update(1.0);
    })
    .join()
    .unwrap();
    assert_eq!(calls.get(), 0);
    stage.update(0.25);
    assert_eq!(
        calls.get(),
        1,
        "live updater remains installed and runnable"
    );
}

struct OwnerDrop {
    owner: std::thread::ThreadId,
    drops: Arc<Mutex<Vec<std::thread::ThreadId>>>,
}

impl Drop for OwnerDrop {
    fn drop(&mut self) {
        let actual = std::thread::current().id();
        assert_eq!(
            actual, self.owner,
            "callback destructor migrated to a worker"
        );
        self.drops.lock().unwrap().push(actual);
    }
}

#[test]
fn consuming_checkpoint_drops_last_callback_and_native_state_on_owner() {
    let owner = std::thread::current().id();
    let drops = Arc::new(Mutex::new(Vec::new()));
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::new());
    stage.add_to_scene(mob).unwrap();
    let callback_marker = Rc::new(OwnerDrop {
        owner,
        drops: Arc::clone(&drops),
    });
    let state_marker = Rc::new(OwnerDrop {
        owner,
        drops: Arc::clone(&drops),
    });
    stage
        .add_dt_updater_with_state(
            mob,
            state_marker,
            |_, _| {},
            move |_, _, _, _| {
                let _ = Rc::strong_count(&callback_marker);
            },
            false,
        )
        .unwrap();
    let snapshot = stage.snapshot();
    drop(stage);
    assert!(drops.lock().unwrap().is_empty());
    let frozen = snapshot.into_render_state(2.5);
    assert_eq!(drops.lock().unwrap().as_slice(), &[owner, owner]);
    std::thread::spawn(move || {
        let frame = frozen.materialize();
        assert_eq!(frame.time(), 2.5);
        assert!(frame.updater_ids(mob).is_empty());
    })
    .join()
    .unwrap();
}

#[test]
fn graph_identity_generation_and_copy_on_write_survive_handoff() {
    let mut stage = Stage::new();
    let stale = stage.add(Mobject::new());
    stage.delete(stale).unwrap();
    let shared = stage.add(Mobject::from_points(&[[1.0, 2.0, 0.0]]));
    let left = stage.add(Mobject::new());
    let right = stage.add(Mobject::new());
    stage.attach(left, shared).unwrap();
    stage.attach(right, shared).unwrap();
    stage.add_to_scene(left).unwrap();
    stage.add_to_scene(right).unwrap();
    let identity = stage.get(shared).unwrap().buffer.storage_id();
    let frozen = stage.freeze_render_state();
    let ready = Arc::new(Barrier::new(2));
    let mutated = Arc::new(Barrier::new(2));
    let worker_ready = Arc::clone(&ready);
    let worker_mutated = Arc::clone(&mutated);
    let worker = std::thread::spawn(move || {
        let frame = frozen.materialize();
        assert!(!frame.contains(stale));
        assert_eq!(frame.family(left), [left, shared]);
        assert_eq!(frame.family(right), [right, shared]);
        assert_eq!(frame.get(shared).unwrap().parents(), &[left, right]);
        assert_eq!(frame.get(shared).unwrap().buffer.storage_id(), identity);
        worker_ready.wait();
        worker_mutated.wait();
        frame.get(shared).unwrap().buffer.read(0, "point").unwrap()
    });
    ready.wait();
    stage
        .get_mut(shared)
        .unwrap()
        .buffer
        .write(0, "point", &[9.0, 8.0, 7.0]);
    mutated.wait();
    assert_eq!(worker.join().unwrap(), [1.0, 2.0, 0.0]);
}

#[test]
fn live_views_are_isolated_and_checkpoint_projection_keeps_replay_bindings() {
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_points(&[[1.0, 2.0, 3.0]]));
    stage.add_to_scene(mob).unwrap();
    stage.add_updater(mob, |_, _| {}, false).unwrap();
    let view = stage.get_mut(mob).unwrap().buffer.export_view(true);
    let frozen = stage.freeze_render_state();
    let checkpoint = stage.snapshot();
    let borrowed = checkpoint.render_state(3.0);
    assert!(view.write(0, "point", &[9.0, 8.0, 7.0]));
    for frame in [frozen.materialize(), borrowed.materialize()] {
        assert_eq!(
            frame.get(mob).unwrap().buffer.read(0, "point").unwrap(),
            [1.0, 2.0, 3.0]
        );
        assert!(frame.updater_ids(mob).is_empty());
    }
    let replay = checkpoint.materialize();
    assert_eq!(replay.updater_ids(mob).len(), 1);
    assert_eq!(
        stage.get(mob).unwrap().buffer.read(0, "point").unwrap(),
        [9.0, 8.0, 7.0]
    );
}
