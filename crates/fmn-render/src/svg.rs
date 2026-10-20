//! Camera projection for bounded final-state SVG publication.
//!
//! Lumen owns the camera and the native record admission. Chisel's existing
//! SVG serializer still owns syntax, paint attributes and y inversion. This
//! adapter never rasterizes or changes the authored Stage.

use std::collections::{HashMap, TryReserveError};
use std::fmt;

use fmn_mobject::{Mob, RenderPrimitive, Stage, StageError};

use crate::Camera;

/// Geometry admitted to a vector document, counting repeated placements.
#[derive(Clone, Copy, Debug)]
pub struct SvgLimits {
    pub max_points: usize,
    pub max_shapes: usize,
}

impl Default for SvgLimits {
    fn default() -> Self {
        Self {
            max_points: 262_144,
            max_shapes: 32_768,
        }
    }
}

/// A scene cannot be expressed by the native flat-paint SVG serializer.
#[derive(Debug)]
pub enum SvgProjectionError {
    /// A native draw or camera effect needs a raster/3D output.
    Unsupported(&'static str),
    /// A shape/point count exceeded the caller's vector budget.
    GeometryLimit,
    /// A detached record or path operation failed.
    Stage(StageError),
    /// Bounded scratch storage could not be reserved.
    Allocation(TryReserveError),
}

impl fmt::Display for SvgProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(detail) => {
                write!(f, "SVG export: {detail}; use PNG for raster/3D rendering")
            }
            Self::GeometryLimit => f.write_str("SVG export: vector geometry budget exceeded"),
            Self::Stage(error) => write!(f, "SVG export: {error}"),
            Self::Allocation(error) => write!(f, "SVG export: {error}"),
        }
    }
}

impl std::error::Error for SvgProjectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Stage(error) => Some(error),
            Self::Allocation(error) => Some(error),
            Self::Unsupported(_) | Self::GeometryLimit => None,
        }
    }
}

impl From<StageError> for SvgProjectionError {
    fn from(error: StageError) -> Self {
        Self::Stage(error)
    }
}

impl From<TryReserveError> for SvgProjectionError {
    fn from(error: TryReserveError) -> Self {
        Self::Allocation(error)
    }
}

fn unsupported(detail: &'static str) -> SvgProjectionError {
    SvgProjectionError::Unsupported(detail)
}

/// Prepare pixel-centered, y-up vector records for Chisel's SVG serializer.
///
/// The returned Stage is a detached publication view. Its flat root sequence
/// preserves Marionette's current painter order, including shared children
/// placed under several roots. Each source identity is projected once. Bare
/// groups, points and camera trackers have no paint program and are omitted,
/// exactly as they are by the renderer. Resolved order replaces z-index keys;
/// the returned Stage is not an editable copy of the author's scene graph.
///
/// `camera.pixel_shape()` supplies both SVG pixel dimensions and frame extents
/// when serializing this view. The serializer performs y inversion and may add
/// `camera.background()` as its first vector rectangle. Stroke widths remain
/// in pixels, including the native zoom/fixed-frame mix.
///
/// # Errors
/// Refuses non-vector draws, varying paint/width, depth, lighting, clip planes,
/// and perspective-varying or depth-clipped quadratics. Constant homogeneous
/// weight is required: projecting the controls of a rational curve as an SVG
/// quadratic would change its shape. Validation and budgets precede geometry
/// copies; every failure leaves the input Stage unchanged.
pub fn project_svg_stage(
    stage: &Stage,
    camera: &Camera,
    limits: SvgLimits,
) -> Result<Stage, SvgProjectionError> {
    let sequence = admitted_sequence(stage, limits)?;
    let mut projected = Stage::new();
    let mut mapped = HashMap::<Mob, Mob>::new();
    mapped.try_reserve(sequence.len())?;
    let mut roots = Vec::new();
    roots.try_reserve_exact(sequence.len())?;
    let (width, height) = camera.pixel_shape();
    for source in sequence {
        if let Some(&target) = mapped.get(&source) {
            roots.push(target);
            continue;
        }
        let points = stage
            .get_points(source)
            .ok_or_else(|| unsupported("stale point records"))?;
        let uniforms = *stage
            .uniforms(source)
            .ok_or_else(|| unsupported("stale uniforms"))?;
        let mut weight = None;
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(points.len())?;
        for point in points {
            let clip = camera.project(point, uniforms.is_fixed_in_frame);
            let w = clip.clip[3];
            if !clip.clip.iter().all(|v| v.is_finite())
                || w <= 0.0
                || clip.clip[2].abs() > w
                || weight.is_some_and(|old| old != w)
            {
                return Err(unsupported("perspective-varying or depth-clipped paths"));
            }
            weight = Some(w);
            let pixel = clip
                .pixel((width, height))
                .ok_or_else(|| unsupported("invalid camera projection"))?;
            let point = [
                pixel[0] - f64::from(width) / 2.0,
                f64::from(height) / 2.0 - pixel[1],
                0.0,
            ];
            if point
                .iter()
                .any(|value| !value.is_finite() || value.abs() > f64::from(f32::MAX))
            {
                return Err(unsupported(
                    "projected coordinates exceed native record precision",
                ));
            }
            pixels.push(point);
        }
        let target = stage.copy_entry_into(source, &mut projected)?;
        projected.set_points(target, &pixels)?;
        projected.set_z_index(target, 0, false);
        if uniforms.scale_stroke_with_zoom {
            let scale = (1.0 - uniforms.is_fixed_in_frame) / camera.frame().scale()
                + uniforms.is_fixed_in_frame;
            let entry = projected.get_mut(target).ok_or(StageError::StaleHandle)?;
            let mut widths = entry
                .buffer
                .read_column("stroke_width")
                .ok_or_else(|| unsupported("non-vector record schema"))?;
            for width in &mut widths {
                let scaled = f64::from(*width) * scale;
                if !scaled.is_finite() || scaled.abs() > f64::from(f32::MAX) {
                    return Err(unsupported(
                        "projected stroke width exceeds native record precision",
                    ));
                }
                *width = scaled as f32;
            }
            if !entry.buffer.write_range("stroke_width", 0, &widths) {
                return Err(unsupported("invalid vector style records"));
            }
        }
        mapped.insert(source, target);
        roots.push(target);
    }
    projected.add_many_to_scene(&roots)?;
    Ok(projected)
}

