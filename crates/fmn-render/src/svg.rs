//! Camera projection for bounded final-state SVG publication.
//!
//! Lumen owns the camera and the native record admission. Chisel's existing
//! SVG serializer still owns syntax, paint attributes and y inversion. This
//! adapter never rasterizes or changes the authored Stage.

use std::collections::{HashMap, TryReserveError};
use std::fmt;

use fmn_mobject::{Mob, RenderPrimitive, Stage, StageError};

use crate::Camera;

mod clipped;
mod curves;
mod dots;

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
    /// The camera could not clip a native vector path.
    Camera(crate::camera::CameraError),
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
            Self::Camera(error) => write!(f, "SVG export: {error}"),
            Self::Stage(error) => write!(f, "SVG export: {error}"),
            Self::Allocation(error) => write!(f, "SVG export: {error}"),
        }
    }
}

impl std::error::Error for SvgProjectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Stage(error) => Some(error),
            Self::Camera(error) => Some(error),
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

struct ProjectedFamily {
    roots: Vec<Mob>,
    points: usize,
}

struct GeometryBudget {
    limits: SvgLimits,
    points: usize,
    shapes: usize,
}

impl GeometryBudget {
    fn charge(&mut self, points: usize, shapes: usize) -> Result<(), SvgProjectionError> {
        let points = self.points.checked_add(points);
        let shapes = self.shapes.checked_add(shapes);
        match (points, shapes) {
            (Some(points), Some(shapes))
                if points <= self.limits.max_points && shapes <= self.limits.max_shapes =>
            {
                self.points = points;
                self.shapes = shapes;
                Ok(())
            }
            _ => Err(SvgProjectionError::GeometryLimit),
        }
    }
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
/// Hard `DotCloud`/`TrueDot` discs become separate flat-filled ellipse paths, with
/// each record's radius and RGBA. Their camera-facing projective silhouette is
/// retained, including off-center perspective; the quadratic approximation
/// and native-record rounding together stay within 0.1 output pixel. SVG
/// viewers own silhouette antialiasing, as they do for ordinary vector paths.
/// The geometry limits count the expanded paths, including repeated placements.
///
/// Perspective-projected vector paths use bounded quadratic subdivision.
/// Each resulting curve stays within 0.1 output pixel of the camera projection,
/// including native point-record rounding. Depth crossings use Lumen's camera
/// clipping. Clipped fills include the new cut boundaries, strokes include
/// only visible spans of the authored path. World-space clip planes are also
/// supported on fills. Clipped dot surfaces, clipped stroke surfaces at user
/// planes, lighting, depth testing, dot glow and varying paint/width require
/// raster output. Every failure leaves the input Stage unchanged.
pub fn project_svg_stage(
    stage: &Stage,
    camera: &Camera,
    limits: SvgLimits,
) -> Result<Stage, SvgProjectionError> {
    let sequence = admitted_sequence(stage, limits)?;
    let mut projected = Stage::new();
    let mut mapped = HashMap::<Mob, ProjectedFamily>::new();
    mapped.try_reserve(sequence.len())?;
    let mut roots = Vec::new();
    roots.try_reserve_exact(sequence.len())?;
    let mut budget = GeometryBudget {
        limits,
        points: 0,
        shapes: 0,
    };
    for source in sequence {
        if let Some(family) = mapped.get(&source) {
            budget.charge(family.points, family.roots.len())?;
            roots.try_reserve(family.roots.len())?;
            roots.extend_from_slice(&family.roots);
            continue;
        }
        if stage
            .get(source)
            .ok_or(StageError::StaleHandle)?
            .render_primitive()
            == RenderPrimitive::DotCloud
        {
            let family = dots::project(stage, source, camera, &mut budget, &mut projected)?;
            roots.try_reserve(family.roots.len())?;
            roots.extend_from_slice(&family.roots);
            mapped.insert(source, family);
            continue;
        }
        let points = stage
            .get_points(source)
            .ok_or_else(|| unsupported("stale point records"))?;
        let uniforms = *stage
            .uniforms(source)
            .ok_or_else(|| unsupported("stale uniforms"))?;
        let pixels = if clipped::active_planes(uniforms.clip_planes) {
            None
        } else {
            curves::project(&points, uniforms.is_fixed_in_frame, camera, &mut budget)?
        };
        let Some(pixels) = pixels else {
            let family =
                clipped::project(stage, source, &points, camera, &mut budget, &mut projected)?;
            roots.try_reserve(family.roots.len())?;
            roots.extend_from_slice(&family.roots);
            mapped.insert(source, family);
            continue;
        };
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
        mapped.insert(
            source,
            ProjectedFamily {
                roots: vec![target],
                points: pixels.len(),
            },
        );
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
            if points > limits.max_points {
                return Err(SvgProjectionError::GeometryLimit);
            }
            if !matches!(
                entry.render_primitive(),
                RenderPrimitive::Vector | RenderPrimitive::DotCloud
            ) {
                return Err(unsupported(
                    "only quadratic vector families and hard dots are supported",
                ));
            }
            let uniforms = entry.uniforms();
            if uniforms.depth_test || uniforms.shading != [0.0; 3] {
                return Err(unsupported("depth tests and lighting require Lumen"));
            }
            if !uniforms.clip_planes.iter().flatten().all(|v| v.is_finite()) {
                return Err(unsupported("non-finite clip planes"));
            }
            if entry.render_primitive() == RenderPrimitive::DotCloud {
                if clipped::active_planes(uniforms.clip_planes) {
                    return Err(unsupported("clipped dot surfaces require Lumen"));
                }
                dots::validate(entry)?;
            } else {
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
    fn svg_tilted_polygon_keeps_each_straight_edge_in_its_original_budget() {
        let mut stage = Stage::new();
        let shape = square(&mut stage);
        stage.add_to_scene(shape).unwrap();
        let mut camera = camera();
        camera.frame_mut().rotate(0.8, [1.0, 0.0, 0.0]).unwrap();
        let projected = project_svg_stage(
            &stage,
            &camera,
            SvgLimits {
                max_shapes: 1,
                max_points: 9,
            },
        )
        .unwrap();
        let pixels = projected.get_points(projected.roots()[0]).unwrap();
        assert_eq!(pixels.len(), 9);
        for edge in pixels.windows(3).step_by(2) {
            for i in 0..2 {
                assert!((edge[1][i] - 0.5 * (edge[0][i] + edge[2][i])).abs() < 1e-5);
            }
        }
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
        assert!(project_svg_stage(&stage, &camera, SvgLimits::default()).is_ok());
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

    #[test]
    fn depth_crossing_emits_separate_stroke_runs_and_only_fills_the_cut() {
        let camera = camera();
        let mut stage = Stage::new();
        let points = [[-1.0, 0.0, 0.0], [0.0, 1.0, 1000.0], [1.0, 0.0, 0.0]];
        let curve = stage.add(Mobject::vector_from_points(&points));
        let buffer = &mut stage.get_mut(curve).unwrap().buffer;
        buffer.write_color_lanes("fill_rgba", Some([1.0, 0.0, 0.0]), Some(0.8));
        buffer.write_color_lanes("stroke_rgba", Some([0.0, 0.0, 1.0]), Some(0.7));
        buffer.write_scalar_lanes("stroke_width", 4.0);
        stage.add_to_scene(curve).unwrap();
        for behind in [false, true] {
            stage.uniforms_mut(curve).unwrap().stroke_behind = behind;
            let view = project_svg_stage(&stage, &camera, SvgLimits::default()).unwrap();
            assert_eq!(view.roots().len(), 2);
            let fill = view.roots()[usize::from(behind)];
            let stroke = view.roots()[usize::from(!behind)];
            let attr = |mob, key| view.get(mob).unwrap().buffer.read(0, key).unwrap();
            assert_eq!(attr(fill, "fill_rgba"), [1.0, 0.0, 0.0, 0.8]);
            assert_eq!(attr(fill, "stroke_width"), [0.0]);
            assert_eq!(attr(stroke, "fill_rgba")[3], 0.0);
            assert_eq!(attr(stroke, "stroke_width"), [4.0]);
            let boundary = view.get_points(fill).unwrap();
            assert_eq!(boundary.first(), boundary.last());
            let edge = view.get_points(stroke).unwrap();
            let runs = fmn_geom::QuadPath::from_points(edge).unwrap();
            let runs = runs.subpaths();
            assert_eq!(runs.len(), 2, "no stroke across the clipped-out middle");
            // Every source-span endpoint must come from the camera's clipped
            // original quadratic. Cut edges appended only to the fill cannot pass.
            let spans = camera
                .project_quadratic(points, 0.0, [[0.0; 4]; 4])
                .unwrap();
            assert_eq!(spans.len(), runs.len());
            for (span, run) in spans.iter().zip(runs) {
                for (world, svg) in [(span.world[0], run[0]), (span.world[2], run[run.len() - 1])] {
                    let p = camera
                        .project(world, 0.0)
                        .pixel(camera.pixel_shape())
                        .unwrap();
                    assert!((svg[0] - (p[0] - 80.0)).hypot(svg[1] - (45.0 - p[1])) < 0.001);
                }
            }
        }
        assert!(matches!(
            project_svg_stage(
                &stage,
                &camera,
                SvgLimits {
                    max_points: 3,
                    max_shapes: 1
                }
            ),
            Err(SvgProjectionError::GeometryLimit)
        ));
        assert_eq!(stage.get_points(curve).unwrap(), points);
    }

    #[test]
    fn world_clip_planes_trim_fill_regions_and_remove_invisible_objects() {
        let mut stage = Stage::new();
        let rect = square(&mut stage);
        stage.add_to_scene(rect).unwrap();
        stage.uniforms_mut(rect).unwrap().clip_planes[0] = [1.0, 0.0, 0.0, 0.0];
        // Native stroke-surface clipping at a user plane is not a butt cap.
        assert!(matches!(
            project_svg_stage(&stage, &camera(), SvgLimits::default()),
            Err(SvgProjectionError::Unsupported(_))
        ));
        stage
            .get_mut(rect)
            .unwrap()
            .buffer
            .write_scalar_lanes("stroke_width", 0.0);
        stage.uniforms_mut(rect).unwrap().clip_planes[1] = [0.0, 1.0, 0.0, 0.0];
        let source = stage.get_points(rect).unwrap();
        let limits = SvgLimits {
            max_points: 9,
            max_shapes: 1,
        };
        let view = project_svg_stage(&stage, &camera(), limits).unwrap();
        let p = view.get_points(view.roots()[0]).unwrap();
        assert_eq!(p.len(), 9);
        assert_eq!(p.first(), p.last());
        assert!(p.iter().all(|p| p[0] >= 0.0 && p[1] >= 0.0));
        for a in [[0.0, 0.0], [11.25, 0.0], [0.0, 11.25], [11.25, 11.25]] {
            assert!(
                p.iter()
                    .step_by(2)
                    .any(|p| (p[0] - a[0]).hypot(p[1] - a[1]) < 0.001)
            );
        }
        stage.uniforms_mut(rect).unwrap().clip_planes[0][3] = -10.0;
        assert!(
            project_svg_stage(&stage, &camera(), limits)
                .unwrap()
                .roots()
                .is_empty()
        );
        assert_eq!(stage.get_points(rect).unwrap(), source);
    }
}
