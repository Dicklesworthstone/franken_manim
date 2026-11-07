//! Public camera-tile reuse checked against independently rasterized frames.
//!
//! This integration target keeps the production camera/cache contract runnable
//! without code-generating the renderer's much larger internal test module.

use fmn_core::color::LinearRgba;
use fmn_frame::FrameBuffer;
use fmn_mobject::uniforms::Uniforms;
use fmn_mobject::{
    ImageColorSpace, ImageResource, ImageSampler, ImageWrap, Mob, Mobject, RecordBuffer,
    RecordSchema, RenderPrimitive, Stage,
};
use fmn_render::{
    Camera, CameraConfig, EngineIdentity, FrameArena, FrameConfig, PixelTileCache,
    PreparedCameraFrame, RasterTeam, RetainedFrameRenderer, RetainedFrameRendererConfig,
    ScreenMap, Tiling, VectorFrameCompiler, Viewport,
};

fn config() -> RetainedFrameRendererConfig {
    RetainedFrameRendererConfig {
        frame: FrameConfig::new(
            Viewport { width: 96, height: 64 },
            ScreenMap { scale: 8.0, origin: [48.0, 32.0], y_up: true },
            LinearRgba { r: 0.02, g: 0.03, b: 0.04, a: 1.0 },
        ),
        tiling: Tiling { macro_tile: 16, fine_tile: 8 },
        engine: EngineIdentity::certified(),
        threads: 1,
    }
}

fn camera(background: LinearRgba) -> Camera {
    let mut settings = CameraConfig {
        resolution: (96, 64),
        background,
        light_source_position: [-5.0, 5.0, 10.0],
        samples: 4,
        ..CameraConfig::default()
    };
    settings.frame.set_shape([12.0, 8.0]).unwrap();
    Camera::new(settings).unwrap()
}

fn filled_rectangle(bounds: [f32; 4], rgba: [f32; 4]) -> Mobject {
    let [x0, y0, x1, y1] = bounds;
    let points = [
        [x0, y0, 0.0], [(x0 + x1) * 0.5, y0, 0.0], [x1, y0, 0.0],
        [x1, (y0 + y1) * 0.5, 0.0], [x1, y1, 0.0],
        [(x0 + x1) * 0.5, y1, 0.0], [x0, y1, 0.0],
        [x0, (y0 + y1) * 0.5, 0.0], [x0, y0, 0.0],
    ];
    let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len()).unwrap();
    for (index, point) in points.iter().enumerate() {
        assert!(buffer.write(index, "point", point));
        assert!(buffer.write(index, "fill_rgba", &rgba));
        assert!(buffer.write(index, "stroke_rgba", &[0.0; 4]));
        assert!(buffer.write(index, "stroke_width", &[0.0]));
    }
    Mobject::from_buffer(buffer)
}

fn textured_mesh() -> Mobject {
    let schema = RecordSchema::new(
        &[("point", 3), ("d_normal_point", 3), ("im_coords", 2), ("opacity", 1)],
        &["point"],
        &["point", "d_normal_point"],
    ).unwrap();
    let corners = [
        ([-4.5, -1.5, 0.25], [-0.25, 1.25]),
        ([-1.5, -1.5, 0.25], [1.75, 1.25]),
        ([-1.5, 1.5, 0.25], [1.75, -0.25]),
        ([-4.5, -1.5, 0.25], [-0.25, 1.25]),
        ([-1.5, 1.5, 0.25], [1.75, -0.25]),
        ([-4.5, 1.5, 0.25], [-0.25, -0.25]),
    ];
    let mut buffer = RecordBuffer::new(schema, corners.len()).unwrap();
    for (index, (point, uv)) in corners.iter().enumerate() {
        assert!(buffer.write(index, "point", point));
        assert!(buffer.write(index, "d_normal_point", &[point[0], point[1], 1.25]));
        assert!(buffer.write(index, "im_coords", uv));
        assert!(buffer.write(index, "opacity", &[1.0]));
    }
    let image = ImageResource::rgba8(
        2, 2,
        vec![255, 20, 10, 255, 10, 255, 30, 255, 20, 30, 255, 255, 255, 220, 10, 255],
        ImageColorSpace::Srgb,
        ImageSampler::default(),
    ).unwrap();
    // Attaching an image selects ImageQuad; select the intended mesh last.
    Mobject::from_buffer(buffer)
        .with_uniforms(Uniforms {
            shading: [0.3, 0.2, 0.4],
            depth_test: true,
            ..Uniforms::default()
        })
        .with_image_resource(image)
        .with_render_primitive(RenderPrimitive::TriangleMesh)
}

fn scene() -> (Stage, Mob, Mob, Mob) {
    let mut stage = Stage::new();
    let mesh = stage.add(textured_mesh());
    let underlay = stage.add(filled_rectangle([1.0, -1.0, 3.5, 1.0], [0.2, 0.7, 0.3, 0.45]));
    let moving = stage.add(filled_rectangle([2.0, -0.5, 4.0, 0.5], [0.8, 0.2, 0.6, 0.65]));
    stage.add_many_to_scene(&[mesh, underlay, moving]).unwrap();
    (stage, mesh, underlay, moving)
}

