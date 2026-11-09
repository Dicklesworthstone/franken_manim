//! Exact rounded-box recovery over the existing monotone quadratic pieces.
//!
//! Four perimeter side runs and four disjoint, monotone corner chains prove
//! that the region is its outer rectangle minus four corner cutouts. The
//! curves are never fitted to circles or replaced by chords. Only cutouts
//! intersecting a requested subcell invoke the existing analytic accumulator.

use super::{MonoPiece, accumulate_piece_row, box_overlap};
use crate::table::Shape;

type Point = [f64; 2];

#[derive(Debug, Clone, Copy)]
struct Corner {
    bounds: [f64; 4],
    first: u32,
    count: u32,
}

/// Compact frame-arena payload. Its ranges address the owning draw's already
/// derived monotone piece slice; no corner geometry is duplicated per instance.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RoundedRectData {
    bounds: [f64; 4],
    corners: [Corner; 4],
    piece_count: u32,
}

/// A proved rounded rectangle bound to the immutable pieces it examined.
///
/// "Rounded" describes its four monotone corner chains, not a circularity
/// assumption. Unequal, elliptical, subdivided or authored quadratic corners
/// retain their exact geometry when the same decomposition can be proved.
#[derive(Debug)]
pub struct QuadraticRoundedRect<'a> {
    pieces: &'a [MonoPiece],
    data: RoundedRectData,
}

impl<'a> QuadraticRoundedRect<'a> {
    /// Prove one closed contour from the shape's actual screen-space pieces.
    ///
    /// Ambiguous, non-finite, overlapping, degenerate or nonmonotone corners
    /// return `None`. Any cyclic start and either winding are admitted. The
    /// returned view retains the examined slice so its geometry cannot be
    /// replaced independently of the proof.
    #[must_use]
    pub fn from_shape(shape: &Shape, pieces: &'a [MonoPiece]) -> Option<Self> {
        if shape.subpath_starts.as_slice() != [0] {
            return None;
        }
        Some(Self {
            pieces,
            data: RoundedRectData::prepare(pieces)?,
        })
    }

    /// Exact coverage of the requested subcell, using the same scaled analytic
    /// integral as [`super::coverage_at_subcell`]. `row_y` precedes `cell`, as in
    /// that function. Invalid grids or unrepresentable coordinates return `None`.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn coverage_subcell(
        &self,
        translate: Point,
        row_y: u32,
        cell: u32,
        samples: u32,
        sample_x: u32,
        sample_y: u32,
    ) -> Option<f64> {
        self.data.coverage_subcell(
            self.pieces,
            translate,
            row_y,
            cell,
            samples,
            sample_x,
            sample_y,
        )
    }

    pub(crate) fn data(&self) -> RoundedRectData {
        self.data
    }
}

/// Bottom, right, top, left in positive perimeter order.
fn side(piece: &MonoPiece, bounds: [f64; 4]) -> Option<usize> {
    if piece.p0[1] == piece.p1[1] && piece.p1[1] == piece.p2[1] {
        if piece.p0[1] == bounds[1] {
            return Some(0);
        }
        if piece.p0[1] == bounds[3] {
            return Some(2);
        }
    }
    if piece.p0[0] == piece.p1[0] && piece.p1[0] == piece.p2[0] {
        if piece.p0[0] == bounds[2] {
            return Some(1);
        }
        if piece.p0[0] == bounds[0] {
            return Some(3);
        }
    }
    None
}

fn direction(piece: &MonoPiece, side: usize) -> i32 {
    let axis = side % 2;
    let sign = if piece.p2[axis] > piece.p0[axis] {
        1
    } else {
        -1
    };
    if side < 2 { sign } else { -sign }
}

fn ordered(a: f64, h: f64, b: f64) -> bool {
    a.min(b) <= h && h <= a.max(b)
}

