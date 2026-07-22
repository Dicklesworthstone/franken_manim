//! Exponential and logarithm kernels — classical FDLIBM ports with the
//! published coefficient sets and a fixed operation order, bit-identical
//! on every certified target.
//!
//! * [`exp`] — FDLIBM `e_exp`: Cody–Waite ln2 hi/lo reduction (invln2
//!   multiply, round to `k`), P1..P5 minimax polynomial, `2^k`
//!   reconstruction through the exponent bits ([`scalbn`]).
//! * [`expm1`] — FDLIBM `s_expm1`: Q1..Q5 polynomial with the exact
//!   per-`k` reconstruction branches.
//! * [`ln`] — FDLIBM `e_log`: mantissa reduction with the `sqrt(2)`
//!   midpoint adjustment, `s = f/(2+f)`, Lg1..Lg7 odd/even polynomial
//!   split, `hfsq` correction, `ln2_hi`/`ln2_lo` reconstruction.
//! * [`log2`] — the musl/FreeBSD `log2` reconstruction of the same
//!   reduction in base 2 via the `ivln2` hi/lo split; `log2(2^k) == k`
//!   exactly for every representable power of two.
//!
//! Design accuracy bounds (verified downstream against the committed
//! mpmath vectors): `exp`, `expm1`, `ln`, `log2` < 1 ulp.
//!
//! All magic constants are given as exact bit patterns; the decimal in
//! each comment is the published FDLIBM value they encode.

use crate::bits::{from_parts, hi, horner, lo, with_hi, zero_lo};

const HUGE: f64 = 1.0e300;
const TINY: f64 = 1.0e-300;

/// 2^-1000 (FDLIBM `twom1000`): squared, a clean underflow-to-zero.
const TWOM1000: f64 = f64::from_bits(0x0170_0000_0000_0000);
/// Overflow threshold 7.09782712893383973096e+02 (largest finite-exp x).
const O_THRESHOLD: f64 = f64::from_bits(0x4086_2E42_FEFA_39EF);
/// Underflow threshold -7.45133219101941108420e+02.
const U_THRESHOLD: f64 = f64::from_bits(0xC087_4910_D52D_3051);
/// ln2 high part, 6.93147180369123816490e-01 (20 trailing zero bits).
const LN2_HI: f64 = f64::from_bits(0x3FE6_2E42_FEE0_0000);
/// ln2 low part, 1.90821492927058770002e-10.
const LN2_LO: f64 = f64::from_bits(0x3DEA_39EF_3579_3C76);
/// 1/ln2, 1.44269504088896338700e+00.
const INV_LN2: f64 = f64::from_bits(0x3FF7_1547_652B_82FE);
/// 2^54 (FDLIBM `two54`), the denormal pre-scale for the logarithms.
const TWO54: f64 = f64::from_bits(0x4350_0000_0000_0000);

// e_exp minimax polynomial P1..P5.
const P1: f64 = f64::from_bits(0x3FC5_5555_5555_553E); //  1.66666666666666019037e-01
const P2: f64 = f64::from_bits(0xBF66_C16C_16BE_BD93); // -2.77777777770155933842e-03
const P3: f64 = f64::from_bits(0x3F11_566A_AF25_DE2C); //  6.61375632143793436117e-05
const P4: f64 = f64::from_bits(0xBEBB_BD41_C5D2_6BF1); // -1.65339022054652515390e-06
const P5: f64 = f64::from_bits(0x3E66_3769_72BE_A4D0); //  4.13813679705723846039e-08

// s_expm1 minimax polynomial Q1..Q5.
const Q1: f64 = f64::from_bits(0xBFA1_1111_1111_10F4); // -3.33333333333331316428e-02
const Q2: f64 = f64::from_bits(0x3F5A_01A0_19FE_5585); //  1.58730158725481460165e-03
const Q3: f64 = f64::from_bits(0xBF14_CE19_9EAA_DBB7); // -7.93650757867487942473e-05
const Q4: f64 = f64::from_bits(0x3ED0_CFCA_86E6_5239); //  4.00821782732936239552e-06
const Q5: f64 = f64::from_bits(0xBE8A_FDB7_6E09_C32D); // -2.01099218183624371326e-07

