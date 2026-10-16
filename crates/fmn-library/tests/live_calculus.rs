//! Live chart calculus against analytic corners and quadratic restrictions.
use fmn_core::constants::{BLUE, RED};
use fmn_geom::QuadPath;
use fmn_library::graphs::SamplingBudget;
use fmn_library::{
    Axes, AxisConfig, CoordinateFrame, CoordinateFrameError, CoordinateSystem, CoordsError,
    FontBook, LiveCoordinateSystem, RiemannConfig, VMobject,
};
use fmn_mobject::{Mob, Placement, Stage};
use std::cell::{Cell, RefCell};

fn setup(budget: usize) -> (Axes, Stage, Mob, LiveCoordinateSystem) {
    let axes = Axes::new()
        .x_range([-2.0, 3.0, 1.0])
        .y_range([-1.0, 3.0, 1.0])
        .width(5.0)
        .height(4.0)
        .sampling_budget(SamplingBudget::new(budget))
        .axis_config(AxisConfig {
            include_tip: Some(false),
            include_ticks: Some(false),
            ..AxisConfig::default()
        })
        .build(&FontBook::bundled().unwrap())
        .unwrap();
    let mut stage = Stage::new();
    let root = stage.add(axes.clone());
    stage.add_to_scene(root).unwrap();
    let live = axes.bind(&stage, root).unwrap();
    (axes, stage, root, live)
}
fn affine() -> Placement {
    Placement::new(
        [[-1.2, 0.3, 0.0], [0.4, 0.8, 0.0], [0.5, -0.2, 1.0]],
        [2.1, -0.7, 0.4],
    )
}
fn near(actual: [f64; 3], expected: [f64; 3]) {
    for i in 0..3 {
        assert!(
            (actual[i] - expected[i]).abs() < 3e-6 * (1.0 + expected[i].abs()),
            "{actual:?} != {expected:?}"
        );
    }
}
fn graph(frame: &CoordinateFrame, points: &[[f64; 3]]) -> VMobject {
    VMobject::from_points(points.iter().map(|p| frame.c2p(p)).collect())
}

#[test]
fn riemann_corners_follow_affine_axes_with_signed_colors_and_partial_bins() {
    let (axes, mut stage, root, live) = setup(64);
    let original = live.snapshot(&stage).unwrap();
    stage.apply_affine(root, affine());
    for mode in ["left", "right", "center"] {
        let calls = RefCell::new(Vec::new());
        let f = |x| {
            calls.borrow_mut().push(x);
            x - 0.7
        };
        let config = RiemannConfig {
            colors: vec![BLUE],
            ..RiemannConfig::default()
        };
        let bins = live
            .riemann_rectangles_styled(&stage, &f, Some([0.0, 1.3]), Some(0.6), mode, &config)
            .unwrap();
        assert_eq!(bins.children().len(), 3);
        for (i, (left, right)) in [(0.0, 0.6), (0.6, 1.2), (1.2, 1.3)].into_iter().enumerate() {
            let sample = match mode {
                "left" => left,
                "right" => right,
                _ => left + (right - left) / 2.0,
            };
            assert_eq!(calls.borrow()[i], sample);
            let height = sample - 0.7;
            let child = &bins.children()[i];
            for (p, data) in child.path().unwrap().anchors().iter().zip([
                [left, 0.0, 0.0],
                [right, 0.0, 0.0],
                [right, height, 0.0],
                [left, height, 0.0],
            ]) {
                near(*p, affine().apply_point(axes.c2p(&data)));
            }
            assert_eq!(
                child.style().fill_color,
                if height < 0.0 { RED } else { BLUE }
            );
        }
        assert_eq!(calls.borrow().len(), 3);
        let unsigned = live
            .riemann_rectangles_styled(
                &stage,
                &|_| -2.0,
                Some([0.0, 1.0]),
                Some(0.5),
                mode,
                &RiemannConfig {
                    show_signed_area: false,
                    colors: vec![BLUE],
                    ..config
                },
            )
            .unwrap();
        assert!(
            unsigned
                .children()
                .iter()
                .all(|v| v.style().fill_color == BLUE)
        );
    }
    let held = original
        .get_riemann_rectangles(&|_| 1.0, Some([0.0, 1.0]), Some(1.0), "left")
        .unwrap();
    near(held.children()[0].points()[0], axes.c2p(&[0.0, 0.0]));
}

