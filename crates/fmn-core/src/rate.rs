//! The §6.8 rate-function catalog — the Reference's formulas, kept exactly.
//!
//! Every function mirrors `manimlib/utils/rate_functions.py` at the pinned
//! commit and is locked at 10⁴+1 samples by `fixtures/rate_functions.bin`
//! (see `tests/parity.rs`). [`squish_rate_func`] and [`not_quite_there`]
//! are *combinators* (they wrap another rate function); the API schema
//! marks them as such.

/// Identity: `t`.
#[must_use]
#[inline(never)]
pub fn linear(t: f64) -> f64 {
    t
}

/// Zero first and second derivatives at `t = 0` and `t = 1`
/// (equivalent to `bezier([0, 0, 0, 1, 1, 1])`).
#[must_use]
#[inline(never)]
pub fn smooth(t: f64) -> f64 {
    let s = 1.0 - t;
    fmn_dmath::powi(t, 3) * (10.0 * s * s + 5.0 * s * t + t * t)
}

/// The first half of [`smooth`], rescaled: starts gently, ends fast.
#[must_use]
#[inline(never)]
pub fn rush_into(t: f64) -> f64 {
    2.0 * smooth(0.5 * t)
}

/// The second half of [`smooth`], rescaled: starts fast, ends gently.
#[must_use]
#[inline(never)]
pub fn rush_from(t: f64) -> f64 {
    2.0 * smooth(0.5 * (t + 1.0)) - 1.0
}

/// Quarter-circle ease: `sqrt(1 - (1 - t)²)`.
#[must_use]
#[inline(never)]
pub fn slow_into(t: f64) -> f64 {
    (1.0 - (1.0 - t) * (1.0 - t)).sqrt()
}

/// Two chained [`smooth`] ramps with a midpoint plateau in velocity.
#[must_use]
#[inline(never)]
pub fn double_smooth(t: f64) -> f64 {
    if t < 0.5 {
        0.5 * smooth(2.0 * t)
    } else {
        0.5 * (1.0 + smooth(2.0 * t - 1.0))
    }
}

/// Up with [`smooth`] and back down again, symmetric about `t = 0.5`.
#[must_use]
#[inline(never)]
pub fn there_and_back(t: f64) -> f64 {
    let new_t = if t < 0.5 { 2.0 * t } else { 2.0 * (1.0 - t) };
    smooth(new_t)
}

/// [`there_and_back`] holding at 1 for the middle `pause_ratio` of the run
/// (the Reference's default is `1/3`).
#[must_use]
pub fn there_and_back_with_pause(t: f64, pause_ratio: f64) -> f64 {
    let a = 2.0 / (1.0 - pause_ratio);
    if t < 0.5 - pause_ratio / 2.0 {
        smooth(a * t)
    } else if t < 0.5 + pause_ratio / 2.0 {
        1.0
    } else {
        smooth(a - a * t)
    }
}

/// Pull back before launching: `bezier([0, 0, p, p, 1, 1, 1])` with
/// `pull_factor` p (the Reference's default is `-0.5`).
#[must_use]
pub fn running_start(t: f64, pull_factor: f64) -> f64 {
    bernstein(&[0.0, 0.0, pull_factor, pull_factor, 1.0, 1.0, 1.0], t)
}

/// Overshoot the target and settle: `bezier([0, 0, p, p, 1, 1])` with
/// `pull_factor` p (the Reference's default is `1.5`).
#[must_use]
pub fn overshoot(t: f64, pull_factor: f64) -> f64 {
    bernstein(&[0.0, 0.0, pull_factor, pull_factor, 1.0, 1.0], t)
}

/// [`there_and_back`] modulated by a sine with `wiggles` half-periods
/// (the Reference's default is `2`).
#[must_use]
pub fn wiggle(t: f64, wiggles: f64) -> f64 {
    there_and_back(t) * fmn_dmath::sin(wiggles * std::f64::consts::PI * t)
}

