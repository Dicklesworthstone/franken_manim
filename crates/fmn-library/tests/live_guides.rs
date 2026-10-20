//! Current affine axes, not detached builder state, own projection geometry.
use fmn_core::constants::{GREY_A, RED};
use fmn_library::graphs::SamplingBudget;
use fmn_library::{
    Axes, AxisConfig, CoordinateFrame, CoordinateSystem, CoordsError, DashedLine, FontBook,
    LiveCoordinateSystem, Style, ThreeDAxes, VMobject,
};
use fmn_mobject::{Mob, Placement, Stage};
use std::cell::Cell;

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
fn near(a: [f64; 3], b: [f64; 3]) {
    for i in 0..3 {
        assert!(
            (a[i] - b[i]).abs() < 3e-6 * (1.0 + b[i].abs()),
            "{a:?} != {b:?}"
        );
    }
}
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn expected_projection(frame: &CoordinateFrame, index: usize, p: [f64; 3]) -> [f64; 3] {
    let origin = frame.c2p(&[0.0; 3]);
    let mut coords = [0.0; 3];
    coords[index] = 1.0;
    let direction = sub(frame.c2p(&coords), origin);
    let factor = dot(sub(p, origin), direction) / dot(direction, direction);
    std::array::from_fn(|i| origin[i] + direction[i] * factor)
}
fn same_geometry(actual: &VMobject, expected: &VMobject) {
    assert_eq!(actual.points().len(), expected.points().len());
    assert_eq!(actual.children().len(), expected.children().len());
    assert_eq!(actual.style(), expected.style());
    for (&a, &b) in actual.points().iter().zip(expected.points()) {
        near(a, b);
    }
    for (a, b) in actual.children().iter().zip(expected.children()) {
        same_geometry(a, b);
    }
}