#[test]
fn invalid_riemann_requests_do_not_evaluate_callbacks_or_return_partial_geometry() {
    let (_, mut stage, root, live) = setup(4);
    let calls = Cell::new(0);
    let f = |_| {
        calls.set(calls.get() + 1);
        1.0
    };
    for (range, dx, mode) in [
        ([0.0, 10.0], 1.0, "left"),
        ([1.0, 0.0], 1.0, "left"),
        ([0.0, 1.0], 0.0, "left"),
        ([0.0, 1.0], f64::NAN, "left"),
        ([0.0, 1.0], 1.0, "other"),
    ] {
        assert!(
            live.get_riemann_rectangles(&stage, &f, Some(range), Some(dx), mode)
                .is_err()
        );
        assert_eq!(calls.get(), 0);
    }
    let bad = RiemannConfig {
        fill_opacity: f64::NAN,
        ..RiemannConfig::default()
    };
    assert!(
        live.riemann_rectangles_styled(&stage, &f, Some([0.0, 1.0]), Some(1.0), "left", &bad)
            .is_err()
    );
    assert_eq!(calls.get(), 0);
    assert!(
        live.get_riemann_rectangles(
            &stage,
            &|_| f64::INFINITY,
            Some([0.0, 1.0]),
            Some(1.0),
            "left"
        )
        .is_err()
    );
    assert!(
        live.get_riemann_rectangles(&stage, &|_| f64::MAX, Some([0.0, 1.0]), Some(1.0), "left")
            .is_err()
    );
    let empty = live
        .get_riemann_rectangles(&stage, &f, Some([1.0, 1.0]), Some(1.0), "left")
        .unwrap();
    assert!(empty.children().is_empty());
    stage.detach(root, live.axis(0).unwrap());
    assert!(
        live.get_riemann_rectangles(&stage, &f, None, None, "left")
            .is_err()
    );
    assert_eq!(calls.get(), 0);
}

#[test]
fn areas_clip_actual_quadratic_x_not_curve_index_and_preserve_source() {
    let (_, mut stage, root, live) = setup(100);
    stage.apply_affine(root, affine());
    let frame = live.snapshot(&stage).unwrap();
    // x(t)=4t² and y(t)=1+2t. Bounds .25..2.25 mean t=.25...75.
    let curve = graph(&frame, &[[0.0, 1.0, 0.0], [0.0, 2.0, 0.0], [4.0, 3.0, 0.0]]);
    let before = curve.points().to_vec();
    let region = live
        .get_area_under_graph(&stage, &curve, Some([0.25, 2.25]))
        .unwrap();
    let expected = [[0.25, 1.5, 0.0], [0.75, 2.0, 0.0], [2.25, 2.5, 0.0]];
    for (actual, data) in region.points()[..3].iter().zip(expected) {
        near(*actual, frame.c2p(&data));
    }
    let anchors = region.path().unwrap().anchors();
    near(anchors[2], frame.c2p(&[2.25, 0.0]));
    near(anchors[3], frame.c2p(&[0.25, 0.0]));
    near(*anchors.last().unwrap(), anchors[0]);
    let reversed = VMobject::from_points(curve.points().iter().rev().copied().collect());
    let backward = frame
        .get_area_under_graph(&reversed, Some([0.25, 2.25]))
        .unwrap();
    assert_eq!(backward.points().len(), region.points().len());
    for (a, b) in backward.points().iter().zip(region.points()) {
        near(*a, *b);
    }
    assert_eq!(curve.points(), before);
}