fn positive_box(bounds: [f64; 4]) -> bool {
    let width = bounds[2] - bounds[0];
    let height = bounds[3] - bounds[1];
    width.is_normal()
        && width > 0.0
        && height.is_normal()
        && height > 0.0
        && (width * height).is_normal()
}

impl RoundedRectData {
    fn prepare(pieces: &[MonoPiece]) -> Option<Self> {
        let count = pieces.len();
        if count < 8 {
            return None;
        }
        let piece_count = u32::try_from(count).ok()?;
        let mut bounds = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for (index, piece) in pieces.iter().enumerate() {
            if piece.p2 != pieces[(index + 1) % count].p0 || piece.p0 == piece.p2 {
                return None;
            }
            if [piece.p0, piece.p1, piece.p2]
                .into_iter()
                .flatten()
                .any(|v| !v.is_finite())
            {
                return None;
            }
            for axis in 0..2 {
                if !ordered(piece.p0[axis], piece.p1[axis], piece.p2[axis]) {
                    return None;
                }
                bounds[axis] = bounds[axis].min(piece.p0[axis]).min(piece.p2[axis]);
                bounds[axis + 2] = bounds[axis + 2].max(piece.p0[axis]).max(piece.p2[axis]);
            }
        }
        if !positive_box(bounds) {
            return None;
        }
        // Start at the beginning of a side run, even when the supplied cyclic
        // run starts part-way through a subdivided side or corner.
        let first = (0..count).find(|&index| {
            side(&pieces[index], bounds).is_some()
                && side(&pieces[(index + count - 1) % count], bounds)
                    != side(&pieces[index], bounds)
        })?;
        let winding = direction(&pieces[first], side(&pieces[first], bounds)?);
        let mut corners = [Corner {
            bounds: [0.0; 4],
            first: 0,
            count: 0,
        }; 4];
        let mut visited = 0;
        let mut index = first;
        let mut sides = 0u8;
        for _ in 0..4 {
            let current_side = side(&pieces[index], bounds)?;
            if sides & (1 << current_side) != 0 {
                return None;
            }
            sides |= 1 << current_side;
            while visited < count && side(&pieces[index], bounds) == Some(current_side) {
                if direction(&pieces[index], current_side) != winding {
                    return None;
                }
                index = (index + 1) % count;
                visited += 1;
            }
            let corner_first = index;
            let before_corner = visited;
            let start = pieces[index].p0;
            while visited < count && side(&pieces[index], bounds).is_none() {
                index = (index + 1) % count;
                visited += 1;
            }
            let corner_count = visited - before_corner;
            let next_side = side(&pieces[index], bounds)?;
            let expected = (current_side as i32 + winding).rem_euclid(4) as usize;
            if corner_count == 0 || next_side != expected {
                return None;
            }
            let end = pieces[index].p0;
            let corner_bounds = [
                start[0].min(end[0]),
                start[1].min(end[1]),
                start[0].max(end[0]),
                start[1].max(end[1]),
            ];
            if !positive_box(corner_bounds) {
                return None;
            }
            // Both coordinates traverse the same direction throughout a
            // corner. Combined with the disjoint boxes below, this excludes
            // loops, inward reversals, side crossings and ambiguous cutouts.
            for offset in 0..corner_count {
                let piece = &pieces[(corner_first + offset) % count];
                for axis in 0..2 {
                    for point in [piece.p0, piece.p1, piece.p2] {
                        if !(corner_bounds[axis]..=corner_bounds[axis + 2]).contains(&point[axis]) {
                            return None;
                        }
                    }
                    if (end[axis] > start[axis] && piece.p2[axis] < piece.p0[axis])
                        || (end[axis] < start[axis] && piece.p2[axis] > piece.p0[axis])
                    {
                        return None;
                    }
                }
            }
            let quadrant = match (current_side.min(next_side), current_side.max(next_side)) {
                (0, 3) => 0, // bottom left
                (0, 1) => 1, // bottom right
                (1, 2) => 2, // top right
                (2, 3) => 3, // top left
                _ => return None,
            };
            corners[quadrant] = Corner {
                bounds: corner_bounds,
                first: u32::try_from(corner_first).ok()?,
                count: u32::try_from(corner_count).ok()?,
            };
        }
        if visited != count || index != first || sides != 15 {
            return None;
        }
        for (index, corner) in corners.iter().enumerate() {
            for other in &corners[index + 1..] {
                if corner.bounds[0].max(other.bounds[0]) < corner.bounds[2].min(other.bounds[2])
                    && corner.bounds[1].max(other.bounds[1]) < corner.bounds[3].min(other.bounds[3])
                {
                    return None;
                }
            }
        }
        Some(Self {
            bounds,
            corners,
            piece_count,
        })
    }

