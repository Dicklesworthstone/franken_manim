//! Per-source camera preparation checked against cold, uncached frame controls.

use fmn_core::color::LinearRgba;
use fmn_mobject::uniforms::Uniforms;
use fmn_mobject::{
    ImageColorSpace, ImageResource, ImageSampler, Mob, Mobject, RecordBuffer, RecordSchema,
    RenderPrimitive, Stage,
};
use fmn_render::{
    Camera, CameraConfig, CameraPreparationStats, EngineIdentity, FrameConfig, PreparedCameraFrame,
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
        background: config().frame.background,
        ..CameraConfig::default()
    };
    settings.frame.set_shape([12.0, 8.0]).unwrap();
    Camera::new(settings).unwrap()
}

fn vector() -> Mobject {
    let points = [
        [-1.5, -2.0, 0.0],
        [0.0, -2.0, 0.0],
        [1.5, -2.0, 0.0],
        [1.5, -1.25, 0.0],
        [1.5, -0.5, 0.0],
        [0.0, -0.5, 0.0],
        [-1.5, -0.5, 0.0],
        [-1.5, -1.25, 0.0],
        [-1.5, -2.0, 0.0],
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
        &[("point", 3), ("radius", 1), ("rgba", 4)],
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
    }
    Mobject::from_buffer(records).with_render_primitive(RenderPrimitive::DotCloud)
}

fn scene() -> (Stage, [Mob; 4]) {
    let mut stage = Stage::new();
    let objects = [vector(), surface(), image(), dots()].map(|object| stage.add(object));
    stage.add_many_to_scene(&objects).unwrap();
    (stage, objects)
}

fn work(
    meshes_built: usize,
    meshes_reused: usize,
    dot_clouds_built: usize,
    dot_clouds_reused: usize,
) -> CameraPreparationStats {
    CameraPreparationStats {
        meshes_built,
        meshes_reused,
        dot_clouds_built,
        dot_clouds_reused,
        dot_rows_built: dot_clouds_built * 3,
        dot_rows_reused: dot_clouds_reused * 3,
    }
}

fn capture(
    stage: &Stage,
    camera: &Camera,
    compiler: &mut RetainedFrameRenderer,
    expected_work: CameraPreparationStats,
    captures: &mut Vec<(PreparedCameraFrame, Vec<u8>)>,
) {
    // Every control starts with an empty compiler and bypasses the pixel cache
    // and persistent raster team. It cannot reuse any preceding world resource.
    let mut cold = RetainedFrameRenderer::new(config()).unwrap();
    cold.set_camera_projection_cache_budget_bytes(0);
    let reference = cold
        .prepare_with_camera(stage, camera)
        .unwrap()
        .render(1)
        .unwrap();
    assert_eq!(cold.camera_preparation_stats(), work(2, 0, 1, 0));
    let frame = compiler.prepare_with_camera(stage, camera).unwrap();
    assert_eq!(compiler.camera_preparation_stats(), expected_work);
    captures.push((frame, reference.as_bytes().to_vec()));
}

#[test]
fn camera_preparation_rebuilds_only_changed_world_sources_and_preserves_frozen_frames() {
    let (mut stage, [moving, mesh, image, dots]) = scene();
    let checkpoint = stage.snapshot();
    let mut camera = camera();
    let mut compiler = RetainedFrameRenderer::new(config()).unwrap();
    assert_eq!(
        compiler.camera_preparation_stats(),
        CameraPreparationStats::default()
    );
    let mut captures = Vec::new();
    capture(
        &stage,
        &camera,
        &mut compiler,
        work(2, 0, 1, 0),
        &mut captures,
    );

    stage.shift(moving, [0.5, 0.0, 0.0]);
    capture(
        &stage,
        &camera,
        &mut compiler,
        work(0, 2, 0, 1),
        &mut captures,
    );
    camera.frame_mut().set_center([0.5, 0.0, 0.0]).unwrap();
    capture(
        &stage,
        &camera,
        &mut compiler,
        work(0, 2, 0, 1),
        &mut captures,
    );
    stage.shift(mesh, [0.25, 0.0, 0.0]);
    capture(
        &stage,
        &camera,
        &mut compiler,
        work(1, 1, 0, 1),
        &mut captures,
    );
    assert!(
        stage
            .get_mut(dots)
            .unwrap()
            .buffer
            .write(0, "rgba", &[0.95, 0.1, 0.8, 0.9])
    );
    capture(
        &stage,
        &camera,
        &mut compiler,
        work(0, 2, 1, 0),
        &mut captures,
    );
    assert!(stage.set_image_resource(image, Some(pixels(true))).unwrap());
    capture(
        &stage,
        &camera,
        &mut compiler,
        work(1, 1, 0, 1),
        &mut captures,
    );
    stage.uniforms_mut(mesh).unwrap().clip_planes[0] = [1.0, 0.0, 0.0, 3.5];
    capture(
        &stage,
        &camera,
        &mut compiler,
        work(1, 1, 0, 1),
        &mut captures,
    );
    stage.restore(&checkpoint);
    capture(
        &stage,
        &camera,
        &mut compiler,
        work(2, 0, 1, 0),
        &mut captures,
    );

    // Refusal must not publish partial work counters or replace cached sources.
    assert!(
        stage
            .get_mut(mesh)
            .unwrap()
            .buffer
            .write(0, "point", &[f32::NAN, -1.5, 0.2])
    );
    assert!(compiler.prepare_with_camera(&stage, &camera).is_err());
    assert_eq!(
        compiler.camera_preparation_stats(),
        CameraPreparationStats::default()
    );
    assert!(
        stage
            .get_mut(mesh)
            .unwrap()
            .buffer
            .write(0, "point", &[-4.5, -1.5, 0.2])
    );
    capture(
        &stage,
        &camera,
        &mut compiler,
        work(1, 1, 0, 1),
        &mut captures,
    );
    assert_eq!(
        captures[8].1, captures[7].1,
        "repair must restore the captured pixels"
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
        work(1, 1, 0, 1),
        &mut captures,
    );
    capture(
        &stage,
        &camera,
        &mut compiler,
        work(1, 1, 0, 1),
        &mut captures,
    );
    drop(view);
    capture(
        &stage,
        &camera,
        &mut compiler,
        work(1, 1, 0, 1),
        &mut captures,
    );
    capture(
        &stage,
        &camera,
        &mut compiler,
        work(0, 2, 0, 1),
        &mut captures,
    );
    assert_eq!(
        captures[9].1, captures[12].1,
        "closing a view preserves its final data"
    );

    for (before, after) in [(0, 1), (2, 3), (3, 4), (4, 5), (5, 6), (8, 9)] {
        assert_ne!(
            captures[before].1, captures[after].1,
            "the mutation must change visible pixels"
        );
    }
    // Captures own their immutable source resources even after all live and
    // checkpoint state is gone. Replay out of capture order on every team size.
    drop(compiler);
    drop(stage);
    drop(checkpoint);
    for threads in [1, 4, 16] {
        for index in [12, 0, 4, 9, 3, 11, 1, 8, 2, 10, 5, 7, 6] {
            assert_eq!(
                captures[index].0.render(threads).unwrap().as_bytes(),
                captures[index].1
            );
        }
    }
}