#[test]
fn areas_preserve_discontinuity_gaps_and_intersect_the_drawn_domain() {
    let (_, stage, _, live) = setup(64);
    let frame = live.snapshot(&stage).unwrap();
    let mut path = QuadPath::new();
    for data in [
        [[0.0, 2.0, 0.0], [0.5, 2.0, 0.0], [1.0, 2.0, 0.0]],
        [[2.0, 1.0, 0.0], [3.0, 1.0, 0.0], [4.0, 1.0, 0.0]],
    ] {
        path.add_subpath(&data.map(|p| frame.c2p(&p))).unwrap();
    }
    let curve = VMobject::from_path(&path);
    let area = frame
        .get_area_under_graph(&curve, Some([-100.0, 100.0]))
        .unwrap();
    let regions = area.path().unwrap();
    assert_eq!(regions.subpaths().len(), 2);
    for (points, (left, right, height)) in regions
        .subpaths()
        .iter()
        .zip([(0.0, 1.0, 2.0), (2.0, 4.0, 1.0)])
    {
        let corners = QuadPath::from_points(points.to_vec()).unwrap().anchors();
        for (point, data) in corners.iter().zip([
            [left, height],
            [right, height],
            [right, 0.0],
            [left, 0.0],
            [left, height],
        ]) {
            near(*point, frame.c2p(&data));
        }
    }
    for bounds in [[1.2, 1.8], [2.0, 2.0], [5.0, 6.0]] {
        assert!(
            frame
                .get_area_under_graph(&curve, Some(bounds))
                .unwrap()
                .points()
                .is_empty()
        );
    }
}

#[test]
fn area_refuses_foldbacks_bad_bounds_invalid_records_and_exhausted_output_budget() {
    let (_, stage, _, live) = setup(64);
    let frame = live.snapshot(&stage).unwrap();
    let fold = graph(&frame, &[[0.0, 1.0, 0.0], [2.0, 1.0, 0.0], [1.0, 2.0, 0.0]]);
    assert!(matches!(
        frame.get_area_under_graph(&fold, None),
        Err(CoordsError::LiveFrame(
            CoordinateFrameError::InvalidGeometry(_)
        ))
    ));
    let line = graph(&frame, &[[0.0, 1.0, 0.0], [0.5, 1.0, 0.0], [1.0, 1.0, 0.0]]);
    for bounds in [[2.0, 1.0], [0.0, f64::INFINITY]] {
        assert!(frame.get_area_under_graph(&line, Some(bounds)).is_err());
    }
    for points in [
        vec![[f64::NAN, 0.0, 0.0]; 3],
        vec![[f64::MAX, 0.0, 0.0]; 3],
        vec![[0.0; 3]; 2],
    ] {
        assert!(
            frame
                .get_area_under_graph(&VMobject::from_points(points), None)
                .is_err()
        );
    }
    let (_, stage, _, live) = setup(8);
    let frame = live.snapshot(&stage).unwrap();
    let line = graph(&frame, &[[0.0, 1.0, 0.0], [0.5, 1.0, 0.0], [1.0, 1.0, 0.0]]);
    assert!(matches!(
        frame.get_area_under_graph(&line, None),
        Err(CoordsError::Sampling(_))
    ));
}

#[test]
fn regenerating_regions_during_native_axis_animation_uses_the_current_frame() {
    use fmn_anim::{AnimConfig, Animation, RateFunc, Transform};
    let (axes, mut stage, root, live) = setup(100);
    let target = stage.copy_family(root).unwrap();
    let shift = [1.5, -0.75, 0.5];
    stage.shift(target, shift);
    let mut animation = Transform::new(root, target).with_config(AnimConfig {
        rate_func: RateFunc::linear(),
        ..AnimConfig::default()
    });
    animation.begin(&mut stage).unwrap();
    for alpha in [0.0, 0.25, 0.75, 1.0] {
        animation.interpolate(&mut stage, alpha);
        let base = axes.c2p(&[0.0, 0.0]);
        let expected = std::array::from_fn(|i| base[i] + alpha * shift[i]);
        let bars = live
            .get_riemann_rectangles(&stage, &|_| 1.0, Some([0.0, 1.0]), Some(0.5), "center")
            .unwrap();
        near(bars.children()[0].points()[0], expected);
        let frame = live.snapshot(&stage).unwrap();
        let curve = graph(&frame, &[[0.0, 1.0, 0.0], [0.5, 1.0, 0.0], [1.0, 1.0, 0.0]]);
        let area = live.get_area_under_graph(&stage, &curve, None).unwrap();
        near(area.path().unwrap().anchors()[3], expected);
        let off_plane = curve.clone().shifted([0.0, 0.0, 1.0]);
        assert!(live.get_area_under_graph(&stage, &off_plane, None).is_err());
    }
    animation.finish(&mut stage);
}
