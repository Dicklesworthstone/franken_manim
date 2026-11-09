//! Exact saturation shortcuts for geometrically proved circular strokes.
//!
//! An ordinary quadratic Circle is not a true circle. The circle is therefore
//! only a distance bound: this module returns coverage zero or one when the
//! entire possible distance interval is in that same AA saturation region.
//! Every uncertain sample still uses the original quadratic-distance kernel.

use super::{ScreenMap, half_width_px};
use crate::table::{Segment, Shape, Style};

type Point = [f64; 2];

// A performance admission limit, not a permitted change to a rendered pixel.
// A wider enclosure sends more samples back to the ordinary distance solver.
const MAX_RADIAL_ERROR_PX: f64 = 0.25;

fn dot(a: Point, b: Point) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}

fn cross(a: Point, b: Point) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}

fn length(a: Point) -> f64 {
    dot(a, a).sqrt()
}

fn midpoint(a: Point, b: Point) -> Point {
    [0.5 * a[0] + 0.5 * b[0], 0.5 * a[1] + 0.5 * b[1]]
}

fn winding_step(a: Point, b: Point) -> i32 {
    let side = cross(a, b);
    if a[1] <= 0.0 && b[1] > 0.0 && side > 0.0 {
        1
    } else if a[1] > 0.0 && b[1] <= 0.0 && side < 0.0 {
        -1
    } else {
        0
    }
}

/// One immutable draw's circular distance enclosure and constant stroke state.
///
/// Construction proves the current controls rather than consulting a semantic
/// hint. The descriptor owns no geometry and cannot outlive its copied values;
/// callers retain the ordinary prepared stroke for every uncertain sample.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CircularStroke {
    center: Point,
    radius: f64,
    start: Point,
    end: Point,
    orientation: f64,
    major: bool,
    complete: bool,
    error: f64,
    reserve: f64,
    half_width: f64,
    half_aa: f64,
}

