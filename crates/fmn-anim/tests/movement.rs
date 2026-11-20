//! Functional-map, rotation, and update-driven semantics (fm-cye, §9.4
//! family 5 + rotation.py + update.py): Homotopy's match-then-map rule,
//! PhaseFlow's deliberate path dependence, MoveAlongPath's true-arclength
//! constant speed (BN-03), Rotating's absolute per-frame pose, and the
//! live-stage update closures.

use std::cell::Cell;
use std::rc::Rc;

use fmn_anim::animation::{AnimConfig, Animation, sub_alpha, time_spanned_alpha};
use fmn_anim::{
    AnimError, Homotopy, MaintainPositionRelativeTo, MoveAlongPath, PhaseFlow, RateFunc,
    UpdateFromFunc, complex_homotopy, rotate, smoothed_homotopy,
};
use fmn_mobject::record::{RecordBuffer, RecordSchema};
use fmn_mobject::{Mob, Mobject, Stage, StageError};

fn vmob(stage: &mut Stage, points: &[[f64; 3]]) -> Mob {
    let mob = stage.add(Mobject::new());
    let entry = stage.get_mut(mob).unwrap();
    entry.buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len()).unwrap();
    #[allow(clippy::cast_possible_truncation)]
    let flat: Vec<f32> = points
        .iter()
        .flat_map(|p| p.iter().map(|v| *v as f32))
        .collect();
    entry.buffer.write_range("point", 0, &flat);
    mob
}

/// A 2-curve straight-line vmobject with x = [0, 1, 2, 3, 4].
fn line5(stage: &mut Stage) -> Mob {
    vmob(
        stage,
        &[
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
            [4.0, 0.0, 0.0],
        ],
    )
}

fn coords(stage: &Stage, mob: Mob, lane: usize) -> Vec<f32> {
    #[allow(clippy::cast_possible_truncation)]
    stage
        .get_points(mob)
        .unwrap()
        .into_iter()
        .map(|point| point[lane] as f32)
        .collect()
}

fn assert_close(actual: &[f32], expected: &[f32], tol: f32) {
    assert_eq!(actual.len(), expected.len());
    for (a, e) in actual.iter().zip(expected) {
        assert!((a - e).abs() <= tol, "{actual:?} !~ {expected:?}");
    }
}

// -------------------------------------------------------------- Homotopy

#[test]
fn homotopy_maps_points_from_the_start_each_frame() {
    let mut stage = Stage::new();
    let mob = line5(&mut stage);
    let mut anim = Homotopy::new(|x, y, z, t| [x, y + t * x, z], mob);
    assert_eq!(anim.state().config.run_time, 3.0);
    anim.state_mut().config.rate_func = RateFunc::linear();
    anim.begin(&mut stage).unwrap();
    anim.interpolate(&mut stage, 0.5);
    assert_close(&coords(&stage, mob, 1), &[0.0, 0.5, 1.0, 1.5, 2.0], 1e-6);
    // Absolute per-frame semantics: re-interpolating the same alpha does
    // not accumulate (match_points restores from the start first).
    anim.interpolate(&mut stage, 0.5);
    assert_close(&coords(&stage, mob, 1), &[0.0, 0.5, 1.0, 1.5, 2.0], 1e-6);
    anim.finish(&mut stage);
    assert_close(&coords(&stage, mob, 1), &[0.0, 1.0, 2.0, 3.0, 4.0], 1e-6);
}

#[test]
fn complex_homotopy_maps_the_plane_and_carries_z() {
    let mut stage = Stage::new();
    let mob = vmob(&mut stage, &[[1.0, 2.0, 7.0]; 3]);
    let mut anim = complex_homotopy(|re, im, t| (re + 3.0 * t, im), mob);
    assert_eq!(anim.state().config.name, "ComplexHomotopy");
    anim.state_mut().config.rate_func = RateFunc::linear();
    anim.begin(&mut stage).unwrap();
    anim.interpolate(&mut stage, 1.0);
    assert_close(&coords(&stage, mob, 0), &[4.0; 3], 1e-6);
    assert_close(&coords(&stage, mob, 1), &[2.0; 3], 1e-6);
    assert_close(&coords(&stage, mob, 2), &[7.0; 3], 1e-6);
}

#[test]
fn smoothed_homotopy_carries_the_reference_name() {
    let mut stage = Stage::new();
    let mob = line5(&mut stage);
    let anim = smoothed_homotopy(|x, y, z, _| [x, y, z], mob);
    assert_eq!(anim.state().config.name, "SmoothedVectorizedHomotopy");
}

