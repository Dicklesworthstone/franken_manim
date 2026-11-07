//! Public projection reuse checked against independently compiled camera frames.

use fmn_core::color::LinearRgba;
use fmn_mobject::uniforms::Uniforms;
use fmn_mobject::{
    ImageColorSpace, ImageResource, ImageSampler, Mob, Mobject, RecordBuffer, RecordSchema,
    RenderPrimitive, Stage,
};
use fmn_render::{
    Camera, CameraConfig, CameraProjectionStats, EngineIdentity, FrameConfig, PreparedCameraFrame,
    RetainedFrameRenderer, RetainedFrameRendererConfig, ScreenMap, Tiling, Viewport,
};

fn config() -> RetainedFrameRendererConfig {
    RetainedFrameRendererConfig {
        frame: FrameConfig::new(
            Viewport {
                width: 96,
                height: 64,
            },
            ScreenMap {
                scale: 8.0,
                origin: [48.0, 32.0],
                y_up: true,
            },
            LinearRgba {
                r: 0.02,
                g: 0.03,
                b: 0.04,
                a: 1.0,
            },
        ),
        tiling: Tiling {
            macro_tile: 16,
            fine_tile: 8,
        },
        engine: EngineIdentity::certified(),
        threads: 1,
    }
}

fn camera() -> Camera {
    let mut settings = CameraConfig {
        resolution: (96, 64),
        samples: 4,
        background: config().frame.background,
        ..CameraConfig::default()
    };
    settings.frame.set_shape([12.0, 8.0]).unwrap();
    Camera::new(settings).unwrap()
}

fn vector() -> Mobject {
    let points = [
        [-3.0, -2.0, 0.0],
        [0.0, -2.0, 0.0],
        [3.0, -2.0, 0.0],
        [3.0, -1.25, 0.0],
        [3.0, -0.5, 0.0],
        [0.0, -0.5, 0.0],
        [-3.0, -0.5, 0.0],
        [-3.0, -1.25, 0.0],
        [-3.0, -2.0, 0.0],
    ];
    let mut records = RecordBuffer::new(RecordSchema::vmobject(), points.len()).unwrap();
    for (index, point) in points.iter().enumerate() {
        assert!(records.write(index, "point", point));
        assert!(records.write(index, "fill_rgba", &[0.15, 0.3, 0.95, 0.7]));
        assert!(records.write(index, "stroke_width", &[0.0]));
    }
    Mobject::from_buffer(records)
}

fn surface() -> Mobject {
    let schema = RecordSchema::new(
        &[("point", 3), ("d_normal_point", 3), ("rgba", 4)],
        &["point"],
        &["point", "d_normal_point"],
    )
    .unwrap();
    let points = [
        [-4.5, -1.5, 0.2],
        [-4.5, 1.5, 0.2],
        [-2.5, -1.5, 0.2],
        [-2.5, 1.5, 0.2],
    ];
    let mut records = RecordBuffer::new(schema, points.len()).unwrap();
    for (index, point) in points.iter().enumerate() {
        assert!(records.write(index, "point", point));
        assert!(records.write(index, "d_normal_point", &[point[0], point[1], 1.2]));
        assert!(records.write(index, "rgba", &[0.8, 0.2, 0.1, 0.9]));
    }
    Mobject::from_buffer(records)
        .with_uniforms(Uniforms {
            shading: [0.3, 0.2, 0.4],
            depth_test: true,
            ..Uniforms::default()
        })
        .with_render_primitive(RenderPrimitive::SurfaceGrid { resolution: (2, 2) })
}

fn pixels(changed: bool) -> ImageResource {
    ImageResource::rgba8(
        2,
        2,
        if changed {
            vec![
                20, 220, 250, 255, 230, 20, 240, 255, 250, 240, 20, 255, 20, 40, 240, 255,
            ]
        } else {
            vec![
                240, 30, 20, 255, 20, 230, 40, 255, 30, 40, 250, 255, 240, 220, 30, 255,
            ]
        },
        ImageColorSpace::Srgb,
        ImageSampler::default(),
    )
    .unwrap()
}

