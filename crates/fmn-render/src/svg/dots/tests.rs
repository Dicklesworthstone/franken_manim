use super::*;
use crate::{
    CameraConfig,
    svg::{SvgLimits, project_svg_stage},
};
use fmn_mobject::{RecordBuffer, RecordSchema, RenderPrimitive};

fn camera() -> Camera {
    Camera::new(CameraConfig {
        resolution: (640, 360),
        ..CameraConfig::default()
    })
    .unwrap()
}

fn cloud(rows: &[(Vec3, f32, [f32; 4])]) -> Mobject {
    let schema = RecordSchema::new(
        &[("point", 3), ("radius", 1), ("rgba", 4)],
        &["point"],
        &["point"],
    )
    .unwrap();
    let mut buffer = RecordBuffer::new(schema, rows.len()).unwrap();
    for (row, (point, radius, rgba)) in rows.iter().enumerate() {
        assert!(buffer.write(row, "point", &point.map(|v| v as f32)));
        assert!(buffer.write(row, "radius", &[*radius]));
        assert!(buffer.write(row, "rgba", rgba));
    }
    Mobject::from_buffer(buffer).with_render_primitive(RenderPrimitive::DotCloud)
}

fn covariance(ellipse: &Ellipse) -> [[f64; 2]; 2] {
    std::array::from_fn(|i| {
        std::array::from_fn(|j| {
            ellipse.axes[0][i] * ellipse.axes[0][j] + ellipse.axes[1][i] * ellipse.axes[1][j]
        })
    })
}

#[test]
fn off_center_disc_matches_independent_perspective_ellipse() {
    let camera = camera();
    let x = 2.75;
    let radius = 0.4;
    let ellipse = Ellipse::project(&camera, [x, 0.0, 0.0], radius, 0.0).unwrap();
    // Intersecting rays from (0,0,d) with the disc's plane gives these
    // independent principal-axis formulas. An off-axis disc's silhouette
    // is larger and farther from the image center than a screen-space circle.
    let d = camera.location()[2];
    let scale = 1.0 / camera.pixel_size();
    let length = (x * x + d * d).sqrt();
    let h = 1.0 - radius * radius * x * x / (length * length * d * d);
    let expected_center = scale * x * (1.0 + radius * radius / (d * d * h));
    let rx = scale * radius * length / (d * h);
    let ry = scale * radius / h.sqrt();
    assert!((ellipse.center[0] - expected_center).abs() < 1e-10);
    assert!(ellipse.center[1].abs() < 1e-10);
    let actual = covariance(&ellipse);
    assert!((actual[0][0] - rx * rx).abs() < 1e-9);
    assert!((actual[1][1] - ry * ry).abs() < 1e-9);
    assert!(actual[0][1].abs() < 1e-9);
    assert!(ellipse.center[0] > scale * x);
    assert!(rx > ry);
}

#[test]
fn ellipse_matches_projected_circle_under_camera_motion_and_fractional_attachment() {
    let mut camera = camera();
    camera.frame_mut().set_center([0.75, -0.4, 0.2]).unwrap();
    camera.frame_mut().set_height(5.0).unwrap();
    camera.frame_mut().rotate(0.55, [1.0, 1.0, 0.0]).unwrap();
    let center = [-1.25, 0.7, 0.1];
    let radius = 0.35;
    let location = camera.location();
    let normal: Vec3 = std::array::from_fn(|i| location[i] - center[i]);
    let normalize = |v: Vec3| {
        let norm = v.iter().map(|v| v * v).sum::<f64>().sqrt();
        v.map(|v| v / norm)
    };
    let normal = normalize(normal);
    // A different orthonormal basis from the production billboard's proves
    // the projected locus, not its choice of tangent orientation.
    let a = normalize([0.0, normal[2], -normal[1]]);
    let b = [
        normal[1] * a[2] - normal[2] * a[1],
        normal[2] * a[0] - normal[0] * a[2],
        normal[0] * a[1] - normal[1] * a[0],
    ];
    for fixed in [0.0, 0.5, 1.0] {
        let ellipse = Ellipse::project(&camera, center, radius, fixed).unwrap();
        let [u, v] = ellipse.axes;
        let determinant = u[0] * v[1] - u[1] * v[0];
        for index in 0..128 {
            let angle = std::f64::consts::TAU * f64::from(index) / 128.0;
            let world = std::array::from_fn(|i| {
                center[i] + radius * (a[i] * fmn_dmath::cos(angle) + b[i] * fmn_dmath::sin(angle))
            });
            let pixel = camera.project(world, fixed).pixel((640, 360)).unwrap();
            let delta = [
                pixel[0] - 320.0 - ellipse.center[0],
                180.0 - pixel[1] - ellipse.center[1],
            ];
            let x = (delta[0] * v[1] - delta[1] * v[0]) / determinant;
            let y = (u[0] * delta[1] - u[1] * delta[0]) / determinant;
            assert!(
                (x * x + y * y - 1.0).abs() < 1e-10,
                "fixed={fixed}, sample={index}"
            );
        }
    }
}

