//! Transform-mechanism semantics (fm-cye slice B, §9.4): path formulas
//! from utils/paths.py, the lerp core from mobject.py:1810, the
//! begin/lock/finish/unlock order from transform.py, and the first zoo
//! parameterizations.

use fmn_anim::animation::Animation;
use fmn_anim::{AnimError, PathFunc, Transform, classify_play};
use fmn_mobject::record::{RecordBuffer, RecordSchema};
use fmn_mobject::{Mob, Mobject, Stage};

fn vmob(stage: &mut Stage, points: &[[f64; 3]], rgba: [f32; 4]) -> Mob {
    let mob = stage.add(Mobject::new());
    let entry = stage.get_mut(mob).unwrap();
    entry.buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len());
    #[allow(clippy::cast_possible_truncation)]
    let flat: Vec<f32> = points
        .iter()
        .flat_map(|p| p.iter().map(|v| *v as f32))
        .collect();
    entry.buffer.write_range("point", 0, &flat);
    let fill: Vec<f32> = rgba
        .iter()
        .copied()
        .cycle()
        .take(points.len() * 4)
        .collect();
    entry.buffer.write_range("fill_rgba", 0, &fill);
    mob
}

fn points_of(stage: &Stage, mob: Mob) -> Vec<f32> {
    stage.get(mob).unwrap().buffer.read_column("point").unwrap()
}

// ------------------------------------------------------------- path funcs

#[test]
fn straight_path_is_plain_lerp() {
    let p = PathFunc::Straight.eval([0.0, 0.0, 0.0], [2.0, 4.0, 6.0], 0.25);
    assert_eq!(p, [0.5, 1.0, 1.5]);
}

#[test]
fn arc_below_threshold_collapses_to_straight() {
    assert_eq!(
        PathFunc::from_path_arc(0.009, [0.0, 0.0, 1.0]),
        PathFunc::Straight
    );
}

#[test]
fn arc_path_endpoints_and_semicircle_midpoint() {
    let arc = PathFunc::from_path_arc(std::f64::consts::PI, [0.0, 0.0, 1.0]);
    let start = [1.0, 0.0, 0.0];
    let end = [-1.0, 0.0, 0.0];
    let at = |alpha: f64| arc.eval(start, end, alpha);
    for (p, expect) in [(at(0.0), start), (at(1.0), end)] {
        for k in 0..3 {
            assert!((p[k] - expect[k]).abs() < 1e-9, "endpoint exact");
        }
    }
    // A π arc about OUT bulges through (0, 1, 0): the semicircle midpoint.
    let mid = at(0.5);
    assert!((mid[0]).abs() < 1e-9 && (mid[1] - 1.0).abs() < 1e-9);
}

#[test]
fn zero_axis_defaults_to_out() {
    let arc = PathFunc::Arc {
        angle: std::f64::consts::PI,
        axis: [0.0, 0.0, 0.0],
    };
    let mid = arc.eval([1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], 0.5);
    assert!((mid[1] - 1.0).abs() < 1e-9, "OUT fallback");
}

// -------------------------------------------------------------- Transform

#[test]
fn transform_aligns_and_lands_on_target() {
    let mut stage = Stage::new();
    // Different point counts force real alignment.
    let a = vmob(
        &mut stage,
        &[[0.0; 3], [1.0, 1.0, 0.0], [2.0, 0.0, 0.0]],
        [1.0, 0.0, 0.0, 1.0],
    );
    let b = vmob(
        &mut stage,
        &[
            [0.0, 5.0, 0.0],
            [1.0, 6.0, 0.0],
            [2.0, 5.0, 0.0],
            [3.0, 4.0, 0.0],
            [4.0, 5.0, 0.0],
        ],
        [0.0, 0.0, 1.0, 1.0],
    );
    let mut t = Transform::new(a, b);
    t.begin(&mut stage).unwrap();
    let target_copy = t.target_copy().unwrap();
    assert_ne!(target_copy, b, "unaligned target is copied, not mutated");
    assert!(
        stage.is_aligned_with(a, target_copy),
        "setup aligned the pair"
    );
    t.interpolate(&mut stage, 1.0);
    t.finish(&mut stage);
    assert_eq!(
        points_of(&stage, a),
        points_of(&stage, target_copy),
        "alpha 1 lands exactly on the aligned target"
    );
    let fill = stage
        .get(a)
        .unwrap()
        .buffer
        .read_column("fill_rgba")
        .unwrap();
    assert!(
        fill.chunks(4).all(|c| c[2] == 1.0 && c[0] == 0.0),
        "non-point fields lerped to the target's values"
    );
}

#[test]
fn aligned_target_is_shared_not_copied() {
    let mut stage = Stage::new();
    let pts = [[0.0; 3], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]];
    let a = vmob(&mut stage, &pts, [1.0, 0.0, 0.0, 1.0]);
    let b = vmob(&mut stage, &pts, [0.0, 1.0, 0.0, 1.0]);
    let mut t = Transform::new(a, b);
    t.begin(&mut stage).unwrap();
    assert_eq!(
        t.target_copy(),
        Some(b),
        "is_aligned_with pair shares the target (transform.py:60)"
    );
}

