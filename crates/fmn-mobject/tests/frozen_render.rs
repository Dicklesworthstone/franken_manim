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

#[test]
fn every_render_handoff_seeds_independent_allocation_history() {
    let mut source = Stage::new();
    let empty = source.snapshot();
    let mut discarded = Vec::new();
    for _ in 0..3 {
        let first = source.add(Mobject::new());
        let second = source.add(Mobject::new());
        discarded.extend([first, second]);
        source.delete(first).unwrap();
        source.delete(second).unwrap();
    }
    let live = source.add(Mobject::from_points(&[[1.0, 2.0, 0.0]]));
    source.add_to_scene(live).unwrap();
    let removed_updater = source.add_updater(live, |_, _| {}, false).unwrap();
    source.remove_updater(live, removed_updater);
    let handoffs = [
        source.freeze_render_state(),
        source.snapshot().render_state(0.0),
        source.snapshot().into_render_state(0.0),
    ];
    let mut branch_identities = Vec::new();
    for handoff in handoffs {
        let mut frame = handoff.materialize();
        assert!(frame.contains(live));
        assert_eq!(frame.roots(), &[live]);
        frame.restore(&empty);
        let first = frame.add(Mobject::new());
        let second = frame.add(Mobject::new());
        assert_ne!(first, live);
        assert_ne!(second, live);
        assert!(discarded.iter().all(|&old| !frame.contains(old)));
        let updater = frame.add_updater(first, |_, _| {}, false).unwrap();
        assert_ne!(updater, removed_updater);
        branch_identities.push((first, second, updater));
    }
    assert!(branch_identities.windows(2).all(|pair| pair[0] == pair[1]));
    assert!(
        source.contains(live),
        "materialized branches leave the source intact"
    );
}

#[test]
fn visible_handoffs_keep_shared_roots_without_retaining_detached_family_links() {
    let mut stage = Stage::new();
    let shared = stage.add(Mobject::from_points(&[[1.0, 2.0, 0.0]]));
    let left = stage.add(Mobject::new());
    let right = stage.add(Mobject::new());
    let invisible_parent = stage.add(Mobject::new());
    for parent in [left, right, invisible_parent] {
        stage.attach(parent, shared).unwrap();
    }
    stage.add_many_to_scene(&[right, left]).unwrap();
    let saved = stage.save_state(shared).unwrap();
    let target = stage.generate_target(shared).unwrap();
    let snapshot = stage.snapshot();
    let handoffs = [
        stage.freeze_visible_render_state(),
        snapshot.visible_render_state(0.0),
        snapshot.clone().into_visible_render_state(0.0),
    ];
    for handoff in handoffs {
        let frame = handoff.materialize();
        assert_eq!(frame.roots(), &[right, left]);
        assert_eq!(frame.family(right), [right, shared]);
        assert_eq!(frame.family(left), [left, shared]);
        assert_eq!(frame.get(shared).unwrap().parents(), &[left, right]);
        assert_eq!(frame.get_points(shared).unwrap(), [[1.0, 2.0, 0.0]]);
        assert_eq!(frame.target(shared), None);
        assert_eq!(frame.saved_state(shared), None);
        for detached in [invisible_parent, saved, target] {
            assert!(!frame.contains(detached));
        }
    }
    // Native succession/interpolation and handle-based readback still get the
    // complete arena through the original APIs, including detached endpoints.
    for full in [
        stage.freeze_render_state().materialize(),
        snapshot.render_state(0.0).materialize(),
        snapshot.into_render_state(0.0).materialize(),
    ] {
        assert_eq!(full.target(shared), Some(target));
        assert_eq!(full.saved_state(shared), Some(saved));
        assert!(full.contains(invisible_parent));
        assert!(full.contains(target));
        assert!(full.contains(saved));
    }
    assert_eq!(
        stage.get(shared).unwrap().parents(),
        &[left, right, invisible_parent]
    );
}