fn capture(
    stage: &Stage,
    camera: &Camera,
    compiler: &mut RetainedFrameRenderer,
    frames: &mut Vec<PreparedCameraFrame>,
    expected: &mut Vec<Vec<u8>>,
) {
    // Build a separate compiler for every control and rasterize directly,
    // without PixelTileCache or the persistent RasterTeam under test.
    let mut fresh = RetainedFrameRenderer::new(config()).unwrap();
    let reference = fresh.prepare_with_camera(stage, camera).unwrap().render(1).unwrap();
    expected.push(reference.as_bytes().to_vec());
    frames.push(compiler.prepare_with_camera(stage, camera).unwrap());
}

#[test]
fn camera_tiles_match_uncached_frames_across_edits_seeks_and_route_switches() {
    let config = config();
    let (mut stage, mesh, underlay, moving) = scene();
    let base_camera = camera(config.frame.background);
    let mut current_camera = base_camera.clone();
    let checkpoint = stage.snapshot();
    let mut compiler = RetainedFrameRenderer::new(config).unwrap();
    let mut frames = Vec::new();
    let mut expected = Vec::new();
    capture(&stage, &current_camera, &mut compiler, &mut frames, &mut expected);

    stage.shift(moving, [0.25, 0.125, 0.0]);
    capture(&stage, &current_camera, &mut compiler, &mut frames, &mut expected);
    // Cross several tile boundaries: both the vacated and new regions matter.
    stage.shift(moving, [-4.0, 0.75, 0.0]);
    capture(&stage, &current_camera, &mut compiler, &mut frames, &mut expected);
    let resource = stage.get(mesh).unwrap().image_resource().unwrap().clone();
    assert!(stage.set_image_resource(mesh, Some(resource.with_sampler(ImageSampler {
        wrap_u: ImageWrap::ClampToEdge,
        wrap_v: ImageWrap::ClampToEdge,
    }))).unwrap());
    capture(&stage, &current_camera, &mut compiler, &mut frames, &mut expected);
    stage.uniforms_mut(mesh).unwrap().clip_planes[0] = [1.0, 0.0, 0.0, 3.0];
    capture(&stage, &current_camera, &mut compiler, &mut frames, &mut expected);
    current_camera.set_light_source_position([5.0, -5.0, -10.0]).unwrap();
    capture(&stage, &current_camera, &mut compiler, &mut frames, &mut expected);
    current_camera.frame_mut().set_center([0.5, 0.0, 0.0]).unwrap();
    capture(&stage, &current_camera, &mut compiler, &mut frames, &mut expected);
    current_camera.set_background(LinearRgba { r: 0.09, g: 0.12, b: 0.16, a: 1.0 }).unwrap();
    capture(&stage, &current_camera, &mut compiler, &mut frames, &mut expected);

    stage.restore(&checkpoint);
    current_camera = base_camera.clone();
    capture(&stage, &current_camera, &mut compiler, &mut frames, &mut expected);
    assert_eq!(expected[8], expected[0], "snapshot restoration must restore the scene pixels");
    {
        let buffer = &mut stage.get_mut(underlay).unwrap().buffer;
        for index in 0..buffer.len() {
            assert!(buffer.write(index, "fill_rgba", &[0.95, 0.2, 0.1, 0.45]));
        }
    }
    capture(&stage, &current_camera, &mut compiler, &mut frames, &mut expected);
    stage.add_many_to_scene(&[mesh, moving, underlay]).unwrap();
    capture(&stage, &current_camera, &mut compiler, &mut frames, &mut expected);
    let view = stage.get_mut(underlay).unwrap().buffer.export_view(true);
    let exposed_revision = stage.get(underlay).unwrap().buffer.revision();
    for index in 0..9 {
        assert!(view.write_foreign(index, "fill_rgba", &[0.1, 0.9, 0.8, 0.45]));
    }
    assert_eq!(stage.get(underlay).unwrap().buffer.revision(), exposed_revision);
    capture(&stage, &current_camera, &mut compiler, &mut frames, &mut expected);
    // A second foreign write occurs after the latest live-view capture.
    // It has no revision callback; detaching must make it observable anyway.
    for index in 0..9 {
        assert!(view.write_foreign(index, "fill_rgba", &[0.7, 0.15, 0.9, 0.45]));
    }
    assert_eq!(stage.get(underlay).unwrap().buffer.revision(), exposed_revision);
    drop(view);
    assert_ne!(stage.get(underlay).unwrap().buffer.revision(), exposed_revision);
    capture(&stage, &current_camera, &mut compiler, &mut frames, &mut expected);
    assert_ne!(expected[12], expected[11], "the second foreign write must survive view detachment");

    stage.restore(&checkpoint);
    current_camera = camera(LinearRgba { r: 0.15, g: 0.04, b: 0.08, a: 1.0 });
    assert_eq!(current_camera.revision(), base_camera.revision());
    capture(&stage, &current_camera, &mut compiler, &mut frames, &mut expected);
    assert_ne!(expected[13], expected[0], "equal camera revisions can describe different pixels");
    current_camera.set_samples(2);
    assert_ne!(current_camera.edge_sample_limit(), base_camera.edge_sample_limit());
    capture(&stage, &current_camera, &mut compiler, &mut frames, &mut expected);

    // Neither adjacent capture observes this view while it is attached.
    // The persistent detach revision must invalidate the previous source key.
    assert!(!stage.get(underlay).unwrap().buffer.writable_view_affects("fill_rgba"));
    let between_view = stage.get_mut(underlay).unwrap().buffer.export_view(true);
    let between_revision = stage.get(underlay).unwrap().buffer.revision();
    for index in 0..9 {
        assert!(between_view.write_foreign(index, "fill_rgba", &[0.4, 0.1, 0.95, 0.45]));
    }
    assert_eq!(stage.get(underlay).unwrap().buffer.revision(), between_revision);
    drop(between_view);
    assert!(!stage.get(underlay).unwrap().buffer.writable_view_affects("fill_rgba"));
    assert_ne!(stage.get(underlay).unwrap().buffer.revision(), between_revision);
    capture(&stage, &current_camera, &mut compiler, &mut frames, &mut expected);
    for index in 1..expected.len() {
        // Lowering the AA ceiling need not alter these fixture pixels.
        if index != 14 {
            assert_ne!(expected[index], expected[index - 1], "mutation {index} must be visible");
        }
    }

    let sequence = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 12, 13, 14, 15, 0];
    for threads in [1, 4, 16] {
        for budget in [0, 1, PixelTileCache::DEFAULT_KEY_BUDGET_BYTES] {
            let mut team = RasterTeam::new(threads).unwrap();
            let mut cache = PixelTileCache::with_budgets(PixelTileCache::DEFAULT_KEY_BUDGET_BYTES, 0);
            cache.set_camera_budget_bytes(budget);
            let mut pixels = FrameBuffer::new(config.frame.layout().unwrap());
            for (position, index) in sequence.into_iter().enumerate() {
                pixels.plane_mut(0).fill(0xA5);
                let stats = frames[index]
                    .render_cached_into_with_team(&mut team, &mut cache, &mut pixels)
                    .unwrap();
                assert_eq!(
                    pixels.as_bytes(), expected[index],
                    "camera tiles: threads={threads}, budget={budget}, position={position}"
                );
                assert_eq!(stats.cache, cache.stats());
                assert!(cache.retained_camera_bytes() <= budget);
                if budget <= 1 {
                    assert_eq!(cache.retained_camera_bytes(), 0);
                    assert_eq!(stats.cache.hits, 0);
                } else if position == 1 {
                    assert!(stats.cache.hits > 0, "static tiles must survive local movement");
                    assert!(stats.cache.misses > 0, "moving tiles must be rasterized");
                } else if position == 13 {
                    assert!(stats.cache.hits > 0, "an identical frozen frame must reuse pixels");
                }
                if index == 14 {
                    assert_eq!(stats.cache.hits, 0, "changing the AA ceiling must invalidate camera tiles");
                }
                if index == 15 && budget > 1 {
                    assert!(stats.cache.hits > 0, "unaffected sources remain reusable after a detached write");
                    assert!(stats.cache.misses > 0, "an unobserved foreign-view lifetime must invalidate its tiles");
                }
                if threads == 1 {
                    assert_eq!(team.stats().band_buffer_bytes, 0, "inline camera rendering needs no band surface");
                }
            }

            // Both routes share the cache and destination. A planar draw must
            // retire camera payloads; returning to the old camera must not
            // mistake the overwritten destination for a reusable camera frame.
            let mut planar_stage = Stage::new();
            let full = planar_stage.add(filled_rectangle([-8.0, -8.0, 8.0, 8.0], [0.3, 0.9, 0.1, 1.0]));
            planar_stage.add_to_scene(full).unwrap();
            let mut planar = VectorFrameCompiler::new(config).unwrap();
            let frame = planar.capture(&planar_stage, 0).unwrap();
            frame.render_cached_into_with_team(
                &mut team, &mut FrameArena::new(), &mut cache, &mut pixels, None,
            ).unwrap();
            assert_eq!(cache.retained_camera_bytes(), 0);
            assert_ne!(pixels.as_bytes(), expected[0]);
            frames[0].render_cached_into_with_team(&mut team, &mut cache, &mut pixels).unwrap();
            assert_eq!(pixels.as_bytes(), expected[0]);
            assert!(cache.retained_camera_bytes() <= budget);
        }
    }
}