/// Combinator: run `func`'s whole arc inside `[a, b]`, clamping outside
/// (the Reference's defaults are `a = 0.4`, `b = 0.6`). Degenerate `a == b`
/// returns `a`, as the Reference does.
pub fn squish_rate_func(func: impl Fn(f64) -> f64, a: f64, b: f64) -> impl Fn(f64) -> f64 {
    move |t| {
        if a == b {
            a
        } else if t < a {
            func(0.0)
        } else if t > b {
            func(1.0)
        } else {
            func((t - a) / (b - a))
        }
    }
}

/// Combinator: scale `func` to top out at `proportion` (the Reference's
/// default wraps [`smooth`] at `0.7`).
pub fn not_quite_there(func: impl Fn(f64) -> f64, proportion: f64) -> impl Fn(f64) -> f64 {
    move |t| proportion * func(t)
}

/// Linear motion finishing at 80% of the run, then holding
/// (`squish_rate_func(linear, 0, 0.8)`).
#[must_use]
#[inline(never)]
pub fn lingering(t: f64) -> f64 {
    squish_rate_func(linear, 0.0, 0.8)(t)
}

/// `1 - exp(-t / half_life)` (the Reference's default half-life is `0.1`).
#[must_use]
pub fn exponential_decay(t: f64, half_life: f64) -> f64 {
    1.0 - fmn_dmath::exp(-t / half_life)
}

// ------------------------------------------------------- FMTL/1 tag catalog

/// A validated FMTL/1 catalog identity, independent of function addresses.
///
/// The discriminants are the existing wire tags, not a new serialization
/// format. Keep this identity when decoding or composing a catalog rate:
/// code generation may give the same function different addresses in
/// different compilation units, so recovering a known tag from a pointer
/// can fail in optimized builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CatalogRate {
    /// Identity.
    Linear = 0,
    /// Smooth start and finish.
    Smooth = 1,
    /// Smooth start, fast finish.
    RushInto = 2,
    /// Fast start, smooth finish.
    RushFrom = 3,
    /// Quarter-circle ease.
    SlowInto = 4,
    /// Two smooth ramps.
    DoubleSmooth = 5,
    /// Smooth out-and-back motion.
    ThereAndBack = 6,
    /// Linear motion followed by a hold.
    Lingering = 7,
}

impl CatalogRate {
    /// Decode the existing catalog, refusing every unknown tag.
    #[must_use]
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::Linear),
            1 => Some(Self::Smooth),
            2 => Some(Self::RushInto),
            3 => Some(Self::RushFrom),
            4 => Some(Self::SlowInto),
            5 => Some(Self::DoubleSmooth),
            6 => Some(Self::ThereAndBack),
            7 => Some(Self::Lingering),
            _ => None,
        }
    }

    /// The original FMTL/1 wire tag.
    #[must_use]
    pub const fn tag(self) -> u8 {
        self as u8
    }

    /// Resolve this identity to its unchanged numeric implementation.
    #[must_use]
    pub fn function(self) -> fn(f64) -> f64 {
        TAG_CATALOG[usize::from(self.tag())]
    }
}

/// The FMTL/1 rate-function catalog (fm-oee, `docs/FMNT1_TIMELINE_BUNDLE.md`):
/// the bundle format's normative `rate: u8` table. Tag order IS this
/// declaration order — the zero-parameter catalog entries only. The
/// parameterized forms (`there_and_back_with_pause`, `running_start`,
/// `overshoot`, `wiggle`, `exponential_decay`) and the combinators carry
/// arguments a bare tag cannot identity, so they have no tag: an exporter
/// that meets one records the segment statefully instead.
///
/// This is one static table, not a const expression instantiated in each
/// caller. Its eight functions are deliberately `inline(never)` so nominal
/// `fn` callers do not acquire cross-crate copies in optimized builds. That
/// supports the legacy pointer-based lookup; durable identities use
/// [`CatalogRate`] and never depend on a successful pointer comparison.
pub static TAG_CATALOG: [fn(f64) -> f64; 8] = [
    linear,
    smooth,
    rush_into,
    rush_from,
    slow_into,
    double_smooth,
    there_and_back,
    lingering,
];