fn image() -> Mobject {
    let schema = RecordSchema::new(
        &[("point", 3), ("im_coords", 2), ("opacity", 1)],
        &["point"],
        &["point"],
    )
    .unwrap();
    let corners = [
        ([2.5, -2.0, 0.25], [0.0, 1.0]),
        ([4.5, -2.0, 0.25], [1.0, 1.0]),
        ([4.5, 0.0, 0.25], [1.0, 0.0]),
        ([2.5, -2.0, 0.25], [0.0, 1.0]),
        ([4.5, 0.0, 0.25], [1.0, 0.0]),
        ([2.5, 0.0, 0.25], [0.0, 0.0]),
    ];
    let mut records = RecordBuffer::new(schema, corners.len()).unwrap();
    for (index, (point, uv)) in corners.iter().enumerate() {
        assert!(records.write(index, "point", point));
        assert!(records.write(index, "im_coords", uv));
        assert!(records.write(index, "opacity", &[1.0]));
    }
    Mobject::from_buffer(records).with_image_resource(pixels(false))
}

fn dots() -> Mobject {
    let schema = RecordSchema::new(
        &[("point", 3), ("radius", 1), ("rgba", 4), ("glow_factor", 1)],
        &["point"],
        &["point"],
    )
    .unwrap();
    let mut records = RecordBuffer::new(schema, 3).unwrap();
    for (index, point) in [[-1.2, 1.75, 0.4], [0.0, 1.25, 0.4], [1.2, 1.75, 0.4]]
        .iter()
        .enumerate()
    {
        assert!(records.write(index, "point", point));
        assert!(records.write(index, "radius", &[0.4]));
        assert!(records.write(index, "rgba", &[0.3, 0.95, 0.2, 0.9]));
        assert!(records.write(index, "glow_factor", &[0.0]));
    }
    Mobject::from_buffer(records).with_render_primitive(RenderPrimitive::DotCloud)
}

fn scene() -> (Stage, [Mob; 4]) {
    let mut stage = Stage::new();
    let objects = [vector(), surface(), image(), dots()].map(|object| stage.add(object));
    stage.add_many_to_scene(&objects).unwrap();
    (stage, objects)
}

const DRAWS: usize = 6;

fn work(reused_draws: usize) -> CameraProjectionStats {
    CameraProjectionStats {
        compiled_draws: DRAWS - reused_draws,
        reused_draws,
    }
}

fn cold_pixels(stage: &Stage, camera: &Camera) -> Vec<u8> {
    let mut compiler = RetainedFrameRenderer::new(config()).unwrap();
    // Zero is the unchanged ThreeDJob compiler, not a second retained cache.
    compiler.set_camera_projection_cache_budget_bytes(0);
    let frame = compiler.prepare_with_camera(stage, camera).unwrap();
    assert_eq!(frame.projection_preparation_stats(), None);
    let pixels = frame.render(1).unwrap();
    assert_eq!(frame.projection_preparation_stats(), Some(work(0)));
    assert_eq!(compiler.retained_camera_projection_bytes(), 0);
    pixels.as_bytes().to_vec()
}

struct Capture {
    frame: PreparedCameraFrame,
    pixels: Vec<u8>,
    stats: CameraProjectionStats,
    label: &'static str,
}

fn capture(
    stage: &Stage,
    camera: &Camera,
    compiler: &mut RetainedFrameRenderer,
    budget: usize,
    expected_reuses: usize,
    label: &'static str,
    captures: &mut Vec<Capture>,
) {
    let expected = cold_pixels(stage, camera);
    let frame = compiler.prepare_with_camera(stage, camera).unwrap();
    assert_eq!(frame.projection_preparation_stats(), None, "{label}");
    assert_eq!(
        frame.render(1).unwrap().as_bytes(),
        expected,
        "{label}, budget={budget}"
    );
    let stats = work(if budget <= 1 { 0 } else { expected_reuses });
    assert_eq!(
        frame.projection_preparation_stats(),
        Some(stats),
        "{label}, budget={budget}"
    );
    let retained = compiler.retained_camera_projection_bytes();
    assert!(retained <= budget, "{label}: {retained} > {budget}");
    if budget <= 1 {
        assert_eq!(retained, 0, "{label}");
    } else {
        assert!(
            retained > 0,
            "{label}: the fixture must retain actual projected rows"
        );
    }
    captures.push(Capture {
        frame,
        pixels: expected,
        stats,
        label,
    });
}

