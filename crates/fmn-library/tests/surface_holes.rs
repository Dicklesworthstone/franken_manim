//! Explicit undefined surface samples without non-finite Stage records.
use std::convert::Infallible;

use fmn_library::solids::{SurfaceSampleError, SurfaceSpec};
use fmn_library::{SurfaceMesh, TexturedSurface};
use fmn_mobject::stage::{SURFACE_VALIDITY_FIELD, surface_validity};
use fmn_mobject::{Mobject, Stage};

fn spec() -> SurfaceSpec {
    SurfaceSpec {
        u_range: (10.0, 14.0),
        v_range: (20.0, 24.0),
        resolution: (5, 5),
        ..SurfaceSpec::default()
    }
}

fn pole(u: f64, v: f64) -> Result<[f64; 3], Infallible> {
    Ok([
        u,
        v,
        if u == 12.0 && v == 22.0 {
            f64::INFINITY
        } else {
            1.0
        },
    ])
}

#[test]
fn undefined_sample_removes_only_its_incident_triangles_and_not_the_uv_grid() {
    let surface = spec().try_sample_with_holes(pole).unwrap();
    assert_eq!(surface.resolution(), (5, 5));
    assert_eq!(surface.points().len(), 25);
    assert_eq!(
        surface
            .sample_validity()
            .iter()
            .filter(|&&valid| !valid)
            .count(),
        1
    );
    assert!(!surface.sample_validity()[12]);
    // Six of the original 32 triangles meet the interior grid vertex.
    assert_eq!(surface.triangle_indices().len(), 26 * 3);
    assert!(!surface.triangle_indices().contains(&12));
    assert_eq!(
        surface.extent(),
        Some(([10.0, 20.0, 1.0], [14.0, 24.0, 1.0]))
    );
    assert_eq!(surface.unit_normals()[12], [0.0; 3]);
    assert_eq!(surface.try_uv_to_point(12.0, 22.0), None);
    assert_eq!(surface.try_uv_to_point(11.5, 22.0), None);
    assert_eq!(surface.try_uv_to_point(11.0, 22.0), Some([11.0, 22.0, 1.0]));

    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from(surface));
    assert_eq!(stage.get_center(mob), [12.0, 22.0, 1.0]);
    for field in stage.get(mob).unwrap().buffer.schema().fields() {
        assert!(
            stage
                .get(mob)
                .unwrap()
                .buffer
                .read_column(&field.name)
                .unwrap()
                .iter()
                .all(|v| v.is_finite())
        );
    }
    // Even a foreign edit to the inert row cannot enter the bounding box.
    stage
        .get_mut(mob)
        .unwrap()
        .buffer
        .write(12, "point", &[1e10, -1e10, 1e10]);
    assert_eq!(stage.get_center(mob), [12.0, 22.0, 1.0]);
    stage
        .get_mut(mob)
        .unwrap()
        .buffer
        .write(12, SURFACE_VALIDITY_FIELD, &[1.0]);
    assert!(stage.get_center(mob)[0] > 1e9);
}

#[test]
fn finite_samples_keep_the_exact_existing_schema_and_record_bits() {
    let strict: Mobject = spec().sample(|u, v| [u, v, u * v]).into();
    let holes: Mobject = spec()
        .try_sample_with_holes(|u, v| Ok::<_, Infallible>([u, v, u * v]))
        .unwrap()
        .into();
    assert_eq!(strict.buffer.schema(), holes.buffer.schema());
    assert!(surface_validity(&holes.buffer).unwrap().is_none());
    for field in strict.buffer.schema().fields() {
        assert!(strict.buffer.column_eq(&holes.buffer, &field.name));
    }
    assert!(matches!(
        spec().try_sample(pole),
        Err(SurfaceSampleError::InvalidSample { index: 12, .. })
    ));
}

#[test]
fn holes_never_swallow_callback_errors_or_finite_coordinate_overflow() {
    let mut calls = 0;
    let result = spec().try_sample_with_holes(|_, _| {
        calls += 1;
        if calls == 7 {
            Err("authored failure")
        } else {
            Ok([f64::NAN, 0.0, 0.0])
        }
    });
    assert_eq!(calls, 7);
    assert!(matches!(
        result,
        Err(SurfaceSampleError::Callback("authored failure"))
    ));
    assert!(matches!(
        spec().try_sample_with_holes(|_, _| Ok::<_, Infallible>([f64::MAX, 0.0, 0.0])),
        Err(SurfaceSampleError::InvalidSample { index: 0, .. })
    ));
}

