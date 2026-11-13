//! Native .animate uses the complete Transform data plane, including after
//! host-side geometry materialization and under out-of-order reconstruction.
use fmn_anim::purity::{Purity, reconstruct_pure_frame};
use fmn_anim::{
    AnimConfig, AnimError, Animation, AnimationGroup, AnimationSignature, MethodAnimation,
    MethodOverride, RateFunc, RationalFrameClock, Transform, VFade, play_segment,
    prepare_animation, prepare_animations, v_fade_out,
};
use fmn_core::{rate, rng::RngRoot};
use fmn_mobject::animate::{AnimateArgs, OverrideAnimation};
use fmn_mobject::{AnimateError, BuiltAnimate, Mob, Mobject, Stage};
use std::cell::Cell;
use std::rc::Rc;

fn square(stage: &mut Stage) -> Mob {
    stage.add(Mobject::from_points(&[
        [-0.5, -0.5, 0.0],
        [0.5, -0.5, 0.0],
        [0.5, 0.5, 0.0],
        [-0.5, 0.5, 0.0],
    ]))
}

fn close(actual: [f64; 3], expected: [f64; 3]) {
    for axis in 0..3 {
        assert!(
            (actual[axis] - expected[axis]).abs() < 1e-6,
            "{actual:?} != {expected:?}"
        );
    }
}

#[test]
fn explicit_axis_curves_the_native_builder_in_the_xz_plane() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);
    let revision = stage.get(mob).unwrap().buffer.field_revision("point");
    let builder = mob
        .animate()
        .set_anim_args(AnimateArgs {
            path_arc: Some(std::f64::consts::PI),
            path_arc_axis: Some([0.0, 1.0, 0.0]),
            rate_func: Some(rate::linear),
            ..AnimateArgs::default()
        })
        .unwrap()
        .shift([2.0, 0.0, 0.0])
        .unwrap();
    let mut animation = prepare_animation(builder, &mut stage).unwrap();
    animation.begin(&mut stage).unwrap();
    animation.interpolate(&mut stage, 0.5);
    close(stage.get_center(mob), [1.0, 0.0, 1.0]);
    assert_eq!(
        stage.get(mob).unwrap().buffer.field_revision("point"),
        revision
    );
    animation.finish(&mut stage);
    close(stage.get_center(mob), [2.0, 0.0, 0.0]);
}

#[test]
fn native_wag_interpolates_from_frozen_start_to_deformed_target() {
    let mut stage = Stage::new();
    let original = [[0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 2.0, 0.0]];
    let mob = stage.add(Mobject::vector_from_points(&original));
    let builder = mob
        .animate()
        .set_anim_args(AnimateArgs {
            rate_func: Some(rate::linear),
            ..AnimateArgs::default()
        })
        .unwrap()
        .wag([4.0, 0.0, 0.0], [0.0, 3.0, 0.0], 2.0)
        .unwrap();
    let mut animation = prepare_animation(builder, &mut stage).unwrap();
    animation.begin(&mut stage).unwrap();
    assert_eq!(stage.get_points(mob).unwrap(), original);
    // Seeking backwards must reconstruct from the start, not compound wag.
    for alpha in [1.0, 0.25, 0.5, 0.0] {
        animation.interpolate(&mut stage, alpha);
        let expected = [[0.0, 0.0, 0.0], [alpha, 1.0, 0.0], [4.0 * alpha, 2.0, 0.0]];
        let actual = stage.get_points(mob).unwrap();
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.into_iter().zip(expected) {
            close(actual, expected);
        }
    }
    animation.finish(&mut stage);
    assert_eq!(
        stage.get_points(mob).unwrap(),
        [[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [4.0, 2.0, 0.0],]
    );
}

#[test]
fn a_released_host_view_does_not_compound_an_intermediate_placement() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);
    let start = stage.get_points(mob).unwrap();
    let builder = mob
        .animate()
        .set_anim_args(AnimateArgs {
            rate_func: Some(rate::linear),
            ..AnimateArgs::default()
        })
        .unwrap()
        .shift([2.0, 0.0, 0.0])
        .unwrap();
    let mut animation = prepare_animation(builder, &mut stage).unwrap();
    animation.begin(&mut stage).unwrap();
    animation.interpolate(&mut stage, 0.5);
    // Record access can bake the placement, then release its view before the
    // next frame. It must not become a new interpolation origin.
    stage.bake_placement(mob).unwrap();
    animation.interpolate(&mut stage, 0.75);
    for (actual, original) in stage.get_points(mob).unwrap().into_iter().zip(&start) {
        close(actual, [original[0] + 1.5, original[1], original[2]]);
    }
    animation.finish(&mut stage);
    close(stage.get_center(mob), [2.0, 0.0, 0.0]);
}