impl CircularStroke {
    /// Prove a single planar circular outline at this exact output scale.
    ///
    /// Profiles, non-round joins, camera-dependent styles, disconnected paths,
    /// angular reversals, multiple revolutions and numerically doubtful inputs
    /// decline. This changes no source record, hint, or authored style.
    pub(crate) fn from_shape(
        shape: &Shape,
        segments: &[Segment],
        style: &Style,
        map: ScreenMap,
        translate: Point,
    ) -> Option<Self> {
        if shape.subpath_starts.as_slice() != [0]
            || segments.is_empty()
            || style.stroke_profile.is_some()
            || style.stroke_station_is_observable()
            || style.stroke_width != style.stroke_width_end
            || style.stroke_rgba != style.stroke_rgba_end
            || !style.stroke_width.is_finite()
            || style.stroke_width <= 0.0
            || !style.stroke_rgba.into_iter().all(f32::is_finite)
            || !style.anti_alias_width.is_finite()
            || style.anti_alias_width <= 0.0
            || !matches!(
                style.joint_type,
                fmn_mobject::JointType::Auto | fmn_mobject::JointType::NoJoint
            )
            || style.is_fixed_in_frame != 0.0
            || style.scale_stroke_with_zoom
            || style.depth_test
            || style.shading != [0.0; 3]
            || style.clip_planes != [[0.0; 4]; 4]
            || !map.scale.is_finite()
            || map.scale <= 0.0
            || !map.origin.iter().chain(&translate).all(|v| v.is_finite())
        {
            return None;
        }

        let mut low = [f64::INFINITY; 2];
        let mut high = [f64::NEG_INFINITY; 2];
        let mut magnitude = 0.0f64;
        for (index, segment) in segments.iter().enumerate() {
            if index > 0 && segments[index - 1].p2 != segment.p0 {
                return None;
            }
            for point in [segment.p0, segment.p1, segment.p2] {
                if !point.into_iter().all(f64::is_finite) || point[2] != 0.0 {
                    return None;
                }
                magnitude = magnitude.max(point[0].abs()).max(point[1].abs());
            }
            for point in [segment.p0, segment.p2] {
                for axis in 0..2 {
                    low[axis] = low[axis].min(point[axis]);
                    high[axis] = high[axis].max(point[axis]);
                }
            }
        }
        let span = (high[0] - low[0]).max(high[1] - low[1]);
        if !(span > 0.0 && span.is_finite()) {
            return None;
        }

        // Three separated anchors propose a centre, but do not certify it.
        // Normalize before the circumcentre arithmetic so tiny/large inputs
        // cannot overflow the candidate fit. A one-curve arc uses its actual
        // midpoint as the third proposal; the subsequent full proof is the same.
        let origin = [segments[0].p0[0], segments[0].p0[1]];
        let normalized =
            |point: [f64; 3]| [(point[0] - origin[0]) / span, (point[1] - origin[1]) / span];
        let mut farthest = [0.0; 2];
        for segment in segments {
            let point = normalized(segment.p2);
            if dot(point, point) > dot(farthest, farthest) {
                farthest = point;
            }
        }
        let mut third = [0.0; 2];
        for segment in segments {
            let point = normalized(segment.p2);
            if cross(farthest, point).abs() > cross(farthest, third).abs() {
                third = point;
            }
        }
        if cross(farthest, third) == 0.0 && segments.len() == 1 {
            let segment = segments[0];
            let a = normalized(segment.p0);
            let h = normalized(segment.p1);
            let b = normalized(segment.p2);
            third = midpoint(midpoint(a, h), midpoint(h, b));
        }
        let determinant = 2.0 * cross(farthest, third);
        if !determinant.is_normal() {
            return None;
        }
        let u = dot(farthest, farthest);
        let v = dot(third, third);
        let center = [
            origin[0] + span * (u * third[1] - v * farthest[1]) / determinant,
            origin[1] + span * (v * farthest[0] - u * third[0]) / determinant,
        ];
        if !center.into_iter().all(f64::is_finite) {
            return None;
        }
        let relative = |point: [f64; 3]| [point[0] - center[0], point[1] - center[1]];
        let first = relative(segments[0].p0);
        let last = relative(segments[segments.len() - 1].p2);
        let radius_squared = dot(first, first);
        if !radius_squared.is_normal() {
            return None;
        }
        let radius = radius_squared.sqrt();
        let pixel_radius = radius * map.scale;
        let pixel_center = map.to_pixel(center[0], center[1]);
        let pixel_center = [
            pixel_center[0] + translate[0],
            pixel_center[1] + translate[1],
        ];
        let half_width = half_width_px(style, map, 0.0);
        let half_aa = 0.5 * f64::from(style.anti_alias_width);
        magnitude = magnitude
            .max(center[0].abs())
            .max(center[1].abs())
            .max(radius);
        let pixel_magnitude = magnitude * map.scale
            + map.origin[0].abs().max(map.origin[1].abs())
            + translate[0].abs().max(translate[1].abs());
        // Match the ordinary stroke cull's deliberately conservative solver
        // and norm reserve, not just the enclosure's arithmetic error bound.
        let reserve = 1e-9 * (1.0 + pixel_magnitude + pixel_radius + half_width + half_aa);
        if !pixel_radius.is_finite()
            || !pixel_center.into_iter().all(f64::is_finite)
            || !half_width.is_finite()
            || !reserve.is_finite()
            || reserve >= MAX_RADIAL_ERROR_PX / 8.0
        {
            return None;
        }
        let allowance = ((MAX_RADIAL_ERROR_PX - 4.0 * reserve) / map.scale).min(0.25 * radius);
        let roundoff = 128.0 * f64::EPSILON * magnitude * magnitude;
        let lower = (radius - allowance) * (radius - allowance) + roundoff;
        let upper = (radius + allowance) * (radius + allowance) - roundoff;
        if !(allowance > 0.0
            && allowance.is_finite()
            && lower.is_finite()
            && upper.is_finite()
            && lower > 0.0
            && lower < upper)
        {
            return None;
        }

        let initial_cross = cross(first, relative(segments[0].p2));
        if initial_cross.abs() <= roundoff {
            return None;
        }
        let orientation = if initial_cross > 0.0 { 1.0 } else { -1.0 };
        let phase = |point: Point| [dot(first, point), orientation * cross(first, point)];
        let mut wraps = 0usize;
        let mut winding = 0i64;
        for segment in segments {
            let [a, h, b] = [
                relative(segment.p0),
                relative(segment.p1),
                relative(segment.p2),
            ];
            let direction = [a[0] + b[0], a[1] + b[1]];
            // A common open half-plane bounds this curve's angular span by pi.
            // The Bernstein coefficients of cross(B(t), B'(t)) are
            // [2 cross(a,h), cross(a,b), 2 cross(h,b)]; a common strict sign
            // proves monotone angular coverage of the entire endpoint sector.
            if [a, h, b].into_iter().any(|p| dot(p, direction) <= roundoff)
                || [cross(a, h), cross(a, b), cross(h, b)]
                    .into_iter()
                    .any(|value| orientation * value <= roundoff)
                || !crate::fill::radial_enclosure([a, h, b], lower, upper, 4)
            {
                return None;
            }
            let pa = phase(a);
            let pb = phase(b);
            if pa[1] < 0.0 && pb[1] >= 0.0 {
                wraps += 1;
            }
            winding += i64::from(winding_step(a, b));
        }
        winding += i64::from(winding_step(last, first));
        let gap = length([last[0] - first[0], last[1] - first[1]]) * map.scale;
        let complete = winding.abs() == 1 && gap <= allowance * map.scale * 0.25;
        if wraps > usize::from(complete) || winding.abs() > 1 {
            return None;
        }
        let last_radius = length(last);
        if !last_radius.is_normal() {
            return None;
        }
        // ScreenMap's y convention may reflect the angular orientation.
        let y_sign = if map.y_up { -1.0 } else { 1.0 };
        Some(Self {
            center: pixel_center,
            radius: pixel_radius,
            start: [first[0] / radius, y_sign * first[1] / radius],
            end: [last[0] / last_radius, y_sign * last[1] / last_radius],
            orientation: orientation * y_sign,
            major: phase(last)[1] < 0.0,
            complete,
            // The closing chord can cover an angle absent from the authored
            // open endpoint run. Charging its full length bounds that gap too.
            error: allowance * map.scale + if complete { gap } else { 0.0 },
            reserve,
            half_width,
            half_aa,
        })
    }

