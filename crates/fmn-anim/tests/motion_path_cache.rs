//! Repeated path-following uses the same Chisel law, not a sampled approximation.
#![forbid(unsafe_code)]

use fmn_anim::{AnimError, Animation, MoveAlongPath, RateFunc};
use fmn_mobject::record::{RecordBuffer, RecordSchema};
use fmn_mobject::{Mob, Mobject, Placement, Stage};

const POINTS: [[f64; 3]; 7] = [
    [-3.0, -0.5, 0.0],
    [-2.5, 3.0, 0.5],
    [-1.0, 0.0, 1.0],
    [0.25, -2.5, 1.25],
    [2.0, 1.0, 0.0],
    [4.0, 1.25, -0.5],
    [4.5, 2.0, 2.0],
];

fn setup() -> (Stage, Mob, Mob, MoveAlongPath) {
    let mut stage = Stage::new();
    let path = stage.add(Mobject::from_points(&POINTS));
    let moving = stage.add(Mobject::from_points(&[[0.0; 3]]));
    let mut animation = MoveAlongPath::new(moving, path);
    animation.state_mut().config.rate_func = RateFunc::linear();
    animation.begin(&mut stage).unwrap();
    (stage, path, moving, animation)
}

fn positions(stage: &Stage, mob: Mob) -> Vec<[u64; 3]> {
    stage
        .get_points(mob)
        .unwrap()
        .into_iter()
        .map(|p| p.map(f64::to_bits))
        .collect()
}

fn check(stage: &mut Stage, path: Mob, moving: Mob, animation: &mut MoveAlongPath, alpha: f64) {
    // Independent observation: no MoveAlongPath/cache call in the oracle.
    // Use the same public move_to operation so even translation rounding is
    // compared, rather than silently assuming move_to stores an exact point.
    let rated = animation.state().config.rate_func.eval(alpha);
    let destination = stage.point_from_proportion(path, rated).unwrap();
    let mut expected = stage.snapshot().materialize();
    expected.move_to(moving, destination, [0.0; 3]);
    animation.interpolate(stage, alpha);
    assert_eq!(positions(stage, moving), positions(&expected, moving));
    assert_eq!(positions(stage, path), positions(&expected, path));
}

#[test]
fn static_path_reuses_one_table_without_pinning_views_or_forcing_cow() {
    let (mut stage, path, moving, mut animation) = setup();
    assert_eq!(animation.path_materializations(), 1);
    let storage = stage.get(path).unwrap().buffer.storage_id();
    for sample in 0..1024 {
        check(
            &mut stage,
            path,
            moving,
            &mut animation,
            f64::from(sample) / 1024.0,
        );
    }
    assert_eq!(animation.path_materializations(), 1);
    assert_eq!(stage.get(path).unwrap().buffer.live_view_count(), 0);
    stage
        .get_mut(path)
        .unwrap()
        .buffer
        .write(0, "rgba", &[0.25; 4]);
    assert_eq!(stage.get(path).unwrap().buffer.storage_id(), storage);
    check(&mut stage, path, moving, &mut animation, 0.3);
    assert_eq!(animation.path_materializations(), 1);
    // The old equal-curve approximation would evaluate the first quadratic
    // at local t = 0.9. This independent negative must differ from true length.
    let points = stage.get_points(path).unwrap();
    let t = 0.9;
    let quick: [f64; 3] = std::array::from_fn(|lane| {
        (1.0 - t) * (1.0 - t) * points[0][lane]
            + 2.0 * (1.0 - t) * t * points[1][lane]
            + t * t * points[2][lane]
    });
    assert_ne!(stage.point_from_proportion(path, 0.3).unwrap(), quick);
}