#[test]
fn different_local_geometry_is_baked_before_interpolation() {
    let mut stage = Stage::new();
    let source = square(&mut stage);
    let target = square(&mut stage);
    stage
        .set_points(target, &[[4.0, -1.0, 0.0], [8.0, 1.0, 0.0]])
        .unwrap();
    stage.shift(source, [3.0, 2.0, 0.0]);
    stage.shift(target, [7.0, -4.0, 0.0]);
    let target_before = stage.get_points(target).unwrap();
    let mut animation = MethodAnimation::new(BuiltAnimate {
        source,
        target,
        overridden: None,
        args: AnimateArgs {
            rate_func: Some(rate::linear),
            ..AnimateArgs::default()
        },
    })
    .unwrap();
    animation.begin(&mut stage).unwrap();
    let rows = animation.all_mobjects();
    let a = stage.get_points(rows[1]).unwrap();
    let b = stage.get_points(rows[2]).unwrap();
    assert_eq!(a.len(), b.len());
    animation.interpolate(&mut stage, 0.25);
    for ((actual, a), b) in stage.get_points(source).unwrap().into_iter().zip(a).zip(b) {
        close(
            actual,
            std::array::from_fn(|axis| 0.75 * a[axis] + 0.25 * b[axis]),
        );
    }
    animation.finish(&mut stage);
    assert_eq!(
        stage.get_points(target).unwrap(),
        target_before,
        "user target is immutable"
    );
}

#[test]
fn method_animations_interpolate_trackers_and_uniforms_and_unlock_on_abort() {
    let mut stage = Stage::new();
    let source = stage.add_value_tracker(2.0);
    let target = stage.add_value_tracker(10.0);
    stage.get_mut(target).unwrap().uniforms_mut().shading = [0.4, 0.8, 0.2];
    let initial_shading = stage.get(source).unwrap().uniforms().shading;
    let mut animation = MethodAnimation::new(BuiltAnimate {
        source,
        target,
        overridden: None,
        args: AnimateArgs {
            rate_func: Some(rate::linear),
            ..AnimateArgs::default()
        },
    })
    .unwrap();
    animation.begin(&mut stage).unwrap();
    animation.interpolate(&mut stage, 0.25);
    assert_eq!(stage.tracker_value(source), Some(4.0));
    let actual = stage.get(source).unwrap().uniforms().shading;
    let target_shading = [0.4, 0.8, 0.2];
    close(
        actual,
        std::array::from_fn(|i| 0.75 * initial_shading[i] + 0.25 * target_shading[i]),
    );
    animation.abort(&mut stage);
    assert_eq!(
        stage.tracker_value(source),
        Some(4.0),
        "abort must not land the endpoint"
    );

    let source = square(&mut stage);
    let builder = source.animate().shift([1.0, 0.0, 0.0]).unwrap();
    let mut animation = prepare_animation(builder, &mut stage).unwrap();
    animation.begin(&mut stage).unwrap();
    assert!(stage.get(source).unwrap().buffer.is_locked("point"));
    animation.abort(&mut stage);
    assert!(!stage.get(source).unwrap().buffer.is_locked("point"));
}