#[test]
fn matching_data_locks_during_play_and_unlocks_at_finish() {
    let mut stage = Stage::new();
    let pts = [[0.0; 3], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]];
    // Same points, different fill: point column matches → locked.
    let a = vmob(&mut stage, &pts, [1.0, 0.0, 0.0, 1.0]);
    let b = vmob(&mut stage, &pts, [0.0, 1.0, 0.0, 1.0]);
    let mut t = Transform::new(a, b);
    t.begin(&mut stage).unwrap();
    let buffer = &stage.get(a).unwrap().buffer;
    assert!(buffer.is_locked("point"), "identical column locked");
    assert!(!buffer.is_locked("fill_rgba"), "differing column live");
    t.finish(&mut stage);
    assert!(
        !stage.get(a).unwrap().buffer.is_locked("point"),
        "teardown unlocks (transform.py:74)"
    );
}

#[test]
fn updaters_disable_locking() {
    let mut stage = Stage::new();
    let pts = [[0.0; 3], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]];
    let a = vmob(&mut stage, &pts, [1.0, 0.0, 0.0, 1.0]);
    let b = vmob(&mut stage, &pts, [0.0, 1.0, 0.0, 1.0]);
    stage.add_updater(a, |_, _| {}, false).unwrap();
    let mut t = Transform::new(a, b);
    t.begin(&mut stage).unwrap();
    assert!(
        !stage.get(a).unwrap().buffer.is_locked("point"),
        "lock_matching_data no-ops with updaters (mobject.py:1852)"
    );
}

#[test]
fn arc_transform_bulges_off_the_straight_line() {
    let mut stage = Stage::new();
    let a = vmob(&mut stage, &[[1.0, 0.0, 0.0]], [1.0; 4]);
    let b = vmob(&mut stage, &[[-1.0, 0.0, 0.0]], [1.0; 4]);
    let mut t = Transform::new(a, b).with_path_arc(std::f64::consts::PI, [0.0, 0.0, 1.0]);
    t.begin(&mut stage).unwrap();
    t.interpolate(&mut stage, 0.5);
    let p = points_of(&stage, a);
    assert!(
        (f64::from(p[1]) - 1.0).abs() < 1e-6,
        "midpoint rides the semicircle, not the chord (got y = {})",
        p[1]
    );
    t.finish(&mut stage);
}

#[test]
fn transform_is_pure_for_the_classifier() {
    let mut stage = Stage::new();
    let a = vmob(&mut stage, &[[0.0; 3]], [1.0; 4]);
    let b = vmob(&mut stage, &[[1.0, 0.0, 0.0]], [1.0; 4]);
    let mut t: Box<dyn Animation> = Box::new(Transform::new(a, b));
    t.begin(&mut stage).unwrap();
    let mut anims = vec![t];
    assert!(
        classify_play(&stage, &anims).is_pure(),
        "Transform joins the Pure allowlist"
    );
    anims[0].finish(&mut stage);
}

// ------------------------------------------------------------------- zoo

#[test]
fn move_to_target_and_restore_consume_the_links() {
    let mut stage = Stage::new();
    let a = vmob(&mut stage, &[[0.0; 3]], [1.0; 4]);
    assert!(matches!(
        fmn_anim::move_to_target(&stage, a),
        Err(AnimError::MissingTarget)
    ));
    assert!(matches!(
        fmn_anim::restore(&stage, a),
        Err(AnimError::MissingSavedState)
    ));

    stage.save_state(a).unwrap();
    let target = stage.generate_target(a).unwrap();
    stage.shift(target, [3.0, 0.0, 0.0]);
    let mut t = fmn_anim::move_to_target(&stage, a).unwrap();
    t.begin(&mut stage).unwrap();
    t.interpolate(&mut stage, 1.0);
    t.finish(&mut stage);
    assert_eq!(points_of(&stage, a), vec![3.0, 0.0, 0.0]);

    let mut back = fmn_anim::restore(&stage, a).unwrap();
    back.begin(&mut stage).unwrap();
    back.interpolate(&mut stage, 1.0);
    back.finish(&mut stage);
    assert_eq!(points_of(&stage, a), vec![0.0, 0.0, 0.0], "restored");
}