// ------------------------------------------------------------- PhaseFlow

#[test]
fn phase_flow_integrates_forward_euler() {
    let mut stage = Stage::new();
    let mob = line5(&mut stage);
    let mut anim = PhaseFlow::new(|_| [1.0, 0.0, 0.0], mob, None);
    assert_eq!(anim.state().config.run_time, 3.0);
    anim.begin(&mut stage).unwrap();
    // begin's interpolate(0) only records last_alpha.
    assert_close(&coords(&stage, mob, 0), &[0.0, 1.0, 2.0, 3.0, 4.0], 1e-6);
    // dt = virtual_time · Δα = 3 · 0.5 = 1.5.
    anim.interpolate(&mut stage, 0.5);
    assert_close(&coords(&stage, mob, 0), &[1.5, 2.5, 3.5, 4.5, 5.5], 1e-5);
    // Path-dependent by design: the next step advects from where it is.
    anim.interpolate(&mut stage, 1.0);
    assert_close(&coords(&stage, mob, 0), &[3.0, 4.0, 5.0, 6.0, 7.0], 1e-5);
}

// ---------------------------------------------------------- MoveAlongPath

#[test]
fn move_along_path_is_constant_speed_by_true_arc_length() {
    let mut stage = Stage::new();
    // Two curves of very different lengths: 0→1 (length 1), 1→5 (length 4).
    let path = vmob(
        &mut stage,
        &[
            [0.0, 0.0, 0.0],
            [0.5, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
            [5.0, 0.0, 0.0],
        ],
    );
    let dot = vmob(&mut stage, &[[9.0, 9.0, 0.0]]);
    let mut anim = MoveAlongPath::new(dot, path);
    anim.state_mut().config.rate_func = RateFunc::linear();
    anim.begin(&mut stage).unwrap();
    assert_close(&coords(&stage, dot, 0), &[0.0], 1e-6);
    // Half the *arc length* is 2.5 — the Reference's
    // quick_point_from_proportion would sit at the curve boundary (x = 1).
    // Constant speed under the original name is BN-03.
    anim.interpolate(&mut stage, 0.5);
    assert_close(&coords(&stage, dot, 0), &[2.5], 1e-4);
    anim.finish(&mut stage);
    assert_close(&coords(&stage, dot, 0), &[5.0], 1e-5);
}

#[test]
fn move_along_empty_path_is_the_named_error() {
    let mut stage = Stage::new();
    let dot = vmob(&mut stage, &[[0.0; 3]]);
    let path = vmob(&mut stage, &[]);
    let mut anim = MoveAlongPath::new(dot, path);
    assert!(matches!(
        anim.begin(&mut stage),
        Err(AnimError::EmptyMobject)
    ));
}

// -------------------------------------------------------------- Rotating

#[test]
fn rotate_lands_the_absolute_pose_without_accumulating() {
    let mut stage = Stage::new();
    let mob = line5(&mut stage);
    let mut anim = rotate(mob, std::f64::consts::FRAC_PI_2);
    assert_eq!(anim.state().config.run_time, 1.0);
    anim.begin(&mut stage).unwrap();
    // Same alpha twice: the pose is absolute (restore-then-rotate), so
    // nothing accumulates.
    anim.interpolate(&mut stage, 0.5);
    let mid = coords(&stage, mob, 1);
    anim.interpolate(&mut stage, 0.5);
    assert_close(&coords(&stage, mob, 1), &mid, 1e-6);
    // Full turn: 90° about the center (x = 2) sends x ∈ [0,4] to
    // y ∈ [−2,2] at x = 2.
    anim.finish(&mut stage);
    assert_close(&coords(&stage, mob, 0), &[2.0; 5], 1e-5);
    assert_close(&coords(&stage, mob, 1), &[-2.0, -1.0, 0.0, 1.0, 2.0], 1e-5);
}

// ------------------------------------------------------------ update.py

#[test]
fn update_from_func_runs_live_each_frame() {
    let mut stage = Stage::new();
    let mob = line5(&mut stage);
    let mut anim = UpdateFromFunc::new(mob, |stage, mob| {
        stage.shift(mob, [1.0, 0.0, 0.0]);
    });
    anim.begin(&mut stage).unwrap(); // interpolate(0) → one call
    anim.interpolate(&mut stage, 0.5);
    anim.interpolate(&mut stage, 0.9);
    // Three calls, three shifts — live semantics, deliberately impure.
    assert_close(&coords(&stage, mob, 0), &[3.0, 4.0, 5.0, 6.0, 7.0], 1e-6);
}

#[test]
fn update_from_alpha_func_receives_the_true_alpha() {
    let mut stage = Stage::new();
    let mob = line5(&mut stage);
    let mut anim = UpdateFromFunc::new_alpha(mob, |stage, mob, alpha| {
        stage.set_x(mob, 10.0 * alpha);
    });
    anim.state_mut().config.rate_func = RateFunc::linear();
    anim.begin(&mut stage).unwrap();
    anim.interpolate(&mut stage, 0.3);
    let center = stage.get_center(mob);
    assert!((center[0] - 3.0).abs() < 1e-5);
}

#[test]
fn maintain_position_holds_the_construction_offset() {
    let mut stage = Stage::new();
    let mob = line5(&mut stage); // center x = 2
    let tracked = vmob(&mut stage, &[[0.0, 0.0, 0.0]]);
    let mut anim = MaintainPositionRelativeTo::new(&stage, mob, tracked);
    anim.begin(&mut stage).unwrap();
    stage.shift(tracked, [1.0, 1.0, 0.0]);
    anim.interpolate(&mut stage, 0.5);
    let center = stage.get_center(mob);
    assert!((center[0] - 3.0).abs() < 1e-5, "kept diff of +2 in x");
    assert!((center[1] - 1.0).abs() < 1e-5, "followed in y");
}

fn plain_family(stage: &mut Stage) -> (Mob, Mob, Mob) {
    let root = stage.add(Mobject::new());
    let first = stage.add(Mobject::from_points(&[[0.0, 1.0, 0.0], [0.5, 1.0, 0.0]]));
    let second = stage.add(Mobject::from_points(&[[2.0, 1.0, 0.0], [3.0, 1.0, 0.0]]));
    stage.attach(root, first).unwrap();
    stage.attach(root, second).unwrap();
    (root, first, second)
}

#[test]
fn homotopy_live_views_keep_original_observations_and_atomic_maps_opt_in() {
    for atomic in [false, true] {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_points(&[[1.0, 0.0, 0.0]]));
        let view = Rc::new(stage.get_mut(mob).unwrap().buffer.export_view(false));
        let observed = Rc::clone(&view);
        let map = move |x, y, z, _| [x + f64::from(observed.read(0, "point").unwrap()[0]), y, z];
        let mut anim = if atomic {
            Homotopy::try_new(move |x, y, z, t| Ok(map(x, y, z, t)), mob)
        } else {
            Homotopy::new(map, mob)
        };
        anim.begin(&mut stage).unwrap();
        assert_eq!(stage.get_points(mob).unwrap()[0][0], 2.0);
        for (alpha, expected) in [
            (0.5, if atomic { 3.0 } else { 2.0 }),
            (0.75, if atomic { 4.0 } else { 2.0 }),
        ] {
            anim.interpolate(&mut stage, alpha);
            assert!(anim.deferred_error().is_none());
            assert_eq!(stage.get_points(mob).unwrap()[0][0], expected);
            assert!(view.is_attached_to(&stage.get(mob).unwrap().buffer));
        }
    }
}

