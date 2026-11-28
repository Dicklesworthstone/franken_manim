//! Clip vector fills as regions and strokes as independent centerline spans.
//! This keeps real contour boundaries separate from synthetic cut edges.
use fmn_core::types::Vec3;
use fmn_geom::quadpath::is_subpath_break;
use fmn_mobject::{Mob, Mobject, Stage, StageError};

use crate::{Camera, camera::CameraError, fill::RationalPiece};

use super::{GeometryBudget, ProjectedFamily, SvgProjectionError, curves, unsupported};

pub(super) fn active_planes(planes: [[f64; 4]; 4]) -> bool {
    planes.iter().any(|plane| plane[..3] != [0.0; 3])
}

fn camera_error(error: CameraError) -> SvgProjectionError {
    match error {
        CameraError::ClipLimitExceeded { .. } => SvgProjectionError::GeometryLimit,
        error => SvgProjectionError::Camera(error),
    }
}

/// Append one visible rational span on the shared SVG point grid. A pen-up
/// transition occupies two records, just like an authored contour break.
fn span(
    world: [Vec3; 3],
    clip: [[f64; 4]; 3],
    new_run: bool,
    camera: &Camera,
    budget: &mut GeometryBudget,
    pixels: &mut Vec<Vec3>,
) -> Result<(), SvgProjectionError> {
    if !clip.iter().flatten().all(|v| v.is_finite()) || clip[0][3] <= 0.0 || clip[2][3] <= 0.0 {
        return Err(unsupported("invalid clipped camera projection"));
    }
    let (width, height) = camera.pixel_shape();
    let rational = RationalPiece {
        p: clip.map(|[x, y, _, w]| {
            [
                x * (0.5 * f64::from(width)),
                y * (0.5 * f64::from(height)),
                w,
            ]
        }),
    };
    let start = curves::record(rational.point(0.0))?;
    if let Some(&last) = pixels.last() {
        if new_run {
            if !is_subpath_break([last, last, start]) {
                return Err(unsupported("camera projection collapses a subpath break"));
            }
            budget.charge(2, 0)?;
            pixels.try_reserve(2)?;
            pixels.extend_from_slice(&[last, start]);
        }
    } else {
        budget.charge(1, 0)?;
        pixels.try_reserve(1)?;
        pixels.push(start);
    }
    budget.charge(2, 0)?;
    let from = *pixels.last().expect("curve start");
    if rational.p.iter().all(|control| control[2] > 0.0)
        && let Some(line) = curves::straight_controls(world, rational, from)?
    {
        pixels.try_reserve(2)?;
        pixels.extend_from_slice(&line);
        return Ok(());
    }
    curves::append(rational, 0, budget, pixels)
}

