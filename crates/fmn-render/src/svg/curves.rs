//! Preserve perspective curvature and contour breaks on the SVG point grid.

use fmn_core::types::Vec3;
use fmn_geom::quadpath::is_subpath_break;

use super::{GeometryBudget, SvgProjectionError, unsupported};
use crate::Camera;
use crate::fill::{FLATTEN_MAX_DEPTH, RationalPiece};

const MAX_ERROR_PX: f64 = 0.1;

pub(super) fn project(
    points: &[Vec3],
    attachment: f64,
    camera: &Camera,
    budget: &mut GeometryBudget,
) -> Result<Option<Vec<Vec3>>, SvgProjectionError> {
    let projection = camera.projection();
    let mut clips = Vec::new();
    clips.try_reserve_exact(points.len())?;
    for &point in points {
        let clip = projection.project(point, attachment);
        let [_, _, z, w] = clip.clip;
        if !clip.clip.iter().all(|v| v.is_finite()) {
            return Err(unsupported("invalid camera projection"));
        }
        if w <= 0.0 || z.abs() > w {
            return Ok(None);
        }
        clips.push(clip);
    }
    budget.charge(points.len(), 1)?;
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(points.len())?;
    let Some(first) = clips.first() else {
        return Ok(Some(pixels));
    };
    let (width, height) = camera.pixel_shape();
    let (half_width, half_height) = (f64::from(width) / 2.0, f64::from(height) / 2.0);
    if clips.iter().all(|point| point.clip[3] == first.clip[3]) {
        // Preserve the exact arithmetic of the original pan/zoom and overlay path.
        for clip in clips {
            let p = clip
                .pixel((width, height))
                .ok_or_else(|| unsupported("invalid camera projection"))?;
            let point = [p[0] - half_width, half_height - p[1], 0.0];
            if point
                .iter()
                .any(|v| !v.is_finite() || v.abs() > f64::from(f32::MAX))
            {
                return Err(unsupported(
                    "projected coordinates exceed native record precision",
                ));
            }
            pixels.push(point);
        }
        return Ok(Some(pixels));
    }
    if points.len().is_multiple_of(2) {
        return Err(unsupported("invalid quadratic point count"));
    }
    let homogeneous = |[x, y, _, w]: [f64; 4]| [x * half_width, y * half_height, w];
    let anchor = |[x, y, w]: Vec3| record([x / w, y / w]);
    pixels.push(anchor(homogeneous(first.clip))?);
    for index in (0..points.len() - 2).step_by(2) {
        let controls = std::array::from_fn(|i| homogeneous(clips[index + i].clip));
        let world = [points[index], points[index + 1], points[index + 2]];
        let from = *pixels.last().expect("initial anchor");
        let piece = RationalPiece { p: controls };
        if is_subpath_break(world) {
            let to = anchor(controls[2])?;
            if !is_subpath_break([from, from, to]) {
                return Err(unsupported("camera projection collapses a subpath break"));
            }
            pixels.try_reserve(2)?;
            pixels.extend_from_slice(&[from, to]);
        } else if let Some(line) = straight_controls(world, piece, from)? {
            pixels.try_reserve(2)?;
            pixels.extend_from_slice(&line);
        } else {
            append(piece, 0, budget, &mut pixels)?;
        }
    }
    Ok(Some(pixels))
}

/// A straight edge stays straight under perspective, even though its speed
/// along the edge changes. Keep one SVG quadratic for authored line segments.
pub(super) fn straight_controls(
    [start, handle, end]: [Vec3; 3],
    piece: RationalPiece,
    first_record: Vec3,
) -> Result<Option<[Vec3; 2]>, SvgProjectionError> {
    if handle != std::array::from_fn(|i| 0.5 * start[i] + 0.5 * end[i]) {
        return Ok(None);
    }
    let a = piece.point(0.0);
    let b = piece.point(1.0);
    let midpoint = [0.5 * a[0] + 0.5 * b[0], 0.5 * a[1] + 0.5 * b[1]];
    let rounded = [first_record, record(midpoint)?, record(b)?];
    // Replacing the projected middle control by its ideal point on the chord
    // moves the rational curve by at most that displacement (positive weights).
    // The replacement traverses the entire segment. The rounded SVG controls
    // add at most their largest displacement to the geometric error.
    let [x, y, w] = piece.p[1];
    let t = piece.p[2][2] / (piece.p[0][2] + piece.p[2][2]);
    let ideal = |axis: usize| (1.0 - t) * a[axis] + t * b[axis];
    let chord_error = (x / w - ideal(0)).hypot(y / w - ideal(1));
    let record_error = [a, midpoint, b]
        .into_iter()
        .zip(rounded)
        .map(|(p, q)| (p[0] - q[0]).hypot(p[1] - q[1]))
        .fold(0.0, f64::max);
    Ok(
        (chord_error + record_error <= MAX_ERROR_PX && !is_subpath_break(rounded))
            .then_some([rounded[1], rounded[2]]),
    )
}

#[allow(clippy::cast_possible_truncation)]
pub(super) fn record([x, y]: [f64; 2]) -> Result<Vec3, SvgProjectionError> {
    let point = [f64::from(x as f32), f64::from(y as f32), 0.0];
    if point.iter().all(|value| value.is_finite()) {
        Ok(point)
    } else {
        Err(unsupported(
            "projected coordinates exceed native record precision",
        ))
    }
}

pub(super) fn append(
    piece: RationalPiece,
    depth: u32,
    budget: &mut GeometryBudget,
    pixels: &mut Vec<Vec3>,
) -> Result<(), SvgProjectionError> {
    let candidate = piece.integral_approximation();
    let controls = [
        *pixels.last().expect("shared curve anchor"),
        record(candidate.p1)?,
        record(candidate.p2)?,
    ];
    if !is_subpath_break(controls) && within_tolerance(piece, controls) {
        pixels.try_reserve(2)?;
        pixels.extend_from_slice(&controls[1..]);
        return Ok(());
    }
    if depth == FLATTEN_MAX_DEPTH {
        return Err(unsupported(
            "perspective curve exceeds native SVG precision",
        ));
    }
    // The source curve's two rows are already charged. Each split introduces
    // exactly one additional handle/anchor pair, even across repeated roots.
    budget.charge(2, 0)?;
    let (left, right) = piece.split(0.5);
    append(left, depth + 1, budget, pixels)?;
    append(right, depth + 1, budget, pixels)
}

/// Bound the whole interval, including the rounding already applied to Q.
/// For R = X/W, R-Q = (X-WQ)/W. Elevating numerator and denominator to
/// degree four gives a rational Bernstein curve with positive weights,
/// contained in the convex hull of its five control vectors. In each ratio
/// the common degree-four binomial divisor cancels.
fn within_tolerance(piece: RationalPiece, q: [Vec3; 3]) -> bool {
    let binomial = [1.0, 2.0, 1.0];
    let mut difference = [[0.0; 2]; 5];
    let mut weights = [0.0; 5];
    for (i, [x, y, w]) in piece.p.into_iter().enumerate() {
        for (j, [qx, qy, _]) in q.iter().copied().enumerate() {
            let factor = binomial[i] * binomial[j];
            difference[i + j][0] += factor * (x - w * qx);
            difference[i + j][1] += factor * (y - w * qy);
            weights[i + j] += factor * w;
        }
    }
    difference
        .into_iter()
        .zip(weights)
        .all(|([x, y], w)| w.is_finite() && x.hypot(y) <= MAX_ERROR_PX * w)
}
