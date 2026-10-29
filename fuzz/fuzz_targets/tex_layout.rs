//! fm-fmd-math-fuzz-budgets-k8fe fuzz target: the whole Tex pipeline —
//! fmd-math parse, layout, and outline emission into the VMobject family —
//! through `fmn_library::{Tex, TexText}` over the default preamble pack.
//!
//! Resource-budget contract (§16.5/R14): for ANY UTF-8 input, a build either
//! succeeds inside fmd-math's output budgets, one child per placed primitive,
//! or fails with a precise fmd-math error naming a construct or a byte
//! position — never a panic, a hang, a stack overflow, or an engine fault.
//! Each input must finish within 100 ms. A slower one is re-timed three
//! times beside a fixed calibration formula, and is a finding only if its
//! best time is past every one of: 100 ms; 50 times the calibration's time;
//! and 10 times the calibration's time per unit, times the input's units, a
//! unit being a source byte or a placed primitive, whichever the input has
//! more of relative to the calibration. A loaded host slows the calibration
//! too, and a large input or a large output is allowed linear cost; an
//! algorithmic blowup (the physics backtracking in
//! fuzz/findings/tex_physics_backtracking_2026-10-29) outgrows all three.
//! (A fixed 100 ms was not enough. On a fleet VM at load average 14, a
//! TexText that builds in 14-38 ms warm under ASan took 507 ms twice. A
//! 1 039-byte inline Tex of 827 glyphs, linear in fmd-math, took 107 ms under
//! ASan to build. A 1 053-byte display Tex places 9 087 glyphs, ten nested
//! `\left(` extended over ninety `\\` rows, and builds in 13-20 ms in
//! release, 306 ms under ASan.)
//!
//! The first byte picks the surface (display `Tex`, inline `Tex`, `TexText`);
//! the rest is the source.
#![no_main]

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use fmn_library::{Tex, TexText};
use libfuzzer_sys::fuzz_target;

static ENGINE: LazyLock<fmn_tex::TexEngine> = LazyLock::new(|| {
    fmn_tex::TexEngine::new("fmd-math/pack/default", None).expect("bundled engine constructs")
});

const PER_INPUT: Duration = Duration::from_millis(100);

/// How many times the calibration formula's time an input may take before
/// a slow time counts against it.
const CALIBRATION_RATIO: u32 = 50;

/// How many times the calibration formula's time per unit (source byte or
/// placed primitive) an input may take per unit.
const PER_UNIT_RATIO: f64 = 10.0;

/// The calibration formula's placed primitives.
static CALIBRATION_PRIMITIVES: LazyLock<usize> =
    LazyLock::new(|| build(Surface::Display, CALIBRATION).max(1));

/// A typical corpus-shaped formula, timed beside a slow input.
const CALIBRATION: &str = r"\sum_{i=0}^{n} i = \frac{n(n+1)}{2}";

/// One build's wall time and placed primitives.
fn timed(surface: Surface, source: &str) -> (Duration, usize) {
    let start = Instant::now();
    let primitives = build(surface, source);
    (start.elapsed(), primitives)
}

/// Builds `source` and checks the contract; returns the placed primitives
/// (zero for a refusal).

#[derive(Clone, Copy)]
enum Surface {
    Display,
    Inline,
    Text,
}

/// The never-garble bar of the in-tree campaign (`fuzz_campaign.rs`): a
/// precise fmd-math error names a construct (backtick-quoted) or a byte
/// position (digits).
fn precise(message: &str) -> bool {
    !message.is_empty() && (message.bytes().any(|b| b.is_ascii_digit()) || message.contains('`'))
}

fn build(surface: Surface, source: &str) -> usize {
    let engine = &*ENGINE;
    let built = match surface {
        Surface::Display => Tex::new(source).build(engine),
        Surface::Inline => Tex::new(source)
            .math_style(fmn_tex::Style::Text)
            .build(engine),
        Surface::Text => TexText::new(source).build(engine),
    };
    match built {
        Ok(mob) => {
            let layout = &mob.typeset.layout;
            let budgets = fmd_math::Budgets::DEFAULT;
            assert!(
                layout.glyphs.len() <= budgets.max_glyphs,
                "{} glyphs past the {} budget",
                layout.glyphs.len(),
                budgets.max_glyphs
            );
            let segments: usize = layout
                .paths
                .iter()
                .flat_map(|path| &path.contours)
                .map(|contour| contour.segments.len())
                .sum();
            assert!(
                segments <= budgets.max_path_segments,
                "{segments} path segments past the {} budget",
                budgets.max_path_segments
            );
            assert_eq!(
                mob.vmob.children().len(),
                mob.typeset.subs.len(),
                "one child per placed primitive"
            );
            mob.typeset.subs.len()
        }
        Err(fmn_library::TexMobjectError::Tex(fmn_tex::TexError::Math(error))) => {
            let message = error.to_string();
            assert!(precise(&message), "imprecise fmd-math error: {message:?}");
            0
        }
        // ubs:ignore -- a libFuzzer target reports a finding by panicking.
        Err(other) => panic!("engine fault, not an input error: {other}"),
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };
    let Ok(source) = std::str::from_utf8(rest) else {
        return;
    };
    let surface = match selector % 3 {
        0 => Surface::Display,
        1 => Surface::Inline,
        _ => Surface::Text,
    };
    let (first, primitives) = timed(surface, source);
    if first <= PER_INPUT {
        return;
    }
    let (mut best, mut calibration) = (first, Duration::MAX);
    for _ in 0..3 {
        calibration = calibration.min(timed(Surface::Display, CALIBRATION).0);
        best = best.min(timed(surface, source).0);
    }
    let units = (source.len() as f64 / CALIBRATION.len() as f64)
        .max(primitives as f64 / *CALIBRATION_PRIMITIVES as f64);
    let linear = calibration.mul_f64(PER_UNIT_RATIO * units);
    let bound = PER_INPUT.max(calibration * CALIBRATION_RATIO).max(linear);
    assert!(
        best <= bound,
        "{} bytes ({primitives} primitives) took at best {best:?} over four runs, past the \
         {bound:?} bound (the most of {PER_INPUT:?}, {CALIBRATION_RATIO} x the calibration \
         formula's {calibration:?}, and {PER_UNIT_RATIO} x its time per unit for {units:.1} \
         of its sizes)",
        source.len()
    );
});