#[test]
fn homotopy_live_family_callbacks_observe_previously_published_members() {
    let mut stage = Stage::new();
    let root = stage.add(Mobject::new());
    let first = stage.add(Mobject::from_points(&[[1.0, 0.0, 0.0]]));
    let second = stage.add(Mobject::from_points(&[[2.0, 0.0, 0.0]]));
    stage.attach(root, first).unwrap();
    stage.attach(root, second).unwrap();
    let view = stage.get_mut(first).unwrap().buffer.export_view(false);
    let mut anim = Homotopy::new(
        move |x, y, z, _| [x + f64::from(view.read(0, "point").unwrap()[0]), y, z],
        root,
    );
    anim.begin(&mut stage).unwrap();
    for alpha in [0.25, 0.5, 1.0] {
        anim.interpolate(&mut stage, alpha);
        assert_eq!(stage.get_points(first).unwrap()[0][0], 2.0);
        assert_eq!(stage.get_points(second).unwrap()[0][0], 4.0);
    }
}

#[test]
fn phase_flow_live_and_atomic_fields_have_explicit_view_observation_boundaries() {
    for atomic in [false, true] {
        let mut stage = Stage::new();
        let root = stage.add(Mobject::new());
        let first = stage.add(Mobject::from_points(&[[1.0, 0.0, 0.0]]));
        let second = stage.add(Mobject::from_points(&[[10.0, 0.0, 0.0]]));
        stage.attach(root, first).unwrap();
        stage.attach(root, second).unwrap();
        let view = stage.get_mut(first).unwrap().buffer.export_view(false);
        let field = move |_| [f64::from(view.read(0, "point").unwrap()[0]), 0.0, 0.0];
        let mut anim = if atomic {
            PhaseFlow::try_new(move |point| Ok(field(point)), root, Some(1.0))
        } else {
            PhaseFlow::new(field, root, Some(1.0))
        };
        anim.begin(&mut stage).unwrap();
        anim.interpolate(&mut stage, 0.5);
        assert_eq!(stage.get_points(first).unwrap()[0][0], 1.5);
        assert_eq!(
            stage.get_points(second).unwrap()[0][0],
            if atomic { 10.5 } else { 10.75 }
        );
        anim.interpolate(&mut stage, 1.0);
        assert_eq!(stage.get_points(first).unwrap()[0][0], 2.25);
        assert_eq!(
            stage.get_points(second).unwrap()[0][0],
            if atomic { 11.25 } else { 11.875 }
        );
        assert!(anim.deferred_error().is_none());
    }
}