/// The catalog entry for a tag, or `None` for a tag the table does not
/// define (the reader's named refusal, never a guess).
#[must_use]
pub fn from_tag(tag: u8) -> Option<fn(f64) -> f64> {
    CatalogRate::from_tag(tag).map(CatalogRate::function)
}

/// Best-effort recognition of a nominal catalog function.
///
/// Pointer equality is not an intrinsic function identity. A successful
/// same-signature comparison identifies equivalent callable code, but a
/// miss is not evidence of different mathematics. Never use this to recover
/// a tag already decoded from an artifact: retain [`CatalogRate`] instead.
/// Unknown callbacks remain untagged; no finite sample set proves identity.
#[must_use]
pub fn tag_of(func: fn(f64) -> f64) -> Option<u8> {
    TAG_CATALOG
        .iter()
        .position(|&entry| std::ptr::fn_addr_eq(entry, func))
        .map(|index| u8::try_from(index).unwrap_or(u8::MAX))
}

/// Evaluate a 1-D Bézier by the Bernstein sum, mirroring the Reference's
/// `bezier()` term order: `Σₖ (1-t)^(n-k) · t^k · C(n,k) · pₖ`.
fn bernstein(points: &[f64], t: f64) -> f64 {
    let n = points.len() - 1;
    points
        .iter()
        .enumerate()
        .map(|(k, p)| {
            fmn_dmath::powi(1.0 - t, (n - k) as i32)
                * fmn_dmath::powi(t, k as i32)
                * choose(n, k) as f64
                * p
        })
        .sum()
}

/// Binomial coefficient (exact for the small orders used here).
fn choose(n: usize, k: usize) -> u64 {
    let k = k.min(n - k);
    let mut result: u64 = 1;
    for i in 0..k {
        result = result * (n - i) as u64 / (i + 1) as u64;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_exact() {
        assert_eq!(smooth(0.0), 0.0);
        assert_eq!(smooth(1.0), 1.0);
        assert_eq!(there_and_back(0.0), 0.0);
        assert_eq!(double_smooth(1.0), 1.0);
        assert_eq!(lingering(1.0), 1.0);
    }

    #[test]
    fn combinators_match_reference_semantics() {
        let squished = squish_rate_func(smooth, 0.4, 0.6);
        assert_eq!(squished(0.0), 0.0); // func(0) below a
        assert_eq!(squished(1.0), 1.0); // func(1) above b
        assert_eq!(squished(0.5), smooth(0.5));
        let degenerate = squish_rate_func(smooth, 0.3, 0.3);
        assert_eq!(degenerate(0.9), 0.3); // a == b returns a
        let nqt = not_quite_there(smooth, 0.7);
        assert_eq!(nqt(1.0), 0.7);
    }

    #[test]
    fn choose_is_exact() {
        assert_eq!(choose(6, 3), 20);
        assert_eq!(choose(5, 0), 1);
        assert_eq!(choose(5, 5), 1);
    }

    #[test]
    fn every_catalog_entry_round_trips_its_tag() {
        for (tag, &func) in TAG_CATALOG.iter().enumerate() {
            let tag = u8::try_from(tag).unwrap_or(u8::MAX);
            assert_eq!(tag_of(func), Some(tag), "tag_of catalog[{tag}]");
            let decoded = from_tag(tag).unwrap_or(linear);
            assert!(
                std::ptr::fn_addr_eq(decoded, func),
                "from_tag({tag}) is not the catalog entry"
            );
        }
        assert!(from_tag(u8::MAX).is_none(), "unknown tags refuse");
        assert!(
            tag_of(|t| t * t).is_none(),
            "a non-catalog function has no tag"
        );
        // The parameterized forms are deliberately untaggable.
        assert!(tag_of(|t| overshoot(t, 1.5)).is_none());
    }
}
