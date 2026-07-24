//! Anchor-mode smoothing: the approximate local handle rule and the true
//! spline solve — ports of `approx_smooth_quadratic_bezier_handles`,
//! `get_smooth_cubic_bezier_handle_points`, and `smooth_quadratic_path`
//! (`manimlib/utils/bezier.py` @ `6199a00d`), computed in f64.
//!
//! The linear solves at the bottom of this file (banded for open paths,
//! dense for closed) are **temporary owned implementations**: doctrine D4
//! routes them through fsci-linalg, which becomes consumable once the
//! SUITE.lock plumbing lands (fm-g2c). The migration bead depends on that
//! work; the public functions here keep their signatures when it happens.

use crate::GeomError;
use crate::cubic;
use crate::space_ops;
use crate::vec;
use fmn_core::types::Vec3;

/// `approx_smooth_quadratic_bezier_handles`: local (solver-free) handles that
/// make each quadratic part of a parabola through its neighbor anchors.
/// Returns one handle per gap (`anchors.len() - 1`), or a single point for a
/// one-anchor input, matching the Reference's degenerate returns.
#[must_use]
pub fn approx_smooth_quadratic_handles(anchors: &[Vec3]) -> Vec<Vec3> {
    let n = anchors.len();
    if n == 1 {
        return vec![anchors[0]];
    }
    if n == 2 {
        return vec![space_ops::midpoint(anchors[0], anchors[1])];
    }
    // smooth_to_right[i] = ¼ p[i] + p[i+1] − ¼ p[i+2] on the forward points;
    // smooth_to_left is the same rule on the reversed sequence.
    let str_at = |i: usize| -> Vec3 {
        vec::sub(
            vec::add(vec::scale(anchors[i], 0.25), anchors[i + 1]),
            vec::scale(anchors[i + 2], 0.25),
        )
    };
    let stl_at = |i: usize| -> Vec3 {
        // reversed points: rp[k] = p[n-1-k]
        vec::sub(
            vec::add(vec::scale(anchors[n - 1 - i], 0.25), anchors[n - 2 - i]),
            vec::scale(anchors[n - 3 - i], 0.25),
        )
    };
    let closed = vec::np_isclose_all(anchors[0], anchors[n - 1]);
    let (last_str, last_stl) = if closed {
        (
            vec::sub(
                vec::add(vec::scale(anchors[n - 2], 0.25), anchors[n - 1]),
                vec::scale(anchors[1], 0.25),
            ),
            vec::sub(
                vec::add(vec::scale(anchors[1], 0.25), anchors[0]),
                vec::scale(anchors[n - 2], 0.25),
            ),
        )
    } else {
        (stl_at(0), str_at(0))
    };
    (0..n - 1)
        .map(|i| {
            let first = if i < n - 2 { str_at(i) } else { last_str };
            let second = if i == 0 { last_stl } else { stl_at(n - 2 - i) };
            vec::scale(vec::add(first, second), 0.5)
        })
        .collect()
}