// e_log minimax polynomial Lg1..Lg7.
const LG1: f64 = f64::from_bits(0x3FE5_5555_5555_5593); // 6.666666666666735130e-01
const LG2: f64 = f64::from_bits(0x3FD9_9999_9997_FA04); // 3.999999999940941908e-01
const LG3: f64 = f64::from_bits(0x3FD2_4924_9422_9359); // 2.857142874366239149e-01
const LG4: f64 = f64::from_bits(0x3FCC_71C5_1D8E_78AF); // 2.222219843214978396e-01
const LG5: f64 = f64::from_bits(0x3FC7_4664_96CB_03DE); // 1.818357216161805012e-01
const LG6: f64 = f64::from_bits(0x3FC3_9A09_D078_C69F); // 1.531383769920937332e-01
const LG7: f64 = f64::from_bits(0x3FC2_F112_DF3E_5244); // 1.479819860511658591e-01

/// Nearest double to 1/3 (FDLIBM writes the literal 0.33333333333333333).
const THIRD: f64 = f64::from_bits(0x3FD5_5555_5555_5555);

// log2 reconstruction: 1/ln2 split hi (32 zero low bits) + lo tail.
const IVLN2_HI: f64 = f64::from_bits(0x3FF7_1547_6520_0000); // 1.44269504072144627571e+00
const IVLN2_LO: f64 = f64::from_bits(0x3DE7_05FC_2EEF_A200); // 1.67517131648865118353e-10

/// 2^1023, the top step of the overflow-side `scalbn` ladder.
const TWO1023: f64 = f64::from_bits(0x7FE0_0000_0000_0000);
/// 2^-969 = 2^-1022 * 2^53: the denormal-side step, chosen so the final
/// multiply is the single rounding (no double rounding in the subnormal
/// range).
const TWOM969: f64 = f64::from_bits(0x0360_0000_0000_0000);

/// Multiply `x` by 2^n with exactly one rounding, using only exponent
/// bits — the FDLIBM-style two-step ladder, no platform `ldexp`.
///
/// Overflow saturates to ±inf, underflow to ±0, denormal outputs are
/// produced by one final (rounding) multiply.
pub(crate) fn scalbn(x: f64, n: i32) -> f64 {
    let mut y = x;
    let mut n = n;
    if n > 1023 {
        y *= TWO1023;
        n -= 1023;
        if n > 1023 {
            y *= TWO1023;
            n -= 1023;
            if n > 1023 {
                n = 1023;
            }
        }
    } else if n < -1022 {
        y *= TWOM969;
        n += 969;
        if n < -1022 {
            y *= TWOM969;
            n += 969;
            if n < -1022 {
                n = -1022;
            }
        }
    }
    // 2^n is exactly representable for n in [-1022, 1023].
    y * f64::from_bits((u64::from((0x3ff + n) as u32)) << 52)
}

