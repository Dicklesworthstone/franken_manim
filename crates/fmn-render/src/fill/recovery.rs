//! Geometry proofs for bounded primitive recovery from immutable snapshots.
//!
//! A live point writer invalidates semantic hints. Recovering a route therefore
//! needs a proof about every current curve, not a constructor name, three fitted
//! points, or the midpoint-only bound appropriate to trusted circular arcs.

use super::{FillKernel, HINT_BUDGET_PX, ScreenMap};
use crate::table::{Segment, Shape};

type Point = [f64; 2];

fn dot(a: Point, b: Point) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}

fn midpoint(a: Point, b: Point) -> Point {
    [0.5 * a[0] + 0.5 * b[0], 0.5 * a[1] + 0.5 * b[1]]
}

/// Squared distance along a quadratic is a quartic Bernstein polynomial.
/// Its five coefficients enclose the complete curve's squared radius. Splitting
/// a doubtful enclosure tightens it without replacing the curve by samples.
/// Four subdivisions bound work even for an adversarial almost-admitted path.
fn radial_enclosure(points: [Point; 3], lower: f64, upper: f64, depth: u8) -> bool {
    let [a, h, b] = points;
    let coefficients = [
        dot(a, a),
        dot(a, h),
        (dot(a, b) + 2.0 * dot(h, h)) / 3.0,
        dot(h, b),
        dot(b, b),
    ];
    if coefficients
        .iter()
        .all(|value| value.is_finite() && (lower..=upper).contains(value))
    {
        return true;
    }
    if depth == 0
        || !(lower..=upper).contains(&coefficients[0])
        || !(lower..=upper).contains(&coefficients[4])
    {
        return false;
    }
    let ah = midpoint(a, h);
    let hb = midpoint(h, b);
    let middle = midpoint(ah, hb);
    // An actual out-of-annulus midpoint is already a counterexample. Most
    // ordinary large circles decline here without walking the subdivision tree.
    if !(lower..=upper).contains(&dot(middle, middle)) {
        return false;
    }
    radial_enclosure([a, ah, middle], lower, upper, depth - 1)
        && radial_enclosure([middle, hb, b], lower, upper, depth - 1)
}

/// A common open half-plane keeps the curve and its endpoint chord away from
/// the centre throughout their homotopy. Their winding contributions then
/// agree, so the ordinary polygon crossing rule counts the actual curves too.
fn same_half_plane([a, h, b]: [Point; 3], roundoff: f64) -> bool {
    let direction = [a[0] + b[0], a[1] + b[1]];
    [a, h, b]
        .into_iter()
        .all(|point| dot(point, direction) > roundoff)
}

fn winding_step(a: Point, b: Point) -> i32 {
    let cross = a[0] * b[1] - a[1] * b[0];
    if a[1] <= 0.0 && b[1] > 0.0 && cross > 0.0 {
        1
    } else if a[1] > 0.0 && b[1] <= 0.0 && cross < 0.0 {
        -1
    } else {
        0
    }
}

pub(super) fn bounded_disc(
    shape: &Shape,
    segments: &[Segment],
    map: ScreenMap,
    translate: Point,
) -> FillKernel {
    if shape.subpath_starts.as_slice() != [0]
        || segments.len() < 4
        || !map.scale.is_finite()
        || map.scale <= 0.0
        || !map.origin.iter().chain(&translate).all(|v| v.is_finite())
    {
        return FillKernel::General;
    }
    let mut low = [f64::INFINITY; 2];
    let mut high = [f64::NEG_INFINITY; 2];
    let mut largest_coordinate = 0.0f64;
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 && segments[index - 1].p2 != segment.p0 {
            return FillKernel::General;
        }
        for point in [segment.p0, segment.p1, segment.p2] {
            if !point.iter().all(|v| v.is_finite()) || point[2] != 0.0 {
                return FillKernel::General;
            }
            largest_coordinate = largest_coordinate.max(point[0].abs()).max(point[1].abs());
        }
        for point in [segment.p0, segment.p2] {
            for axis in 0..2 {
                low[axis] = low[axis].min(point[axis]);
                high[axis] = high[axis].max(point[axis]);
            }
        }
    }
    // Opposite anchors make this centre independent of the starting angle of
    // an even-component circle. It is merely a candidate: every subsequent
    // curve is proved against it, so ellipses or asymmetric edits cannot rely
    // on fitting a few points to acquire a circular interpretation.
    let center = midpoint(low, high);
    let relative = |point: [f64; 3]| [point[0] - center[0], point[1] - center[1]];
    let first = relative(segments[0].p0);
    let radius_squared = dot(first, first);
    // Public callers can supply f64 geometry far smaller than widened record
    // f32 values. Subnormal squared radii lose the relative-error bound used
    // below and may underflow its arithmetic reserve entirely.
    if !radius_squared.is_normal() {
        return FillKernel::General;
    }
    let radius = radius_squared.sqrt();
    let pixel_radius = radius * map.scale;
    let pixel_center = map.to_pixel(center[0], center[1]);
    let pixel_center = [
        pixel_center[0] + translate[0],
        pixel_center[1] + translate[1],
    ];
    // Reserve arithmetic error from the existing budget rather than adding a
    // new tolerance to it. Huge/cancelling coordinates conservatively decline.
    let pixel_magnitude = largest_coordinate * map.scale
        + map.origin[0].abs().max(map.origin[1].abs())
        + translate[0].abs().max(translate[1].abs());
    let allowance = (HINT_BUDGET_PX - 128.0 * f64::EPSILON * pixel_magnitude) / map.scale;
    if !radius.is_finite()
        || !pixel_radius.is_finite()
        || !pixel_center.iter().all(|v| v.is_finite())
        || !allowance.is_finite()
        || allowance <= 0.0
        || radius <= allowance
    {
        return FillKernel::General;
    }
    let magnitude = largest_coordinate.max(radius);
    let roundoff = 128.0 * f64::EPSILON * magnitude * magnitude;
    let lower = (radius - allowance) * (radius - allowance) + roundoff;
    let upper = (radius + allowance) * (radius + allowance) - roundoff;
    if !lower.is_finite() || !upper.is_finite() || lower <= 0.0 || lower >= upper {
        return FillKernel::General;
    }
    let mut winding = 0i64;
    for segment in segments {
        let points = [
            relative(segment.p0),
            relative(segment.p1),
            relative(segment.p2),
        ];
        if !same_half_plane(points, roundoff) || !radial_enclosure(points, lower, upper, 4) {
            return FillKernel::General;
        }
        winding += i64::from(winding_step(points[0], points[2]));
    }
    // Fill implicitly closes every contour. A full Circle may retain a tiny
    // trigonometric endpoint discrepancy, but a partial arc's chord must pass
    // the same complete-curve bound; its unvisited sector is never filled in.
    let last = relative(segments[segments.len() - 1].p2);
    let closing = [last, midpoint(last, first), first];
    if !same_half_plane(closing, roundoff) || !radial_enclosure(closing, lower, upper, 4) {
        return FillKernel::General;
    }
    winding += i64::from(winding_step(last, first));
    if winding.abs() != 1 {
        return FillKernel::General;
    }
    // The actual contour lies inside the proven radial annulus. Its absolute
    // winding is one inside the inner radius and zero outside the outer radius;
    // therefore any disagreement with the disc is confined to that same band.
    FillKernel::Disc {
        center: pixel_center,
        radius: pixel_radius,
    }
}