#[test]
fn projected_sources_reuse_exact_rows_through_local_edits_views_and_frozen_replay() {
    let default_budget = RetainedFrameRenderer::DEFAULT_CAMERA_PROJECTION_BUDGET_BYTES;
    for budget in [0, 1, default_budget] {
        let (mut stage, [moving, mesh, image, dots]) = scene();
        let checkpoint = stage.snapshot();
        let original_camera = camera();
        let mut camera = original_camera.clone();
        let mut compiler = RetainedFrameRenderer::new(config()).unwrap();
        compiler.set_camera_projection_cache_budget_bytes(budget);
        assert_eq!(compiler.retained_camera_projection_bytes(), 0);
        let mut captures = Vec::new();

        capture(
            &stage,
            &camera,
            &mut compiler,
            budget,
            0,
            "cold",
            &mut captures,
        );
        stage.shift(moving, [0.5, 0.0, 0.0]);
        capture(
            &stage,
            &camera,
            &mut compiler,
            budget,
            5,
            "vector",
            &mut captures,
        );
        stage.shift(mesh, [0.25, 0.0, 0.0]);
        capture(
            &stage,
            &camera,
            &mut compiler,
            budget,
            5,
            "surface",
            &mut captures,
        );
        assert!(
            stage
                .get_mut(dots)
                .unwrap()
                .buffer
                .write(0, "rgba", &[0.95, 0.1, 0.8, 0.9],)
        );
        capture(
            &stage,
            &camera,
            &mut compiler,
            budget,
            3,
            "dot rows",
            &mut captures,
        );
        assert!(stage.set_image_resource(image, Some(pixels(true))).unwrap());
        capture(
            &stage,
            &camera,
            &mut compiler,
            budget,
            5,
            "image",
            &mut captures,
        );
        stage.uniforms_mut(mesh).unwrap().clip_planes[0] = [1.0, 0.0, 0.0, 3.5];
        capture(
            &stage,
            &camera,
            &mut compiler,
            budget,
            5,
            "clip plane",
            &mut captures,
        );
        camera.frame_mut().set_center([0.5, 0.0, 0.0]).unwrap();
        capture(
            &stage,
            &camera,
            &mut compiler,
            budget,
            0,
            "camera",
            &mut captures,
        );
        camera.set_light_source_position([5.0, -5.0, 10.0]).unwrap();
        capture(
            &stage,
            &camera,
            &mut compiler,
            budget,
            0,
            "light",
            &mut captures,
        );
        camera.set_samples(2);
        capture(
            &stage,
            &camera,
            &mut compiler,
            budget,
            0,
            "sample ceiling",
            &mut captures,
        );

        // Every source changes its draw-plan slot. The translucent vector
        // overlaps the image, so this also has an independent painter witness.
        stage
            .add_many_to_scene(&[dots, image, mesh, moving])
            .unwrap();
        capture(
            &stage,
            &camera,
            &mut compiler,
            budget,
            0,
            "painter order",
            &mut captures,
        );

        let view = stage
            .get_mut(mesh)
            .unwrap()
            .buffer
            .export_field_view("rgba", true)
            .unwrap();
        let revision = stage.get(mesh).unwrap().buffer.revision();
        for index in 0..4 {
            assert!(view.write_foreign(index, "rgba", &[0.1, 0.9, 0.7, 0.9]));
        }
        assert_eq!(stage.get(mesh).unwrap().buffer.revision(), revision);
        capture(
            &stage,
            &camera,
            &mut compiler,
            budget,
            5,
            "live view",
            &mut captures,
        );
        for index in 0..4 {
            assert!(view.write_foreign(index, "rgba", &[0.7, 0.15, 0.9, 0.9]));
        }
        assert_eq!(stage.get(mesh).unwrap().buffer.revision(), revision);
        drop(view);
        assert_ne!(stage.get(mesh).unwrap().buffer.revision(), revision);
        capture(
            &stage,
            &camera,
            &mut compiler,
            budget,
            5,
            "detached write",
            &mut captures,
        );

        stage.restore(&checkpoint);
        camera = original_camera;
        capture(
            &stage,
            &camera,
            &mut compiler,
            budget,
            0,
            "snapshot",
            &mut captures,
        );
        assert_eq!(captures[12].pixels, captures[0].pixels);
        for index in 1..12 {
            if index != 8 {
                assert_ne!(
                    captures[index - 1].pixels,
                    captures[index].pixels,
                    "{} must change visible pixels",
                    captures[index].label,
                );
            }
        }

        // Whole-projection sharing does not replace its construction counters
        // with a synthetic zero-work result on each subsequent raster call.
        let retained = compiler.retained_camera_projection_bytes();
        let same = compiler.prepare_with_camera(&stage, &camera).unwrap();
        assert_eq!(
            same.projection_preparation_stats(),
            Some(captures[12].stats)
        );
        assert_eq!(same.render(4).unwrap().as_bytes(), captures[12].pixels);
        assert_eq!(compiler.retained_camera_projection_bytes(), retained);
        drop(same);
        drop(compiler);
        drop(stage);
        drop(checkpoint);

        for threads in [1, 4, 16] {
            for index in [12, 0, 10, 2, 6, 11, 3, 1, 9, 4, 7, 5, 8] {
                let capture = &captures[index];
                assert_eq!(
                    capture.frame.render(threads).unwrap().as_bytes(),
                    capture.pixels,
                    "{} at {threads} threads, budget={budget}",
                    capture.label,
                );
                assert_eq!(
                    capture.frame.projection_preparation_stats(),
                    Some(capture.stats)
                );
            }
        }
    }
}

