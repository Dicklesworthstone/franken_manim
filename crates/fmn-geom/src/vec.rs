//! Minimal `Vec3` and 3×3-matrix helpers consumed by the path model.
//!
//! These are faithful ports of the handful of `manimlib/utils/space_ops.py`
//! functions the QuadPath layer needs (`3b1b/manim` @ `6199a00d`). The full
//! §7.5 space_ops surface — quaternions, Euler conventions, the scipy-
//! `Rotation` semantics fixed at singularities — is bead fm-ngx and will
//! grow from this seed; nothing here is public API yet.

use crate::scalar;
use fmn_core::constants::{DOWN, OUT, RIGHT, UP};
use fmn_core::types::Vec3;

/// Row-major 3×3 matrix: `m[row][col]`.
pub(crate) type Mat3 = [[f64; 3]; 3];

pub(crate) const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

#[inline]
pub(crate) fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
pub(crate) fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
pub(crate) fn scale(a: Vec3, s: f64) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

#[inline]
pub(crate) fn dot(a: Vec3, b: Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
pub(crate) fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// The z component of the cross product of the xy projections
/// (`space_ops.cross2d`).
#[inline]
pub(crate) fn cross2d(a: Vec3, b: Vec3) -> f64 {
    a[0] * b[1] - b[0] * a[1]
}

/// `space_ops.get_norm`: the summation order (x, then y, then z) is fixed.
#[inline]
pub(crate) fn norm(v: Vec3) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

#[inline]
pub(crate) fn midpoint(a: Vec3, b: Vec3) -> Vec3 {
    [
        (a[0] + b[0]) / 2.0,
        (a[1] + b[1]) / 2.0,
        (a[2] + b[2]) / 2.0,
    ]
}

#[inline]
pub(crate) fn lerp(a: Vec3, b: Vec3, t: f64) -> Vec3 {
    [
        (1.0 - t) * a[0] + t * b[0],
        (1.0 - t) * a[1] + t * b[1],
        (1.0 - t) * a[2] + t * b[2],
    ]
}

#[inline]
fn clip(x: f64, lo: f64, hi: f64) -> f64 {
    x.max(lo).min(hi)
}

/// `space_ops.angle_between_vectors`: always in `[0, π]`; zero for a zero
/// vector.
pub(crate) fn angle_between_vectors(v1: Vec3, v2: Vec3) -> f64 {
    let n1 = norm(v1);
    let n2 = norm(v2);
    if n1 == 0.0 || n2 == 0.0 {
        return 0.0;
    }
    scalar::acos(clip(dot(v1, v2) / (n1 * n2), -1.0, 1.0))
}

/// `space_ops.rotation_matrix`: rotation about `axis` by `angle`
/// (Rodrigues form, equivalent to scipy `Rotation.from_rotvec`).
pub(crate) fn rotation_matrix(angle: f64, axis: Vec3) -> Mat3 {
    let n = norm(axis);
    if n == 0.0 {
        return IDENTITY;
    }
    let [x, y, z] = scale(axis, 1.0 / n);
    let c = scalar::cos(angle);
    let s = scalar::sin(angle);
    let t = 1.0 - c;
    [
        [t * x * x + c, t * x * y - s * z, t * x * z + s * y],
        [t * x * y + s * z, t * y * y + c, t * y * z - s * x],
        [t * x * z - s * y, t * y * z + s * x, t * z * z + c],
    ]
}

/// `space_ops.rotation_about_z`.
pub(crate) fn rotation_about_z(angle: f64) -> Mat3 {
    let c = scalar::cos(angle);
    let s = scalar::sin(angle);
    [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
}

/// `space_ops.rotation_between_vectors`, including its degenerate-axis
/// fallback chain (RIGHT, then UP).
pub(crate) fn rotation_between_vectors(v1: Vec3, v2: Vec3) -> Mat3 {
    let atol = 1e-8;
    if norm(sub(v1, v2)) < atol {
        return IDENTITY;
    }
    let mut axis = cross(v1, v2);
    if norm(axis) < atol {
        axis = cross(v1, RIGHT);
    }
    if norm(axis) < atol {
        axis = cross(v1, UP);
    }
    rotation_matrix(angle_between_vectors(v1, v2), axis)
}

/// `space_ops.z_to_vector`.
pub(crate) fn z_to_vector(vector: Vec3) -> Mat3 {
    rotation_between_vectors(OUT, vector)
}

/// `space_ops.get_unit_normal`, with the Reference's DOWN fallback when both
/// candidate normals degenerate.
pub(crate) fn unit_normal_from(v1: Vec3, v2: Vec3, tol: f64) -> Vec3 {
    let n1 = norm(v1);
    let n2 = norm(v2);
    let u1 = if n1 > 0.0 {
        scale(v1, 1.0 / n1)
    } else {
        [0.0; 3]
    };
    let u2 = if n2 > 0.0 {
        scale(v2, 1.0 / n2)
    } else {
        [0.0; 3]
    };
    let cp = cross(u1, u2);
    let cp_norm = norm(cp);
    if cp_norm < tol {
        let new_cp = cross(cross(u1, OUT), u1);
        let new_cp_norm = norm(new_cp);
        if new_cp_norm < tol {
            return DOWN;
        }
        return scale(new_cp, 1.0 / new_cp_norm);
    }
    scale(cp, 1.0 / cp_norm)
}

/// `space_ops.find_intersection` (single-point form): the intersection of the
/// line through `p0` along `v0` with the line through `p1` along `v1`; for 3D
/// inputs, the point on the first ray closest to the second. A near-parallel
/// configuration (denominator under `threshold`) returns `p0`, exactly as the
/// Reference's `denom → ∞` masking does.
pub(crate) fn find_intersection(p0: Vec3, v0: Vec3, p1: Vec3, v1: Vec3, threshold: f64) -> Vec3 {
    let is_3d = p0[2] != 0.0 || v0[2] != 0.0 || p1[2] != 0.0 || v1[2] != 0.0;
    let (numer, denom) = if !is_3d {
        (cross2d(v1, sub(p1, p0)), cross2d(v1, v0))
    } else {
        let cp1 = cross(v1, sub(p1, p0));
        let cp2 = cross(v1, v0);
        (dot(cp1, cp1), dot(cp1, cp2))
    };
    let ratio = if denom.abs() < threshold {
        0.0
    } else {
        numer / denom
    };
    add(p0, scale(v0, ratio))
}

/// `p @ m` in numpy terms: treat `p` as a row vector.
#[inline]
pub(crate) fn mul_point_mat(p: Vec3, m: &Mat3) -> Vec3 {
    [
        p[0] * m[0][0] + p[1] * m[1][0] + p[2] * m[2][0],
        p[0] * m[0][1] + p[1] * m[1][1] + p[2] * m[2][1],
        p[0] * m[0][2] + p[1] * m[1][2] + p[2] * m[2][2],
    ]
}

#[inline]
pub(crate) fn transpose(m: &Mat3) -> Mat3 {
    [
        [m[0][0], m[1][0], m[2][0]],
        [m[0][1], m[1][1], m[2][1]],
        [m[0][2], m[1][2], m[2][2]],
    ]
}

/// `np.isclose` with numpy's default tolerances (`rtol=1e-5`, `atol=1e-8`),
/// applied per component — the closure test several Reference formulas
/// depend on.
pub(crate) fn np_isclose_all(a: Vec3, b: Vec3) -> bool {
    a.iter()
        .zip(b.iter())
        .all(|(x, y)| (x - y).abs() <= 1e-8 + 1e-5 * y.abs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_core::constants::{OUT, PI};

    fn assert_vec_close(a: Vec3, b: Vec3, tol: f64) {
        for i in 0..3 {
            assert!((a[i] - b[i]).abs() < tol, "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn rotation_matrix_matches_z_rotation() {
        let m = rotation_matrix(PI / 2.0, OUT);
        let z = rotation_about_z(PI / 2.0);
        for i in 0..3 {
            assert_vec_close(m[i], z[i], 1e-15);
        }
    }

    #[test]
    fn rotation_between_vectors_handles_antiparallel() {
        let m = rotation_between_vectors(OUT, [0.0, 0.0, -1.0]);
        let rotated = mul_point_mat(OUT, &transpose(&m));
        assert_vec_close(rotated, [0.0, 0.0, -1.0], 1e-12);
    }

    #[test]
    fn find_intersection_2d_lines() {
        // x-axis meets the vertical line x = 1.
        let p = find_intersection(
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, -1.0, 0.0],
            [0.0, 1.0, 0.0],
            1e-5,
        );
        assert_vec_close(p, [1.0, 0.0, 0.0], 1e-12);
    }

    #[test]
    fn find_intersection_parallel_returns_p0() {
        let p = find_intersection(
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, 0.0, 0.0],
            1e-5,
        );
        assert_vec_close(p, [0.0, 0.0, 0.0], 1e-12);
    }

    #[test]
    fn unit_normal_degenerate_falls_back() {
        assert_eq!(unit_normal_from([0.0; 3], [0.0; 3], 1e-6), DOWN);
        // Aligned with OUT: the in-plane fallback also degenerates → DOWN.
        assert_eq!(unit_normal_from(OUT, OUT, 1e-6), DOWN);
    }
}