/// `get_smooth_cubic_bezier_handle_points`: the two cubic handle sequences
/// making a C² spline through `anchors` — the banded system of
/// particleincell.com/2012/bezier-splines for open paths, with the
/// Reference's first/second-derivative row replacements for closed ones.
pub fn smooth_cubic_handles(anchors: &[Vec3]) -> Result<(Vec<Vec3>, Vec<Vec3>), GeomError> {
    let n_pts = anchors.len();
    if n_pts < 2 {
        return Ok((Vec::new(), Vec::new()));
    }
    let num_handles = n_pts - 1;
    let n = 2 * num_handles;
    let (l, u) = (2usize, 1usize);

    // LAPACK band storage: ab[u + i - j][j] = A[i][j].
    let mut ab = vec![vec![0.0; n]; l + u + 1];
    for j in (1..n).step_by(2) {
        ab[0][j] = -1.0;
    }
    for j in (2..n).step_by(2) {
        ab[0][j] = 1.0;
    }
    for j in (0..n).step_by(2) {
        ab[1][j] = 2.0;
    }
    for j in (1..n).step_by(2) {
        ab[1][j] = 1.0;
    }
    if n >= 2 {
        for j in (1..n.saturating_sub(2)).step_by(2) {
            ab[2][j] = -2.0;
        }
        for j in (0..n.saturating_sub(3)).step_by(2) {
            ab[3][j] = 1.0;
        }
        ab[2][n - 2] = -1.0;
        ab[1][n - 1] = 2.0;
    }

    let mut b = vec![[0.0f64; 3]; n];
    for (k, anchor) in anchors.iter().enumerate().skip(1) {
        b[2 * k - 1] = vec::scale(*anchor, 2.0);
    }
    b[0] = anchors[0];
    b[n - 1] = anchors[n_pts - 1];

    let closed = {
        // np.allclose(points[0], points[-1]) with numpy defaults.
        vec::np_isclose_all(anchors[0], anchors[n_pts - 1])
    };

    let mut solution = vec![[0.0f64; 3]; n];
    if closed {
        let mut matrix = band_to_dense(l, u, &ab, n);
        // Last row relates second derivatives across the seam,
        // first row relates first derivatives.
        for x in matrix[n - 1].iter_mut() {
            *x = 0.0;
        }
        matrix[n - 1][0] = 2.0;
        matrix[n - 1][1] = -1.0;
        matrix[n - 1][n - 2] = 1.0;
        matrix[n - 1][n - 1] = -2.0;
        for x in matrix[0].iter_mut() {
            *x = 0.0;
        }
        matrix[0][0] = 1.0;
        matrix[0][n - 1] = 1.0;
        b[0] = vec::scale(anchors[0], 2.0);
        b[n - 1] = [0.0; 3];
        for dim in 0..3 {
            let mut rhs: Vec<f64> = b.iter().map(|row| row[dim]).collect();
            solve_dense(&mut matrix.clone(), &mut rhs)?;
            for (row, value) in solution.iter_mut().zip(rhs) {
                row[dim] = value;
            }
        }
    } else {
        for dim in 0..3 {
            let mut rhs: Vec<f64> = b.iter().map(|row| row[dim]).collect();
            solve_banded(l, u, &ab, &mut rhs)?;
            for (row, value) in solution.iter_mut().zip(rhs) {
                row[dim] = value;
            }
        }
    }

    let h1 = solution.iter().step_by(2).copied().collect();
    let h2 = solution.iter().skip(1).step_by(2).copied().collect();
    Ok((h1, h2))
}

/// `smooth_quadratic_path`: a smooth quadratic spline through `anchors`, in
/// shared-anchor layout. Non-flat inputs are rotated to a plane, smoothed,
/// and rotated back, exactly as the Reference does.
///
/// The per-segment cubic→quadratic step currently uses the two-quad split of
/// [`cubic::quadratic_approximation_of_cubic`] — the Reference's own fallback
/// path. fm-6cf swaps in the error-bounded converter (§7.2), which subdivides
/// adaptively; outputs then gain resolution but keep this exact contract.
pub fn smooth_quadratic_path(anchors: &[Vec3]) -> Result<Vec<Vec3>, GeomError> {
    if anchors.len() < 2 {
        return Ok(anchors.to_vec());
    }
    if anchors.len() == 2 {
        let mean = space_ops::midpoint(anchors[0], anchors[1]);
        return Ok(vec![anchors[0], mean, anchors[1]]);
    }

    let is_flat = anchors.iter().all(|p| p[2] == 0.0);
    let mut working: Vec<Vec3> = anchors.to_vec();
    let mut rot = vec::IDENTITY;
    let mut shift = 0.0;
    if !is_flat {
        let normal = space_ops::cross(
            vec::sub(anchors[2], anchors[1]),
            vec::sub(anchors[1], anchors[0]),
        );
        rot = space_ops::z_to_vector(normal);
        for p in working.iter_mut() {
            *p = vec::mul_point_mat(*p, &rot);
        }
        shift = working[0][2];
        for p in working.iter_mut() {
            p[2] -= shift;
        }
    }

    let (h1s, h2s) = smooth_cubic_handles(&working)?;
    // Work in the xy-plane like the Reference (it collects 2D rows and lifts
    // back at the end); z is zero here by construction.
    let mut quads: Vec<Vec3> = vec![[working[0][0], working[0][1], 0.0]];
    for i in 0..working.len() - 1 {
        let approx =
            cubic::quadratic_approximation_of_cubic(working[i], h1s[i], h2s[i], working[i + 1]);
        for p in &approx[1..] {
            quads.push([p[0], p[1], 0.0]);
        }
    }

    if !is_flat {
        let rot_t = vec::transpose(&rot);
        for p in quads.iter_mut() {
            p[2] += shift;
            *p = vec::mul_point_mat(*p, &rot_t);
        }
    }
    Ok(quads)
}

