//! Actual retained records, analytic kernels and pixels, without shader doubles.
use fmn_core::color::LinearRgba;
use fmn_mobject::{Mob, Mobject, Placement, RecordBuffer, RecordSchema, Stage};
use fmn_render::{
    Camera, CameraConfig, EngineIdentity, FrameConfig, RenderPlan, RenderPlanLimits,
    RetainedFrameRenderer, RetainedFrameRendererConfig, ScreenMap, StrokeKnot, StrokeProfile,
    Style, StyleTable, Tiling, Viewport,
};

fn object(points: &[[f32; 3]], widths: &[f32]) -> Mobject {
    let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len()).unwrap();
    for (i, point) in points.iter().enumerate() {
        buffer.write(i, "point", point);
        buffer.write(i, "stroke_width", &[widths[i]]);
        buffer.write(i, "stroke_rgba", &[0.0, 0.0, 1.0, 1.0]);
    }
    Mobject::from_buffer(buffer)
}
fn fixture() -> (Stage, Mob) {
    let mut stage = Stage::new();
    let mob = stage.add(object(
        &[
            [-3., 0., 0.],
            [-1.5, 0., 0.],
            [0., 0., 0.],
            [1.5, 0., 0.],
            [3., 0., 0.],
        ],
        &[0., 40., 80., 40., 0.],
    ));
    stage.add_to_scene(mob).unwrap();
    (stage, mob)
}
fn renderer(threads: usize) -> (RetainedFrameRenderer, Camera) {
    let background = LinearRgba {
        r: 0.,
        g: 0.,
        b: 0.,
        a: 1.,
    };
    let config = RetainedFrameRendererConfig {
        frame: FrameConfig::new(
            Viewport {
                width: 64,
                height: 36,
            },
            ScreenMap {
                scale: 4.5,
                origin: [32., 18.],
            },
            background,
        ),
        tiling: Tiling {
            macro_tile: 16,
            fine_tile: 8,
        },
        engine: EngineIdentity::certified(),
        threads,
    };
    (
        RetainedFrameRenderer::new(config).unwrap(),
        Camera::new(CameraConfig {
            resolution: (64, 36),
            background,
            ..Default::default()
        })
        .unwrap(),
    )
}
fn blue(frame: &fmn_frame::FrameBuffer) -> f64 {
    frame
        .plane(0)
        .as_chunks::<8>()
        .0
        .iter()
        .map(|p| fmn_frame::half::f16_to_f64(u16::from_le_bytes([p[4], p[5]])))
        .sum()
}
fn style(plan: &RenderPlan) -> &Style {
    plan.styles()
        .get(plan.shapes().instances()[0].style)
        .unwrap()
}

#[test]
fn interior_records_survive_endpoint_zeroes_and_reach_both_cpu_cameras() {
    let (stage, _) = fixture();
    for camera_route in [false, true] {
        let (mut render, camera) = renderer(1);
        if camera_route {
            render.render_with_camera(&stage, &camera).unwrap();
        } else {
            render.render(&stage, 0).unwrap();
        }
        assert!(
            blue(render.frame()) > 1.0,
            "interior stroke vanished on camera route {camera_route}"
        );
        let row = style(render.plan());
        let profile = row.stroke_profile.as_ref().unwrap();
        assert_eq!(profile.knots().len(), 5);
        assert_eq!(row.stroke_width_at(0.5), 80.0);
        assert_eq!(row.stroke_width_at(0.0), 0.0);
        assert_eq!(row.stroke_width_at(1.0), 0.0);
        let expected = render.frame().as_bytes().to_vec();
        for threads in [2, 4, 16] {
            let (mut replay, camera) = renderer(threads);
            if camera_route {
                replay.render_with_camera(&stage, &camera).unwrap();
            } else {
                replay.render(&stage, 0).unwrap();
            }
            assert_eq!(replay.frame().as_bytes(), expected);
        }
    }
}

#[test]
fn interior_alpha_is_not_culled_when_both_ends_are_transparent() {
    let (mut stage, mob) = fixture();
    for i in 0..5 {
        stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .write(i, "stroke_width", &[80.]);
        stage.get_mut(mob).unwrap().buffer.write(
            i,
            "stroke_rgba",
            &[0., 0., 1., if i == 2 { 1. } else { 0. }],
        );
    }
    let (mut render, camera) = renderer(1);
    render.render(&stage, 0).unwrap();
    assert!(blue(render.frame()) > 1.0);
    render.render_with_camera(&stage, &camera).unwrap();
    assert!(blue(render.frame()) > 1.0);
}

#[test]
fn interior_style_edits_invalidate_pixels_identity_and_snapshot_not_geometry() {
    let (mut stage, mob) = fixture();
    let (mut render, _) = renderer(1);
    render.render(&stage, 0).unwrap();
    let before = render.frame().as_bytes().to_vec();
    let snapshot = fmn_render::snapshot::encode(render.plan()).unwrap();
    let shape = render.plan().shapes().instances()[0].shape;
    stage
        .get_mut(mob)
        .unwrap()
        .buffer
        .write(2, "stroke_rgba", &[0., 1., 0., 1.]);
    render.render(&stage, 0).unwrap();
    assert_ne!(render.frame().as_bytes(), before);
    assert_ne!(
        fmn_render::snapshot::encode(render.plan()).unwrap(),
        snapshot
    );
    assert_eq!(render.plan().shapes().instances()[0].shape, shape);
    assert_eq!(render.plan().stats().shapes_compiled, 0);
    let expected = render.frame().as_bytes().to_vec();
    render.render(&stage, 0).unwrap();
    assert_eq!(render.frame().as_bytes(), expected);
}

