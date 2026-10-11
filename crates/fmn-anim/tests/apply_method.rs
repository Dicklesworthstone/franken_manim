//! Native authored methods use begin-time state and the shared Transform law.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use fmn_anim::{
    AnimConfig, AnimError, Animation, AnimationSignature, ApplyMethod, PathFunc, RateFunc,
    RationalFrameClock, Succession, Timeline, Transform, play_segment,
};
use fmn_core::rng::RngRoot;
use fmn_mobject::{Mob, Mobject, Stage};

fn config() -> AnimConfig {
    AnimConfig {
        rate_func: RateFunc::linear(),
        name: "ApplyMethod".into(),
        ..AnimConfig::default()
    }
}

fn point(stage: &mut Stage, x: f64) -> Mob {
    stage.add(Mobject::from_points(&[[x, 0.0, 0.0]]))
}

fn assert_x(stage: &Stage, mob: Mob, expected: f64) {
    let actual = stage.get_center(mob);
    assert!(
        (actual[0] - expected).abs() < 1e-6,
        "{actual:?} != {expected}"
    );
    assert_eq!(&actual[1..], &[0.0, 0.0]);
}

#[test]
fn target_is_copied_and_method_called_only_at_begin() {
    let mut stage = Stage::new();
    let source = point(&mut stage, 1.0);
    let count = Rc::new(Cell::new(0));
    let calls = Rc::clone(&count);
    let before = stage.snapshot().to_bytes().unwrap();
    let mut animation = ApplyMethod::new(source, move |s, target| {
        calls.set(calls.get() + 1);
        assert_ne!(target, source);
        s.shift(target, [2.0, 0.0, 0.0]);
        Ok(())
    })
    .with_config(config());
    assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
    assert_eq!(animation.target_copy(), None);
    assert_eq!(count.get(), 0);
    stage.shift(source, [4.0, 0.0, 0.0]);
    animation.begin(&mut stage).unwrap();
    assert_eq!(count.get(), 1);
    assert_x(&stage, source, 5.0);
    assert_x(&stage, animation.target_copy().unwrap(), 7.0);
    for (alpha, expected) in [(0.25, 5.5), (0.5, 6.0), (0.75, 6.5)] {
        animation.interpolate(&mut stage, alpha);
        assert_x(&stage, source, expected);
    }
    animation.finish(&mut stage);
    assert_x(&stage, source, 7.0);
    assert_eq!(count.get(), 1, "sampling/finish must not call the method");
}

#[test]
fn succession_samples_noncommuting_operations_from_each_actual_start() {
    let mut stage = Stage::new();
    let source = point(&mut stage, 1.0);
    let starts = Rc::new(RefCell::new(Vec::new()));
    let starts2 = Rc::clone(&starts);
    let first = ApplyMethod::new(source, |s, target| {
        s.shift(target, [2.0, 0.0, 0.0]);
        Ok(())
    })
    .with_config(config());
    let second = ApplyMethod::new(source, move |s, target| {
        starts2.borrow_mut().push(s.get_center(target)[0]);
        s.apply_points_function(target, |p| [p[0] * p[0], p[1], p[2]], Some([0.0; 3]), None);
        Ok(())
    })
    .with_config(config());
    let mut succession =
        Succession::new(&mut stage, vec![Box::new(first), Box::new(second)]).unwrap();
    succession.begin(&mut stage).unwrap();
    assert!(starts.borrow().is_empty());
    for (alpha, expected) in [(0.25, 2.0), (0.5, 3.0), (0.75, 6.0), (1.0, 9.0)] {
        succession.interpolate(&mut stage, alpha);
        assert_x(&stage, source, expected);
    }
    succession.finish(&mut stage);
    assert_eq!(*starts.borrow(), vec![3.0]);
    assert_x(&stage, source, 9.0);
}

#[test]
fn repeat_begin_and_restored_stage_rebuild_expired_targets() {
    let mut stage = Stage::new();
    let source = point(&mut stage, 1.0);
    let base = stage.snapshot();
    let mut increment = 1.0;
    let mut animation = ApplyMethod::new(source, move |s, target| {
        s.shift(target, [increment, 0.0, 0.0]);
        increment += 1.0;
        Ok(())
    })
    .with_config(config());
    animation.begin(&mut stage).unwrap();
    animation.finish(&mut stage);
    assert_x(&stage, source, 2.0);
    let abandoned = animation.target_copy().unwrap();
    stage.restore(&base);
    assert!(!stage.contains(abandoned));
    animation.validate_begin(&stage).unwrap();
    animation.begin(&mut stage).unwrap();
    animation.finish(&mut stage);
    assert_x(&stage, source, 3.0);
    animation.begin(&mut stage).unwrap();
    animation.finish(&mut stage);
    assert_x(&stage, source, 6.0);
}