#[test]
fn projection_budget_changes_retire_cache_epochs_without_invalidating_owned_frames() {
    let budget = RetainedFrameRenderer::DEFAULT_CAMERA_PROJECTION_BUDGET_BYTES;
    let (mut stage, [moving, mesh, _, _]) = scene();
    let camera = camera();
    let mut compiler = RetainedFrameRenderer::new(config()).unwrap();
    compiler.set_camera_projection_cache_budget_bytes(budget);

    let original_pixels = cold_pixels(&stage, &camera);
    let original = compiler.prepare_with_camera(&stage, &camera).unwrap();
    assert_eq!(original.projection_preparation_stats(), None);
    assert_eq!(original.render(1).unwrap().as_bytes(), original_pixels);
    assert_eq!(original.projection_preparation_stats(), Some(work(0)));
    assert!(compiler.retained_camera_projection_bytes() > 0);

    stage.shift(moving, [0.5, 0.0, 0.0]);
    let pending_pixels = cold_pixels(&stage, &camera);
    let pending = compiler.prepare_with_camera(&stage, &camera).unwrap();
    assert_eq!(pending.projection_preparation_stats(), None);

    compiler.set_camera_projection_cache_budget_bytes(0);
    assert_eq!(compiler.retained_camera_projection_bytes(), 0);
    assert_eq!(original.render(4).unwrap().as_bytes(), original_pixels);
    assert_eq!(original.projection_preparation_stats(), Some(work(0)));
    assert_eq!(compiler.retained_camera_projection_bytes(), 0);

    // A previously frozen but unprepared frame observes the current disabled
    // policy when it initializes; it cannot repopulate the retired cache.
    assert_eq!(pending.render(4).unwrap().as_bytes(), pending_pixels);
    assert_eq!(pending.projection_preparation_stats(), Some(work(0)));
    assert_eq!(compiler.retained_camera_projection_bytes(), 0);

    compiler.set_camera_projection_cache_budget_bytes(budget);
    let restarted = compiler.prepare_with_camera(&stage, &camera).unwrap();
    assert_eq!(restarted.projection_preparation_stats(), None);
    assert_eq!(restarted.render(1).unwrap().as_bytes(), pending_pixels);
    assert_eq!(restarted.projection_preparation_stats(), Some(work(0)));
    let retained = compiler.retained_camera_projection_bytes();
    assert!(retained > 0 && retained <= budget);
    assert_eq!(pending.render(16).unwrap().as_bytes(), pending_pixels);
    assert_eq!(compiler.retained_camera_projection_bytes(), retained);

    stage.shift(mesh, [0.25, 0.0, 0.0]);
    let changed_pixels = cold_pixels(&stage, &camera);
    let changed = compiler.prepare_with_camera(&stage, &camera).unwrap();
    assert_eq!(changed.projection_preparation_stats(), None);
    assert_eq!(changed.render(1).unwrap().as_bytes(), changed_pixels);
    assert_eq!(changed.projection_preparation_stats(), Some(work(5)));
    assert!(compiler.retained_camera_projection_bytes() <= budget);

    let mut planar = Stage::new();
    let overlay = planar.add(vector());
    planar.add_to_scene(overlay).unwrap();
    compiler.render(&planar, 0).unwrap();
    assert_eq!(compiler.retained_camera_projection_bytes(), 0);
    assert_eq!(restarted.render(16).unwrap().as_bytes(), pending_pixels);
    assert_eq!(compiler.retained_camera_projection_bytes(), 0);
}