#[test]
fn ellipse_quadratics_refine_to_the_declared_screen_error() {
    // Large, rotated and anisotropic ellipses exercise refinement past the
    // ordinary 16 arcs. Check actual curve samples against the radial point
    // on the exact ellipse, including the subsequent f32 record rounding.
    for axes in [[[3.0, 0.0], [0.0, 3.0]], [[4000.0, 700.0], [-300.0, 900.0]]] {
        let ellipse = Ellipse {
            center: [70.25, -30.125],
            axes,
        };
        let count = ellipse.component_count(4096).unwrap();
        if axes[0][0] > 100.0 {
            assert!(count > 16);
        }
        let points: Vec<Vec3> = ellipse
            .points(count)
            .unwrap()
            .into_iter()
            .map(|p| p.map(|v| f64::from(v as f32)))
            .collect();
        let [u, v] = axes;
        let determinant = u[0] * v[1] - u[1] * v[0];
        assert_eq!(points[0], *points.last().unwrap());
        for curve in (0..points.len() - 2).step_by(2) {
            for step in 0..=32 {
                let t = f64::from(step) / 32.0;
                let p: [f64; 2] = std::array::from_fn(|i| {
                    (1.0 - t) * (1.0 - t) * points[curve][i]
                        + 2.0 * t * (1.0 - t) * points[curve + 1][i]
                        + t * t * points[curve + 2][i]
                });
                let d = [p[0] - ellipse.center[0], p[1] - ellipse.center[1]];
                let x = (d[0] * v[1] - d[1] * v[0]) / determinant;
                let y = (u[0] * d[1] - u[1] * d[0]) / determinant;
                let norm = (x * x + y * y).sqrt();
                let exact: [f64; 2] =
                    std::array::from_fn(|i| ellipse.center[i] + (u[i] * x + v[i] * y) / norm);
                let error = ((p[0] - exact[0]).powi(2) + (p[1] - exact[1]).powi(2)).sqrt();
                assert!(error <= 0.1, "{count} components, error {error}");
            }
        }
    }
}