#[test]
fn fallible_homotopy_retains_the_entire_last_sample_and_attached_views() {
    let mut stage = Stage::new();
    let (root, first, second) = plain_family(&mut stage);
    let view = stage.get_mut(first).unwrap().buffer.export_view(false);
    let calls = Rc::new(Cell::new(0));
    let observed = Rc::clone(&calls);
    let expected = AnimError::UpdateFailed {
        operation: "homotopy domain",
        detail: "singularity".to_owned(),
    };
    let failure = expected.clone();
    let mut anim = Homotopy::try_new(
        move |x, y, z, t| {
            observed.set(observed.get() + 1);
            if t >= 0.5 && x >= 2.0 {
                Err(failure.clone())
            } else {
                Ok([x + t, y, z])
            }
        },
        root,
    )
    .with_config(AnimConfig {
        rate_func: RateFunc::linear(),
        ..AnimConfig::default()
    });
    anim.begin(&mut stage).unwrap();
    anim.interpolate(&mut stage, 0.25);
    let before: Vec<_> = [first, second]
        .into_iter()
        .map(|mob| {
            (
                stage.get_points(mob).unwrap(),
                stage.get(mob).unwrap().buffer.revision(),
            )
        })
        .collect();
    let successful_calls = calls.get();
    anim.interpolate(&mut stage, 0.5);
    assert_eq!(anim.deferred_error(), Some(expected.clone()));
    assert_eq!(calls.get(), successful_calls + 3);
    for (mob, before) in [first, second].into_iter().zip(before) {
        assert_eq!(stage.get_points(mob).unwrap(), before.0);
        assert_eq!(stage.get(mob).unwrap().buffer.revision(), before.1);
    }
    assert!(view.is_attached_to(&stage.get(first).unwrap().buffer));
    assert_eq!(view.read(0, "point").unwrap(), [0.25, 1.0, 0.0]);
    let failed_calls = calls.get();
    anim.interpolate(&mut stage, 0.0);
    anim.finish(&mut stage);
    assert_eq!(calls.get(), failed_calls);
    assert_eq!(anim.deferred_error(), Some(expected));
    anim.begin(&mut stage).unwrap();
    assert!(anim.deferred_error().is_none());
    assert_eq!(calls.get(), failed_calls + 4);
    anim.abort(&mut stage);
}

#[test]
fn homotopy_names_invalid_numeric_outputs_in_both_publication_modes() {
    for atomic in [false, true] {
        for bad in [f64::NAN, f64::INFINITY, f64::MAX] {
            let mut stage = Stage::new();
            let (root, first, second) = plain_family(&mut stage);
            let map = move |x, y, z, t| [if t >= 0.5 && x >= 2.0 { bad } else { x + t }, y, z];
            let mut anim = if atomic {
                Homotopy::try_new(move |x, y, z, t| Ok(map(x, y, z, t)), root)
            } else {
                Homotopy::new(map, root)
            }
            .with_config(AnimConfig {
                rate_func: RateFunc::linear(),
                ..AnimConfig::default()
            });
            anim.begin(&mut stage).unwrap();
            let second_before = stage.get_points(second);
            anim.interpolate(&mut stage, 0.5);
            assert!(matches!(
                anim.deferred_error(),
                Some(AnimError::Stage(StageError::InvalidPointMap(_)))
            ));
            assert_eq!(
                stage.get_points(first).unwrap()[0][0],
                if atomic { 0.0 } else { 0.5 }
            );
            assert_eq!(stage.get_points(second), second_before);
            let failed = stage.get_points(first);
            anim.interpolate(&mut stage, 0.75);
            assert_eq!(stage.get_points(first), failed);
        }
    }
}