/// e^x — FDLIBM `e_exp`.
///
/// Special values: `exp(NaN) = NaN`, `exp(+inf) = +inf`, `exp(-inf) = 0`,
/// `exp(±0) = 1` exactly; overflows to `+inf` above ~709.782, underflows
/// to `+0` below ~-745.133. Design accuracy < 1 ulp.
#[must_use]
pub fn exp(x: f64) -> f64 {
    let hx0 = hi(x);
    let xsb = ((hx0 >> 31) & 1) as usize; // sign bit of x
    let hx = hx0 & 0x7fff_ffff; // high word of |x|

    // Filter out non-finite and out-of-range arguments.
    if hx >= 0x4086_2e42 {
        // |x| >= 709.78...
        if hx >= 0x7ff0_0000 {
            if ((hx & 0x000f_ffff) | lo(x)) != 0 {
                return x + x; // NaN
            }
            return if xsb == 0 { x } else { 0.0 }; // exp(±inf) = {inf, 0}
        }
        if x > O_THRESHOLD {
            return HUGE * HUGE; // overflow
        }
        if x < U_THRESHOLD {
            return TWOM1000 * TWOM1000; // underflow
        }
    }

    // Argument reduction: x = k*ln2 + r, |r| <= 0.5*ln2.
    let mut x = x;
    let mut hi_r = 0.0;
    let mut lo_r = 0.0;
    let mut k: i32 = 0;
    if hx > 0x3fd6_2e42 {
        // |x| > 0.5 ln2
        if hx < 0x3ff0_a2b2 {
            // and |x| < 1.5 ln2
            if xsb == 0 {
                hi_r = x - LN2_HI;
                lo_r = LN2_LO;
                k = 1;
            } else {
                hi_r = x + LN2_HI;
                lo_r = -LN2_LO;
                k = -1;
            }
        } else {
            let half = if xsb == 0 { 0.5 } else { -0.5 };
            k = (INV_LN2 * x + half) as i32;
            let t = f64::from(k);
            hi_r = x - t * LN2_HI; // t*ln2_hi is exact here
            lo_r = t * LN2_LO;
        }
        x = hi_r - lo_r;
    } else if hx < 0x3e30_0000 {
        // |x| < 2^-28
        return 1.0 + x;
    }

    // x now in the primary range: P1..P5 polynomial in t = x^2.
    let t = x * x;
    let c = x - t * horner(t, &[P5, P4, P3, P2, P1]);
    if k == 0 {
        return 1.0 - ((x * c / (c - 2.0)) - x);
    }
    let y = 1.0 - ((lo_r - (x * c / (2.0 - c))) - hi_r);
    // 2^k reconstruction via the exponent bits.
    scalbn(y, k)
}

/// e^x - 1 — FDLIBM `s_expm1`, accurate even for tiny `x`.
///
/// Special values: `expm1(NaN) = NaN`, `expm1(+inf) = +inf`,
/// `expm1(-inf) = -1`, `expm1(±0) = ±0` exactly; saturates at `-1` below
/// -56·ln2. Design accuracy < 1 ulp.
#[must_use]
pub fn expm1(x: f64) -> f64 {
    let hx0 = hi(x);
    let xsb = hx0 & 0x8000_0000; // sign bit of x
    let hx = hx0 & 0x7fff_ffff; // high word of |x|

    // Filter out huge and non-finite arguments.
    if hx >= 0x4043_687a {
        // |x| >= 56 ln2
        if hx >= 0x4086_2e42 {
            // |x| >= 709.78...
            if hx >= 0x7ff0_0000 {
                if ((hx & 0x000f_ffff) | lo(x)) != 0 {
                    return x + x; // NaN
                }
                return if xsb == 0 { x } else { -1.0 }; // expm1(±inf)
            }
            if x > O_THRESHOLD {
                return HUGE * HUGE; // overflow
            }
        }
        if xsb != 0 {
            return TINY - 1.0; // x < -56 ln2: -1 (with inexact in C)
        }
    }

    // Argument reduction, keeping the reduction error c.
    let mut x = x;
    let mut k: i32 = 0;
    let mut c = 0.0;
    if hx > 0x3fd6_2e42 {
        // |x| > 0.5 ln2
        let hi_r;
        let lo_r;
        if hx < 0x3ff0_a2b2 {
            // and |x| < 1.5 ln2
            if xsb == 0 {
                hi_r = x - LN2_HI;
                lo_r = LN2_LO;
                k = 1;
            } else {
                hi_r = x + LN2_HI;
                lo_r = -LN2_LO;
                k = -1;
            }
        } else {
            let half = if xsb == 0 { 0.5 } else { -0.5 };
            k = (INV_LN2 * x + half) as i32;
            let t = f64::from(k);
            hi_r = x - t * LN2_HI; // t*ln2_hi is exact here
            lo_r = t * LN2_LO;
        }
        x = hi_r - lo_r;
        c = (hi_r - x) - lo_r;
    } else if hx < 0x3c90_0000 {
        // |x| < 2^-54: expm1(x) = x
        return x;
    }
    // else k = 0, c = 0

    // x now in the primary range: Q1..Q5 rational approximation.
    let hfx = 0.5 * x;
    let hxs = x * hfx;
    let r1 = 1.0 + hxs * horner(hxs, &[Q5, Q4, Q3, Q2, Q1]);
    let t = 3.0 - r1 * hfx;
    let mut e = hxs * ((r1 - t) / (6.0 - x * t));
    if k == 0 {
        return x - (x * e - hxs); // c is 0
    }

    // Reconstruction, branch by branch exactly as in s_expm1.
    e = x * (e - c) - c;
    e -= hxs;
    if k == -1 {
        return 0.5 * (x - e) - 0.5;
    }
    if k == 1 {
        if x < -0.25 {
            return -2.0 * (e - (x + 0.5));
        }
        return 1.0 + 2.0 * (x - e);
    }
    if k <= -2 || k > 56 {
        // Suffices to return exp(x) - 1.
        let y = 1.0 - (e - x);
        let y = with_hi(y, hi(y).wrapping_add((k as u32) << 20)); // 2^k
        return y - 1.0;
    }
    if k < 20 {
        let t = with_hi(1.0, 0x3ff0_0000 - (0x0020_0000 >> k)); // t = 1 - 2^-k
        let y = t - (e - x);
        with_hi(y, hi(y).wrapping_add((k as u32) << 20)) // 2^k
    } else {
        let t = from_parts(((0x3ff - k) as u32) << 20, 0); // t = 2^-k
        let y = (x - (e + t)) + 1.0;
        with_hi(y, hi(y).wrapping_add((k as u32) << 20)) // 2^k
    }
}