#[test]
fn scale_in_place_and_replacement_flag() {
    let mut stage = Stage::new();
    let a = vmob(
        &mut stage,
        &[[-1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [1.0, 0.0, 0.0]],
        [1.0; 4],
    );
    let mut t = fmn_anim::scale_in_place(&mut stage, a, 2.0).unwrap();
    assert!(!t.replaces_mobject_in_scene());
    t.begin(&mut stage).unwrap();
    t.interpolate(&mut stage, 1.0);
    t.finish(&mut stage);
    let p = points_of(&stage, a);
    assert_eq!((p[0], p[6]), (-2.0, 2.0), "doubled about the center");

    let b = vmob(&mut stage, &[[0.0; 3]], [1.0; 4]);
    assert!(fmn_anim::replacement_transform(a, b).replaces_mobject_in_scene());
}

#[test]
fn swap_exchanges_centers() {
    let mut stage = Stage::new();
    let a = vmob(&mut stage, &[[0.0; 3]], [1.0; 4]);
    let b = vmob(&mut stage, &[[4.0, 0.0, 0.0]], [1.0; 4]);
    let mut anims = fmn_anim::swap(&mut stage, a, b).unwrap();
    for t in &mut anims {
        t.begin(&mut stage).unwrap();
    }
    for t in &mut anims {
        t.interpolate(&mut stage, 1.0);
        t.finish(&mut stage);
    }
    // The 90° arc path leaves a ~1e-16 sin(θ) residue at alpha 1, exactly
    // as the Reference's float arithmetic does — compare at tolerance.
    for (mob, expect) in [(a, [4.0, 0.0, 0.0]), (b, [0.0, 0.0, 0.0])] {
        let center = stage.get_center(mob);
        for k in 0..3 {
            assert!(
                (center[k] - expect[k]).abs() < 1e-12,
                "center {center:?} vs {expect:?}"
            );
        }
    }
}

// ------------------------------------------------- late transform zoo

use fmn_anim::{
    apply_complex_function, apply_matrix_2d, apply_pointwise_function_to_center, fade_to_color,
};

#[test]
fn apply_matrix_lands_the_mapped_points() {
    let mut stage = Stage::new();
    let mob = vmob(
        &mut stage,
        &[[1.0, 0.0, 0.0], [2.0, 0.0, 0.0], [3.0, 0.0, 0.0]],
        [0.1, 0.2, 0.3, 1.0],
    );
    // Scale x by 2, y by 3.
    let mut anim = apply_matrix_2d(&mut stage, mob, [[2.0, 0.0], [0.0, 3.0]]).unwrap();
    assert_eq!(anim.state().config.name, "ApplyMatrix");
    assert_eq!(anim.state().config.run_time, 3.0);
    anim.begin(&mut stage).unwrap();
    anim.finish(&mut stage);
    let xs: Vec<f32> = points_of(&stage, mob)
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| c[0])
        .collect();
    assert_eq!(xs, vec![2.0, 4.0, 6.0]);
}

#[test]
fn fade_to_color_lands_the_rgb() {
    let mut stage = Stage::new();
    let mob = vmob(
        &mut stage,
        &[[0.0; 3], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]],
        [0.1, 0.2, 0.3, 1.0],
    );
    let mut anim = fade_to_color(&mut stage, mob, [0.0, 1.0, 0.0]).unwrap();
    anim.begin(&mut stage).unwrap();
    anim.finish(&mut stage);
    let fill = stage
        .get(mob)
        .unwrap()
        .buffer
        .read_column("fill_rgba")
        .unwrap();
    assert_eq!(&fill[..4], &[0.0, 1.0, 0.0, 1.0]);
}

#[test]
fn apply_complex_function_derives_the_path_arc() {
    let mut stage = Stage::new();
    let mob = vmob(
        &mut stage,
        &[[1.0, 0.0, 0.0], [2.0, 0.0, 0.0], [3.0, 0.0, 0.0]],
        [0.1, 0.2, 0.3, 1.0],
    );
    // f(z) = i·z: rotate the plane 90°; arg(f(1)) = π/2 becomes path_arc.
    let mut anim = apply_complex_function(&mut stage, mob, |re, im| (-im, re)).unwrap();
    assert_eq!(anim.state().config.name, "ApplyComplexFunction");
    anim.begin(&mut stage).unwrap();
    anim.finish(&mut stage);
    let pts = points_of(&stage, mob);
    let (xs, ys): (Vec<f32>, Vec<f32>) =
        pts.as_chunks::<3>().0.iter().map(|c| (c[0], c[1])).unzip();
    for x in xs {
        assert!(x.abs() < 1e-5, "x collapses to 0");
    }
    assert_eq!(ys, vec![1.0, 2.0, 3.0]);
}

#[test]
fn apply_pointwise_function_to_center_moves_to_mapped_center() {
    let mut stage = Stage::new();
    let mob = vmob(
        &mut stage,
        &[[1.0, 0.0, 0.0], [2.0, 0.0, 0.0], [3.0, 0.0, 0.0]],
        [0.1, 0.2, 0.3, 1.0],
    );
    let mut anim =
        apply_pointwise_function_to_center(&mut stage, mob, |c| [c[0] + 10.0, c[1], c[2]]).unwrap();
    anim.begin(&mut stage).unwrap();
    anim.finish(&mut stage);
    let center = stage.get_center(mob);
    assert!((center[0] - 12.0).abs() < 1e-5);
}