#[test]
fn atomic_homotopy_names_removed_starting_handles_without_partial_motion() {
    let mut stage = Stage::new();
    let (root, first, _) = plain_family(&mut stage);
    let mut anim = Homotopy::try_new(|x, y, z, t| Ok([x + t, y, z]), root);
    anim.begin(&mut stage).unwrap();
    let deleted = anim.state().families()[2][1];
    stage.delete(deleted).unwrap();
    let before = stage.get_points(first);
    anim.interpolate(&mut stage, 0.5);
    assert_eq!(anim.deferred_error(), Some(AnimError::StaleHandle(deleted)));
    assert_eq!(stage.get_points(first), before);
}

#[test]
fn functional_maps_report_malformed_geometry_and_schema() {
    for smoothed in [false, true] {
        let mut stage = Stage::new();
        let (root, _, second) = plain_family(&mut stage);
        stage.get_mut(second).unwrap().buffer =
            RecordBuffer::new(RecordSchema::vmobject(), 2).unwrap();
        let mut anim = if smoothed {
            smoothed_homotopy(|x, y, z, _| [x, y, z], root)
        } else {
            Homotopy::new(|x, y, z, _| [x, y, z], root)
        };
        assert!(matches!(
            anim.begin(&mut stage),
            Err(AnimError::Stage(StageError::Geometry(_)))
        ));
    }
    let mut stage = Stage::new();
    let schema = RecordSchema::new(&[("point", 2)], &["point"], &["point"]).unwrap();
    let mob = stage.add(Mobject::from_buffer(RecordBuffer::new(schema, 3).unwrap()));
    let mut anim = Homotopy::new(
        |_, _, _, _| panic!("malformed schema must refuse first"),
        mob,
    );
    assert_eq!(
        anim.begin(&mut stage),
        Err(AnimError::Stage(StageError::SchemaMismatch))
    );

    let (root, first, second) = plain_family(&mut stage);
    stage.get_mut(second).unwrap().buffer = RecordBuffer::new(RecordSchema::vmobject(), 2).unwrap();
    let before = stage.get_points(first);
    let mut flow = PhaseFlow::try_new(|_| Ok([1.0, 0.0, 0.0]), root, Some(1.0));
    flow.begin(&mut stage).unwrap();
    flow.interpolate(&mut stage, 0.5);
    assert!(matches!(
        flow.deferred_error(),
        Some(AnimError::Stage(StageError::Geometry(_)))
    ));
    assert_eq!(stage.get_points(first), before);
}

#[test]
fn atomic_homotopy_does_not_publish_when_a_later_exact_rate_is_missing() {
    let mut stage = Stage::new();
    let (root, first, second) = plain_family(&mut stage);
    stage.set_points(root, &[[1.0, 0.0, 0.0]]).unwrap();
    let sampler = fmn_anim::rate_table::RateSampler::new(4, Ok);
    let collecting = RateFunc::Preparing(std::sync::Arc::clone(&sampler));
    assert_eq!(collecting.eval(0.0), 0.0);
    assert_eq!(collecting.eval(1.0), 1.0);
    let calls = Rc::new(Cell::new(0));
    let observed = Rc::clone(&calls);
    let mut anim = Homotopy::try_new(
        move |x, y, z, t| {
            observed.set(observed.get() + 1);
            Ok([x + t, y, z])
        },
        root,
    )
    .with_config(AnimConfig {
        lag_ratio: 0.5,
        rate_func: sampler.finish().unwrap(),
        ..AnimConfig::default()
    });
    anim.begin(&mut stage).unwrap();
    let before = [root, first, second].map(|mob| stage.get_points(mob));
    let begin_calls = calls.get();
    anim.interpolate(&mut stage, 0.5);
    assert!(matches!(
        anim.deferred_error(),
        Some(AnimError::UpdateFailed { .. })
    ));
    assert_eq!(calls.get(), begin_calls + 1);
    assert_eq!(
        [root, first, second].map(|mob| stage.get_points(mob)),
        before
    );
}

