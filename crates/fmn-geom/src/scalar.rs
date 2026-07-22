//! The transcendental funnel: every trig/inverse-trig call in fmn-geom routes
//! through here so that the fm-7y6 sweep onto fmn-dmath (§6.6, the certified
//! deterministic elementary-function layer) is a one-file change. `sqrt` is
//! exempt: IEEE 754 requires correct rounding, so `f64::sqrt` is already
//! bit-reproducible on every certified platform.

#[inline]
pub(crate) fn sin(x: f64) -> f64 {
    x.sin()
}

#[inline]
pub(crate) fn cos(x: f64) -> f64 {
    x.cos()
}

#[inline]
pub(crate) fn acos(x: f64) -> f64 {
    x.acos()
}

#[inline]
pub(crate) fn atan2(y: f64, x: f64) -> f64 {
    y.atan2(x)
}