/// Expand LAPACK band storage into a dense matrix
/// (`bezier.diag_to_matrix`).
fn band_to_dense(l: usize, u: usize, ab: &[Vec<f64>], n: usize) -> Vec<Vec<f64>> {
    let mut m = vec![vec![0.0; n]; n];
    for (r, band_row) in ab.iter().enumerate().take(l + u + 1) {
        for (j, &value) in band_row.iter().enumerate() {
            // A[i][j] with i = r + j - u.
            let i = r as isize + j as isize - u as isize;
            if (0..n as isize).contains(&i) {
                m[i as usize][j] = value;
            }
        }
    }
    m
}

/// Banded Gaussian elimination with partial pivoting (the dgbsv scheme):
/// solve `A x = b` in place, `A` given in band storage
/// `ab[u + i - j][j] = A[i][j]`.
///
/// Temporary owned solver — migrates to fsci-linalg (see module docs).
fn solve_banded(l: usize, u: usize, ab: &[Vec<f64>], b: &mut [f64]) -> Result<(), GeomError> {
    let n = b.len();
    let width = 2 * l + u + 1;
    // Working band with room for pivoting fill-in:
    // w[l + u + i - j][j] = A[i][j].
    let mut w = vec![vec![0.0; n]; width];
    for (r, band_row) in ab.iter().enumerate().take(l + u + 1) {
        for (j, &value) in band_row.iter().enumerate() {
            let i = r as isize + j as isize - u as isize;
            if (0..n as isize).contains(&i) {
                // w row = l + u + i - j = r + l, always in range.
                w[r + l][j] = value;
            }
        }
    }

    // Element A[i][j] lives at w[l + u + i - j][j]; callers keep j within
    // [i - l, i + u + l] so the row index never leaves [0, 2l + u].
    let idx = |i: usize, j: usize| -> (usize, usize) { (l + u + i - j, j) };

    for k in 0..n {
        let i_max = (k + l).min(n - 1);
        // Partial pivot over rows k..=i_max in column k.
        let mut piv_row = k;
        let mut piv_val = {
            let (r, c) = idx(k, k);
            w[r][c].abs()
        };
        for i in k + 1..=i_max {
            let (r, c) = idx(i, k);
            if w[r][c].abs() > piv_val {
                piv_val = w[r][c].abs();
                piv_row = i;
            }
        }
        if piv_val == 0.0 {
            return Err(GeomError::SingularSystem);
        }
        let j_max = (k + u + l).min(n - 1);
        if piv_row != k {
            for j in k..=j_max {
                let (r1, c1) = idx(piv_row, j);
                let (r2, c2) = idx(k, j);
                let tmp = w[r1][c1];
                w[r1][c1] = w[r2][c2];
                w[r2][c2] = tmp;
            }
            b.swap(piv_row, k);
        }
        let (rk, ck) = idx(k, k);
        let pivot = w[rk][ck];
        for i in k + 1..=i_max {
            let (ri, ci) = idx(i, k);
            let factor = w[ri][ci] / pivot;
            if factor == 0.0 {
                continue;
            }
            for j in k..=j_max {
                let (r1, c1) = idx(k, j);
                let (r2, c2) = idx(i, j);
                w[r2][c2] -= factor * w[r1][c1];
            }
            b[i] -= factor * b[k];
        }
    }

    // Back substitution.
    for i in (0..n).rev() {
        let j_max = (i + u + l).min(n - 1);
        let mut sum = b[i];
        #[allow(clippy::needless_range_loop)] // band indexing needs j itself
        for j in i + 1..=j_max {
            let (r, c) = idx(i, j);
            sum -= w[r][c] * b[j];
        }
        let (r, c) = idx(i, i);
        b[i] = sum / w[r][c];
    }
    Ok(())
}