#[test]
fn curved_method_frames_reconstruct_bit_exactly_out_of_order() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);
    stage.add_to_scene(mob).unwrap();
    let builder = mob
        .animate()
        .set_anim_args(AnimateArgs {
            path_arc: Some(-std::f64::consts::FRAC_PI_2),
            rate_func: Some(rate::linear),
            ..AnimateArgs::default()
        })
        .unwrap()
        .shift([2.0, 1.0, 0.0])
        .unwrap()
        .set_opacity(0.25)
        .unwrap();
    let mut animations = vec![prepare_animation(builder, &mut stage).unwrap()];
    let root = RngRoot::from_seed(42);
    let mut clock = RationalFrameClock::new(8).unwrap();
    let mut serial = Vec::new();
    let report = play_segment(
        &mut stage,
        &mut clock,
        &root,
        &mut animations,
        false,
        &mut |packet| serial.push(packet),
    )
    .unwrap();
    assert_eq!(report.purity, Purity::Pure);
    let samples: Vec<_> = RationalFrameClock::new(8)
        .unwrap()
        .segment(report.run_time)
        .unwrap()
        .samples()
        .collect();
    for index in [7, 0, 4, 1, 5, 2, 6, 3] {
        let packet =
            reconstruct_pure_frame(&mut stage, &mut animations, &report, &root, &samples[index])
                .unwrap();
        let expected = serial[index].materialize_stage();
        let actual = packet.materialize_stage();
        assert_eq!(
            actual.snapshot().to_bytes().unwrap(),
            expected.snapshot().to_bytes().unwrap()
        );
    }
}

#[test]
fn method_override_resolves_live_targets_once_and_keeps_its_native_lifecycle() {
    let mut stage = Stage::new();
    let source = square(&mut stage);
    let anchor = square(&mut stage);
    stage.add_to_scene(source).unwrap();
    let calls = Rc::new(Cell::new(0));
    let factory_calls = Rc::clone(&calls);
    let owner = std::thread::current().id();
    let recording = source
        .animate()
        .set_anim_args(AnimateArgs {
            run_time_frames: Some(4),
            rate_func: Some(rate::linear),
            path_arc: Some(std::f64::consts::PI),
            path_arc_axis: Some([0.0, 1.0, 0.0]),
            ..AnimateArgs::default()
        })
        .unwrap();
    let overridden = MethodOverride::new(recording, "move-to-anchor", move |arena, mob, args| {
        assert_eq!(std::thread::current().id(), owner);
        factory_calls.set(factory_calls.get() + 1);
        let target = arena.copy_family(mob)?;
        let start = arena.get_center(mob);
        let end = arena.get_center(anchor);
        arena.shift(target, std::array::from_fn(|axis| end[axis] - start[axis]));
        Ok(Transform::new(mob, target)
            .with_path_arc(args.path_arc.unwrap(), args.path_arc_axis.unwrap())
            .with_config(AnimConfig {
                run_time: 8.0,
                name: "factory-transform".to_owned(),
                remover: true,
                final_alpha_value: 0.75,
                suspend_mobject_updating: true,
                ..AnimConfig::default()
            }))
    })
    .unwrap();
    assert_eq!(calls.get(), 0, "recording must not run authored code");
    let before = stage.len();
    stage.shift(source, [1.0, 0.0, 0.0]);
    stage.shift(anchor, [3.0, 0.0, 0.0]);
    let mut animation = prepare_animation(overridden, &mut stage).unwrap();
    assert_eq!(calls.get(), 1);
    assert_eq!(
        stage.len(),
        before + 1,
        "only the factory's target was copied"
    );
    let config = &animation.state().config;
    assert_eq!(config.run_time_frames, Some(4));
    assert_eq!(config.name, "factory-transform");
    assert_eq!(config.final_alpha_value, 0.75);
    assert!(config.suspend_mobject_updating);
    assert!(animation.is_remover());
    close(stage.get_center(source), [1.0, 0.0, 0.0]);
    animation.begin(&mut stage).unwrap();
    assert_eq!(animation.effect_signature(), AnimationSignature::Pure);
    assert!(animation.freeze_interpolation().is_some());
    animation.interpolate(&mut stage, 0.5);
    close(stage.get_center(source), [2.0, 0.0, 1.0]);
    animation.finish(&mut stage);
    close(
        stage.get_center(source),
        [
            2.0 + std::f64::consts::FRAC_1_SQRT_2,
            0.0,
            std::f64::consts::FRAC_1_SQRT_2,
        ],
    );
    animation.clean_up_from_scene(&mut stage);
    assert!(!stage.roots().contains(&source));
    assert_eq!(
        calls.get(),
        1,
        "begin, frames and finish do not rerun the factory"
    );
}