#[test]
fn stale_dependencies_invalid_path_and_hollow_span_never_call_authored_code() {
    let mut stage = Stage::new();
    let source = point(&mut stage, 1.0);
    let stale = point(&mut stage, 2.0);
    stage.delete(stale).unwrap();
    let count = Rc::new(Cell::new(0));
    for mode in 0..4 {
        let count2 = Rc::clone(&count);
        let mob = if mode == 0 { stale } else { source };
        let mut animation = ApplyMethod::new(mob, move |_, _| {
            count2.set(count2.get() + 1);
            Ok(())
        });
        if mode == 1 {
            animation = animation.with_dependencies([source, stale, source]);
        }
        if mode == 2 {
            animation = animation.with_path_arc(f64::NAN, [0.0, 0.0, 1.0]);
        }
        if mode == 3 {
            animation.state_mut().config.time_span = Some((1.0, 1.0));
        }
        let before = stage.snapshot().to_bytes().unwrap();
        assert!(animation.validate_begin(&stage).is_err());
        assert!(animation.begin(&mut stage).is_err());
        assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
        assert_eq!(count.get(), 0);
    }
}

#[test]
fn failure_reclaims_only_private_copies_and_preserves_the_original_error() {
    let mut stage = Stage::new();
    let source = point(&mut stage, 1.0);
    let child = point(&mut stage, 3.0);
    stage.attach(source, child).unwrap();
    let dependency = point(&mut stage, 9.0);
    let copies = Rc::new(RefCell::new(Vec::new()));
    let capture = Rc::clone(&copies);
    let mut animation = ApplyMethod::new(source, move |s, target| {
        *capture.borrow_mut() = s.family(target);
        s.attach(target, dependency)?;
        s.shift(dependency, [1.0, 0.0, 0.0]);
        Err(AnimError::MissingTarget)
    })
    .with_dependencies([dependency]);
    assert_eq!(animation.begin(&mut stage), Err(AnimError::MissingTarget));
    assert_eq!(copies.borrow().len(), 2);
    assert!(copies.borrow().iter().all(|&mob| !stage.contains(mob)));
    assert!(stage.contains(source) && stage.contains(child) && stage.contains(dependency));
    assert_x(&stage, dependency, 10.0);
    assert_eq!(stage.get(source).unwrap().submobjects(), &[child]);
    assert!(!stage.is_updating_suspended(source));
}

#[test]
fn begin_failure_in_the_driver_restores_the_stage_and_allows_retry() {
    let mut stage = Stage::new();
    let source = point(&mut stage, 1.0);
    stage.add_to_scene(source).unwrap();
    let failure = Rc::new(Cell::new(true));
    let should_fail = Rc::clone(&failure);
    let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(
        ApplyMethod::new(source, move |s, target| {
            s.shift(target, [3.0, 0.0, 0.0]);
            if should_fail.get() {
                s.shift(source, [99.0, 0.0, 0.0]);
                return Err(AnimError::MissingSavedState);
            }
            Ok(())
        })
        .with_config(config()),
    )];
    let before = stage.snapshot().to_bytes().unwrap();
    let mut clock = RationalFrameClock::new(4).unwrap();
    let rng = RngRoot::from_seed(1);
    let mut frames = Vec::new();
    assert_eq!(
        play_segment(
            &mut stage,
            &mut clock,
            &rng,
            &mut animations,
            false,
            &mut |p| frames.push(p)
        )
        .err()
        .unwrap(),
        AnimError::MissingSavedState
    );
    assert!(frames.is_empty());
    assert_eq!(clock.now().frames(), 0);
    assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
    failure.set(false);
    let report = play_segment(
        &mut stage,
        &mut clock,
        &rng,
        &mut animations,
        false,
        &mut |p| frames.push(p),
    )
    .unwrap();
    assert_eq!(frames.len(), 4);
    assert!(!report.purity.is_pure());
    assert!(report.begin_state.is_none());
    assert_x(&stage, source, 4.0);
}