#[test]
fn atomic_homotopy_preserves_inert_auxiliary_rows_in_surface_holes() {
    let mut stage = Stage::new();
    let schema = RecordSchema::new(
        &[("point", 3), ("normal_seed", 3), ("surface_validity", 1)],
        &["point"],
        &["point", "normal_seed"],
    )
    .unwrap();
    let mut buffer = RecordBuffer::new(schema, 2).unwrap();
    buffer.write(0, "point", &[1.0, 2.0, 3.0]);
    buffer.write(0, "normal_seed", &[1.0, 2.0, 3.25]);
    buffer.write(0, "surface_validity", &[1.0]);
    buffer.write(1, "point", &[f32::NAN; 3]);
    buffer.write(1, "normal_seed", &[f32::NAN; 3]);
    let mob = stage.add(Mobject::from_buffer(buffer));
    let view = stage.get_mut(mob).unwrap().buffer.export_view(false);
    let mut anim = Homotopy::try_new(|x, y, z, t| Ok([x + t, y, z]), mob).with_config(AnimConfig {
        rate_func: RateFunc::linear(),
        ..AnimConfig::default()
    });
    anim.begin(&mut stage).unwrap();
    anim.interpolate(&mut stage, 0.5);
    assert!(anim.deferred_error().is_none());
    assert_eq!(stage.get_points(mob).unwrap(), [[1.5, 2.0, 3.0], [0.0; 3]]);
    assert_eq!(view.read(0, "normal_seed").unwrap(), [1.0, 2.0, 3.25]);
    assert!(
        view.read(1, "normal_seed")
            .unwrap()
            .iter()
            .all(|value| value.is_nan())
    );
    assert!(view.is_attached_to(&stage.get(mob).unwrap().buffer));
}

#[test]
fn fallible_phase_flow_preserves_family_and_restarts_its_euler_history() {
    let mut stage = Stage::new();
    let (root, first, second) = plain_family(&mut stage);
    let failing = Rc::new(Cell::new(true));
    let fail = Rc::clone(&failing);
    let calls = Rc::new(Cell::new(0));
    let observed = Rc::clone(&calls);
    let expected = AnimError::UpdateFailed {
        operation: "field domain",
        detail: "undefined velocity".to_owned(),
    };
    let failure = expected.clone();
    let mut anim = PhaseFlow::try_new(
        move |point| {
            observed.set(observed.get() + 1);
            if fail.get() && point[0] >= 2.0 {
                Err(failure.clone())
            } else {
                Ok([1.0, 0.0, 0.0])
            }
        },
        root,
        Some(1.0),
    );
    let before = [stage.get_points(first), stage.get_points(second)];
    anim.begin(&mut stage).unwrap();
    assert_eq!(calls.get(), 0);
    anim.interpolate(&mut stage, 0.25);
    assert_eq!(anim.deferred_error(), Some(expected.clone()));
    assert_eq!(calls.get(), 3);
    assert_eq!([stage.get_points(first), stage.get_points(second)], before);
    failing.set(false);
    anim.interpolate(&mut stage, 0.5);
    anim.finish(&mut stage);
    assert_eq!(calls.get(), 3);
    assert_eq!(anim.deferred_error(), Some(expected));
    anim.begin(&mut stage).unwrap();
    anim.interpolate(&mut stage, 0.25);
    assert!(anim.deferred_error().is_none());
    assert_eq!(stage.get_points(first).unwrap()[0], [0.25, 1.0, 0.0]);
    assert_eq!(stage.get_points(second).unwrap()[0], [2.25, 1.0, 0.0]);
}

#[test]
fn phase_flow_preserves_raw_alpha_and_signed_euler_steps() {
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_points(&[[1.0, 0.0, 0.0]]));
    let mut anim = PhaseFlow::new(|point| point, mob, Some(2.0)).with_config(AnimConfig {
        run_time: 9.0,
        lag_ratio: 0.9,
        time_span: Some((2.0, 8.0)),
        rate_func: RateFunc::Base(|_| panic!("PhaseFlow never samples a rate")),
        ..AnimConfig::default()
    });
    anim.begin(&mut stage).unwrap();
    anim.interpolate(&mut stage, 0.5);
    assert_eq!(stage.get_points(mob).unwrap(), [[2.0, 0.0, 0.0]]);
    anim.interpolate(&mut stage, 0.25);
    assert_eq!(stage.get_points(mob).unwrap(), [[1.0, 0.0, 0.0]]);
    anim.finish(&mut stage);
    assert_eq!(stage.get_points(mob).unwrap(), [[2.5, 0.0, 0.0]]);
}