pub(super) fn project(
    stage: &Stage,
    source: Mob,
    points: &[Vec3],
    camera: &Camera,
    budget: &mut GeometryBudget,
    output: &mut Stage,
) -> Result<ProjectedFamily, SvgProjectionError> {
    let entry = stage.get(source).ok_or(StageError::StaleHandle)?;
    let uniforms = entry.uniforms();
    let attachment = uniforms.is_fixed_in_frame;
    let planes = uniforms.clip_planes;
    if points.len().is_multiple_of(2) || !points.iter().flatten().all(|v| v.is_finite()) {
        return Err(unsupported("invalid vector path for clipping"));
    }
    let projection = camera.projection();
    for &point in points {
        let projected = projection.project(point, attachment);
        if !projected.clip.iter().all(|v| v.is_finite())
            || planes.iter().any(|&plane| {
                projected
                    .user_clip_distance(plane)
                    .is_some_and(|d| !d.is_finite())
            })
        {
            return Err(unsupported("invalid clipped camera projection"));
        }
    }
    let rgba = |field| -> Result<[f32; 4], SvgProjectionError> {
        entry
            .buffer
            .read(0, field)
            .and_then(|v| v.try_into().ok())
            .ok_or_else(|| unsupported("invalid vector style records"))
    };
    let fill = rgba("fill_rgba")?;
    let stroke = rgba("stroke_rgba")?;
    let width = entry
        .buffer
        .read(0, "stroke_width")
        .and_then(|v| v.first().copied())
        .map(f64::from)
        .ok_or_else(|| unsupported("invalid vector style records"))?;
    let has_stroke = stroke[3] > 0.0 && width > 0.0;
    if has_stroke && active_planes(planes) {
        // Lumen clips the stroke surface, not just its centerline. A butt
        // cap at the plane crossing cannot reproduce an oblique surface cut.
        return Err(unsupported(
            "user clip planes on vector strokes require Lumen",
        ));
    }
    let mut fill_pixels = Vec::new();
    let mut stroke_pixels = Vec::new();
    let mut contour = Vec::new();
    contour.try_reserve_exact(points.len() / 2)?;
    let mut end_visible = false;
    for i in (0..points.len().saturating_sub(1)).step_by(2) {
        let world = [points[i], points[i + 1], points[i + 2]];
        if !is_subpath_break(world) {
            if fill[3] > 0.0 {
                contour.push(world);
            }
            if has_stroke {
                let spans = camera
                    .project_quadratic_with_limit(
                        world,
                        attachment,
                        planes,
                        budget.limits.max_points,
                    )
                    .map_err(camera_error)?;
                let mut last_t = None;
                for piece in spans {
                    let continues = last_t == Some(piece.source_t[0])
                        || (end_visible && piece.source_t[0] == 0.0);
                    span(
                        piece.world,
                        piece.clip,
                        !continues,
                        camera,
                        budget,
                        &mut stroke_pixels,
                    )?;
                    last_t = Some(piece.source_t[1]);
                }
                end_visible = last_t == Some(1.0);
            }
            if i + 3 < points.len() {
                continue;
            }
        }
        if !contour.is_empty() {
            let clipped = camera
                .project_fill_contour(&contour, attachment, planes, budget.limits.max_points)
                .map_err(camera_error)?;
            for (j, piece) in clipped.into_iter().enumerate() {
                span(
                    piece.world,
                    piece.clip,
                    j == 0,
                    camera,
                    budget,
                    &mut fill_pixels,
                )?;
            }
            contour.clear();
        }
        end_visible = false;
    }

    let mut roots = Vec::new();
    roots.try_reserve_exact(2)?;
    let points = fill_pixels.len() + stroke_pixels.len();
    if !fill_pixels.is_empty() {
        budget.charge(0, 1)?;
        let mut object = Mobject::vector_from_points(&fill_pixels);
        let [r, g, b, a] = fill.map(f64::from);
        object
            .buffer
            .write_color_lanes("fill_rgba", Some([r, g, b]), Some(a));
        object.buffer.write_scalar_lanes("stroke_width", 0.0);
        roots.push(output.add(object));
    }
    if !stroke_pixels.is_empty() {
        budget.charge(0, 1)?;
        let scale = if uniforms.scale_stroke_with_zoom {
            (1.0 - attachment) / camera.frame().scale() + attachment
        } else {
            1.0
        };
        let width = width * scale;
        if !width.is_finite() || width.abs() > f64::from(f32::MAX) {
            return Err(unsupported(
                "projected stroke width exceeds native record precision",
            ));
        }
        let mut object = Mobject::vector_from_points(&stroke_pixels);
        let [r, g, b, a] = stroke.map(f64::from);
        object
            .buffer
            .write_color_lanes("stroke_rgba", Some([r, g, b]), Some(a));
        object.buffer.write_scalar_lanes("stroke_width", width);
        object
            .buffer
            .write_color_lanes("fill_rgba", None, Some(0.0));
        roots.push(output.add(object));
    }
    if uniforms.stroke_behind {
        roots.reverse();
    }
    Ok(ProjectedFamily { roots, points })
}
