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

#[test]
fn rooted_render_handoffs_preserve_planar_and_camera_pixels_after_hidden_history_and_edits() {
    for use_camera in [false, true] {
        let mut stage = Stage::new();
        let shared = stage.add(vector());
        let left = stage.add(Mobject::new());
        let right = stage.add(Mobject::new());
        let invisible_parent = stage.add(Mobject::new());
        // Incoming hidden parents must not change the shared child's world
        // placement when their edges are removed from a rooted handoff.
        stage.shift(left, [-0.3, 0.2, 0.0]);
        stage.shift(right, [0.4, -0.1, 0.0]);
        stage.shift(invisible_parent, [100.0, 100.0, 0.0]);
        for parent in [left, right, invisible_parent] {
            stage.attach(parent, shared).unwrap();
        }
        let mut colored = Vec::new();
        for (parent, color, offset) in [
            (left, [0.9_f32, 0.1, 0.2, 0.6], [-0.75, 0.3, 0.0]),
            (right, [0.1_f32, 0.9, 0.2, 0.6], [0.75, -0.3, 0.0]),
        ] {
            let child = stage.add(vector());
            assert!(stage.get_mut(child).unwrap().buffer.write_range(
                "fill_rgba",
                0,
                &color.repeat(9)
            ));
            stage.shift(child, offset);
            stage.attach(parent, child).unwrap();
            colored.push(child);
        }
        stage.add_many_to_scene(&[right, left]).unwrap();
        let image_mob = use_camera.then(|| {
            let objects = [surface(), image(), dots()].map(|object| stage.add(object));
            stage.add_many_to_scene(&objects).unwrap();
            objects[1]
        });
        let target = stage.generate_target(shared).unwrap();
        let saved = stage.save_state(shared).unwrap();
        let hidden_image = stage.add(image());
        let mut history = Vec::new();
        for _ in 0..512 {
            let previous = stage.add(vector());
            stage.add_to_scene(previous).unwrap();
            stage.remove_from_scene(previous);
            history.push(previous);
        }

        let camera = use_camera.then(camera);
        let mut serial = RetainedFrameRenderer::new(config()).unwrap();
        let mut captures = Vec::new();
        for frame_index in 0..3 {
            if let Some(camera) = &camera {
                serial.render_with_camera(&stage, camera).unwrap();
            } else {
                serial.render(&stage, 0).unwrap();
            }
            let expected = serial.frame().as_bytes().to_vec();
            let checkpoint = stage.snapshot();
            let states = [
                stage.freeze_visible_render_state(),
                checkpoint.visible_render_state(0.0),
                stage.snapshot().into_visible_render_state(0.0),
            ];
            let expected_objects = 5 + 3 * usize::from(use_camera) + usize::from(frame_index > 0);
            for state in &states {
                assert_eq!(state.retained_object_count(), expected_objects);
                assert_eq!(state.image_resources().count(), usize::from(use_camera));
                let frame = state.clone().materialize();
                assert_eq!(frame.roots(), stage.roots());
                assert_eq!(frame.get(shared).unwrap().parents(), &[left, right]);
                for omitted in [invisible_parent, hidden_image, target, saved] {
                    assert!(!frame.contains(omitted));
                }
            }
            captures.push((states, expected));
            // Every frozen state is rendered only after later edits and after
            // the live arena has gone away. Removed history can still return
            // to the authored scene on its next frame.
            stage.shift(shared, [0.25, 0.25, 0.0]);
            stage.shift(colored[0], [0.0, 0.2, 0.0]);
            if frame_index == 0 {
                stage.shift(history[0], [0.0, 4.0, 0.0]);
                stage.add_to_scene(history[0]).unwrap();
                if let Some(image) = image_mob {
                    assert!(stage.set_image_resource(image, Some(pixels(true))).unwrap());
                }
            }
        }
        assert_ne!(captures[0].1, captures[1].1);
        assert_ne!(captures[1].1, captures[2].1);
        assert!(history.iter().all(|&mob| stage.contains(mob)));
        assert_eq!(stage.target(shared), Some(target));
        assert_eq!(stage.saved_state(shared), Some(saved));
        drop(stage);
        drop(serial);

        for threads in [1, 4, 16] {
            let captures = captures.clone();
            let camera = camera.clone();
            std::thread::spawn(move || {
                let mut settings = config();
                settings.threads = threads;
                let mut renderer = RetainedFrameRenderer::new(settings).unwrap();
                // Revisit an older frozen frame after a newer one to expose
                // stale retained geometry, image and topology dependencies.
                for index in [2, 0, 1] {
                    let (states, expected) = &captures[index];
                    for state in states {
                        let frame = state.clone().materialize();
                        if let Some(camera) = &camera {
                            renderer.render_with_camera(&frame, camera).unwrap();
                        } else {
                            renderer.render(&frame, 0).unwrap();
                        }
                        assert_eq!(
                            renderer.frame().as_bytes(),
                            expected,
                            "{threads} threads changed rooted frame {index}; camera={}",
                            camera.is_some(),
                        );
                    }
                }
            })
            .join()
            .unwrap();
        }
    }
}