#[test]
fn phase_flow_refuses_bad_timing_and_stale_targets_before_callbacks() {
    for bad in [f64::NAN, f64::INFINITY] {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_points(&[[0.0; 3]]));
        let mut anim = PhaseFlow::new(
            |_| panic!("invalid time must not call the field"),
            mob,
            Some(bad),
        );
        assert!(matches!(
            anim.begin(&mut stage),
            Err(AnimError::InvalidPath(_))
        ));
        let mut anim = PhaseFlow::new(
            |_| panic!("invalid alpha must not call the field"),
            mob,
            Some(1.0),
        );
        anim.begin(&mut stage).unwrap();
        anim.interpolate(&mut stage, bad);
        assert!(matches!(
            anim.deferred_error(),
            Some(AnimError::InvalidPath(_))
        ));
    }
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from_points(&[[0.0; 3]]));
    let mut anim = PhaseFlow::new(
        |_| panic!("overflowed dt must not call the field"),
        mob,
        Some(f64::MAX),
    );
    anim.begin(&mut stage).unwrap();
    anim.interpolate(&mut stage, 2.0);
    assert!(matches!(
        anim.deferred_error(),
        Some(AnimError::InvalidPath(_))
    ));
    let mut anim = PhaseFlow::new(
        |_| panic!("deleted target must not call the field"),
        mob,
        None,
    );
    anim.begin(&mut stage).unwrap();
    stage.delete(mob).unwrap();
    anim.interpolate(&mut stage, 0.5);
    assert_eq!(anim.deferred_error(), Some(AnimError::StaleHandle(mob)));
}

#[test]
fn relative_position_refuses_removed_handles_and_nonfinite_centers() {
    for delete_tracked in [false, true] {
        let mut stage = Stage::new();
        let mob = line5(&mut stage);
        let tracked = stage.add(Mobject::from_points(&[[1.0, 0.0, 0.0]]));
        let mut anim = MaintainPositionRelativeTo::new(&stage, mob, tracked);
        anim.begin(&mut stage).unwrap();
        let before = stage.get_points(mob);
        let deleted = if delete_tracked { tracked } else { mob };
        stage.delete(deleted).unwrap();
        anim.interpolate(&mut stage, 0.5);
        assert_eq!(anim.deferred_error(), Some(AnimError::StaleHandle(deleted)));
        if delete_tracked {
            assert_eq!(stage.get_points(mob), before);
        }
    }
    let mut stage = Stage::new();
    let mob = line5(&mut stage);
    let tracked = stage.add(Mobject::from_points(&[[1.0, 0.0, 0.0]]));
    let mut anim = MaintainPositionRelativeTo::new(&stage, mob, tracked);
    anim.begin(&mut stage).unwrap();
    let before = stage.get_points(mob);
    stage
        .get_mut(tracked)
        .unwrap()
        .buffer
        .write(0, "point", &[f32::INFINITY, 0.0, 0.0]);
    anim.interpolate(&mut stage, 0.5);
    assert!(matches!(
        anim.deferred_error(),
        Some(AnimError::Stage(StageError::InvalidPointMap(_)))
    ));
    assert_eq!(stage.get_points(mob), before);
}