#[test]
fn textures_and_wireframes_keep_missing_samples_without_false_connections() {
    let surface = spec().try_sample_with_holes(pole).unwrap();
    let textured = TexturedSurface::new(surface.clone(), "checker.png");
    assert_eq!(textured.sample_validity(), surface.sample_validity());
    assert_eq!(textured.triangle_indices(), surface.triangle_indices());
    let textured: Mobject = textured.into();
    assert_eq!(
        surface_validity(&textured.buffer).unwrap().unwrap()[12],
        0.0
    );
    assert_eq!(
        textured.buffer.read(12, "im_coords").unwrap(),
        vec![0.5, 0.5]
    );
    let mesh = SurfaceMesh::new(surface)
        .resolution(5, 5)
        .normal_nudge(0.0)
        .build();
    assert_eq!(mesh.children().len(), 10);
    assert_eq!(mesh.children()[2].path().unwrap().subpaths().len(), 2);
    assert_eq!(mesh.children()[7].path().unwrap().subpaths().len(), 2);
    assert_eq!(mesh.children()[0].path().unwrap().subpaths().len(), 1);
}

#[test]
fn finite_and_hole_surfaces_align_partially_reveal_and_become_each_other() {
    let mut stage = Stage::new();
    let hole = stage.add(Mobject::from(spec().try_sample_with_holes(pole).unwrap()));
    let finite = stage.add(Mobject::from(
        SurfaceSpec {
            resolution: (3, 3),
            ..spec()
        }
        .sample(|u, v| [u, v, 1.0]),
    ));
    stage.align_surface_points(finite, hole).unwrap();
    assert_eq!(
        stage.get(finite).unwrap().buffer.schema(),
        stage.get(hole).unwrap().buffer.schema()
    );
    assert!(
        surface_validity(&stage.get(finite).unwrap().buffer)
            .unwrap()
            .unwrap()
            .iter()
            .all(|&v| v == 1.0)
    );
    stage
        .surface_pointwise_become_partial(finite, hole, (5, 5), 1, 0.0, 0.5)
        .unwrap();
    let mask = surface_validity(&stage.get(finite).unwrap().buffer)
        .unwrap()
        .unwrap();
    assert_eq!(mask[12], 0.0);
    assert_eq!(mask[13], 0.0);
    assert_eq!(mask[14], 0.0);
    stage
        .surface_pointwise_become_partial(finite, hole, (5, 5), 1, 0.0, 1.0)
        .unwrap();
    assert_eq!(
        surface_validity(&stage.get(finite).unwrap().buffer).unwrap(),
        surface_validity(&stage.get(hole).unwrap().buffer).unwrap()
    );
    let complete = stage.add(Mobject::from(spec().sample(|u, v| [u, v, 1.0])));
    stage.become_mobject(finite, complete, false).unwrap();
    assert!(
        surface_validity(&stage.get(finite).unwrap().buffer)
            .unwrap()
            .unwrap()
            .iter()
            .all(|&v| v == 1.0)
    );
    stage.become_mobject(complete, hole, false).unwrap();
    assert_eq!(
        surface_validity(&stage.get(complete).unwrap().buffer)
            .unwrap()
            .unwrap()[12],
        0.0
    );
}

#[test]
fn equal_shape_mask_promotion_preserves_defaults_locks_and_old_views() {
    let mut stage = Stage::new();
    let hole = stage.add(Mobject::from(spec().try_sample_with_holes(pole).unwrap()));
    let finite = stage.add(Mobject::from(spec().sample(|u, v| [u, v, 1.0])));
    let entry = stage.get_mut(finite).unwrap();
    entry
        .buffer
        .write_default_record("rgba", &[0.1, 0.2, 0.3, 0.4]);
    entry.buffer.lock_data(["rgba"]);
    let view = entry.buffer.export_view(true);
    stage.align_surface_points(finite, hole).unwrap();
    let buffer = &stage.get(finite).unwrap().buffer;
    assert_eq!(
        buffer.default_record_slice("rgba"),
        Some([0.1, 0.2, 0.3, 0.4].as_slice())
    );
    assert_eq!(
        buffer.default_record_slice(SURFACE_VALIDITY_FIELD),
        Some([1.0].as_slice())
    );
    assert!(buffer.is_locked("rgba"));
    assert!(view.write(0, "point", &[99.0, 99.0, 99.0]));
    assert_eq!(stage.get_points(finite).unwrap()[0], [10.0, 20.0, 1.0]);
}

#[test]
fn regridding_never_interpolates_a_missing_vertex_into_present_geometry() {
    let mut stage = Stage::new();
    let hole = stage.add(Mobject::from(spec().try_sample_with_holes(pole).unwrap()));
    stage.resample_surface_grid(hole, (9, 9)).unwrap();
    let mask = surface_validity(&stage.get(hole).unwrap().buffer)
        .unwrap()
        .unwrap();
    assert_eq!(mask.iter().filter(|&&v| v == 0.0).count(), 9);
    assert_eq!(
        mask[4 * 9 + 2],
        1.0,
        "zero-weight hole cannot erase a finite knot"
    );
    assert_eq!(stage.get_center(hole), [12.0, 22.0, 1.0]);
}