#[test]
fn guides_follow_sheared_reflected_and_out_of_plane_axes() {
    let (axes, mut stage, root, live) = setup(4096);
    for affine in [
        Placement::IDENTITY,
        Placement::from_translation([2.0, -1.0, 0.5]),
        Placement::new(
            [[-1.2, 0.3, 0.0], [0.4, 0.8, 0.0], [0.5, -0.2, 1.0]],
            [2.1, -0.7, 0.4],
        ),
        Placement::new(
            [[0.0, 0.0, 1.0], [0.0, 2.0, 0.0], [-0.5, 0.0, 0.0]],
            [-2.0, 0.0, 1.0],
        ),
    ] {
        // A fresh chart each pass prevents accumulated test transforms.
        let mut trial = stage.snapshot().materialize();
        trial.apply_affine(root, affine);
        let frame = live.snapshot(&trial).unwrap();
        let point = affine.apply_point(axes.c2p(&[1.1, 1.7]));
        for index in 0..2 {
            let expected = expected_projection(&frame, index, point);
            near(
                live.project_to_axis(&trial, index, point).unwrap(),
                expected,
            );
            let line = live
                .get_line_from_axis_to_point(&trial, index, point)
                .unwrap();
            let oracle = DashedLine::new(expected, point)
                .style(Style::default().stroke(GREY_A, 2.0, 1.0))
                .build()
                .unwrap();
            same_geometry(&line, &oracle);
            same_geometry(
                &line,
                &frame.get_line_from_axis_to_point(index, point).unwrap(),
            );
        }
        let style = Style::default().stroke(RED, 3.5, 0.4);
        let styled = live
            .line_from_axis_to_point_styled(&trial, 0, point, style)
            .unwrap();
        assert!(styled.children().iter().all(|d| d.style() == style));
    }
    // The axis-aligned convenience methods are projections, not data-zeroing.
    stage.apply_affine(
        root,
        Placement::new(
            [[1.0, 1.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            [0.0; 3],
        ),
    );
    let p = live.c2p(&stage, &[1.0, 1.0]).unwrap();
    let projected = live.project_to_axis(&stage, 0, p).unwrap();
    assert!((projected[0] - live.c2p(&stage, &[1.0, 0.0]).unwrap()[0]).abs() > 0.5);
}

#[test]
fn graph_queries_invoke_once_and_frozen_guides_survive_axis_movement() {
    let (_, mut stage, root, live) = setup(4096);
    let frozen = live.snapshot(&stage).unwrap();
    let calls = Cell::new(0);
    let f = |x: f64| {
        calls.set(calls.get() + 1);
        x * x
    };
    let p = live.input_to_graph_point(&stage, 0.7, &f).unwrap();
    assert_eq!(calls.get(), 1);
    near(live.i2gp(&stage, 0.7, &f).unwrap(), p);
    assert_eq!(calls.get(), 2);
    let old = frozen.get_v_line_to_graph(0.7, &f).unwrap();
    assert_eq!(calls.get(), 3);
    stage.shift(root, [1.5, -0.75, 0.4]);
    let new = live.get_v_line_to_graph(&stage, 0.7, &f).unwrap();
    assert_eq!(calls.get(), 4);
    let translated = old.clone().shifted([1.5, -0.75, 0.4]);
    same_geometry(&new, &translated);
    same_geometry(&old, &frozen.get_v_line_to_graph(0.7, &f).unwrap());
    let h = live.get_h_line_to_graph(&stage, 0.7, &f).unwrap();
    same_geometry(
        &h,
        &live
            .get_h_line(&stage, live.c2p(&stage, &[0.7, 0.49]).unwrap())
            .unwrap(),
    );
}

#[test]
fn three_dimensional_projection_accepts_all_axes_and_off_plane_points() {
    let axes = ThreeDAxes::new()
        .x_range([-2.0, 2.0, 1.0])
        .y_range([-2.0, 2.0, 1.0])
        .z_range([-2.0, 2.0, 1.0])
        .width(4.0)
        .height(4.0)
        .depth(4.0)
        .axis_config(AxisConfig {
            include_tip: Some(false),
            include_ticks: Some(false),
            ..AxisConfig::default()
        })
        .build(&FontBook::bundled().unwrap())
        .unwrap();
    let mut stage = Stage::new();
    let root = stage.add(axes.clone());
    let live = axes.bind(&stage, root).unwrap();
    stage.apply_affine(
        root,
        Placement::new(
            [[1.0, 0.2, 0.3], [0.1, 0.8, 0.0], [0.0, 0.4, -1.0]],
            [0.5, -1.0, 0.2],
        ),
    );
    let frame = live.snapshot(&stage).unwrap();
    let point = [2.0, 1.0, 3.0];
    for axis in 0..3 {
        near(
            live.project_to_axis(&stage, axis, point).unwrap(),
            expected_projection(&frame, axis, point),
        );
        assert!(
            !live
                .get_line_from_axis_to_point(&stage, axis, point)
                .unwrap()
                .children()
                .is_empty()
        );
    }
    assert!(live.get_line_from_axis_to_point(&stage, 3, point).is_err());
    assert!(
        live.get_line_from_axis_to_point(&stage, usize::MAX, point)
            .is_err()
    );
}

#[test]
fn guide_admission_refuses_bad_inputs_without_callbacks_or_stage_mutation() {
    let (_, mut stage, root, live) = setup(4096);
    let calls = Cell::new(0);
    let f = |_: f64| {
        calls.set(calls.get() + 1);
        1.0
    };
    for x in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(live.input_to_graph_point(&stage, x, &f).is_err());
        assert!(live.get_v_line_to_graph(&stage, x, &f).is_err());
        assert!(live.get_h_line_to_graph(&stage, x, &f).is_err());
    }
    assert_eq!(calls.get(), 0);
    let frame = live.snapshot(&stage).unwrap();
    assert!(frame.get_v_line([f64::MAX, 0.0, 0.0]).is_err());
    assert!(frame.try_input_to_graph_point(1.0, &|_| f64::NAN).is_err());
    assert!(frame.try_input_to_graph_point(1.0, &|_| f64::MAX).is_err());
    for style in [
        Style::default().stroke_width(-1.0),
        Style::default().stroke_width(f64::INFINITY),
        Style::default().fill_opacity(1.1),
    ] {
        assert!(
            frame
                .line_from_axis_to_point_styled(0, [1.0, 1.0, 0.0], style)
                .is_err()
        );
    }
    let axis = live.axis(0).unwrap();
    stage.detach(root, axis);
    assert!(live.get_v_line_to_graph(&stage, 1.0, &f).is_err());
    assert_eq!(calls.get(), 0);
    // Snapshot queries remain valid without their original Stage topology.
    assert!(frame.get_v_line_to_graph(1.0, &f).is_ok());
    assert_eq!(calls.get(), 1);
    let (_, stage, _, live) = setup(2);
    assert!(matches!(
        live.get_v_line_to_graph(&stage, 1.0, &f),
        Err(CoordsError::Sampling(_))
    ));
    assert_eq!(calls.get(), 1);
    let (_, stage, _, live) = setup(3);
    assert!(matches!(
        live.get_v_line(&stage, [0.0, 4.0, 0.0]),
        Err(CoordsError::Sampling(_))
    ));
}

#[test]
fn projection_handles_extreme_axis_scales_and_degenerate_guides() {
    for size in [1e-150, 1e150] {
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
            .unwrap();
        let mut stage = Stage::new();
        let root = stage.add(axes.clone());
        let live = axes.bind(&stage, root).unwrap();
        stage.apply_affine(
            root,
            Placement::new(
                [[size, 0.0, 0.0], [0.0, size, 0.0], [0.0, 0.0, 1.0]],
                [0.0; 3],
            ),
        );
        let point = [1.0, 2.0, 3.0];
        near(
            live.project_to_axis(&stage, 0, point).unwrap(),
            [1.0, 0.0, 0.0],
        );
        near(
            live.project_to_axis(&stage, 1, point).unwrap(),
            [0.0, 2.0, 0.0],
        );
    }
    let (_, stage, _, live) = setup(4096);
    let p = live.c2p(&stage, &[1.0, 0.0]).unwrap();
    let guide = live.get_v_line(&stage, p).unwrap();
    assert!(
        guide
            .children()
            .iter()
            .flat_map(|d| d.points())
            .flatten()
            .all(|x| x.is_finite())
    );
}

#[test]
fn native_updaters_regenerate_guides_after_each_transform_sample() {
    use fmn_anim::{AnimConfig, Animation, RateFunc, Transform};
    let (_, mut stage, root, live) = setup(4096);
    let binding = live.clone();
    let guide = stage.always_redraw(move |s| {
        let geometry = binding.get_v_line_to_graph(s, 0.75, &|x| x * x).unwrap();
        s.add(geometry)
    });
    stage.add_to_scene(guide).unwrap();
    let target = stage.copy_family(root).unwrap();
    stage.shift(target, [2.0, -1.0, 0.5]);
    let mut transform = Transform::new(root, target).with_config(AnimConfig {
        rate_func: RateFunc::linear(),
        ..AnimConfig::default()
    });
    transform.begin(&mut stage).unwrap();
    for alpha in [0.0, 0.25, 0.75, 0.25, 1.0] {
        transform.interpolate(&mut stage, alpha);
        stage.update(0.125);
        let frame = live.snapshot(&stage).unwrap();
        let point = frame.c2p(&[0.75, 0.75 * 0.75]);
        let projection = expected_projection(&frame, 0, point);
        let group = stage.get(guide).unwrap().submobjects()[0];
        let first_dash = stage.get(group).unwrap().submobjects()[0];
        let start = stage.get_start(first_dash).unwrap();
        near(start, projection);
        for child in stage.family(group) {
            for p in stage.get_points(child).unwrap() {
                // Every dash stays on the analytical normal from the current axis.
                let d = sub(point, projection);
                let off = sub(p, projection);
                let cross = [
                    off[1] * d[2] - off[2] * d[1],
                    off[2] * d[0] - off[0] * d[2],
                    off[0] * d[1] - off[1] * d[0],
                ];
                near(cross, [0.0; 3]);
            }
        }
    }
    transform.finish(&mut stage);
}