/// Dense Gaussian elimination with partial pivoting, in place.
///
/// Temporary owned solver — migrates to fsci-linalg (see module docs).
fn solve_dense(m: &mut [Vec<f64>], b: &mut [f64]) -> Result<(), GeomError> {
    let n = b.len();
    for k in 0..n {
        let mut piv_row = k;
        let mut piv_val = m[k][k].abs();
        for (i, row) in m.iter().enumerate().skip(k + 1) {
            if row[k].abs() > piv_val {
                piv_val = row[k].abs();
                piv_row = i;
            }
        }
        if piv_val == 0.0 {
            return Err(GeomError::SingularSystem);
        }
        if piv_row != k {
            m.swap(piv_row, k);
            b.swap(piv_row, k);
        }
        let pivot = m[k][k];
        for i in k + 1..n {
            let factor = m[i][k] / pivot;
            if factor == 0.0 {
                continue;
            }
            let (upper_rows, lower_rows) = m.split_at_mut(i);
            let pivot_row = &upper_rows[k];
            for (j, cell) in lower_rows[0].iter_mut().enumerate().skip(k) {
                *cell -= factor * pivot_row[j];
            }
            b[i] -= factor * b[k];
        }
    }
    for i in (0..n).rev() {
        let mut sum = b[i];
        for j in i + 1..n {
            sum -= m[i][j] * b[j];
        }
        b[i] = sum / m[i][i];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_anchor_handles_sit_at_thirds() {
        // Closed form: the natural cubic through two points is the straight
        // line, handles at ⅓ and ⅔.
        let anchors = [[0.0, 0.0, 0.0], [3.0, 0.0, 0.0]];
        let (h1, h2) = smooth_cubic_handles(&anchors).unwrap();
        assert_eq!(h1.len(), 1);
        assert!((h1[0][0] - 1.0).abs() < 1e-12);
        assert!((h2[0][0] - 2.0).abs() < 1e-12);
    }

    #[test]
    fn collinear_anchors_yield_collinear_handles() {
        let anchors = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
        ];
        let (h1, h2) = smooth_cubic_handles(&anchors).unwrap();
        for h in h1.iter().chain(h2.iter()) {
            assert!(h[1].abs() < 1e-12 && h[2].abs() < 1e-12);
        }
        // Handles are ordered along the line within each segment.
        for i in 0..3 {
            assert!(h1[i][0] > i as f64 && h2[i][0] < (i + 1) as f64 + 1.0);
        }
    }

    #[test]
    fn smooth_path_passes_through_anchors() {
        let anchors = [
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [2.0, 0.0, 0.0],
            [3.0, 1.0, 0.0],
        ];
        let path = smooth_quadratic_path(&anchors).unwrap();
        assert_eq!(path.len() % 2, 1);
        // Every input anchor appears as an anchor of the output spline.
        for a in anchors {
            assert!(
                path.iter()
                    .step_by(2)
                    .any(|p| space_ops::get_norm(vec::sub(*p, a)) < 1e-9),
                "anchor {a:?} missing from smoothed path"
            );
        }
    }

    #[test]
    fn approx_handles_degenerate_inputs() {
        let single = approx_smooth_quadratic_handles(&[[1.0, 2.0, 0.0]]);
        assert_eq!(single, vec![[1.0, 2.0, 0.0]]);
        let pair = approx_smooth_quadratic_handles(&[[0.0, 0.0, 0.0], [2.0, 2.0, 0.0]]);
        assert_eq!(pair, vec![[1.0, 1.0, 0.0]]);
    }
}