#[test]
fn invisible_records_are_not_shared_but_visible_views_still_freeze() {
    let mut stage = Stage::new();
    let visible = stage.add(Mobject::from_points(&[[1.0, 2.0, 3.0]]));
    let detached = stage.add(Mobject::from_points(&[[4.0, 5.0, 6.0]]));
    stage.add_to_scene(visible).unwrap();
    let visible_view = stage.get_mut(visible).unwrap().buffer.export_view(true);
    let detached_storage = stage.get(detached).unwrap().buffer.storage_id();
    let frozen = stage.freeze_visible_render_state();
    assert!(visible_view.write(0, "point", &[9.0, 8.0, 7.0]));
    stage
        .get_mut(detached)
        .unwrap()
        .buffer
        .write(0, "point", &[7.0, 6.0, 5.0]);
    assert_eq!(
        stage.get(detached).unwrap().buffer.storage_id(),
        detached_storage,
        "a raster capture must not force copy-on-write for invisible history"
    );
    let frame = frozen.materialize();
    assert_eq!(frame.get_points(visible).unwrap(), [[1.0, 2.0, 3.0]]);
    assert!(!frame.contains(detached));
    // Becoming visible on a later frame is ordinary authored state, not an
    // excuse to discard or modify the original hidden object.
    stage.add_to_scene(detached).unwrap();
    let next = stage.freeze_visible_render_state().materialize();
    assert_eq!(next.get_points(detached).unwrap(), [[7.0, 6.0, 5.0]]);
}

#[test]
fn visible_capture_payload_does_not_grow_with_removed_scene_history() {
    let mut stage = Stage::new();
    let visible = stage.add(Mobject::from_points(&[[1.0, 2.0, 0.0]]));
    stage.add_to_scene(visible).unwrap();
    let first = stage.freeze_visible_render_state();
    assert_eq!(first.retained_object_count(), 1);
    let mut history = Vec::new();
    for _ in 0..4096 {
        let mob = stage.add(Mobject::from_points(&[[3.0, 4.0, 0.0]; 64]));
        stage.add_to_scene(mob).unwrap();
        stage.remove_from_scene(mob);
        history.push(mob);
    }
    // Keep a writable view over hidden geometry: full capture would eagerly
    // isolate it, but picture capture has no reason to retain any of its rows.
    let view = stage.get_mut(history[0]).unwrap().buffer.export_view(true);
    let snapshot = stage.snapshot();
    for handoff in [
        stage.freeze_visible_render_state(),
        snapshot.visible_render_state(0.0),
        snapshot.clone().into_visible_render_state(0.0),
    ] {
        assert_eq!(handoff.retained_object_count(), 1);
        assert_eq!(handoff.image_resources().count(), 0);
        let frame = handoff.materialize();
        assert_eq!(frame.roots(), &[visible]);
        assert_eq!(frame.get_points(visible).unwrap(), [[1.0, 2.0, 0.0]]);
        assert!(history.iter().all(|&mob| !frame.contains(mob)));
    }
    assert_eq!(stage.freeze_render_state().retained_object_count(), 4097);
    assert_eq!(snapshot.render_state(0.0).retained_object_count(), 4097);
    assert!(history.iter().all(|&mob| stage.contains(mob)));
    assert!(view.write(0, "point", &[8.0, 9.0, 0.0]));
    stage.add_to_scene(history[0]).unwrap();
    let next = stage.freeze_visible_render_state();
    assert_eq!(next.retained_object_count(), 2);
    assert_eq!(
        next.materialize().get_points(history[0]).unwrap()[0],
        [8.0, 9.0, 0.0]
    );
    assert_eq!(first.retained_object_count(), 1);
}