#[test]
fn paths_native_trackers_and_numeric_uniforms_use_the_same_transform() {
    for path in [
        PathFunc::Straight,
        PathFunc::from_path_arc(std::f64::consts::PI, [0.0, 0.0, 1.0]),
    ] {
        let mut stage = Stage::new();
        let source = point(&mut stage, 1.0);
        let tracker = stage.add_complex_value_tracker(1.0, -2.0);
        stage.attach(source, tracker).unwrap();
        let control = stage.copy_family(source).unwrap();
        let control_tracker = stage.get(control).unwrap().submobjects()[0];
        let target = stage.copy_family(control).unwrap();
        let operation = |s: &mut Stage, mob: Mob| -> Result<(), AnimError> {
            s.shift(mob, [-2.0, 0.0, 0.0]);
            s.get_mut(mob).unwrap().uniforms_mut().shading = [0.75, 0.5, 0.25];
            let child = s.get(mob).unwrap().submobjects()[0];
            s.set_tracker_complex_value(child, 5.0, 6.0)?;
            Ok(())
        };
        operation(&mut stage, target).unwrap();
        let mut control_animation = Transform::new(control, target)
            .with_config(config())
            .with_path_func(path);
        let mut animation = ApplyMethod::new(source, operation)
            .with_config(config())
            .with_path_func(path);
        assert_eq!(
            animation.effect_signature(),
            AnimationSignature::Unclassified
        );
        animation.begin(&mut stage).unwrap();
        control_animation.begin(&mut stage).unwrap();
        for alpha in [0.0, 0.25, 0.5, 0.75, 1.0] {
            animation.interpolate(&mut stage, alpha);
            control_animation.interpolate(&mut stage, alpha);
            assert_eq!(stage.get_points(source), stage.get_points(control));
            assert_eq!(
                stage.tracker_complex_value(tracker),
                stage.tracker_complex_value(control_tracker)
            );
            assert_eq!(
                stage.get(source).unwrap().uniforms().shading,
                stage.get(control).unwrap().uniforms().shading
            );
        }
        animation.finish(&mut stage);
        control_animation.finish(&mut stage);
        assert_eq!(stage.tracker_complex_value(tracker), Some((5.0, 6.0)));
        assert!(!stage.is_updating_suspended(source));
    }
}

#[test]
fn timeline_backward_seek_rebuilds_the_target_without_replaying_callbacks_per_frame() {
    let mut stage = Stage::new();
    let source = point(&mut stage, 1.0);
    stage.add_to_scene(source).unwrap();
    let count = Rc::new(Cell::new(0));
    let count2 = Rc::clone(&count);
    let mut timeline = Timeline::new(4).unwrap();
    timeline
        .play(vec![Box::new(
            ApplyMethod::new(source, move |s, target| {
                count2.set(count2.get() + 1);
                s.shift(target, [4.0, 0.0, 0.0]);
                Ok(())
            })
            .with_config(config()),
        )])
        .unwrap();
    let rng = RngRoot::from_seed(4);
    for (frame, expected) in [(4, 5.0), (1, 2.0), (3, 4.0), (1, 2.0)] {
        timeline.seek(&mut stage, &rng, frame).unwrap();
        assert_x(&stage, source, expected);
    }
    assert_eq!(count.get(), 4);
}

#[test]
fn config_controls_time_span_final_alpha_and_removal_without_extra_calls() {
    let mut stage = Stage::new();
    let source = point(&mut stage, 1.0);
    stage.add_to_scene(source).unwrap();
    let count = Rc::new(Cell::new(0));
    let calls = Rc::clone(&count);
    let mut animation = ApplyMethod::new(source, move |s, target| {
        calls.set(calls.get() + 1);
        s.shift(target, [4.0, 0.0, 0.0]);
        Ok(())
    })
    .with_config(AnimConfig {
        time_span: Some((0.5, 1.0)),
        final_alpha_value: 0.5,
        remover: true,
        ..config()
    });
    animation.begin(&mut stage).unwrap();
    animation.interpolate(&mut stage, 0.5);
    assert_x(&stage, source, 1.0);
    animation.interpolate(&mut stage, 0.75);
    assert_x(&stage, source, 3.0);
    animation.finish(&mut stage);
    assert_x(&stage, source, 1.0);
    animation.clean_up_from_scene(&mut stage);
    assert!(!stage.roots().contains(&source));
    assert_eq!(count.get(), 1);
    assert!(!stage.is_updating_suspended(source));
}
