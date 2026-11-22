//! Object-to-world affine placement for Marionette entries (§10.8, fm-7if).
//!
//! The `RecordBuffer` owns object-space points. Positional operations compose
//! one of these maps instead of rewriting those points, so a translation,
//! rotation, scale, or stretch can invalidate screen-space derivations without
//! invalidating object-space curve coefficients and arc-length tables.

use fmn_core::types::Vec3;
use fmn_geom::Mat3;

/// A deterministic 3D affine object-to-world map.
///
/// The linear part is row-major and the translation is applied afterwards:
/// `world = linear * object + translation`. The type contains semantic state
/// only; [`crate::Entry`] owns the independent monotonic revision counter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    linear: Mat3,
    translation: Vec3,
}

/// A surface normal map compiled once for an affine placement.
///
/// Directions use the inverse transpose, without dividing by a possibly tiny
/// determinant. Normal-control records retain the length of their ordinary
/// transformed offset, while its direction stays perpendicular to the surface.
/// Similarity maps retain the existing point/vector arithmetic exactly.
#[derive(Debug, Clone, Copy)]
pub struct NormalTransform {
    placement: Placement,
    cofactors: Mat3,
    row_scales: Vec3,
    orientation: f64,
    similarity: bool,
}

impl NormalTransform {
    pub(crate) const fn maps_normals_as_vectors(self) -> bool {
        self.similarity
    }

    /// Transform a normal direction. Its magnitude is not part of the contract.
    ///
    /// A singular map has no inverse transpose and returns a zero direction.
    /// This remains absent after materialization and subsequent reflections.
    /// Row scaling bounds cofactor products for large or small placements;
    /// extremely ill-conditioned maps retain ordinary f64 rounding limits.
    #[must_use]
    pub fn apply(self, normal: Vec3) -> Vec3 {
        if self.similarity {
            return self.placement.apply_vector(normal);
        }
        let scale = normal.iter().fold(0.0_f64, |scale, x| scale.max(x.abs()));
        if scale == 0.0 {
            return [0.0; 3];
        }
        if !scale.is_finite() || normal.iter().any(|x| !x.is_finite()) {
            return [f64::NAN; 3];
        }
        if self.orientation == 0.0 {
            return [0.0; 3];
        }
        let normal = normal.map(|x| x / scale);
        let numerator = self.cofactors.map(|row| {
            self.orientation * (row[0] * normal[0] + row[1] * normal[1] + row[2] * normal[2])
        });
        normalized_quotients(numerator, self.row_scales)
    }

    /// Map a normal-control point paired with its original surface point.
    ///
    /// Translation belongs to the point, never to its normal. An underflowed
    /// forward offset uses the original nudge length to represent its finite
    /// inverse-normal direction. Zero and singular normals remain zero.
    #[must_use]
    pub fn apply_control_point(self, point: Vec3, normal_point: Vec3) -> Vec3 {
        if self.similarity {
            return self.placement.apply_point(normal_point);
        }
        let delta = std::array::from_fn(|axis| normal_point[axis] - point[axis]);
        let offset = self.apply_offset(delta);
        let placed = self.placement.apply_point(point);
        std::array::from_fn(|axis| placed[axis] + offset[axis])
    }

    /// Transform a normal-control offset without choosing its point anchor.
    ///
    /// Consumers which materialize points with their existing arithmetic can
    /// add this offset to that exact result, avoiding a second, differently
    /// rounded computation of the same point. Nudge magnitude is a storage
    /// detail, not an affine-composition invariant; consumers normalize it.
    #[must_use]
    pub fn apply_offset(self, delta: Vec3) -> Vec3 {
        if self.similarity {
            return self.placement.apply_vector(delta);
        }
        let normal = self.apply(delta);
        if normal == [0.0; 3] {
            return normal;
        }
        let forward = self.placement.apply_vector(delta);
        let length = norm(forward);
        let length = if length > 0.0 && length.is_finite() {
            length
        } else {
            norm(delta)
        };
        normal.map(|component| length * component)
    }
}