#[test]
fn auxiliary_families_keep_frozen_light_geometry_without_drawing_or_retaining_other_inputs() {
    use fmn_mobject::{ImageColorSpace, ImageResource, ImageSampler};

    let mut stage = Stage::new();
    let visible = stage.add(Mobject::from_points(&[[1.0, 2.0, 0.0]]));
    stage.add_to_scene(visible).unwrap();
    let stale = stage.add(Mobject::new());
    stage.delete(stale).unwrap();
    let later_occupant = stage.add(Mobject::new());
    let light = stage.add(Mobject::new());
    let light_point = stage.add(Mobject::from_points(&[[7.0, 8.0, 9.0]]));
    stage.attach(light, light_point).unwrap();
    let image = ImageResource::rgba8(
        1,
        1,
        vec![10, 20, 30, 255],
        ImageColorSpace::Srgb,
        ImageSampler::default(),
    )
    .unwrap();
    let hidden_image = stage.add(Mobject::new().with_image_resource(image));
    let foreign = Stage::new().add(Mobject::new());
    let checkpoint = stage.snapshot();
    assert_eq!(checkpoint.image_resources().count(), 1);
    let auxiliary = [light, light, stale, foreign];
    let borrowed = checkpoint.visible_render_state_with_auxiliary(1.0, &auxiliary);
    let owned = checkpoint
        .clone()
        .into_visible_render_state_with_auxiliary(1.0, &auxiliary);
    stage.shift(light, [100.0, 0.0, 0.0]);
    for state in [borrowed, owned] {
        assert_eq!(state.retained_object_count(), 3);
        assert_eq!(state.image_resources().count(), 0);
        let frame = state.materialize();
        assert_eq!(frame.roots(), &[visible]);
        assert_eq!(frame.family(visible), [visible]);
        assert_eq!(frame.family(light), [light, light_point]);
        assert_eq!(frame.get_center(light), [7.0, 8.0, 9.0]);
        assert!(!frame.contains(hidden_image));
        assert!(!frame.contains(stale));
        assert!(!frame.contains(later_occupant));
        assert!(!frame.contains(foreign));
    }
    assert_eq!(checkpoint.image_resources().count(), 1);
    assert!(checkpoint.materialize().contains(hidden_image));
    assert_eq!(stage.get_center(light), [107.0, 8.0, 9.0]);
}

#[test]
fn omitted_live_handles_remain_reserved_after_render_arena_rollback() {
    let mut source = Stage::new();
    let empty = source.snapshot();
    let detached = source.add(Mobject::new());
    let visible = source.add(Mobject::new());
    source.add_to_scene(visible).unwrap();
    for handoff in [
        source.freeze_visible_render_state(),
        source.snapshot().visible_render_state(0.0),
        source.snapshot().into_visible_render_state(0.0),
    ] {
        let mut frame = handoff.materialize();
        assert!(!frame.contains(detached));
        assert!(frame.contains(visible));
        let decoded =
            fmn_mobject::Snapshot::from_bytes(&frame.snapshot_bytes().unwrap(), &frame).unwrap();
        let mut durable = decoded.snapshot.materialize();
        assert_ne!(durable.add(Mobject::new()), detached);
        frame.restore(&empty);
        let replacements = [frame.add(Mobject::new()), frame.add(Mobject::new())];
        assert!(!replacements.contains(&detached));
        assert!(!replacements.contains(&visible));
        assert!(!frame.contains(detached));
        assert!(!frame.contains(visible));
    }
    assert!(source.contains(detached));
    assert!(source.contains(visible));
}

#[test]
fn invisible_checkpoint_callbacks_are_dropped_on_the_capturing_owner() {
    let owner = std::thread::current().id();
    let drops = Arc::new(Mutex::new(Vec::new()));
    let mut stage = Stage::new();
    let detached = stage.add(Mobject::new());
    let marker = Rc::new(OwnerDrop {
        owner,
        drops: Arc::clone(&drops),
    });
    stage
        .add_updater(
            detached,
            move |_, _| {
                let _ = Rc::strong_count(&marker);
            },
            false,
        )
        .unwrap();
    let checkpoint = stage.snapshot();
    drop(stage);
    assert!(drops.lock().unwrap().is_empty());
    let frozen = checkpoint.into_visible_render_state(0.0);
    assert_eq!(drops.lock().unwrap().as_slice(), &[owner]);
    std::thread::spawn(move || {
        let frame = frozen.materialize();
        assert!(frame.roots().is_empty());
        assert!(!frame.contains(detached));
    })
    .join()
    .unwrap();
}
