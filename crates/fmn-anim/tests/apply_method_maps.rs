//! Deferred map targets must use live world-space points, never eager inputs.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use fmn_anim::{AnimConfig, Animation, ApplyMethod, RateFunc, Succession};
use fmn_mobject::{Mobject, Stage};

fn linear() -> AnimConfig {
    AnimConfig {
        rate_func: RateFunc::linear(),
        ..AnimConfig::default()
    }
}

fn close(actual: [f64; 3], expected: [f64; 3]) {
    for (a, b) in actual.into_iter().zip(expected) {
        assert!((a - b).abs() < 1e-6, "{actual:?} != {expected:?}");
    }
}

#[test]
fn pointwise_defaults_and_delayed_world_space_mapping_are_observable() {
    let mut stage = Stage::new();
    let source = stage.add(Mobject::from_points(&[[1.0, 1.0, 0.0]]));
    let calls = Rc::new(Cell::new(0));
    let called = Rc::clone(&calls);
    let mut animation = ApplyMethod::pointwise(source, move |p| {
        called.set(called.get() + 1);
        [p[0] * p[0], p[0] + p[1], p[2]]
    });
    assert_eq!(animation.state().config.name, "ApplyPointwiseFunction");
    assert_eq!(animation.get_run_time(), 3.0);
    assert_eq!(calls.get(), 0);
    stage.shift(source, [2.0, 0.0, 0.0]);
    animation.state_mut().config.rate_func = RateFunc::linear();
    animation.begin(&mut stage).unwrap();
    let calls_at_begin = calls.get();
    assert!(calls_at_begin > 0);
    close(stage.get_center(source), [3.0, 1.0, 0.0]);
    animation.interpolate(&mut stage, 0.5);
    close(stage.get_center(source), [6.0, 2.5, 0.0]);
    animation.finish(&mut stage);
    close(stage.get_center(source), [9.0, 4.0, 0.0]);
    assert_eq!(calls.get(), calls_at_begin);
}

#[test]
fn center_map_uses_the_live_family_envelope_once_and_preserves_offsets() {
    let mut stage = Stage::new();
    let source = stage.add(Mobject::from_points(&[[0.0, 0.0, 0.0]]));
    let child = stage.add(Mobject::from_points(&[[2.0, 0.0, 0.0]]));
    stage.attach(source, child).unwrap();
    let centers = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&centers);
    let mut animation = ApplyMethod::pointwise_to_center(source, move |p| {
        captured.borrow_mut().push(p);
        [p[0] * p[0], p[0], p[2]]
    });
    assert_eq!(
        animation.state().config.name,
        "ApplyPointwiseFunctionToCenter"
    );
    assert_eq!(animation.get_run_time(), 1.0);
    assert!(centers.borrow().is_empty());
    stage.shift(source, [2.0, 0.0, 0.0]);
    animation.state_mut().config.rate_func = RateFunc::linear();
    animation.begin(&mut stage).unwrap();
    assert_eq!(*centers.borrow(), vec![[3.0, 0.0, 0.0]]);
    animation.interpolate(&mut stage, 0.5);
    close(stage.get_center(source), [6.0, 1.5, 0.0]);
    close(stage.get_center(child), [7.0, 1.5, 0.0]);
    animation.finish(&mut stage);
    close(stage.get_center(source), [9.0, 3.0, 0.0]);
    close(stage.get_center(child), [10.0, 3.0, 0.0]);
    assert_eq!(centers.borrow().len(), 1);
}

#[test]
fn retained_center_function_repeats_on_new_starting_geometry() {
    let mut stage = Stage::new();
    let source = stage.add(Mobject::from_points(&[[1.0, 0.0, 0.0]]));
    let mut increment = 1.0;
    let mut animation = ApplyMethod::pointwise_to_center(source, move |p| {
        let result = [p[0] + increment, p[1], p[2]];
        increment += 1.0;
        result
    });
    for expected in [2.0, 4.0, 7.0] {
        animation.begin(&mut stage).unwrap();
        animation.finish(&mut stage);
        close(stage.get_center(source), [expected, 0.0, 0.0]);
    }
}

#[test]
fn center_map_in_succession_observes_the_completed_predecessor() {
    let mut stage = Stage::new();
    let source = stage.add(Mobject::from_points(&[[1.0, 0.0, 0.0]]));
    let a = ApplyMethod::pointwise_to_center(source, |p| [p[0] + 2.0, p[1], p[2]])
        .with_config(linear());
    let b = ApplyMethod::pointwise_to_center(source, |p| [p[0] * p[0], p[1], p[2]])
        .with_config(linear());
    let mut animation = Succession::new(&mut stage, vec![Box::new(a), Box::new(b)]).unwrap();
    animation.begin(&mut stage).unwrap();
    animation.interpolate(&mut stage, 0.75);
    close(stage.get_center(source), [6.0, 0.0, 0.0]);
    animation.finish(&mut stage);
    close(stage.get_center(source), [9.0, 0.0, 0.0]);
}

#[test]
fn mapped_callback_is_not_evaluated_during_invalid_begin_or_preflight() {
    let mut stage = Stage::new();
    let source = stage.add(Mobject::from_points(&[[1.0, 0.0, 0.0]]));
    let mut animation = ApplyMethod::pointwise(source, |_| panic!("invalid path invoked map"))
        .with_path_arc(f64::NAN, [0.0, 0.0, 1.0]);
    assert!(animation.validate_begin(&stage).is_err());
    assert!(animation.begin(&mut stage).is_err());
    let stale = stage.add(Mobject::from_points(&[[0.0; 3]]));
    stage.delete(stale).unwrap();
    let mut animation =
        ApplyMethod::pointwise_to_center(source, |_| panic!("stale dependency invoked map"))
            .with_dependencies([stale]);
    assert!(animation.validate_begin(&stage).is_err());
    assert!(animation.begin(&mut stage).is_err());
    close(stage.get_center(source), [1.0, 0.0, 0.0]);
}