#[test]
fn world_space_placement_edits_refresh_including_nonuniform_scale_and_shear() {
    let (mut stage, path, moving, mut animation) = setup();
    for (index, placement) in [
        Placement::from_translation([1e12, -0.0, -1e10]),
        Placement::new(
            [[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
            [1.5, -2.0, 3.0],
        ),
        Placement::new(
            [[3.0, 0.25, 0.0], [0.0, -0.2, 0.0], [0.3, 0.0, 2.0]],
            [2.0, 4.0, -1.0],
        ),
        Placement::IDENTITY,
    ]
    .into_iter()
    .enumerate()
    {
        stage.set_placement(path, placement).unwrap();
        for alpha in [0.125, 0.3, 0.7, 1.0] {
            check(&mut stage, path, moving, &mut animation, alpha);
        }
        assert_eq!(
            animation.path_materializations(),
            u64::try_from(index).unwrap() + 2
        );
    }
    stage.set_placement(path, Placement::IDENTITY).unwrap();
    check(&mut stage, path, moving, &mut animation, 0.5);
    assert_eq!(animation.path_materializations(), 5);
}

#[test]
fn point_writes_and_locked_field_writes_refresh_the_table() {
    let (mut stage, path, moving, mut animation) = setup();
    for (index, values) in [[3.0, 8.0, 2.0], [-4.0, -5.0, 1.0]].into_iter().enumerate() {
        stage
            .get_mut(path)
            .unwrap()
            .buffer
            .write(1, "point", &values);
        check(&mut stage, path, moving, &mut animation, 0.35);
        assert_eq!(
            animation.path_materializations(),
            u64::try_from(index).unwrap() + 2
        );
        stage.get_mut(path).unwrap().buffer.lock_data(["point"]);
    }
}

#[test]
fn foreign_point_writes_refresh_even_without_revision_changes() {
    let (mut stage, path, moving, mut animation) = setup();
    let view = stage
        .get_mut(path)
        .unwrap()
        .buffer
        .export_field_view("point", true)
        .unwrap();
    let revision = stage.get(path).unwrap().buffer.field_revision("point");
    for point in [[5.0, 9.0, 2.0], [-5.0, -9.0, -2.0]] {
        assert!(view.write_foreign(1, "point", &point));
        assert_eq!(
            stage.get(path).unwrap().buffer.field_revision("point"),
            revision
        );
        let count = animation.path_materializations();
        check(&mut stage, path, moving, &mut animation, 0.4);
        check(&mut stage, path, moving, &mut animation, 0.8);
        assert_eq!(animation.path_materializations(), count + 2);
    }
    drop(view);
    let count = animation.path_materializations();
    check(&mut stage, path, moving, &mut animation, 0.4);
    check(&mut stage, path, moving, &mut animation, 0.8);
    assert_eq!(animation.path_materializations(), count + 1);
}

#[test]
fn unrelated_or_readonly_views_reuse_but_whole_writable_views_refresh() {
    let (mut stage, path, moving, mut animation) = setup();
    let colors = stage
        .get_mut(path)
        .unwrap()
        .buffer
        .export_field_view("rgba", true)
        .unwrap();
    let readonly = stage
        .get_mut(path)
        .unwrap()
        .buffer
        .export_field_view("point", false)
        .unwrap();
    assert!(colors.write_foreign(0, "rgba", &[0.3; 4]));
    check(&mut stage, path, moving, &mut animation, 0.5);
    assert_eq!(animation.path_materializations(), 1);
    drop((colors, readonly));
    check(&mut stage, path, moving, &mut animation, 0.25);
    assert_eq!(animation.path_materializations(), 1);
    let whole = stage.get_mut(path).unwrap().buffer.export_view(true);
    check(&mut stage, path, moving, &mut animation, 0.5);
    check(&mut stage, path, moving, &mut animation, 0.25);
    assert_eq!(animation.path_materializations(), 3);
    drop(whole);
    check(&mut stage, path, moving, &mut animation, 0.5);
    assert_eq!(animation.path_materializations(), 4);
}

#[test]
fn replaced_storage_with_identical_revisions_and_detached_views_is_not_reused() {
    let (mut stage, path, moving, mut animation) = setup();
    let old_view = stage
        .get_mut(path)
        .unwrap()
        .buffer
        .export_field_view("point", true)
        .unwrap();
    stage.set_points(path, &POINTS[..3]).unwrap();
    check(&mut stage, path, moving, &mut animation, 0.4);
    let count = animation.path_materializations();
    assert!(!old_view.is_attached_to(&stage.get(path).unwrap().buffer));
    assert!(old_view.write_foreign(1, "point", &[100.0; 3]));
    check(&mut stage, path, moving, &mut animation, 0.4);
    assert_eq!(animation.path_materializations(), count);
    drop(old_view);
    for index in 1..65_u16 {
        let mut buffer = RecordBuffer::new(RecordSchema::mobject(), 3).unwrap();
        let x = f32::from(index);
        assert!(buffer.write_range(
            "point",
            0,
            &[x, 0.0, 0.0, x + 1.0, 2.0, 0.0, x + 4.0, 0.0, 0.0]
        ));
        stage.get_mut(path).unwrap().buffer = buffer;
        check(&mut stage, path, moving, &mut animation, 0.4);
        assert_eq!(animation.path_materializations(), count + u64::from(index));
    }
}

#[test]
fn clones_start_empty_and_restore_reobserves_the_correct_storage() {
    let (mut stage, path, moving, mut animation) = setup();
    let saved = stage.snapshot();
    let mut cloned = animation.clone();
    assert_eq!(cloned.path_materializations(), 0);
    check(&mut stage, path, moving, &mut cloned, 0.3);
    assert_eq!(cloned.path_materializations(), 1);
    stage.shift(path, [5.0, 2.0, 1.0]);
    check(&mut stage, path, moving, &mut animation, 0.3);
    assert_eq!(animation.path_materializations(), 2);
    stage.restore(&saved);
    check(&mut stage, path, moving, &mut animation, 0.3);
    assert_eq!(animation.path_materializations(), 3);
    // Taking a snapshot remains lazy: the cache neither exports a view nor
    // changes the identity of the shared record storage.
    let restored = stage.snapshot().materialize();
    assert_eq!(
        stage.get(path).unwrap().buffer.storage_id(),
        restored.get(path).unwrap().buffer.storage_id()
    );
}

#[test]
fn malformed_or_deleted_live_path_never_uses_its_cached_points() {
    let (mut stage, path, moving, mut animation) = setup();
    let before = positions(&stage, moving);
    stage.get_mut(path).unwrap().buffer.resize(2).unwrap();
    animation.interpolate(&mut stage, 0.5);
    assert_eq!(positions(&stage, moving), before);
    assert!(matches!(
        animation.setup(&mut stage),
        Err(AnimError::EmptyMobject)
    ));
    stage.set_points(path, &[]).unwrap();
    animation.interpolate(&mut stage, 0.8);
    assert_eq!(positions(&stage, moving), before);
    stage.set_points(path, &POINTS).unwrap();
    check(&mut stage, path, moving, &mut animation, 0.5);
    let before = positions(&stage, moving);
    stage.delete(path).unwrap();
    let replacement = stage.add(Mobject::from_points(&POINTS));
    assert_ne!(replacement, path);
    animation.interpolate(&mut stage, 0.9);
    assert_eq!(positions(&stage, moving), before);
    assert!(matches!(animation.setup(&mut stage), Err(AnimError::StaleHandle(m)) if m == path));
}

#[test]
fn finish_and_abort_release_cached_state_and_replay_builds_again() {
    let (mut stage, path, moving, mut animation) = setup();
    animation.finish(&mut stage);
    assert_eq!(animation.path_materializations(), 1);
    stage.shift(path, [2.0, 3.0, 4.0]);
    animation.begin(&mut stage).unwrap();
    assert_eq!(animation.path_materializations(), 2);
    check(&mut stage, path, moving, &mut animation, 0.3);
    animation.abort(&mut stage);
    animation.begin(&mut stage).unwrap();
    assert_eq!(animation.path_materializations(), 3);
    assert_eq!(stage.get(path).unwrap().buffer.live_view_count(), 0);
}

#[test]
fn degenerate_paths_and_overshooting_rates_keep_existing_semantics() {
    let (mut stage, path, moving, mut animation) = setup();
    for points in [
        vec![[2.0, 3.0, 4.0]],
        vec![[2.0, 3.0, 4.0]; 7],
        vec![
            [0.0; 3],
            [0.5, 1.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [5.0, 3.0, 0.0],
            [6.0, 5.0, 0.0],
            [7.0, 3.0, 0.0],
        ],
    ] {
        stage.set_points(path, &points).unwrap();
        for alpha in [-0.25, -0.0, 0.0, 0.3, 0.5, 1.0, 1.5] {
            check(&mut stage, path, moving, &mut animation, alpha);
        }
    }
    assert_eq!(animation.path_materializations(), 4);
    animation.state_mut().config.rate_func = RateFunc::Base(|alpha| 1.5 * alpha - 0.25);
    for alpha in [0.0, 0.3, 0.8, 1.0] {
        check(&mut stage, path, moving, &mut animation, alpha);
    }
    assert_eq!(animation.path_materializations(), 4);
}

#[test]
fn independent_paths_shared_storage_and_become_are_separate_observations() {
    let (mut stage, path, moving, mut animation) = setup();
    let snapshot = stage.snapshot();
    let original = stage.get(path).unwrap().buffer.storage_id();
    // Genuine CoW replacement invalidates even on a style-only change.
    stage
        .get_mut(path)
        .unwrap()
        .buffer
        .write(0, "rgba", &[0.25; 4]);
    assert_ne!(stage.get(path).unwrap().buffer.storage_id(), original);
    check(&mut stage, path, moving, &mut animation, 0.5);
    assert_eq!(animation.path_materializations(), 2);
    let other = stage.add(Mobject::from_points(&POINTS));
    stage.shift(other, [8.0, 2.0, 4.0]);
    stage.become_mobject(path, other, false).unwrap();
    check(&mut stage, path, moving, &mut animation, 0.5);
    assert_eq!(animation.path_materializations(), 3);
    let mut original_stage = snapshot.materialize();
    check(&mut original_stage, path, moving, &mut animation, 0.5);
    assert_eq!(animation.path_materializations(), 4);
}

#[test]
fn nonowning_storage_witness_preserves_cow_and_survives_generation_replacement() {
    let mut buffer = RecordBuffer::new(RecordSchema::mobject(), 3).unwrap();
    let witness = buffer.storage_identity();
    assert!(witness.matches(&buffer));
    assert_eq!(buffer.live_view_count(), 0);
    let id = buffer.storage_id();
    assert!(buffer.write(0, "point", &[1.0, 2.0, 3.0]));
    assert_eq!(buffer.storage_id(), id);
    let snapshot = buffer.snapshot_clone();
    assert_eq!(snapshot.storage_id(), id);
    assert!(buffer.write(0, "point", &[3.0, 2.0, 1.0]));
    assert!(!witness.matches(&buffer));
    assert!(witness.matches(&snapshot));
    drop(snapshot);
    for _ in 0..128 {
        let replacement = RecordBuffer::new(RecordSchema::mobject(), 3).unwrap();
        assert!(!witness.matches(&replacement));
    }
}