fn admitted_sequence(stage: &Stage, limits: SvgLimits) -> Result<Vec<Mob>, SvgProjectionError> {
    let mut points = 0usize;
    let mut sequence = Vec::new();
    for root in stage.roots() {
        for mob in stage.family(*root) {
            let entry = stage.get(mob).ok_or(StageError::StaleHandle)?;
            if entry.buffer.is_empty() || !entry.paints() {
                continue;
            }
            points = points
                .checked_add(entry.buffer.len())
                .ok_or(SvgProjectionError::GeometryLimit)?;
            if sequence.len() >= limits.max_shapes || points > limits.max_points {
                return Err(SvgProjectionError::GeometryLimit);
            }
            if entry.render_primitive() != RenderPrimitive::Vector {
                return Err(unsupported("only quadratic vector families are supported"));
            }
            let uniforms = entry.uniforms();
            if uniforms.depth_test
                || uniforms.shading != [0.0; 3]
                || uniforms.clip_planes != [[0.0; 4]; 4]
            {
                return Err(unsupported(
                    "depth, lighting and user clip planes require Lumen",
                ));
            }
            for (name, width) in [("fill_rgba", 4), ("stroke_rgba", 4), ("stroke_width", 1)] {
                let values = entry
                    .buffer
                    .read_column(name)
                    .ok_or_else(|| unsupported("non-vector record schema"))?;
                if values.len() != entry.buffer.len() * width
                    || !values.iter().all(|v| v.is_finite())
                {
                    return Err(unsupported("invalid vector style records"));
                }
                if values
                    .chunks_exact(width)
                    .any(|row| row != &values[..width])
                {
                    return Err(unsupported("per-vertex paint and stroke-width variation"));
                }
            }
            sequence.try_reserve(1)?;
            sequence.push(mob);
        }
    }
    Ok(sequence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CameraConfig;
    use fmn_mobject::Mobject;

    fn square(stage: &mut Stage) -> Mob {
        let mob = stage.add(Mobject::vector_from_points(&[
            [-1.0, -1.0, 0.0],
            [0.0, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
            [-1.0, -1.0, 0.0],
        ]));
        for index in 0..9 {
            let entry = stage.get_mut(mob).unwrap();
            assert!(
                entry
                    .buffer
                    .write(index, "fill_rgba", &[0.2, 0.4, 0.6, 0.8])
            );
            assert!(entry.buffer.write(index, "stroke_rgba", &[1.0; 4]));
            assert!(entry.buffer.write(index, "stroke_width", &[4.0]));
        }
        mob
    }

    fn camera() -> Camera {
        Camera::new(CameraConfig {
            resolution: (160, 90),
            ..CameraConfig::default()
        })
        .unwrap()
    }

    #[test]
    fn svg_projection_preserves_camera_and_source_geometry() {
        let mut stage = Stage::new();
        let shape = square(&mut stage);
        stage.add_to_scene(shape).unwrap();
        stage.shift(shape, [2.0, 1.0, 0.0]);
        let before = stage.get_points(shape).unwrap();
        let mut camera = camera();
        camera.frame_mut().set_center([2.0, 1.0, 0.0]).unwrap();
        let projected = project_svg_stage(&stage, &camera, SvgLimits::default()).unwrap();
        let points = projected.get_points(projected.roots()[0]).unwrap();
        assert!((points[0][0] + points[4][0]).abs() < 1e-6);
        assert!((points[0][1] + points[4][1]).abs() < 1e-6);
        assert_eq!(stage.get_points(shape).unwrap(), before);
        camera.frame_mut().set_height(4.0).unwrap();
        let zoomed = project_svg_stage(&stage, &camera, SvgLimits::default()).unwrap();
        assert_ne!(zoomed.get_points(zoomed.roots()[0]).unwrap(), points);
    }

    #[test]
    fn svg_projection_keeps_fractional_attachment_and_zoom_strokes() {
        let mut stage = Stage::new();
        let shape = square(&mut stage);
        stage.add_to_scene(shape).unwrap();
        let mut camera = camera();
        camera.frame_mut().set_center([2.0, 0.0, 0.0]).unwrap();
        camera.frame_mut().set_height(4.0).unwrap();
        let mut centers = Vec::new();
        for attachment in [0.0, 0.5, 1.0] {
            let uniforms = stage.uniforms_mut(shape).unwrap();
            uniforms.is_fixed_in_frame = attachment;
            uniforms.scale_stroke_with_zoom = true;
            let projected = project_svg_stage(&stage, &camera, SvgLimits::default()).unwrap();
            let root = projected.roots()[0];
            let points = projected.get_points(root).unwrap();
            centers.push((points[0][0] + points[4][0]) / 2.0);
            let actual = projected
                .get(root)
                .unwrap()
                .buffer
                .read(0, "stroke_width")
                .unwrap()[0];
            let expected = 4.0 * ((1.0 - attachment) / camera.frame().scale() + attachment);
            assert!((f64::from(actual) - expected).abs() < 1e-6);
        }
        assert!(centers[0] < centers[1] && centers[1] < centers[2]);
        assert!((centers[1] - (centers[0] + centers[2]) / 2.0).abs() < 1e-5);
        assert_eq!(
            stage
                .get(shape)
                .unwrap()
                .buffer
                .read(0, "stroke_width")
                .unwrap(),
            [4.0]
        );
    }

    #[test]
    fn svg_projection_keeps_shared_placements_and_skips_programless_points() {
        let mut stage = Stage::new();
        let child = square(&mut stage);
        let a = stage.add(Mobject::from_points(&[[4.0, 3.0, 0.0], [5.0, 3.0, 0.0]]));
        let b = stage.add(Mobject::new());
        stage.attach(a, child).unwrap();
        stage.attach(b, child).unwrap();
        stage.add_many_to_scene(&[a, b]).unwrap();
        let projected = project_svg_stage(&stage, &camera(), SvgLimits::default()).unwrap();
        assert_eq!(projected.roots().len(), 2);
        assert_eq!(projected.roots()[0], projected.roots()[1]);
        assert_eq!(projected.draw_plan().items().len(), 2);
        assert!(matches!(
            project_svg_stage(
                &stage,
                &camera(),
                SvgLimits {
                    max_shapes: 1,
                    ..SvgLimits::default()
                }
            ),
            Err(SvgProjectionError::GeometryLimit)
        ));
        assert!(matches!(
            project_svg_stage(
                &stage,
                &camera(),
                SvgLimits {
                    max_points: 17,
                    ..SvgLimits::default()
                }
            ),
            Err(SvgProjectionError::GeometryLimit)
        ));
    }

    #[test]
    fn svg_projection_refuses_unrepresentable_paint_camera_and_records() {
        let mut stage = Stage::new();
        let shape = square(&mut stage);
        stage.add_to_scene(shape).unwrap();
        let mut camera = camera();
        stage.uniforms_mut(shape).unwrap().depth_test = true;
        assert!(project_svg_stage(&stage, &camera, SvgLimits::default()).is_err());
        stage.uniforms_mut(shape).unwrap().depth_test = false;
        camera.frame_mut().rotate(0.5, [1.0, 0.0, 0.0]).unwrap();
        assert!(project_svg_stage(&stage, &camera, SvgLimits::default()).is_err());
        stage.uniforms_mut(shape).unwrap().is_fixed_in_frame = 1.0;
        assert!(project_svg_stage(&stage, &camera, SvgLimits::default()).is_ok());
        stage
            .get_mut(shape)
            .unwrap()
            .buffer
            .write(0, "fill_rgba", &[1.0; 4]);
        assert!(project_svg_stage(&stage, &camera, SvgLimits::default()).is_err());
        stage
            .get_mut(shape)
            .unwrap()
            .buffer
            .write(0, "fill_rgba", &[0.2, 0.4, 0.6, 0.8]);
        stage.shift(shape, [1e38, 0.0, 0.0]);
        let before = stage.get_points(shape).unwrap();
        assert!(project_svg_stage(&stage, &camera, SvgLimits::default()).is_err());
        assert_eq!(stage.get_points(shape).unwrap(), before);
    }
}