#[test]
fn projection_preserves_per_record_paint_radius_placement_and_shared_order() {
    let mut stage = Stage::new();
    let dot = stage.add(cloud(&[
        ([-1.0, 0.0, 0.0], 0.2, [1.0, 0.0, 0.0, 0.25]),
        ([1.0, 0.0, 0.0], 0.4, [0.0, 0.0, 1.0, 0.75]),
        ([2.0, 0.0, 0.0], 0.0, [0.0, 1.0, 0.0, 1.0]),
        ([3.0, 0.0, 0.0], 0.5, [0.0, 1.0, 0.0, 0.0]),
    ]));
    let a = stage.add(Mobject::new());
    let b = stage.add(Mobject::new());
    stage.attach(a, dot).unwrap();
    stage.attach(b, dot).unwrap();
    stage.add_many_to_scene(&[a, b]).unwrap();
    stage.stretch(dot, 2.0, 0);
    stage.shift(dot, [0.0, 0.5, 0.0]);
    let before = stage.get_points(dot).unwrap();
    let camera = camera();
    let projected = project_svg_stage(
        &stage,
        &camera,
        SvgLimits {
            max_points: 132,
            max_shapes: 4,
        },
    )
    .unwrap();
    assert_eq!(stage.get_points(dot).unwrap(), before);
    assert_eq!(projected.roots().len(), 4);
    assert_eq!(&projected.roots()[..2], &projected.roots()[2..]);
    for (row, &mob) in projected.roots()[..2].iter().enumerate() {
        let original = stage.get(dot).unwrap();
        let rgba = record::<4>(original, row, "rgba").unwrap();
        let radius = f64::from(record::<1>(original, row, "radius").unwrap()[0]);
        let expected = Ellipse::project(&camera, before[row], radius, 0.0)
            .unwrap()
            .points(16)
            .unwrap();
        let actual = projected.get_points(mob).unwrap();
        assert_eq!(
            actual,
            expected
                .iter()
                .map(|p| p.map(|v| f64::from(v as f32)))
                .collect::<Vec<_>>()
        );
        for point in 0..actual.len() {
            assert_eq!(
                projected
                    .get(mob)
                    .unwrap()
                    .buffer
                    .read(point, "fill_rgba")
                    .unwrap(),
                rgba
            );
            assert_eq!(
                projected
                    .get(mob)
                    .unwrap()
                    .buffer
                    .read(point, "stroke_width")
                    .unwrap(),
                [0.0]
            );
        }
    }
    for limits in [
        SvgLimits {
            max_points: 131,
            max_shapes: 4,
        },
        SvgLimits {
            max_points: 132,
            max_shapes: 3,
        },
    ] {
        assert!(matches!(
            project_svg_stage(&stage, &camera, limits),
            Err(SvgProjectionError::GeometryLimit)
        ));
    }
}

#[test]
fn zero_and_transparent_dots_are_omitted_but_invalid_records_and_effects_refuse() {
    let camera = camera();
    let mut stage = Stage::new();
    let dot = stage.add(cloud(&[
        ([0.0; 3], 0.0, [1.0; 4]),
        ([1.0, 0.0, 0.0], 0.5, [1.0, 0.0, 0.0, 0.0]),
    ]));
    stage.add_to_scene(dot).unwrap();
    assert!(
        project_svg_stage(&stage, &camera, SvgLimits::default())
            .unwrap()
            .roots()
            .is_empty()
    );
    stage
        .get_mut(dot)
        .unwrap()
        .buffer
        .write(0, "radius", &[f32::NAN]);
    assert!(project_svg_stage(&stage, &camera, SvgLimits::default()).is_err());
    stage
        .get_mut(dot)
        .unwrap()
        .buffer
        .write(0, "radius", &[0.2]);
    for effect in 0..5 {
        let uniforms = stage.uniforms_mut(dot).unwrap();
        *uniforms = fmn_mobject::Uniforms::default();
        match effect {
            0 => uniforms.glow_factor = 2.0,
            1 => uniforms.depth_test = true,
            2 => uniforms.shading[0] = 0.5,
            3 => uniforms.clip_planes[0] = [1.0, 0.0, 0.0, 0.0],
            _ => uniforms.anti_alias_width = -1.0,
        }
        let error = project_svg_stage(&stage, &camera, SvgLimits::default())
            .err()
            .expect("unsupported dot effect");
        if effect == 0 {
            assert!(error.to_string().contains("dot glow"));
        }
    }
    *stage.uniforms_mut(dot).unwrap() = fmn_mobject::Uniforms::default();
    stage.shift(dot, camera.location());
    let error = project_svg_stage(&stage, &camera, SvgLimits::default())
        .err()
        .expect("dot crosses the camera horizon");
    assert!(error.to_string().contains("depth or horizon"));
}