#[test]
fn functional_homotopy_preserves_historical_geometry_bits_and_style_resize() {
    fn map(x: f64, y: f64, z: f64, t: f64) -> [f64; 3] {
        [
            (1.0 + 0.27 * t) * x + 0.13 * t * y,
            y + 0.021 * t * x * x,
            z + 0.17 * t,
        ]
    }
    fn legacy_sample(
        stage: &mut Stage,
        rows: &[(Mob, Mob)],
        config: &AnimConfig,
        alpha: f64,
        smooth: bool,
    ) {
        let spanned = time_spanned_alpha(alpha, config.run_time, config.time_span);
        for (index, &(mob, start)) in rows.iter().enumerate() {
            let time = sub_alpha(
                spanned,
                index,
                rows.len(),
                config.lag_ratio,
                &config.rate_func,
            );
            stage.match_points(mob, start);
            let points: Vec<_> = stage
                .get_points(mob)
                .unwrap()
                .into_iter()
                .map(|point| map(point[0], point[1], point[2], time))
                .collect();
            stage.set_points(mob, &points).unwrap();
            if smooth {
                stage.make_family_smooth(mob).unwrap();
            }
        }
    }
    fn compare(stage: &Stage, actual: Mob, expected: Mob) {
        assert_eq!(stage.family(actual).len(), stage.family(expected).len());
        for (actual, expected) in stage.family(actual).into_iter().zip(stage.family(expected)) {
            let actual = stage.get(actual).unwrap();
            let expected = stage.get(expected).unwrap();
            assert_eq!(actual.buffer.schema(), expected.buffer.schema());
            assert_eq!(actual.buffer.len(), expected.buffer.len());
            for field in actual.buffer.schema().fields() {
                let actual: Vec<_> = actual
                    .buffer
                    .read_column(&field.name)
                    .unwrap()
                    .into_iter()
                    .map(f32::to_bits)
                    .collect();
                let expected: Vec<_> = expected
                    .buffer
                    .read_column(&field.name)
                    .unwrap()
                    .into_iter()
                    .map(f32::to_bits)
                    .collect();
                assert_eq!(actual, expected, "{}", field.name);
            }
            assert_eq!(
                actual.placement().linear().map(|row| row.map(f64::to_bits)),
                expected
                    .placement()
                    .linear()
                    .map(|row| row.map(f64::to_bits))
            );
            assert_eq!(
                actual.placement().translation().map(f64::to_bits),
                expected.placement().translation().map(f64::to_bits)
            );
        }
    }

    // The existing smoothed constructor remains live. The atomic constructor
    // exposes plain Homotopy, not an implicit change to the smoothed surface.
    for (smooth, atomic) in [(false, false), (true, false), (false, true)] {
        let mut stage = Stage::new();
        let root = stage.add(Mobject::new());
        let left = stage.add(Mobject::new());
        let right = stage.add(Mobject::new());
        let shared = vmob(
            &mut stage,
            &[
                [0.0, 0.0, 0.0],
                [0.3, 1.1, 0.2],
                [1.0, 0.1, 0.3],
                [1.7, -0.4, 0.8],
                [2.0, 0.2, 0.4],
            ],
        );
        for (index, red) in [0.0, 0.2, 0.4, 0.6, 0.8].into_iter().enumerate() {
            assert!(stage.get_mut(shared).unwrap().buffer.write(
                index,
                "fill_rgba",
                &[red, 0.2, 0.3, 1.0]
            ));
        }
        stage.attach(root, left).unwrap();
        stage.attach(root, right).unwrap();
        stage.attach(left, shared).unwrap();
        stage.attach(right, shared).unwrap();
        let schema = RecordSchema::new(
            &[("point", 3), ("normal_seed", 3), ("rgba", 4)],
            &["point"],
            &["point", "normal_seed"],
        )
        .unwrap();
        let mut buffer = RecordBuffer::new(schema, 1).unwrap();
        buffer.write(0, "point", &[0.2, 0.4, 0.6]);
        buffer.write(0, "normal_seed", &[0.3, 0.5, 1.0]);
        buffer.write(0, "rgba", &[0.2, 0.4, 0.6, 0.8]);
        let surface = stage.add(Mobject::from_buffer(buffer));
        stage.attach(right, surface).unwrap();
        stage.shift(root, [0.125, -0.375, 0.0625]);
        let oracle = stage.copy_family(root).unwrap();
        let starting = stage.copy_family(oracle).unwrap();
        let rows: Vec<_> = stage
            .family(oracle)
            .into_iter()
            .zip(stage.family(starting))
            .collect();
        let oracle_shared = stage
            .get(stage.get(oracle).unwrap().submobjects()[0])
            .unwrap()
            .submobjects()[0];
        let config = AnimConfig {
            run_time: 3.0,
            time_span: Some((0.25, 2.75)),
            lag_ratio: 0.35,
            rate_func: RateFunc::Base(|time| time * time),
            ..AnimConfig::default()
        };
        let mut anim = if atomic {
            Homotopy::try_new(|x, y, z, time| Ok(map(x, y, z, time)), root)
        } else if smooth {
            smoothed_homotopy(map, root)
        } else {
            Homotopy::new(map, root)
        }
        .with_config(config.clone());
        anim.begin(&mut stage).unwrap();
        legacy_sample(&mut stage, &rows, &config, 0.0, smooth);
        compare(&stage, root, oracle);
        for (index, alpha) in [0.13, 0.45, 0.9, 0.45].into_iter().enumerate() {
            if index == 2 {
                // Begin-state restoration resizes both live records and style
                // lanes through the same historical order-preserving rule.
                let replacement = [[0.0; 3], [0.5, 1.0, 0.0], [1.0, 0.0, 0.0]];
                stage.set_points(shared, &replacement).unwrap();
                stage.set_points(oracle_shared, &replacement).unwrap();
            }
            anim.interpolate(&mut stage, alpha);
            assert!(anim.deferred_error().is_none());
            legacy_sample(&mut stage, &rows, &config, alpha, smooth);
            compare(&stage, root, oracle);
        }
    }
}