fn norm(vector: Vec3) -> f64 {
    let scale = vector.iter().fold(0.0_f64, |scale, x| scale.max(x.abs()));
    if scale == 0.0 || !scale.is_finite() {
        return scale;
    }
    let v = vector.map(|x| x / scale);
    scale * (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// Binary decomposition uses only IEEE operations on the certified path.
/// For a nonzero finite value, returns a signed significand in [1, 2) and
/// an exponent, including subnormals without a platform logarithm.
fn binary_parts(value: f64) -> (f64, i32) {
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    if exponent == 0 {
        let (significand, exponent) = binary_parts(value * 18_014_398_509_481_984.0);
        return (significand, exponent - 54);
    }
    (
        f64::from_bits((bits & 0x800f_ffff_ffff_ffff) | (1023_u64 << 52)),
        exponent - 1023,
    )
}

fn normalized_quotients(numerator: Vec3, denominator: Vec3) -> Vec3 {
    let mut parts = [(0.0_f64, 0_i32); 3];
    let mut largest = i32::MIN;
    for axis in 0..3 {
        let n = numerator[axis];
        if n == 0.0 {
            continue;
        }
        if !n.is_finite() || !denominator[axis].is_finite() {
            return [f64::NAN; 3];
        }
        let (n, ne) = binary_parts(n);
        let (d, de) = binary_parts(denominator[axis]);
        parts[axis] = (n / d, ne - de);
        largest = largest.max(ne - de);
    }
    if largest == i32::MIN {
        return [0.0; 3];
    }
    let scaled = parts.map(|(significand, exponent)| {
        if significand == 0.0 {
            return 0.0;
        }
        let exponent = exponent - largest;
        let power = if exponent >= -1022 {
            f64::from_bits(((exponent + 1023) as u64) << 52)
        } else if exponent >= -1074 {
            f64::from_bits(1_u64 << (exponent + 1074))
        } else {
            0.0
        };
        significand * power
    });
    let length = (scaled[0] * scaled[0] + scaled[1] * scaled[1] + scaled[2] * scaled[2]).sqrt();
    scaled.map(|x| x / length)
}

impl Placement {
    /// Identity placement.
    pub const IDENTITY: Self = Self {
        linear: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        translation: [0.0; 3],
    };

    /// Build an affine map from its row-major linear part and translation.
    #[must_use]
    pub const fn new(linear: Mat3, translation: Vec3) -> Self {
        Self {
            linear,
            translation,
        }
    }

    /// A pure translation.
    #[must_use]
    pub const fn from_translation(translation: Vec3) -> Self {
        Self::new(Self::IDENTITY.linear, translation)
    }

    /// The row-major linear part.
    #[must_use]
    pub const fn linear(self) -> Mat3 {
        self.linear
    }

    /// The translation part.
    #[must_use]
    pub const fn translation(self) -> Vec3 {
        self.translation
    }

    /// Apply the affine map to one point.
    #[must_use]
    pub fn apply_point(self, point: Vec3) -> Vec3 {
        let mut out = self.apply_vector(point);
        for (slot, shift) in out.iter_mut().zip(self.translation) {
            *slot += shift;
        }
        out
    }

    /// Apply only the linear part to a vector.
    #[must_use]
    pub fn apply_vector(self, vector: Vec3) -> Vec3 {
        [
            self.linear[0][0] * vector[0]
                + self.linear[0][1] * vector[1]
                + self.linear[0][2] * vector[2],
            self.linear[1][0] * vector[0]
                + self.linear[1][1] * vector[1]
                + self.linear[1][2] * vector[2],
            self.linear[2][0] * vector[0]
                + self.linear[2][1] * vector[1]
                + self.linear[2][2] * vector[2],
        ]
    }

    /// Compile the geometry's normal map once for all surface records.
    ///
    /// Unlike a tangent, a normal transforms by the inverse transpose. Using
    /// the forward matrix tilts surface lighting under stretches and shears.
    /// The determinant sign preserves outward-facing authored normals under
    /// reflections. A singular map records an absent normal, rather than
    /// choosing a side that changes when geometry is materialized.
    #[must_use]
    pub fn normal_transform(self) -> NormalTransform {
        let scale = self
            .linear
            .iter()
            .flatten()
            .fold(0.0_f64, |scale, x| scale.max(x.abs()));
        let bounded = self.linear.map(|row| row.map(|x| x / scale));
        let gram: Mat3 = std::array::from_fn(|row| {
            std::array::from_fn(|column| {
                bounded[0][row] * bounded[0][column]
                    + bounded[1][row] * bounded[1][column]
                    + bounded[2][row] * bounded[2][column]
            })
        });
        // Rotation matrices rounded to f64 need not have an exactly diagonal
        // Gram matrix. Admit only its arithmetic rounding envelope here.
        let tolerance = 32.0 * f64::EPSILON * gram[0][0];
        let similarity = scale == 0.0
            || ((gram[1][1] - gram[0][0]).abs() <= tolerance
                && (gram[2][2] - gram[0][0]).abs() <= tolerance
                && gram[0][1].abs() <= tolerance
                && gram[0][2].abs() <= tolerance
                && gram[1][2].abs() <= tolerance);
        let row_scales = self.linear.map(|row| {
            let scale = row.iter().fold(0.0_f64, |scale, x| scale.max(x.abs()));
            if scale == 0.0 { 1.0 } else { scale }
        });
        let m: Mat3 = std::array::from_fn(|row| self.linear[row].map(|x| x / row_scales[row]));
        let cofactors = [
            [
                m[1][1] * m[2][2] - m[1][2] * m[2][1],
                m[1][2] * m[2][0] - m[1][0] * m[2][2],
                m[1][0] * m[2][1] - m[1][1] * m[2][0],
            ],
            [
                m[0][2] * m[2][1] - m[0][1] * m[2][2],
                m[0][0] * m[2][2] - m[0][2] * m[2][0],
                m[0][1] * m[2][0] - m[0][0] * m[2][1],
            ],
            [
                m[0][1] * m[1][2] - m[0][2] * m[1][1],
                m[0][2] * m[1][0] - m[0][0] * m[1][2],
                m[0][0] * m[1][1] - m[0][1] * m[1][0],
            ],
        ];
        let determinant =
            m[0][0] * cofactors[0][0] + m[0][1] * cofactors[0][1] + m[0][2] * cofactors[0][2];
        NormalTransform {
            placement: self,
            cofactors,
            row_scales,
            orientation: if determinant == 0.0 {
                0.0
            } else if determinant < 0.0 {
                -1.0
            } else {
                1.0
            },
            similarity,
        }
    }

    /// Compose two placements as `self(other(point))`.
    #[must_use]
    pub fn compose(self, other: Self) -> Self {
        let mut linear = [[0.0; 3]; 3];
        for (row, out_row) in linear.iter_mut().enumerate() {
            for (column, slot) in out_row.iter_mut().enumerate() {
                *slot = self.linear[row][0] * other.linear[0][column]
                    + self.linear[row][1] * other.linear[1][column]
                    + self.linear[row][2] * other.linear[2][column];
            }
        }
        Self {
            linear,
            translation: self.apply_point(other.translation),
        }
    }

    /// Construct `linear * (point - pivot) + pivot`.
    #[must_use]
    pub fn about(linear: Mat3, pivot: Vec3) -> Self {
        let map = Self::new(linear, [0.0; 3]);
        let moved = map.apply_vector(pivot);
        Self::new(
            linear,
            [
                pivot[0] - moved[0],
                pivot[1] - moved[1],
                pivot[2] - moved[2],
            ],
        )
    }

    /// Whether this is bit-for-bit identity.
    #[must_use]
    pub fn is_identity(self) -> bool {
        self.same_bits(Self::IDENTITY)
    }

    /// Whether the linear part is bit-for-bit identity.
    #[must_use]
    pub fn is_translation(self) -> bool {
        self.linear
            .iter()
            .flatten()
            .zip(Self::IDENTITY.linear.iter().flatten())
            .all(|(a, b)| a.to_bits() == b.to_bits())
    }

    /// Coefficients in canonical row-major affine order.
    #[must_use]
    pub fn coefficients(self) -> [f64; 12] {
        [
            self.linear[0][0],
            self.linear[0][1],
            self.linear[0][2],
            self.translation[0],
            self.linear[1][0],
            self.linear[1][1],
            self.linear[1][2],
            self.translation[1],
            self.linear[2][0],
            self.linear[2][1],
            self.linear[2][2],
            self.translation[2],
        ]
    }

    /// Bitwise semantic equality, including signed zero and NaN payloads.
    #[must_use]
    pub fn same_bits(self, other: Self) -> bool {
        self.coefficients()
            .iter()
            .zip(other.coefficients())
            .all(|(a, b)| a.to_bits() == b.to_bits())
    }
}

impl Default for Placement {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_is_outer_after_inner() {
        let scale = Placement::new(
            [[2.0, 0.0, 0.0], [0.0, 3.0, 0.0], [0.0, 0.0, 4.0]],
            [0.0; 3],
        );
        let shift = Placement::from_translation([5.0, 7.0, 11.0]);
        let composed = shift.compose(scale);
        assert_eq!(composed.apply_point([1.0, 2.0, 3.0]), [7.0, 13.0, 23.0]);
    }

    #[test]
    fn pivoted_linear_map_keeps_the_pivot_fixed() {
        let map = Placement::about(
            [[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
            [2.0, 3.0, 0.0],
        );
        assert_eq!(map.apply_point([2.0, 3.0, 0.0]), [2.0, 3.0, 0.0]);
        assert_eq!(map.apply_point([3.0, 3.0, 0.0]), [2.0, 4.0, 0.0]);
    }

    fn assert_direction(actual: Vec3, expected: Vec3) {
        let actual_length = norm(actual);
        let expected_length = norm(expected);
        assert!(actual_length > 0.0 && actual_length.is_finite());
        for axis in 0..3 {
            assert!(
                (actual[axis] / actual_length - expected[axis] / expected_length).abs() < 2e-14,
                "normal {actual:?} does not point along {expected:?}"
            );
        }
    }

    #[test]
    fn affine_normals_remain_perpendicular_under_stretch_shear_and_reflection() {
        let cases = [
            (
                [[2.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.5]],
                [1.0, 0.0, 1.0],
                [0.5, 0.0, 2.0],
            ),
            (
                [[1.0, 2.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                [1.0, 0.0, 0.0],
                [1.0, -2.0, 0.0],
            ),
            (
                [[-2.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.5]],
                [1.0, 0.0, 1.0],
                [-0.5, 0.0, 2.0],
            ),
        ];
        for (linear, normal, expected) in cases {
            let placement = Placement::new(linear, [17.0, -3.0, 0.5]);
            let actual = placement.normal_transform().apply(normal);
            assert_direction(actual, expected);
            for tangent in [[-normal[1], normal[0], 0.0], [-normal[2], 0.0, normal[0]]] {
                let tangent = placement.apply_vector(tangent);
                let dot = actual[0] * tangent[0] + actual[1] * tangent[1] + actual[2] * tangent[2];
                assert!(dot.abs() < 2e-14, "normal is not perpendicular: {dot}");
            }
            let point = [0.25, -0.5, 0.75];
            let control = std::array::from_fn(|axis| point[axis] + 0.25 * normal[axis]);
            let transformed = placement
                .normal_transform()
                .apply_control_point(point, control);
            let anchor = placement.apply_point(point);
            assert_direction(
                std::array::from_fn(|axis| transformed[axis] - anchor[axis]),
                expected,
            );
        }
    }

    #[test]
    fn similarity_maps_keep_vector_and_control_point_bits() {
        let rotations = [
            Placement::IDENTITY.linear(),
            fmn_geom::space_ops::rotation_matrix(0.71, [1.0, 2.0, 3.0]),
            [[-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        ];
        for rotation in rotations {
            for scale in [-2.5, 0.0, 0.25, 1.0, 4.0] {
                let placement = Placement::new(
                    rotation.map(|row| row.map(|component| scale * component)),
                    [3.25, -4.5, 0.25],
                );
                let normals = placement.normal_transform();
                assert!(normals.maps_normals_as_vectors());
                let normal = [0.125, -0.25, 0.375];
                assert_eq!(
                    normals.apply(normal).map(f64::to_bits),
                    placement.apply_vector(normal).map(f64::to_bits)
                );
                let point = [0.5, -0.75, 1.0];
                let control = std::array::from_fn(|axis| point[axis] + normal[axis]);
                assert_eq!(
                    normals
                        .apply_control_point(point, control)
                        .map(f64::to_bits),
                    placement.apply_point(control).map(f64::to_bits)
                );
            }
        }
    }

    #[test]
    fn successive_invertible_maps_preserve_the_composed_normal_direction() {
        let stretch = Placement::new(
            [[2.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.5]],
            [0.0; 3],
        );
        let shear = Placement::new(
            [[1.0, 0.0, 2.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            [0.0; 3],
        );
        let source = [0.25, 0.0, 0.25];
        let materialized = stretch.normal_transform().apply_offset(source);
        let successive = shear.normal_transform().apply_offset(materialized);
        let composed = shear.compose(stretch).normal_transform().apply_offset(source);
        assert_direction(successive, [1.0, 0.0, 2.0]);
        assert_direction(composed, [1.0, 0.0, 2.0]);
    }

    #[test]
    fn normal_maps_bound_extreme_finite_scales_and_define_collapses() {
        let normals = Placement::new(
            [[1e300, 0.0, 0.0], [0.0, 1e-300, 0.0], [0.0, 0.0, 1.0]],
            [0.0; 3],
        )
        .normal_transform();
        assert_direction(normals.apply([1.0, 0.0, 1.0]), [1e-300, 0.0, 1.0]);
        assert_direction(normals.apply([1.0, 1.0, 1.0]), [0.0, 1.0, 0.0]);
        let tiny = f64::from_bits(1);
        let normals = Placement::new(
            [
                [tiny, 0.0, 0.0],
                [0.0, 2.0 * tiny, 0.0],
                [0.0, 0.0, 4.0 * tiny],
            ],
            [0.0; 3],
        )
        .normal_transform();
        assert_direction(normals.apply([1.0; 3]), [1.0, 0.5, 0.25]);
        let rank_two = Placement::new(
            [[2.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.0]],
            [0.0; 3],
        )
        .normal_transform();
        assert_eq!(rank_two.apply([0.0, 0.0, 1.0]), [0.0; 3]);
        assert_eq!(rank_two.apply_offset([f64::MAX; 3]), [0.0; 3]);
        assert_eq!(
            rank_two.apply_control_point([1.0, 2.0, 3.0], [1.0, 2.0, 3.25]),
            [2.0, 2.0, 0.0]
        );
        let rank_one = Placement::new(
            [[2.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]],
            [0.0; 3],
        )
        .normal_transform();
        assert_eq!(rank_one.apply([1.0; 3]), [0.0; 3]);
        assert_eq!(rank_two.apply([0.0; 3]), [0.0; 3]);
    }
}
