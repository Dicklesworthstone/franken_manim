//! Publish a clipped compound path and read the resulting SVG as a viewer would.
use fmn_geom::SvgDocument;
use fmn_library::svg_export::stage_svg_document_with_background;
use fmn_mobject::{Mobject, Stage};
use fmn_render::svg::{SvgLimits, project_svg_stage};
use fmn_render::{Camera, CameraConfig};

#[test]
fn clipped_fill_round_trips_with_its_hole_and_paint() {
    let input = br#"<svg width="8" height="8"><path d="M1 1H7V7H1Z M3 3V5H5V3Z"/></svg>"#;
    let authored = SvgDocument::parse(input).unwrap();
    let points: Vec<_> = authored.shapes[0]
        .path
        .points()
        .iter()
        .map(|p| [p[0] - 4.0, p[1] - 4.0, 0.0])
        .collect();
    let mut object = Mobject::vector_from_points(&points);
    object
        .buffer
        .write_color_lanes("fill_rgba", Some([0.2, 0.4, 0.6]), Some(0.75));
    object.buffer.write_scalar_lanes("stroke_width", 0.0);
    let mut stage = Stage::new();
    let mob = stage.add(object);
    stage.add_to_scene(mob).unwrap();
    stage.uniforms_mut(mob).unwrap().clip_planes[0] = [1.0, 0.0, 0.0, 2.0];
    let camera = Camera::new(CameraConfig {
        resolution: (160, 90),
        ..CameraConfig::default()
    })
    .unwrap();
    let projected = project_svg_stage(&stage, &camera, SvgLimits::default()).unwrap();
    let svg =
        stage_svg_document_with_background(&projected, 160.0, 90.0, 160.0, 90.0, None).unwrap();
    let reloaded = SvgDocument::parse(svg.as_bytes()).unwrap();
    assert_eq!(reloaded.shapes.len(), 1);
    let shape = &reloaded.shapes[0];
    assert!(shape.style.fill.is_some() && shape.style.stroke.is_none());
    assert!((shape.style.fill_opacity - 0.75).abs() < 1e-6);
    let contours = shape.path.subpaths();
    assert_eq!(contours.len(), 2);
    let areas = contours
        .iter()
        .map(|contour| {
            assert_eq!(contour.first(), contour.last());
            contour
                .windows(3)
                .step_by(2)
                .map(|s| (s[0][0] * s[2][1] - s[2][0] * s[0][1]) * 0.5)
                .sum::<f64>()
        })
        .collect::<Vec<_>>();
    assert!(areas[0] * areas[1] < 0.0, "inner contour cancels winding");
    // Clipping x >= -2 leaves a 5 × 6 outer rectangle and the complete 2 × 2 hole.
    let pixel_scale = 90.0_f64 / camera.frame().height();
    assert!((areas[0].abs() - 30.0 * pixel_scale.powi(2)).abs() < 0.001);
    assert!((areas[1].abs() - 4.0 * pixel_scale.powi(2)).abs() < 0.001);
    assert_eq!(stage.get_points(mob).unwrap(), points);
}
