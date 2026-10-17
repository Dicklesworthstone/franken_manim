//! Differential constructions are checked against affine and analytic geometry.
use fmn_anim::{AnimConfig, Animation, RateFunc, Transform};
use fmn_core::constants::{GREEN, RED, YELLOW};
use fmn_library::graphs::SamplingBudget;
use fmn_library::{
    Axes, AxisConfig, CoordinateSystem, CoordsError, FontBook, LiveCoordinateSystem, Style,
    VMobject,
};
use fmn_mobject::{Mob, Placement, Stage};
use std::cell::{Cell, RefCell};

fn setup(budget: usize) -> (Axes, Stage, Mob, LiveCoordinateSystem) {
    let axes = Axes::new()
        .x_range([-2.0, 2.0, 1.0])
        .y_range([-2.0, 2.0, 1.0])
        .width(4.0)
        .height(4.0)
        .axis_config(AxisConfig {
            include_tip: Some(false),
            include_ticks: Some(false),
            ..AxisConfig::default()
        })
        .build(&FontBook::bundled().unwrap())
        .unwrap()
        .sampling_budget(SamplingBudget::new(budget));
    let mut stage = Stage::new();
    let root = stage.add(axes.clone());
    stage.add_to_scene(root).unwrap();
    let live = axes.bind(&stage, root).unwrap();
    (axes, stage, root, live)
}
fn near(a: [f64; 3], b: [f64; 3]) {
    for i in 0..3 {
        assert!(
            (a[i] - b[i]).abs() < 3e-6 * (1.0 + b[i].abs()),
            "{a:?} != {b:?}"
        );
    }
}
fn norm(p: [f64; 3]) -> f64 {
    p.iter().map(|x| x * x).sum::<f64>().sqrt()
}
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn ends(v: &VMobject) -> ([f64; 3], [f64; 3]) {
    (*v.points().first().unwrap(), *v.points().last().unwrap())
}
fn affine() -> Placement {
    Placement::new(
        [[-1.0, 0.5, 0.0], [0.25, 1.5, 0.0], [0.75, -0.25, 1.0]],
        [1.0, -0.5, 0.25],
    )
}

#[test]
fn tangent_uses_full_affine_direction_center_and_world_length() {
    let (axes, mut stage, root, live) = setup(32);
    stage.apply_affine(root, affine());
    let frame = live.snapshot(&stage).unwrap();
    for dx in [-0.25, 0.125, 0.5] {
        let x = 0.5;
        let calls = RefCell::new(Vec::new());
        let f = |t: f64| {
            calls.borrow_mut().push(t);
            t * t
        };
        let direction = affine().apply_vector([dx, (x + dx) * (x + dx) - x * x, 0.0]);
        let unit = direction.map(|v| v / norm(direction));
        near(live.tangent_direction(&stage, x, &f, dx).unwrap(), unit);
        assert_eq!(*calls.borrow(), vec![x, x + dx]);
        calls.borrow_mut().clear();
        let style = Style::default().stroke(RED, 3.0, 0.6);
        let tangent = frame.tangent_line_styled(x, &f, 2.5, dx, style).unwrap();
        assert_eq!(*calls.borrow(), vec![x, x + dx]);
        assert_eq!(tangent.style(), style);
        let center = affine().apply_point(axes.c2p(&[x, x * x]));
        let (a, b) = ends(&tangent);
        near(a, std::array::from_fn(|i| center[i] - unit[i] * 1.25));
        near(b, std::array::from_fn(|i| center[i] + unit[i] * 1.25));
        assert!((norm(sub(b, a)) - 2.5).abs() < 1e-12);
        assert!(
            sub(b, a)[2].abs() > 0.01,
            "the 3D component must not be flattened"
        );
    }
    // The identity graph has bit-equal dx and dy even at the EPSILON step,
    // so this oracle isolates affine slope from callback cancellation.
    let f = |x| x;
    let d = affine().apply_vector([1.0, 1.0, 0.0]);
    let angle = live.angle_of_tangent(&stage, 0.25, &f).unwrap();
    assert!((angle - d[1].atan2(d[0])).abs() < 1e-8);
    assert!((live.slope_of_tangent(&stage, 0.25, &f).unwrap() - d[1] / d[0]).abs() < 1e-8);
}

#[test]
fn secant_group_run_and_rise_follow_numeric_axes_and_sample_only_twice() {
    let (axes, mut stage, root, live) = setup(32);
    stage.apply_affine(root, affine());
    for dx in [-0.5, 0.5] {
        let x = 0.5;
        let calls = RefCell::new(Vec::new());
        let f = |t: f64| {
            calls.borrow_mut().push(t);
            t * t
        };
        let group = live
            .get_secant_slope_group(&stage, x, &f, dx, None)
            .unwrap();
        assert_eq!(*calls.borrow(), vec![x, x + dx]);
        assert_eq!(group.children().len(), 3);
        let p0 = affine().apply_point(axes.c2p(&[x, x * x]));
        let corner = affine().apply_point(axes.c2p(&[x + dx, x * x]));
        let p1 = affine().apply_point(axes.c2p(&[x + dx, (x + dx) * (x + dx)]));
        for (child, (start, end, color)) in
            group
                .children()
                .iter()
                .zip([(p0, corner, YELLOW), (corner, p1, GREEN), (p0, p1, RED)])
        {
            let (a, b) = ends(child);
            near(a, start);
            near(b, end);
            assert_eq!(child.style().stroke_color, color);
        }
        let extended = live
            .get_secant_slope_group(&stage, x, &f, dx, Some(4.0))
            .unwrap();
        let (a, b) = ends(&extended.children()[2]);
        assert!((norm(sub(b, a)) - 4.0).abs() < 1e-12);
        near(
            std::array::from_fn(|i| (a[i] + b[i]) * 0.5),
            std::array::from_fn(|i| (p0[i] + p1[i]) * 0.5),
        );
    }
}