#[test]
fn method_override_preserves_factory_defaults_and_exact_duration_precedence() {
    for (args, expected_frames, frame_count) in [
        (AnimateArgs::default(), Some(9), 9),
        (
            AnimateArgs {
                run_time: Some(0.5),
                ..AnimateArgs::default()
            },
            None,
            15,
        ),
        (
            AnimateArgs {
                run_time_frames: Some(7),
                ..AnimateArgs::default()
            },
            Some(7),
            7,
        ),
        (
            AnimateArgs {
                run_time: Some(0.5),
                run_time_frames: Some(7),
                ..AnimateArgs::default()
            },
            Some(7),
            7,
        ),
    ] {
        let mut stage = Stage::new();
        let mob = square(&mut stage);
        let overridden = MethodOverride::new(mob.animate(), "override", |_, source, _| {
            Ok(v_fade_out(source).with_config(AnimConfig {
                run_time: 2.0,
                run_time_frames: Some(9),
                lag_ratio: 0.25,
                rate_func: RateFunc::Base(rate::there_and_back),
                name: "factory-fade".to_owned(),
                remover: true,
                final_alpha_value: 0.25,
                suspend_mobject_updating: true,
                ..AnimConfig::default()
            }))
        })
        .unwrap()
        .set_anim_args(args)
        .unwrap();
        let animation = prepare_animation(overridden, &mut stage).unwrap();
        let config = &animation.state().config;
        assert_eq!(config.run_time_frames, expected_frames);
        assert_eq!(
            animation.duration_on_grid(30).unwrap().n_frames().unwrap(),
            frame_count
        );
        assert_eq!(config.lag_ratio, 0.25);
        assert_eq!(config.rate_func.eval(0.25), rate::there_and_back(0.25));
        assert_eq!(config.name, "factory-fade");
        assert!(config.remover);
        assert_eq!(config.final_alpha_value, 0.25);
        assert!(config.suspend_mobject_updating);
    }
}

#[test]
fn method_override_applies_composition_timing_through_the_animation_hook() {
    for (args, expected_frames) in [
        (
            AnimateArgs {
                run_time: Some(0.5),
                ..AnimateArgs::default()
            },
            3,
        ),
        (
            AnimateArgs {
                run_time_frames: Some(3),
                ..AnimateArgs::default()
            },
            3,
        ),
    ] {
        let mut stage = Stage::new();
        let mob = square(&mut stage);
        stage.add_to_scene(mob).unwrap();
        let overridden =
            MethodOverride::new(mob.animate(), "group-override", |arena, source, _| {
                let target = arena.copy_family(source)?;
                arena.shift(target, [2.0, 0.0, 0.0]);
                let child = Transform::new(source, target).with_config(AnimConfig {
                    rate_func: RateFunc::Base(rate::linear),
                    ..AnimConfig::default()
                });
                AnimationGroup::new(arena, vec![Box::new(child)])
            })
            .unwrap()
            .set_anim_args(args)
            .unwrap();
        let mut animations = prepare_animations(overridden, &mut stage).unwrap();
        let mut clock = RationalFrameClock::new(6).unwrap();
        let mut centers = Vec::new();
        let report = play_segment(
            &mut stage,
            &mut clock,
            &RngRoot::from_seed(11),
            &mut animations,
            false,
            &mut |packet| centers.push(packet.materialize_stage().get_center(mob)),
        )
        .unwrap();
        assert_eq!(report.n_frames, expected_frames);
        assert_eq!(centers.len(), 3);
        for (actual, expected) in centers.into_iter().zip([2.0 / 3.0, 4.0 / 3.0, 2.0]) {
            close(actual, [expected, 0.0, 0.0]);
        }
    }
}