    /// Internal binding is owned by FrameJob: `pieces` is the exact retained or
    /// frame-local slice examined before this metadata entered its arena.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn coverage_subcell(
        &self,
        pieces: &[MonoPiece],
        translate: Point,
        row_y: u32,
        cell: u32,
        samples: u32,
        sample_x: u32,
        sample_y: u32,
    ) -> Option<f64> {
        if pieces.len() != self.piece_count as usize
            || samples == 0
            || sample_x >= samples
            || sample_y >= samples
            || !translate.iter().all(|value| value.is_finite())
        {
            return None;
        }
        let high_x = cell.checked_mul(samples)?.checked_add(sample_x)?;
        let high_y = row_y.checked_mul(samples)?.checked_add(sample_y)?;
        let right = high_x.checked_add(1)?;
        let scale = f64::from(samples);
        let translate = [translate[0] * scale, translate[1] * scale];
        let scaled_box = |bounds: [f64; 4]| {
            [
                bounds[0] * scale + translate[0],
                bounds[1] * scale + translate[1],
                bounds[2] * scale + translate[0],
                bounds[3] * scale + translate[1],
            ]
        };
        let rectangle = scaled_box(self.bounds);
        if !rectangle.iter().all(|value| value.is_finite()) || !positive_box(rectangle) {
            return None;
        }
        let cell_box = [
            f64::from(high_x),
            f64::from(high_y),
            f64::from(right),
            f64::from(high_y) + 1.0,
        ];
        let mut area = box_overlap(rectangle, cell_box);
        if area == 0.0 {
            return Some(0.0);
        }
        for (quadrant, corner) in self.corners.iter().enumerate() {
            if box_overlap(scaled_box(corner.bounds), cell_box) == 0.0 {
                continue;
            }
            let first = corner.first as usize;
            let end = first + corner.count as usize;
            let split = end.min(pieces.len());
            let wrapped = end - split;
            let mut cutout = 0.0;
            for piece in pieces[first..split].iter().chain(&pieces[..wrapped]) {
                cutout += contribution(piece, translate, high_y, high_x, scale);
            }
            let start = pieces[first].p0;
            let end = pieces[(end - 1) % pieces.len()].p2;
            let outer = [
                corner.bounds[if quadrant == 0 || quadrant == 3 { 0 } else { 2 }],
                corner.bounds[if quadrant < 2 { 1 } else { 3 }],
            ];
            // Close the cutout along its two outer-box sides. The horizontal
            // side contributes exactly zero to integral x dy; retain only the
            // vertical one, in the contour's actual winding direction.
            let (a, b) = if end[0] == outer[0] {
                (end, outer)
            } else {
                (outer, start)
            };
            let closure = MonoPiece {
                p0: a,
                p1: [a[0], 0.5 * a[1] + 0.5 * b[1]],
                p2: b,
            };
            cutout += contribution(&closure, translate, high_y, high_x, scale);
            // f64::min would conceal NaN. An unrepresentable accumulator must
            // fall back to the general path, never become an opaque cutout.
            if !cutout.is_finite() {
                return None;
            }
            area -= cutout.abs().min(1.0);
        }
        area.is_finite().then(|| area.clamp(0.0, 1.0))
    }
}