    /// Exact AA saturation, or no shortcut when a quadratic solve is needed.
    ///
    /// Distance to two sets differs by at most their Hausdorff distance. The
    /// radial enclosure plus proved angular coverage bounds the authored path
    /// against this circle/arc; widening by its bound and the solver reserve
    /// therefore encloses the ordinary stroke's distance before AA clamps.
    pub(crate) fn coverage_if_saturated(self, pixel: Point) -> Option<f64> {
        let delta = [pixel[0] - self.center[0], pixel[1] - self.center[1]];
        let radial_length = length(delta);
        let reserve = self.reserve + 1e-9 * radial_length;
        if !radial_length.is_finite() || !reserve.is_finite() {
            return None;
        }
        let radial_distance = (radial_length - self.radius).abs();
        let uncertainty = self.error + reserve;
        // This lower bound holds even outside an open arc's angular sector.
        if radial_distance - uncertainty >= self.half_width + self.half_aa {
            return Some(0.0);
        }
        let distance = if self.complete {
            radial_distance
        } else {
            let from_start = self.orientation * cross(self.start, delta);
            let to_end = self.orientation * cross(delta, self.end);
            let inside = if self.major {
                from_start > reserve || to_end > reserve
            } else {
                from_start > reserve && to_end > reserve
            };
            let outside = if self.major {
                from_start < -reserve && to_end < -reserve
            } else {
                from_start < -reserve || to_end < -reserve
            };
            if inside {
                radial_distance
            } else if outside {
                length([
                    delta[0] - self.radius * self.start[0],
                    delta[1] - self.radius * self.start[1],
                ])
                .min(length([
                    delta[0] - self.radius * self.end[0],
                    delta[1] - self.radius * self.end[1],
                ]))
            } else {
                // A near-boundary angular comparison is not a proof.
                return None;
            }
        };
        if distance - uncertainty >= self.half_width + self.half_aa {
            Some(0.0)
        } else if distance + uncertainty <= self.half_width - self.half_aa {
            Some(1.0)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hint::Hint;
    use crate::stroke::PreparedStroke;
    use crate::table::{compile_shape, shape_digest};
    use fmn_geom::QuadPath;
    use std::f64::consts::{PI, TAU};

    #[allow(clippy::cast_possible_truncation)]
    fn outline(start: f64, sweep: f64, radius: f64, count: usize) -> (Shape, Vec<Segment>) {
        let path = QuadPath::try_arc(start, sweep, radius, [0.0; 3], Some(count)).unwrap();
        // Actual public records narrow authored controls to f32. Test that
        // geometry, not an ideal f64 circle with artificially exact anchors.
        let points = path
            .points()
            .iter()
            .map(|point| point.map(|value| f64::from(value as f32)))
            .collect();
        let path = QuadPath::from_points(points).unwrap();
        compile_shape(shape_digest(path.points()), &path, Hint::General, 0).unwrap()
    }

    fn style() -> Style {
        Style {
            stroke_width: 7.0,
            stroke_width_end: 7.0,
            stroke_rgba: [0.3, 0.7, 0.2, 0.6],
            stroke_rgba_end: [0.3, 0.7, 0.2, 0.6],
            ..Style::default()
        }
    }

    fn compare_samples(
        shape: &Shape,
        segments: &[Segment],
        style: &Style,
        map: ScreenMap,
        translate: Point,
    ) -> [usize; 3] {
        let proof = CircularStroke::from_shape(shape, segments, style, map, translate)
            .expect("ordinary current circle/arc is proved");
        let mut backing = Vec::new();
        let slab =
            PreparedStroke::prepare_into(&mut backing, segments, style, map, translate, false);
        let general = PreparedStroke::from_parts(&backing, slab);
        let mut decisions = [0; 3];
        // Probe the AA transition, both saturated sides, and open end caps
        // at subpixel phases. Every shortcut must equal the prepared oracle.
        for angle_index in 0..160 {
            let angle = TAU * f64::from(angle_index) / 160.0 + 0.0137;
            for offset in -28..=28 {
                let radius = proof.radius + f64::from(offset) * 0.25;
                let pixel = [
                    proof.center[0] + radius * angle.cos(),
                    proof.center[1] + radius * angle.sin(),
                ];
                match proof.coverage_if_saturated(pixel) {
                    Some(coverage) => {
                        let expected = general.shade(segments, &[], style, map, translate, pixel).0;
                        assert_eq!(
                            coverage.to_bits(),
                            expected.to_bits(),
                            "saturated coverage at {pixel:?}, proof={proof:?}"
                        );
                        decisions[usize::from(coverage == 1.0)] += 1;
                    }
                    None => decisions[2] += 1,
                }
            }
        }
        assert!(decisions[0] > 0, "zero saturation must be exercised");
        assert!(decisions[2] > 0, "AA fallback must be exercised");
        decisions
    }

    #[test]
    fn circular_stroke_saturation_matches_real_quadratics_and_round_caps() {
        for (start, sweep, count) in [
            (0.0, TAU, 16),
            (0.193, -TAU, 16),
            (1.9 * PI, 0.6 * PI, 5),
            (0.17, -1.6 * PI, 13),
            (0.4, PI, 8),
        ] {
            let (shape, segments) = outline(start, sweep, 1.0, count);
            for y_up in [false, true] {
                let map = ScreenMap {
                    scale: 135.0,
                    origin: [173.125, 182.375],
                    y_up,
                };
                let decisions = compare_samples(&shape, &segments, &style(), map, [0.375, -0.625]);
                assert!(decisions[1] > 0, "opaque saturation must be exercised");
            }
        }
    }

    #[test]
    fn circular_stroke_single_curve_and_narrow_aa_keep_exact_fallback() {
        let (shape, segments) = outline(-0.13, 0.2, 1.0, 1);
        let map = ScreenMap {
            scale: 95.5,
            origin: [2.125, -0.375],
            y_up: true,
        };
        let narrow = Style {
            anti_alias_width: 0.000_1,
            ..style()
        };
        compare_samples(&shape, &segments, &narrow, map, [1.25, -2.5]);

        let (shape, segments) = outline(0.23, TAU, 1.0, 16);
        let proof = CircularStroke::from_shape(&shape, &segments, &style(), map, [0.0; 2]).unwrap();
        assert_eq!(proof.coverage_if_saturated([f64::INFINITY, 0.0]), None);
        assert_eq!(proof.coverage_if_saturated([f64::NAN, 0.0]), None);
    }

    #[test]
    fn circular_stroke_requires_current_geometry_and_a_single_angular_run() {
        let (shape, segments) = outline(0.2, TAU, 1.0, 16);
        let map = ScreenMap {
            scale: 135.0,
            ..ScreenMap::default()
        };
        assert!(CircularStroke::from_shape(&shape, &segments, &style(), map, [0.0; 2]).is_some());
        // This handle is near the circle's top, so an x-only displacement is
        // mostly tangential. Its complete radial excursion is still admitted;
        // retain it as an exact-coverage witness rather than inventing a new
        // refusal for a curve already covered by the proof.
        let mut bounded_edit = segments.clone();
        bounded_edit[3].p1[0] += 0.04;
        let proof = CircularStroke::from_shape(&shape, &bounded_edit, &style(), map, [0.0; 2])
            .expect("the tangential edit remains inside the proven annulus");
        let mut observed_error = 0.0f64;
        for segment in &bounded_edit {
            for sample in 0..=128 {
                let t = f64::from(sample) / 128.0;
                let point: Point = std::array::from_fn(|axis| {
                    (1.0 - t) * (1.0 - t) * segment.p0[axis]
                        + 2.0 * (1.0 - t) * t * segment.p1[axis]
                        + t * t * segment.p2[axis]
                });
                let pixel = map.to_pixel(point[0], point[1]);
                observed_error = observed_error.max(
                    (length([pixel[0] - proof.center[0], pixel[1] - proof.center[1]])
                        - proof.radius)
                        .abs(),
                );
            }
        }
        assert!(observed_error > 0.20 && observed_error < MAX_RADIAL_ERROR_PX);
        eprintln!("bounded tangential edit: sampled radial excursion {observed_error:.12} px");
        compare_samples(&shape, &bounded_edit, &style(), map, [0.0; 2]);
        for change in 0..5 {
            let mut changed = segments.clone();
            match change {
                0 => {
                    // A radial handle displacement moves the curve's midpoint
                    // more than five pixels at this scale, outside the bound.
                    changed[3].p1[0] *= 1.08;
                    changed[3].p1[1] *= 1.08;
                }
                1 => changed[3].p1[2] = 0.01,
                2 => changed[3].p0[0] += 0.01,
                3 => changed[3].p1[1] = f64::NAN,
                _ => {
                    for segment in &mut changed {
                        for point in [&mut segment.p0, &mut segment.p1, &mut segment.p2] {
                            point[0] *= 1.2;
                        }
                    }
                }
            }
            assert!(
                CircularStroke::from_shape(&shape, &changed, &style(), map, [0.0; 2]).is_none(),
                "edit {change}"
            );
        }
        let mut broken = shape.clone();
        broken.subpath_starts.push(8);
        assert!(CircularStroke::from_shape(&broken, &segments, &style(), map, [0.0; 2]).is_none());
        for sweep in [2.0 * TAU, -2.0 * TAU, 1.2 * TAU] {
            let (shape, segments) = outline(0.2, sweep, 1.0, 32);
            assert!(
                CircularStroke::from_shape(&shape, &segments, &style(), map, [0.0; 2]).is_none()
            );
        }
        let (coarse, segments) = outline(0.2, TAU, 1.0, 4);
        assert!(CircularStroke::from_shape(&coarse, &segments, &style(), map, [0.0; 2]).is_none());
    }

    #[test]
    fn circular_stroke_refuses_variable_or_camera_dependent_stroke_state() {
        let (shape, segments) = outline(0.2, TAU, 1.0, 16);
        let map = ScreenMap {
            scale: 100.0,
            ..ScreenMap::default()
        };
        for change in 0..13 {
            let mut changed = style();
            match change {
                0 => changed.stroke_width_end += 1.0,
                1 => changed.stroke_rgba_end[0] += 0.1,
                2 => changed.anti_alias_width = 0.0,
                3 => changed.anti_alias_width = f32::NAN,
                4 => changed.joint_type = fmn_mobject::JointType::Miter,
                5 => changed.joint_type = fmn_mobject::JointType::Bevel,
                6 => changed.is_fixed_in_frame = 0.5,
                7 => changed.shading[0] = 0.5,
                8 => changed.depth_test = true,
                9 => changed.scale_stroke_with_zoom = true,
                10 => changed.clip_planes[0] = [1.0, 0.0, 0.0, 1.0],
                11 => {
                    changed.stroke_profile = Some(std::sync::Arc::new(
                        crate::StrokeProfile::new(vec![
                            crate::StrokeKnot {
                                s: 0.0,
                                width: 7.0,
                                rgba: changed.stroke_rgba,
                            },
                            crate::StrokeKnot {
                                s: 0.5,
                                width: 9.0,
                                rgba: changed.stroke_rgba,
                            },
                            crate::StrokeKnot {
                                s: 1.0,
                                width: 7.0,
                                rgba: changed.stroke_rgba,
                            },
                        ])
                        .unwrap(),
                    ));
                }
                _ => {
                    changed.stroke_rgba[0] = -0.0;
                    changed.stroke_rgba_end[0] = -0.0;
                    assert!(changed.stroke_station_is_observable());
                }
            }
            assert!(
                CircularStroke::from_shape(&shape, &segments, &changed, map, [0.0; 2]).is_none(),
                "style change {change}"
            );
        }
        let no_joint = Style {
            joint_type: fmn_mobject::JointType::NoJoint,
            ..style()
        };
        assert!(CircularStroke::from_shape(&shape, &segments, &no_joint, map, [0.0; 2]).is_some());
    }

    #[test]
    fn circular_stroke_numeric_reserve_declines_unrepresentable_enclosures() {
        let (shape, segments) = outline(0.0, TAU, 1.0, 16);
        for map in [
            ScreenMap {
                scale: 0.0,
                ..ScreenMap::default()
            },
            ScreenMap {
                scale: f64::INFINITY,
                ..ScreenMap::default()
            },
            ScreenMap {
                scale: 135.0,
                origin: [1e20, -1e20],
                y_up: false,
            },
        ] {
            assert!(
                CircularStroke::from_shape(&shape, &segments, &style(), map, [0.0; 2]).is_none()
            );
        }
        let mut tiny = segments.clone();
        for segment in &mut tiny {
            for point in [&mut segment.p0, &mut segment.p1, &mut segment.p2] {
                point[0] *= 1e-160;
                point[1] *= 1e-160;
            }
        }
        let map = ScreenMap {
            scale: 1e161,
            ..ScreenMap::default()
        };
        assert!(CircularStroke::from_shape(&shape, &tiny, &style(), map, [0.0; 2]).is_none());
    }
}