#[test]
fn method_override_rejects_invalid_requests_before_running_the_factory() {
    for args in [
        AnimateArgs {
            run_time: Some(f64::NAN),
            ..AnimateArgs::default()
        },
        AnimateArgs {
            run_time: Some(f64::NAN),
            time_span: Some((0.0, 1.0)),
            ..AnimateArgs::default()
        },
        AnimateArgs {
            run_time_frames: Some(u64::MAX),
            ..AnimateArgs::default()
        },
        AnimateArgs {
            time_span: Some((1.0, 1.0)),
            ..AnimateArgs::default()
        },
        AnimateArgs {
            time_span: Some((f64::NAN, 1.0)),
            ..AnimateArgs::default()
        },
        AnimateArgs {
            lag_ratio: Some(f64::INFINITY),
            ..AnimateArgs::default()
        },
    ] {
        let mut stage = Stage::new();
        let source = square(&mut stage);
        let before = stage.snapshot().to_bytes().unwrap();
        let called = Cell::new(false);
        let overridden = MethodOverride::new(source.animate(), "invalid", |_, mob, _| {
            called.set(true);
            Ok(v_fade_out(mob))
        })
        .unwrap()
        .set_anim_args(args)
        .unwrap();
        assert!(prepare_animation(overridden, &mut stage).is_err());
        assert!(
            !called.get(),
            "invalid explicit arguments must not run authored code"
        );
        assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
    }

    let mut stage = Stage::new();
    let source = square(&mut stage);
    let called = Cell::new(false);
    let overridden = MethodOverride::new(source.animate(), "deleted", |_, mob, _| {
        called.set(true);
        Ok(v_fade_out(mob))
    })
    .unwrap();
    stage.delete(source).unwrap();
    let before = stage.snapshot().to_bytes().unwrap();
    assert_eq!(
        prepare_animation(overridden, &mut stage).err(),
        Some(AnimError::StaleHandle(source))
    );
    assert!(!called.get());
    assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
}

#[test]
fn method_override_factory_errors_propagate_without_a_fallback_target() {
    let mut stage = Stage::new();
    let source = square(&mut stage);
    let before = stage.snapshot().to_bytes().unwrap();
    let calls = Cell::new(0);
    let overridden = MethodOverride::new(source.animate(), "missing-target", |_, _, _| {
        calls.set(calls.get() + 1);
        Err::<VFade, _>(AnimError::MissingTarget)
    })
    .unwrap();
    assert_eq!(
        prepare_animation(overridden, &mut stage).err(),
        Some(AnimError::MissingTarget)
    );
    assert_eq!(calls.get(), 1);
    assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
}

#[test]
fn method_override_keeps_no_chaining_and_once_only_arguments() {
    let mut stage = Stage::new();
    let mob = square(&mut stage);
    for builder in [
        mob.animate().shift([1.0, 0.0, 0.0]).unwrap(),
        mob.animate()
            .with_override(OverrideAnimation { name: "first" })
            .unwrap(),
    ] {
        assert_eq!(
            MethodOverride::new(builder, "second", |_, source, _| Ok(v_fade_out(source))).err(),
            Some(AnimateError::OverrideNotChainable)
        );
    }
    let builder = mob.animate().set_anim_args(AnimateArgs::default()).unwrap();
    let overridden =
        MethodOverride::new(builder, "once", |_, source, _| Ok(v_fade_out(source))).unwrap();
    assert_eq!(
        overridden.set_anim_args(AnimateArgs::default()).err(),
        Some(AnimateError::ArgsAlreadySet)
    );
}

#[test]
fn unresolved_override_cannot_turn_into_a_silent_method_transform() {
    let mut stage = Stage::new();
    let source = square(&mut stage);
    let marker = OverrideAnimation {
        name: "custom-remover",
    };
    let before = stage.snapshot().to_bytes().unwrap();
    let expected = AnimError::Builder(AnimateError::UnresolvedOverride { name: marker.name });
    assert_eq!(
        prepare_animation(source.animate().with_override(marker).unwrap(), &mut stage).err(),
        Some(expected.clone())
    );
    assert_eq!(stage.snapshot().to_bytes().unwrap(), before);

    // BuiltAnimate's public fields cannot bypass the boundary either.
    let built = BuiltAnimate {
        source,
        target: source,
        args: AnimateArgs::default(),
        overridden: Some(marker),
    };
    assert_eq!(
        MethodAnimation::new(built.clone()).err(),
        Some(expected.clone())
    );
    assert_eq!(prepare_animation(built, &mut stage).err(), Some(expected));
    assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
}