/// Natural logarithm — FDLIBM `e_log`.
///
/// Special values: `ln(1) = +0` exactly, `ln(±0) = -inf`,
/// `ln(x < 0) = NaN`, `ln(+inf) = +inf`, `ln(NaN) = NaN`. Design
/// accuracy < 1 ulp.
#[must_use]
pub fn ln(x: f64) -> f64 {
    let mut x = x;
    let mut hx = hi(x) as i32;
    let lx = lo(x);

    // Normalize: denormals are scaled by 2^54; sign/zero/inf filtered.
    let mut k: i32 = 0;
    if hx < 0x0010_0000 {
        // x < 2^-1022
        if (((hx & 0x7fff_ffff) as u32) | lx) == 0 {
            return f64::NEG_INFINITY; // ln(±0) = -inf
        }
        if hx < 0 {
            return f64::NAN; // ln(negative) = NaN
        }
        k -= 54;
        x *= TWO54; // scale up the subnormal
        hx = hi(x) as i32;
    }
    if hx >= 0x7ff0_0000 {
        return x + x; // +inf or NaN
    }

    // Reduce to x in [sqrt(2)/2, sqrt(2)): high-word k extraction with
    // the sqrt(2) midpoint adjustment.
    k += (hx >> 20) - 1023;
    hx &= 0x000f_ffff;
    let i = (hx + 0x9_5f64) & 0x0010_0000;
    x = with_hi(x, (hx | (i ^ 0x3ff0_0000)) as u32); // normalize x or x/2
    k += i >> 20;
    let f = x - 1.0;

    if (0x000f_ffff & (2 + hx)) < 3 {
        // |f| < 2^-20: short polynomial.
        if f == 0.0 {
            if k == 0 {
                return 0.0; // ln(1) = +0 exactly
            }
            let dk = f64::from(k);
            return dk * LN2_HI + dk * LN2_LO;
        }
        let r = f * f * (0.5 - THIRD * f);
        if k == 0 {
            return f - r;
        }
        let dk = f64::from(k);
        return dk * LN2_HI - ((r - dk * LN2_LO) - f);
    }

    // Main path: s = f/(2+f), z = s^2, w = z^2, odd/even split of
    // Lg1..Lg7.
    let s = f / (2.0 + f);
    let dk = f64::from(k);
    let z = s * s;
    let i = hx - 0x6_147a;
    let w = z * z;
    let j = 0x6_b851 - hx;
    let t1 = w * horner(w, &[LG6, LG4, LG2]);
    let t2 = z * horner(w, &[LG7, LG5, LG3, LG1]);
    let i = i | j;
    let r = t2 + t1;
    if i > 0 {
        let hfsq = 0.5 * f * f;
        if k == 0 {
            f - (hfsq - s * (hfsq + r))
        } else {
            dk * LN2_HI - ((hfsq - (s * (hfsq + r) + dk * LN2_LO)) - f)
        }
    } else if k == 0 {
        f - s * (f - r)
    } else {
        dk * LN2_HI - ((s * (f - r) - dk * LN2_LO) - f)
    }
}