/// Exactly the general subcell path's per-piece expression tree, with the one
/// closed-form accumulator remaining the authority for curve/cell integrals.
fn contribution(piece: &MonoPiece, translate: Point, row_y: u32, cell: u32, scale: f64) -> f64 {
    let scaled = MonoPiece {
        p0: [piece.p0[0] * scale, piece.p0[1] * scale],
        p1: [piece.p1[0] * scale, piece.p1[1] * scale],
        p2: [piece.p2[0] * scale, piece.p2[1] * scale],
    };
    let mut window = [0.0; 2];
    let mut carry = 0.0;
    accumulate_piece_row(
        &scaled,
        translate,
        row_y,
        cell,
        cell + 1,
        &mut window,
        &mut carry,
    );
    carry + window[0]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bin::ScreenMap;
    use crate::fill::{
        Coeffs, FillKernel, HINT_BUDGET_PX, append_subpath, coverage_at_subcell, split_at,
    };
    use crate::hint::Hint;
    use crate::table::{compile_shape, shape_digest};
    use fmn_geom::QuadPath;

    fn unit_map() -> ScreenMap {
        ScreenMap {
            scale: 1.0,
            origin: [0.0; 2],
            y_up: false,
        }
    }

    /// Two tangent-intersection quadratics per quarter, the same curve family
    /// as Atlas's round_corners. This is a geometry fixture, not a replacement
    /// for the separate portal test that calls the actual public constructor.
    fn rounded_path(bounds: [f64; 4], radius: Point) -> QuadPath {
        let k = 2.0f64.sqrt() - 1.0;
        let s = 0.5f64.sqrt();
        let quarter = [[1.0, 0.0], [1.0, k], [s, s], [k, 1.0], [0.0, 1.0]];
        let centers = [
            [bounds[2] - radius[0], bounds[3] - radius[1]],
            [bounds[0] + radius[0], bounds[3] - radius[1]],
            [bounds[0] + radius[0], bounds[1] + radius[1]],
            [bounds[2] - radius[0], bounds[1] + radius[1]],
        ];
        let corners: [Vec<[f64; 3]>; 4] = std::array::from_fn(|index| {
            quarter
                .iter()
                .map(|&[x, y]| {
                    let [x, y] = match index {
                        0 => [x, y],
                        1 => [-y, x],
                        2 => [-x, -y],
                        _ => [y, -x],
                    };
                    [
                        centers[index][0] + radius[0] * x,
                        centers[index][1] + radius[1] * y,
                        0.0,
                    ]
                })
                .collect()
        });
        let mut path = QuadPath::new();
        for index in 0..4 {
            path.add_subpath(&corners[index]).unwrap();
            path.add_line_to(corners[(index + 1) % 4][0], false)
                .unwrap();
        }
        path
    }

    fn fixture(path: &QuadPath, map: ScreenMap) -> (Shape, Vec<MonoPiece>) {
        let (shape, _) = compile_shape(shape_digest(path.points()), path, Hint::General, 0)
            .expect("fixture fits retained geometry");
        let curves: Vec<_> = (0..path.num_curves())
            .map(|index| {
                path.nth_curve_points(index)
                    .unwrap()
                    .map(|point| map.to_pixel(point[0], point[1]))
            })
            .collect();
        let mut pieces = Vec::new();
        append_subpath(&curves, &mut pieces);
        (shape, pieces)
    }

    fn narrowed(path: &QuadPath) -> QuadPath {
        QuadPath::from_points(
            path.points()
                .iter()
                .map(|point| point.map(|value| f64::from(value as f32)))
                .collect(),
        )
        .unwrap()
    }

    fn exact_area(pieces: &[MonoPiece]) -> f64 {
        let mut area = 0.0;
        for piece in pieces {
            let c = Coeffs::<f64>::of(piece, [0.0; 2]);
            area += c.ax * c.by
                + (c.bx * c.by + 2.0 * c.ax * c.cy) / 2.0
                + (c.cx * c.by + 2.0 * c.bx * c.cy) / 3.0
                + 2.0 * c.cx * c.cy / 4.0;
        }
        area.abs()
    }

    #[test]
    fn quadratic_rounded_recovery_preserves_curves_for_all_starts_and_windings() {
        let path = narrowed(&rounded_path([2.25, 2.125, 45.75, 29.875], [6.4, 4.2]));
        let (shape, pieces) = fixture(&path, unit_map());
        let split: Vec<_> = pieces
            .iter()
            .flat_map(|piece| {
                let (a, b) = split_at([piece.p0, piece.p1, piece.p2], 0.5);
                [a, b].map(|[p0, p1, p2]| MonoPiece { p0, p1, p2 })
            })
            .collect();
        for original in [&pieces, &split] {
            for reverse in [false, true] {
                let mut candidate = original.clone();
                if reverse {
                    candidate.reverse();
                    for piece in &mut candidate {
                        std::mem::swap(&mut piece.p0, &mut piece.p2);
                    }
                }
                for start in 0..candidate.len() {
                    candidate.rotate_left(1);
                    let proof = QuadraticRoundedRect::from_shape(&shape, &candidate)
                        .expect("cyclic start, winding and subdivision retain the geometry");
                    for (x, y) in [(3, 3), (44, 3), (3, 28), (44, 28), (24, 15)] {
                        for sy in 0..4 {
                            for sx in 0..4 {
                                let actual =
                                    proof.coverage_subcell([0.0; 2], y, x, 4, sx, sy).unwrap();
                                let expected =
                                    coverage_at_subcell(&candidate, [0.0; 2], y, x, 4, sx, sy);
                                assert!(
                                    (actual - expected).abs() < 2e-12,
                                    "start={start}, reverse={reverse}, ({x},{y})/{sx},{sy}: {actual} vs {expected}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn quadratic_rounded_coverage_matches_the_general_integral_and_enclosed_area() {
        let path = rounded_path([2.25, 2.125, 45.75, 29.875], [6.4, 4.2]);
        for path in [path.clone(), narrowed(&path)] {
            let (shape, pieces) = fixture(&path, unit_map());
            let proof = QuadraticRoundedRect::from_shape(&shape, &pieces).unwrap();
            for translate in [[0.0; 2], [1.125, -0.875]] {
                for samples in [1, 2, 4] {
                    let mut area = 0.0;
                    for y in 0..32 {
                        for x in 0..48 {
                            for sy in 0..samples {
                                for sx in 0..samples {
                                    let actual = proof
                                        .coverage_subcell(translate, y, x, samples, sx, sy)
                                        .unwrap();
                                    let expected = coverage_at_subcell(
                                        &pieces, translate, y, x, samples, sx, sy,
                                    );
                                    assert!(
                                        (actual - expected).abs() < 2e-12,
                                        "{translate:?}, ({x},{y})/{samples}:{sx},{sy}: {actual} vs {expected}"
                                    );
                                    area += actual / f64::from(samples * samples);
                                }
                            }
                        }
                    }
                    let expected_area = exact_area(&pieces);
                    assert!(
                        (area - expected_area).abs() < 1e-10,
                        "area {area} vs {expected_area}, {samples} samples"
                    );
                }
            }
        }

        // Replacing these authored quadratics with perfect circular corners
        // would change pixels beyond the established approximation budget.
        let bounds = [2.25, 2.125, 45.75, 29.875];
        let (shape, pieces) = fixture(&rounded_path(bounds, [8.1; 2]), unit_map());
        let proof = QuadraticRoundedRect::from_shape(&shape, &pieces).unwrap();
        let circular = FillKernel::RoundedRect {
            rect: bounds,
            radius: 8.1,
        };
        let mut worst = 0.0f64;
        for y in 0..32 {
            for x in 0..48 {
                let actual = proof.coverage_subcell([0.0; 2], y, x, 1, 0, 0).unwrap();
                worst = worst.max((actual - circular.coverage(x, y).unwrap()).abs());
            }
        }
        assert!(
            worst > HINT_BUDGET_PX,
            "circular substitution error {worst}"
        );
    }

    #[test]
    fn quadratic_rounded_recovery_handles_record_precision_grid_and_live_edits() {
        let base = narrowed(&rounded_path([-0.19, -0.11, 0.19, 0.11], [0.06; 2]));
        for y_up in [false, true] {
            let map = ScreenMap {
                scale: 135.0,
                origin: [160.0, 90.0],
                y_up,
            };
            let mut paths: Vec<_> = (0..16)
                .map(|index| {
                    let center = [
                        -0.78 + f64::from(index % 4) * 0.52,
                        -0.45 + f64::from(index / 4) * 0.3,
                    ];
                    base.points()
                        .iter()
                        .map(|p| [(p[0] + center[0]) as f32, (p[1] + center[1]) as f32, 0.0])
                        .collect::<Vec<_>>()
                })
                .collect();
            for (mutation, expected) in [(0, 16), (1, 16), (2, 15), (3, 14)] {
                match mutation {
                    0 => {}
                    1 => {
                        for points in &mut paths {
                            for point in points {
                                point[0] += 0.375;
                            }
                        }
                    }
                    2 => paths[0][1][1] += 0.25,
                    _ => {
                        for point in &mut paths[1] {
                            point[0] += 0.5 * point[1];
                        }
                    }
                }
                let admitted = paths
                    .iter()
                    .filter(|points| {
                        let path = QuadPath::from_points(
                            points.iter().map(|point| point.map(f64::from)).collect(),
                        )
                        .unwrap();
                        let (shape, pieces) = fixture(&path, map);
                        QuadraticRoundedRect::from_shape(&shape, &pieces).is_some()
                    })
                    .count();
                assert_eq!(admitted, expected, "mutation {mutation}, y_up={y_up}");
            }
            let circle =
                QuadPath::try_arc(0.0, std::f64::consts::TAU, 0.5, [0.0; 3], Some(16)).unwrap();
            let (shape, pieces) = fixture(&narrowed(&circle), map);
            assert!(QuadraticRoundedRect::from_shape(&shape, &pieces).is_none());

            // Default-size corners need no circular approximation admission:
            // the exact quadratic path remains the integrator's input.
            let path = narrowed(&rounded_path([-2.0, -1.0, 2.0, 1.0], [0.5; 2]));
            let (shape, pieces) = fixture(&path, map);
            assert!(QuadraticRoundedRect::from_shape(&shape, &pieces).is_some());
        }
    }

    #[test]
    fn quadratic_rounded_recovery_refuses_unproved_contours() {
        let (shape, pieces) = fixture(
            &rounded_path([2.25, 2.125, 45.75, 29.875], [6.4, 4.2]),
            unit_map(),
        );
        assert!(QuadraticRoundedRect::from_shape(&shape, &pieces).is_some());
        let mut outside = pieces.clone();
        outside[0].p1[0] += 1.0;
        let mut bowed_side = pieces.clone();
        bowed_side[2].p1[1] += 0.25;
        let mut gap = pieces.clone();
        gap[1].p0[0] += 0.25;
        let mut nonfinite = pieces.clone();
        nonfinite[0].p1[1] = f64::NAN;
        let mut null = pieces.clone();
        null.insert(
            0,
            MonoPiece {
                p0: pieces[0].p0,
                p1: pieces[0].p0,
                p2: pieces[0].p0,
            },
        );
        let mut shear = pieces.clone();
        for piece in &mut shear {
            for point in [&mut piece.p0, &mut piece.p1, &mut piece.p2] {
                point[0] += 0.5 * point[1];
            }
        }
        let mut double_winding = pieces.clone();
        double_winding.extend_from_within(..);
        let mut backtracking = pieces.clone();
        backtracking.splice(
            3..3,
            [
                MonoPiece {
                    p0: pieces[2].p2,
                    p1: pieces[2].p1,
                    p2: pieces[2].p0,
                },
                pieces[2],
            ],
        );
        for (name, candidate) in [
            ("corner excursion", outside),
            ("bowed side", bowed_side),
            ("gap", gap),
            ("nonfinite", nonfinite),
            ("null curve", null),
            ("shear", shear),
            ("double winding", double_winding),
            ("side backtracking", backtracking),
            ("partial contour", pieces[..pieces.len() - 1].to_vec()),
        ] {
            assert!(
                QuadraticRoundedRect::from_shape(&shape, &candidate).is_none(),
                "{name}"
            );
        }
        let mut multi = shape.clone();
        multi.subpath_starts.push(3);
        assert!(QuadraticRoundedRect::from_shape(&multi, &pieces).is_none());
        for radius in [[0.0; 2], [8.0; 2], [5.0; 2]] {
            let (shape, pieces) =
                fixture(&rounded_path([0.0, 0.0, 10.0, 10.0], radius), unit_map());
            assert!(
                QuadraticRoundedRect::from_shape(&shape, &pieces).is_none(),
                "radius {radius:?}"
            );
        }
    }

    #[test]
    fn quadratic_rounded_coverage_checks_grid_limits_and_translated_coordinates() {
        let (shape, pieces) = fixture(
            &rounded_path([2.25, 2.125, 45.75, 29.875], [6.4, 4.2]),
            unit_map(),
        );
        let proof = QuadraticRoundedRect::from_shape(&shape, &pieces).unwrap();
        for (y, x, samples, sx, sy) in [
            (0, 0, 0, 0, 0),
            (0, 0, 2, 2, 0),
            (0, 0, 2, 0, 2),
            (0, u32::MAX, 1, 0, 0),
            (0, u32::MAX / 2, 4, 0, 0),
            (u32::MAX, 0, 4, 0, 0),
        ] {
            assert_eq!(
                proof.coverage_subcell([0.0; 2], y, x, samples, sx, sy),
                None
            );
        }
        for translate in [[f64::NAN, 0.0], [0.0, f64::INFINITY], [f64::MAX, 0.0]] {
            assert_eq!(proof.coverage_subcell(translate, 3, 3, 4, 0, 0), None);
        }
        for factor in [1e-160, 1e160] {
            let mut extreme = pieces.clone();
            for piece in &mut extreme {
                for point in [&mut piece.p0, &mut piece.p1, &mut piece.p2] {
                    point[0] *= factor;
                    point[1] *= factor;
                }
            }
            assert!(QuadraticRoundedRect::from_shape(&shape, &extreme).is_none());
        }
        for offset in [2.0f64.powi(30), -2.0f64.powi(40), 2.0f64.powi(52)] {
            let mut distant = pieces.clone();
            for piece in &mut distant {
                for point in [&mut piece.p0, &mut piece.p1, &mut piece.p2] {
                    point[0] += offset;
                    point[1] += offset;
                }
            }
            let proof = QuadraticRoundedRect::from_shape(&shape, &distant).unwrap();
            let translate = [-offset; 2];
            for y in 0..32 {
                for x in 0..48 {
                    for samples in [1, 4] {
                        for sample in 0..samples {
                            let actual = proof
                                .coverage_subcell(translate, y, x, samples, sample, sample)
                                .unwrap();
                            let expected = coverage_at_subcell(
                                &distant, translate, y, x, samples, sample, sample,
                            );
                            assert!(
                                (actual - expected).abs() < 2e-12,
                                "offset={offset}, ({x},{y})/{samples}:{sample}: {actual} vs {expected}"
                            );
                        }
                    }
                }
            }
        }
    }
}
