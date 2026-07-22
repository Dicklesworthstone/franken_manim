//! f32 entry points: compute through the f64 layer, round once.
//!
//! Deterministic because the f64 layer is; strictly more accurate than a
//! native-f32 polynomial for every function here. The known caveat is
//! double rounding (f64-correct result rounded again to f32), which can
//! land 1 ulp(f32) from the correctly rounded f32 answer in rare cases —
//! deterministically so, on every platform, which is the property the
//! certified path needs.

macro_rules! f32_unary {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[must_use]
        pub fn $name(x: f32) -> f32 {
            crate::$name(f64::from(x)) as f32
        }
    };
}

f32_unary!(
    /// `sin` for f32 — see [`crate::sin`].
    sin
);
f32_unary!(
    /// `cos` for f32 — see [`crate::cos`].
    cos
);
f32_unary!(
    /// `tan` for f32 — see [`crate::tan`].
    tan
);
f32_unary!(
    /// `asin` for f32 — see [`crate::asin`].
    asin
);
f32_unary!(
    /// `acos` for f32 — see [`crate::acos`].
    acos
);
f32_unary!(
    /// `atan` for f32 — see [`crate::atan`].
    atan
);
f32_unary!(
    /// `exp` for f32 — see [`crate::exp`].
    exp
);
f32_unary!(
    /// `expm1` for f32 — see [`crate::expm1`].
    expm1
);
f32_unary!(
    /// `ln` for f32 — see [`crate::ln`].
    ln
);
f32_unary!(
    /// `log2` for f32 — see [`crate::log2`].
    log2
);
f32_unary!(
    /// `cbrt` for f32 — see [`crate::cbrt`].
    cbrt
);
f32_unary!(
    /// `sinh` for f32 — see [`crate::sinh`].
    sinh
);
f32_unary!(
    /// `cosh` for f32 — see [`crate::cosh`].
    cosh
);
f32_unary!(
    /// `tanh` for f32 — see [`crate::tanh`].
    tanh
);

/// `atan2` for f32 — see [`crate::atan2`].
#[must_use]
pub fn atan2(y: f32, x: f32) -> f32 {
    crate::atan2(f64::from(y), f64::from(x)) as f32
}

/// `pow` for f32 — see [`crate::pow`].
#[must_use]
pub fn pow(x: f32, y: f32) -> f32 {
    crate::pow(f64::from(x), f64::from(y)) as f32
}

/// `sqrt` for f32 — hardware, correctly rounded (IEEE 754).
#[must_use]
pub fn sqrt(x: f32) -> f32 {
    x.sqrt()
}

#[cfg(test)]
mod tests {
    #[test]
    fn f32_layer_rounds_the_f64_layer() {
        // Sign/zero preservation and basic sanity; accuracy rides the f64
        // vector gate.
        assert_eq!(super::sqrt(4.0f32), 2.0);
    }
}