/// Base-2 logarithm — the musl/FreeBSD `log2`: `e_log`'s reduction,
/// reconstructed in base 2 with the `ivln2` hi/lo split and hi/lo
/// splitting of `f - hfsq`.
///
/// `log2(2^k) == k` exactly for every representable power of two
/// (k in -1074..=1023). Special values: `log2(1) = +0` exactly,
/// `log2(±0) = -inf`, `log2(x < 0) = NaN`, `log2(+inf) = +inf`,
/// `log2(NaN) = NaN`. Design accuracy < 1 ulp.
#[must_use]
pub fn log2(x: f64) -> f64 {
    let mut x = x;
    let mut hx = hi(x);
    let mut k: i32 = 0;

    if hx < 0x0010_0000 || (hx >> 31) != 0 {
        if x.to_bits() << 1 == 0 {
            return f64::NEG_INFINITY; // log2(±0) = -inf
        }
        if (hx >> 31) != 0 {
            return f64::NAN; // log2(negative) = NaN
        }
        // Subnormal: scale up by 2^54.
        k -= 54;
        x *= TWO54;
        hx = hi(x);
    } else if hx >= 0x7ff0_0000 {
        return x; // +inf or NaN
    } else if hx == 0x3ff0_0000 && lo(x) == 0 {
        return 0.0; // log2(1) = +0 exactly
    }

    // Reduce x into [sqrt(2)/2, sqrt(2)).
    hx = hx.wrapping_add(0x3ff0_0000 - 0x3fe6_a09e);
    k += ((hx >> 20) as i32) - 0x3ff;
    hx = (hx & 0x000f_ffff) + 0x3fe6_a09e;
    x = with_hi(x, hx);

    let f = x - 1.0;
    let hfsq = 0.5 * f * f;
    let s = f / (2.0 + f);
    let z = s * s;
    let w = z * z;
    let t1 = w * horner(w, &[LG6, LG4, LG2]);
    let t2 = z * horner(w, &[LG7, LG5, LG3, LG1]);
    let r = t2 + t1;

    // hi + lo = f - hfsq + s*(hfsq+R) ~ log(1+f), in extra precision:
    // f - hfsq is split hi/lo to survive the cancellation near
    // sqrt(2)^±1.
    let hi_v = zero_lo(f - hfsq);
    let lo_v = f - hi_v - hfsq + s * (hfsq + r);

    // Base-2 reconstruction with the ivln2 hi/lo split.
    let val_hi = hi_v * IVLN2_HI;
    let mut val_lo = (lo_v + hi_v) * IVLN2_LO + lo_v * IVLN2_HI;

    // spadd(val_hi, val_lo, y): exact k + val_hi sum.
    let y = f64::from(k);
    let w2 = y + val_hi;
    val_lo += (y - w2) + val_hi;
    let val_hi = w2;

    val_lo + val_hi
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Monotone integer image of a double: adjacent finite doubles map to
    /// adjacent integers (±0 collapse to 0).
    fn ord(x: f64) -> i128 {
        let b = x.to_bits() as i64;
        i128::from(if b < 0 { i64::MIN.wrapping_sub(b) } else { b })
    }

    /// Bit distance in ulps; 0 when both are NaN, huge when only one is.
    fn ulp_diff(a: f64, b: f64) -> u128 {
        if a.is_nan() || b.is_nan() {
            return if a.is_nan() && b.is_nan() {
                0
            } else {
                u128::MAX
            };
        }
        ord(a).abs_diff(ord(b))
    }

    #[test]
    fn exp_sweep_vs_std() {
        let mut max = 0u128;
        for i in 0..10_000i32 {
            let x = -745.0 + f64::from(i) * (710.0 - -745.0) / 9_999.0;
            let d = ulp_diff(exp(x), x.exp());
            if d > max {
                max = d;
            }
        }
        println!("exp max ulp vs std: {max}");
        assert!(max <= 2, "exp sweep max ulp {max}");
    }

    #[test]
    fn exp_special_values() {
        assert_eq!(exp(0.0).to_bits(), 1.0f64.to_bits());
        assert_eq!(exp(-0.0).to_bits(), 1.0f64.to_bits());
        assert!(exp(f64::NAN).is_nan());
        assert_eq!(exp(f64::INFINITY), f64::INFINITY);
        assert_eq!(exp(f64::NEG_INFINITY).to_bits(), 0.0f64.to_bits());
        assert_eq!(exp(710.0), f64::INFINITY);
        assert_eq!(exp(-746.0).to_bits(), 0.0f64.to_bits());
        // Largest finite input stays finite.
        assert!(exp(O_THRESHOLD).is_finite());
        // Deep denormal outputs still round sanely.
        assert!(exp(-745.0) > 0.0);
    }

    #[test]
    fn expm1_sweep_vs_std() {
        let mut max = 0u128;
        for i in 0..10_000i32 {
            let x = -745.0 + f64::from(i) * (709.0 - -745.0) / 9_999.0;
            let d = ulp_diff(expm1(x), x.exp_m1());
            if d > max {
                max = d;
            }
        }
        println!("expm1 max ulp vs std: {max}");
        assert!(max <= 2, "expm1 sweep max ulp {max}");
    }

    #[test]
    fn expm1_special_values() {
        assert_eq!(expm1(0.0).to_bits(), 0.0f64.to_bits());
        assert_eq!(expm1(-0.0).to_bits(), (-0.0f64).to_bits());
        assert!(expm1(f64::NAN).is_nan());
        assert_eq!(expm1(f64::INFINITY), f64::INFINITY);
        assert_eq!(expm1(f64::NEG_INFINITY), -1.0);
        assert_eq!(expm1(-40.0), -1.0);
        assert_eq!(expm1(710.0), f64::INFINITY);
        let tiny = 1.0e-300;
        assert_eq!(expm1(tiny).to_bits(), tiny.to_bits());
    }

    #[test]
    fn expm1_consistent_with_exp() {
        // expm1(x) ~ exp(x) - 1 wherever the subtraction is
        // well-conditioned (|x| >= 0.5).
        let mut max = 0u128;
        for i in 0..10_000i32 {
            let t = -37.0 + f64::from(i) * (700.0 - -37.0) / 9_999.0;
            let x = if t.abs() < 0.5 { 0.5 } else { t };
            let d = ulp_diff(expm1(x), exp(x) - 1.0);
            if d > max {
                max = d;
            }
        }
        println!("expm1 vs exp-1 max ulp: {max}");
        assert!(max <= 3, "expm1 vs exp-1 max ulp {max}");
    }

    #[test]
    fn ln_sweep_vs_std() {
        // Bit-linear sweep = logarithmic in value, from the smallest
        // denormal up to 1e308.
        let b0 = 1u64;
        let b1 = 1.0e308f64.to_bits();
        let step = (b1 - b0) / 9_999;
        let mut max = 0u128;
        for i in 0..10_000u64 {
            let x = f64::from_bits(b0 + step * i);
            let d = ulp_diff(ln(x), x.ln());
            if d > max {
                max = d;
            }
        }
        println!("ln max ulp vs std: {max}");
        assert!(max <= 2, "ln sweep max ulp {max}");
    }

    #[test]
    fn ln_near_one_sweep_vs_std() {
        let mut max = 0u128;
        for i in 0..10_000i32 {
            let x = 1.0 + (f64::from(i) - 5_000.0) * 2.0e-7;
            let d = ulp_diff(ln(x), x.ln());
            if d > max {
                max = d;
            }
        }
        assert!(max <= 2, "ln near-1 sweep max ulp {max}");
    }

    #[test]
    fn ln_special_values() {
        assert_eq!(ln(1.0).to_bits(), 0.0f64.to_bits()); // +0 exactly
        assert_eq!(ln(0.0), f64::NEG_INFINITY);
        assert_eq!(ln(-0.0), f64::NEG_INFINITY);
        assert!(ln(-1.0).is_nan());
        assert!(ln(f64::NEG_INFINITY).is_nan());
        assert!(ln(f64::NAN).is_nan());
        assert_eq!(ln(f64::INFINITY), f64::INFINITY);
        // Denormal inputs go through the 2^54 scaling.
        assert_eq!(ulp_diff(ln(5.0e-324), 5.0e-324f64.ln()), 0);
    }

    #[test]
    fn exp_ln_round_trip() {
        // exp(ln(x)) ~ x within a few ulp for moderate x, where the
        // |ln x| * eps amplification stays small.
        let mut max = 0u128;
        for i in 0..10_000i32 {
            let x = 0.05 + f64::from(i) * (20.0 - 0.05) / 9_999.0;
            let d = ulp_diff(exp(ln(x)), x);
            if d > max {
                max = d;
            }
        }
        println!("exp(ln(x)) round-trip max ulp: {max}");
        assert!(max <= 4, "exp(ln(x)) max ulp {max}");
    }

    #[test]
    fn log2_sweep_vs_std() {
        let b0 = 1u64;
        let b1 = 1.0e308f64.to_bits();
        let step = (b1 - b0) / 9_999;
        let mut max = 0u128;
        for i in 0..10_000u64 {
            let x = f64::from_bits(b0 + step * i);
            let d = ulp_diff(log2(x), x.log2());
            if d > max {
                max = d;
            }
        }
        println!("log2 max ulp vs std: {max}");
        assert!(max <= 2, "log2 sweep max ulp {max}");
    }

    #[test]
    fn log2_exact_on_powers_of_two() {
        for k in -1074..=1023i32 {
            let x = if k >= -1022 {
                f64::from_bits((u64::from((k + 1023) as u32)) << 52)
            } else {
                f64::from_bits(1u64 << (k + 1074)) // denormal 2^k
            };
            assert_eq!(
                log2(x).to_bits(),
                f64::from(k).to_bits(),
                "log2(2^{k}) not exact"
            );
        }
    }

    #[test]
    fn log2_special_values() {
        assert_eq!(log2(1.0).to_bits(), 0.0f64.to_bits()); // +0 exactly
        assert_eq!(log2(0.0), f64::NEG_INFINITY);
        assert_eq!(log2(-0.0), f64::NEG_INFINITY);
        assert!(log2(-2.0).is_nan());
        assert!(log2(f64::NAN).is_nan());
        assert_eq!(log2(f64::INFINITY), f64::INFINITY);
    }

    #[test]
    fn scalbn_matches_bit_scaling() {
        // Normal-range scaling is exact; denormal output rounds once.
        assert_eq!(scalbn(1.5, 10).to_bits(), 1536.0f64.to_bits());
        assert_eq!(scalbn(1.0, -1074).to_bits(), f64::from_bits(1).to_bits());
        assert_eq!(scalbn(1.0, -1075).to_bits(), 0.0f64.to_bits()); // ties-to-even
        assert_eq!(scalbn(1.0, 1024), f64::INFINITY);
        assert_eq!(scalbn(-1.0, 1024), f64::NEG_INFINITY);
        // 0.75 * 2^1024 = 1.5 * 2^1023, still finite.
        assert_eq!(
            scalbn(0.75, 1024).to_bits(),
            f64::from_bits(0x7FE8_0000_0000_0000).to_bits()
        );
    }
}
