//! Bit-level helpers shared by the kernels. All manipulation goes through
//! `to_bits`/`from_bits` — safe, explicit, and identical on every target.

/// High 32 bits of an f64.
#[inline]
#[must_use]
pub(crate) fn hi(x: f64) -> u32 {
    (x.to_bits() >> 32) as u32
}

/// Low 32 bits of an f64.
#[inline]
#[must_use]
pub(crate) fn lo(x: f64) -> u32 {
    x.to_bits() as u32
}

/// Assemble an f64 from high and low 32-bit halves.
#[inline]
#[must_use]
pub(crate) fn from_parts(hi: u32, lo: u32) -> f64 {
    f64::from_bits((u64::from(hi) << 32) | u64::from(lo))
}

/// Replace the high 32 bits of `x`.
#[inline]
#[must_use]
pub(crate) fn with_hi(x: f64, hi: u32) -> f64 {
    from_parts(hi, lo(x))
}

/// Zero the low 32 bits of `x` (split into head/tail for Dekker-style
/// exact products).
#[inline]
#[must_use]
pub(crate) fn zero_lo(x: f64) -> f64 {
    f64::from_bits(x.to_bits() & 0xffff_ffff_0000_0000)
}

/// Evaluate a polynomial in Horner form with a FIXED operation order —
/// the order every future SIMD lane must reproduce. Coefficients are
/// highest-degree first.
#[inline]
#[must_use]
pub(crate) fn horner(x: f64, coefficients: &[f64]) -> f64 {
    let mut acc = 0.0;
    for &c in coefficients {
        acc = acc * x + c;
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_round_trip() {
        for x in [0.0, -0.0, 1.5, -3.25e100, f64::MIN_POSITIVE, f64::NAN] {
            let rebuilt = from_parts(hi(x), lo(x));
            assert_eq!(rebuilt.to_bits(), x.to_bits());
        }
        assert_eq!(with_hi(1.0, hi(2.0)).to_bits(), 2.0_f64.to_bits());
    }

    #[test]
    fn horner_matches_manual() {
        // 2x^2 + 3x + 4 at x = 5 → 69.
        assert_eq!(horner(5.0, &[2.0, 3.0, 4.0]), 69.0);
    }
}