#[test]
fn native_creation_and_transform_preserve_explicit_holes_at_every_sample() {
    use fmn_anim::{AnimConfig, Animation, RateFunc, Transform, show_surface_creation};
    let mut stage = Stage::new();
    let mob = stage.add(Mobject::from(spec().try_sample_with_holes(pole).unwrap()));
    let target = stage.add(Mobject::from(spec().sample(|u, v| [u, v, 2.0])));
    let mut creation = show_surface_creation(mob, (5, 5), 1);
    creation.begin(&mut stage).unwrap();
    creation.interpolate(&mut stage, 0.5);
    creation.finish(&mut stage);
    assert_eq!(
        surface_validity(&stage.get(mob).unwrap().buffer)
            .unwrap()
            .unwrap()[12],
        0.0
    );
    let mut morph = Transform::new(mob, target).with_config(AnimConfig {
        rate_func: RateFunc::linear(),
        ..AnimConfig::default()
    });
    morph.begin(&mut stage).unwrap();
    morph.interpolate(&mut stage, 0.5);
    assert_eq!(
        surface_validity(&stage.get(mob).unwrap().buffer)
            .unwrap()
            .unwrap()[12],
        0.0
    );
    morph.finish(&mut stage);
    assert!(
        surface_validity(&stage.get(mob).unwrap().buffer)
            .unwrap()
            .unwrap()
            .iter()
            .all(|&v| v == 1.0)
    );
}

fn native_camera_renderer(
    threads: usize,
) -> (fmn_render::Camera, fmn_render::RetainedFrameRenderer) {
    use fmn_core::color::LinearRgba;
    use fmn_render::{
        Camera, CameraConfig, EngineIdentity, FrameConfig, RetainedFrameRenderer,
        RetainedFrameRendererConfig, ScreenMap, Tiling, Viewport,
    };
    let background = LinearRgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    let camera = Camera::new(CameraConfig {
        resolution: (96, 64),
        background,
        ..CameraConfig::default()
    })
    .unwrap();
    let renderer = RetainedFrameRenderer::new(RetainedFrameRendererConfig {
        frame: FrameConfig::new(
            Viewport {
                width: 96,
                height: 64,
            },
            ScreenMap {
                scale: 8.0,
                origin: [48.0, 32.0],
                y_up: false,
            },
            background,
        ),
        tiling: Tiling {
            macro_tile: 32,
            fine_tile: 8,
        },
        engine: EngineIdentity::certified(),
        threads,
    })
    .unwrap();
    (camera, renderer)
}

#[test]
fn native_sampler_animation_and_renderer_preserve_visible_holes() {
    use fmn_anim::{AnimConfig, Animation, RateFunc, Transform, show_surface_creation};
    let spec = SurfaceSpec {
        u_range: (-2.0, 2.0),
        v_range: (-2.0, 2.0),
        resolution: (5, 5),
        shading: [0.0; 3],
        ..SurfaceSpec::default()
    };
    let hole = spec
        .try_sample_with_holes(|u, v| {
            Ok::<_, Infallible>([
                u,
                v,
                if u == 0.0 && v == 0.0 {
                    f64::INFINITY
                } else {
                    0.0
                },
            ])
        })
        .unwrap();
    let complete = spec.sample(|u, v| [u, v, 0.0]);
    let mut serial_frames = None;
    for threads in [1, 3] {
        let (camera, mut renderer) = native_camera_renderer(threads);
        let render = |renderer: &mut fmn_render::RetainedFrameRenderer, stage: &Stage| {
            renderer.render_with_camera(stage, &camera).unwrap();
            renderer.frame().plane(0).to_vec()
        };
        let empty = render(&mut renderer, &Stage::new());
        let offset = 32 * renderer.frame().layout().stride(0) + 48 * 8;
        let center = offset..offset + 8;
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from(hole.clone()));
        stage.add_to_scene(mob).unwrap();
        let baseline = render(&mut renderer, &stage);
        assert_eq!(baseline[center.clone()], empty[center.clone()]);
        assert_ne!(baseline, empty, "the defined surrounding surface must draw");

        let target = stage.add(Mobject::from(complete.clone()));
        let mut creation = show_surface_creation(mob, (5, 5), 1);
        creation.begin(&mut stage).unwrap();
        creation.interpolate(&mut stage, 0.5);
        let partial = render(&mut renderer, &stage);
        assert_eq!(partial[center.clone()], empty[center.clone()]);
        creation.finish(&mut stage);
        assert_eq!(render(&mut renderer, &stage), baseline);

        let mut morph = Transform::new(mob, target).with_config(AnimConfig {
            rate_func: RateFunc::linear(),
            ..AnimConfig::default()
        });
        morph.begin(&mut stage).unwrap();
        morph.interpolate(&mut stage, 0.5);
        let between = render(&mut renderer, &stage);
        assert_eq!(between[center.clone()], empty[center.clone()]);
        morph.finish(&mut stage);
        let filled = render(&mut renderer, &stage);
        assert_ne!(filled[center.clone()], empty[center]);
        let mut reference = Stage::new();
        let whole = reference.add(Mobject::from(complete.clone()));
        reference.add_to_scene(whole).unwrap();
        assert_eq!(filled, render(&mut renderer, &reference));

        let frames = vec![baseline, partial, between, filled];
        if let Some(serial) = &serial_frames {
            assert_eq!(
                &frames, serial,
                "native surface movie samples must agree across worker counts"
            );
        } else {
            serial_frames = Some(frames);
        }
    }
}