#[test]
fn translating_far_from_origin_does_not_erase_tangent_direction() {
    let (_, mut stage, root, live) = setup(32);
    stage.shift(root, [1e14, -1e14, 1e14]);
    let frame = live.snapshot(&stage).unwrap();
    let f = |x: f64| 2.0 * x;
    let small = 1e-7;
    // This is the planted negative: direct world-point subtraction loses both probes.
    assert_eq!(frame.c2p(&[0.0, 0.0]), frame.c2p(&[small, 2.0 * small]));
    let actual = frame.tangent_direction(0.0, &f, small).unwrap();
    near(actual, [1.0 / 5.0_f64.sqrt(), 2.0 / 5.0_f64.sqrt(), 0.0]);
}

#[test]
fn vertical_world_tangents_keep_3d_geometry_and_refuse_undefined_projections() {
    let (_, mut stage, root, live) = setup(32);
    stage.apply_affine(
        root,
        Placement::new(
            [[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]],
            [0.0; 3],
        ),
    );
    let f = |_| 0.0;
    let frame = live.snapshot(&stage).unwrap();
    near(
        frame.tangent_direction(0.0, &f, 0.25).unwrap(),
        [0.0, 0.0, 1.0],
    );
    assert!(frame.angle_of_tangent(0.0, &f).is_err());
    assert!(frame.slope_of_tangent(0.0, &f).is_err());
    let tangent = frame.get_tangent_line(0.0, &f, 2.0).unwrap();
    let (a, b) = ends(&tangent);
    near(a, [0.0, 0.0, -1.0]);
    near(b, [0.0, 0.0, 1.0]);
}

#[test]
fn invalid_differential_requests_refuse_before_callbacks_and_stop_at_first_bad_sample() {
    let (_, mut stage, root, live) = setup(32);
    let calls = Cell::new(0);
    let f = |_: f64| {
        calls.set(calls.get() + 1);
        0.0
    };
    let frame = live.snapshot(&stage).unwrap();
    for (x, dx) in [
        (0.0, 0.0),
        (0.0, f64::NAN),
        (f64::INFINITY, 1.0),
        (1e300, 1.0),
        (f64::MAX, f64::MAX),
    ] {
        assert!(frame.tangent_direction(x, &f, dx).is_err());
        assert!(frame.get_secant_slope_group(x, &f, dx, None).is_err());
    }
    for length in [0.0, -1.0, f64::INFINITY, f64::NAN] {
        assert!(frame.get_tangent_line(0.0, &f, length).is_err());
        assert!(
            frame
                .get_secant_slope_group(0.0, &f, 0.5, Some(length))
                .is_err()
        );
    }
    assert!(
        frame
            .tangent_line_styled(0.0, &f, 2.0, 0.5, Style::default().stroke_width(-1.0))
            .is_err()
    );
    assert_eq!(calls.get(), 0);
    for budget in [1, 2, 8] {
        let (_, s, _, l) = setup(budget);
        assert!(matches!(
            l.get_secant_slope_group(&s, 0.0, &f, 0.5, None),
            Err(CoordsError::Sampling(_))
        ));
    }
    assert_eq!(calls.get(), 0);
    let bad = |_: f64| {
        calls.set(calls.get() + 1);
        f64::NAN
    };
    assert!(frame.get_tangent_line(0.0, &bad, 2.0).is_err());
    assert_eq!(calls.get(), 1);
    let bad_second = |x: f64| {
        calls.set(calls.get() + 1);
        if x == 0.0 { 0.0 } else { f64::INFINITY }
    };
    assert!(frame.get_tangent_line(0.0, &bad_second, 2.0).is_err());
    assert_eq!(calls.get(), 3);
    stage.detach(root, live.axis(0).unwrap());
    assert!(live.get_tangent_line(&stage, 0.0, &f, 2.0).is_err());
    assert_eq!(calls.get(), 3);
}

#[test]
fn tangent_and_secant_helpers_follow_real_animation_samples_without_mutating_snapshots() {
    let (_, mut stage, root, live) = setup(32);
    let frozen = live.snapshot(&stage).unwrap();
    let target = stage.copy_family(root).unwrap();
    stage.shift(target, [2.0, -1.0, 0.5]);
    let mut animation = Transform::new(root, target).with_config(AnimConfig {
        rate_func: RateFunc::linear(),
        ..AnimConfig::default()
    });
    animation.begin(&mut stage).unwrap();
    let f = |x| x * 0.5;
    let original = frozen.get_tangent_line(0.5, &f, 2.0).unwrap();
    for alpha in [0.0, 0.25, 0.75, 0.25, 1.0] {
        animation.interpolate(&mut stage, alpha);
        let tangent = live.get_tangent_line(&stage, 0.5, &f, 2.0).unwrap();
        for (&a, &b) in tangent.points().iter().zip(original.points()) {
            near(a, [b[0] + 2.0 * alpha, b[1] - alpha, b[2] + 0.5 * alpha]);
        }
        let group = live
            .get_secant_slope_group(&stage, 0.5, &f, 0.25, None)
            .unwrap();
        near(
            ends(&group.children()[0]).0,
            live.c2p(&stage, &[0.5, 0.25]).unwrap(),
        );
    }
    animation.finish(&mut stage);
    assert_eq!(
        frozen.get_tangent_line(0.5, &f, 2.0).unwrap().points(),
        original.points()
    );
}