#[test]
fn true_arc_stations_follow_geometry_and_anisotropic_placement() {
    let mut stage = Stage::new();
    let mob = stage.add(object(
        &[
            [0., 0., 0.],
            [0.5, 0., 0.],
            [1., 0., 0.],
            [1., 0.5, 0.],
            [1., 1., 0.],
        ],
        &[0., 40., 80., 40., 0.],
    ));
    stage.add_to_scene(mob).unwrap();
    let mut plan = RenderPlan::default();
    plan.sync(&stage, 0).unwrap();
    assert_eq!(
        style(&plan).stroke_profile.as_ref().unwrap().knots()[2].s,
        0.5
    );
    let shape = plan.shapes().instances()[0].shape;
    stage
        .set_placement(
            mob,
            Placement::new([[3., 0., 0.], [0., 1., 0.], [0., 0., 1.]], [0.; 3]),
        )
        .unwrap();
    let stats = plan.sync(&stage, 0).unwrap();
    assert_eq!(stats.shapes_compiled, 0);
    assert_eq!(plan.shapes().instances()[0].shape, shape);
    assert_eq!(
        style(&plan).stroke_profile.as_ref().unwrap().knots()[2].s,
        0.75
    );
    stage
        .get_mut(mob)
        .unwrap()
        .buffer
        .write(4, "point", &[1., 3., 0.]);
    plan.sync(&stage, 0).unwrap();
    assert_eq!(
        style(&plan).stroke_profile.as_ref().unwrap().knots()[2].s,
        0.5
    );
}

#[test]
fn subpath_breaks_keep_one_sided_colors_without_drawing_a_bridge() {
    let mut stage = Stage::new();
    let points = [
        [-3., 0., 0.],
        [-2.5, 0., 0.],
        [-2., 0., 0.],
        [-2., 0., 0.],
        [2., 0., 0.],
        [2.5, 0., 0.],
        [3., 0., 0.],
    ];
    let mob = stage.add(object(&points, &[80.; 7]));
    for i in 0..3 {
        stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .write(i, "stroke_rgba", &[1., 0., 0., 1.]);
    }
    stage.add_to_scene(mob).unwrap();
    let (mut render, _) = renderer(1);
    render.render(&stage, 0).unwrap();
    let row = style(render.plan());
    assert_eq!(row.stroke_color_at(0.5), [0., 0., 1., 1.]);
    let left = row.stroke_endpoint_parameter(0.5, true);
    assert_eq!(row.stroke_color_at(left), [1., 0., 0., 1.]);
    assert_eq!(render.plan().shapes().shapes()[0].segment_count, 2);
    let frame = render.frame();
    let offset = 18 * frame.layout().stride(0) + 32 * 8;
    assert_eq!(&frame.plane(0)[offset..offset + 6], &[0; 6]);
}

#[test]
fn bad_profile_and_knot_budget_refuse_without_replacing_last_good_plan() {
    let (mut stage, mob) = fixture();
    let mut plan = RenderPlan::default();
    plan.sync(&stage, 0).unwrap();
    let before = fmn_render::snapshot::encode(&plan).unwrap();
    let error = plan
        .sync_with_limits(
            &stage,
            0,
            RenderPlanLimits {
                max_retained_profile_knots: 4,
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("profile knots"));
    assert_eq!(fmn_render::snapshot::encode(&plan).unwrap(), before);
    stage
        .get_mut(mob)
        .unwrap()
        .buffer
        .write(2, "stroke_width", &[f32::NAN]);
    assert!(
        plan.sync(&stage, 0)
            .unwrap_err()
            .to_string()
            .contains("stroke profile")
    );
    assert_eq!(fmn_render::snapshot::encode(&plan).unwrap(), before);
}

#[test]
fn profile_interning_distinguishes_interior_paint_and_counts_shared_knots_once() {
    let make = |width| {
        Style::default().with_stroke_profile(
            StrokeProfile::new(vec![
                StrokeKnot {
                    s: 0.,
                    width: 0.,
                    rgba: [1.; 4],
                },
                StrokeKnot {
                    s: 0.5,
                    width,
                    rgba: [1.; 4],
                },
                StrokeKnot {
                    s: 1.,
                    width: 0.,
                    rgba: [1.; 4],
                },
            ])
            .unwrap(),
        )
    };
    let mut styles = StyleTable::default();
    assert_eq!(
        styles.intern(make(10.)).unwrap(),
        styles.intern(make(10.)).unwrap()
    );
    assert_eq!(styles.profile_knots(), 3);
    assert_ne!(
        styles.intern(make(10.)).unwrap(),
        styles.intern(make(11.)).unwrap()
    );
    assert_eq!(styles.profile_knots(), 6);
}

#[test]
fn uniform_strokes_do_not_acquire_geometry_dependencies_or_profile_storage() {
    let (mut stage, mob) = fixture();
    for i in 0..5 {
        stage
            .get_mut(mob)
            .unwrap()
            .buffer
            .write(i, "stroke_width", &[20.]);
    }
    let mut plan = RenderPlan::default();
    plan.sync(&stage, 0).unwrap();
    assert!(style(&plan).stroke_profile.is_none());
    let row = plan.shapes().instances()[0].style;
    stage.stretch(mob, 2., 0);
    let stats = plan.sync(&stage, 0).unwrap();
    assert_eq!(stats.styles_rebuilt, 0);
    assert_eq!(plan.shapes().instances()[0].style, row);
    assert_eq!(plan.styles().profile_knots(), 0);
}
