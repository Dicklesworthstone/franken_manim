//! The preview frame the spike renders.
//!
//! §20.1 spike 8's acceptance is "a Metal-rendered preview frame demonstrably
//! produced from the prototype IR", so the frame has to be worth rendering: a
//! flat test card would prove the plumbing and nothing about the mathematics.
//! This scene deliberately exercises every property the stroke stage owns:
//!
//! - **curvature** — arcs and a Lissajous figure, so the cubic root solve runs
//!   in its three-real-roots branch, not just the degenerate ones;
//! - **the `Line` primitive hint** — an axis grid of exactly-straight paths,
//!   which take the capsule fast path;
//! - **joins** — polylines with sharp interior angles, where the nearest
//!   segment changes across a pixel;
//! - **round caps** — every open end;
//! - **arc-length width tapers** — `set_stroke(width=[0, 6, 0])`-shaped
//!   profiles, whose interpolation is the reason segments carry arc-length
//!   spans at all;
//! - **overlapping transparency in painter order** — the property that makes
//!   per-tile command order load-bearing rather than incidental;
//! - **sub-pixel geometry** — a hairline thinner than the AA band, the regime
//!   where a wrong profile is most visible.
//!
//! Colours come from `fmn_core::constants`' palette, converted to linear light
//! once here, so the frame reads as a FrankenManim frame rather than as a
//! debug pattern.

use crate::ir::{DrawKind, RenderIr, Style, TileGrid};
use crate::sdf::ANTI_ALIAS_WIDTH_PX;
use fmn_core::color::Srgb;
use fmn_core::constants::{
    BLUE_B, BLUE_D, GREEN_C, GREY_D, MAROON_B, RED_C, TEAL_C, WHITE, YELLOW_C,
};
use fmn_geom::quadpath::QuadPath;

/// The preview surface size. A Studio preview frame, not an export frame:
/// §13.5 puts the annex's first production duty at preview latency, where the
/// picture is a fraction of the export resolution.
pub const PREVIEW_WIDTH: u32 = 960;
/// Preview height (16:9 with [`PREVIEW_WIDTH`]).
pub const PREVIEW_HEIGHT: u32 = 540;
/// Tile edge in pixels. 16×16 = 256 threads per threadgroup, comfortably
/// inside Apple silicon's 1024 ceiling and a multiple of the 32-wide SIMD
/// group, so no partial waves.
pub const TILE: u32 = 16;

/// The Reference's default background, `#333333` (`manimlib` camera config).
fn background() -> [f32; 4] {
    linear(Srgb::from_rgb8(0x33, 0x33, 0x33), 1.0)
}

fn linear(c: Srgb, alpha: f64) -> [f32; 4] {
    let l = c.to_linear(alpha);
    [l.r as f32, l.g as f32, l.b as f32, l.a as f32]
}

fn style(color: Srgb, alpha: f64, w0: f32, w1: f32) -> Style {
    let mut st = Style::flat(linear(color, alpha), w0, ANTI_ALIAS_WIDTH_PX as f32);
    st.width_end = w1;
    st.rgba_end = st.rgba;
    st
}

/// Build the preview frame's IR, binned and ready to dispatch.
pub fn preview_frame() -> RenderIr {
    build(PREVIEW_WIDTH, PREVIEW_HEIGHT, TILE)
}

/// The same scene at an arbitrary size — used by the tests to prove the IR is
/// resolution- and tile-independent.
pub fn build(width: u32, height: u32, tile: u32) -> RenderIr {
    let mut ir = RenderIr::new(
        TileGrid {
            width,
            height,
            tile,
        },
        background(),
    );
    let w = width as f64;
    let h = height as f64;

    // ---- the grid: exactly-straight paths, so the Line hint is exercised.
    for i in 1..12 {
        let x = w * i as f64 / 12.0;
        let mut p = QuadPath::default();
        p.start_new_path([x, h * 0.06, 0.0]);
        p.add_line_to([x, h * 0.94, 0.0], false).unwrap();
        ir.compile_path(&p, style(GREY_D, 1.0, 1.0, 1.0), DrawKind::Stroke);
    }
    for i in 1..7 {
        let y = h * i as f64 / 7.0;
        let mut p = QuadPath::default();
        p.start_new_path([w * 0.04, y, 0.0]);
        p.add_line_to([w * 0.96, y, 0.0], false).unwrap();
        ir.compile_path(&p, style(GREY_D, 1.0, 1.0, 1.0), DrawKind::Stroke);
    }

    // ---- a Lissajous figure: dense curvature, every cubic-root branch.
    //
    // Handles sit at the intersection of the sampled points' tangents, which
    // is the quadratic that actually osculates the curve. A midpoint handle
    // would make every segment exactly straight — the `Line` hint would fire
    // and the general solver would never run, which is the one thing this path
    // exists to exercise.
    {
        let mut p = QuadPath::default();
        let n = 96;
        let cx = w * 0.5;
        let cy = h * 0.5;
        let ax = w * 0.36;
        let ay = h * 0.34;
        let at = |k: usize| -> f64 { k as f64 / n as f64 * std::f64::consts::TAU };
        let point = |t: f64| -> [f64; 2] {
            [
                cx + ax * fmn_dmath::sin(3.0 * t),
                cy + ay * fmn_dmath::sin(2.0 * t + 0.7),
            ]
        };
        let tangent = |t: f64| -> [f64; 2] {
            [
                3.0 * ax * fmn_dmath::cos(3.0 * t),
                2.0 * ay * fmn_dmath::cos(2.0 * t + 0.7),
            ]
        };
        let a0 = point(at(0));
        p.start_new_path([a0[0], a0[1], 0.0]);
        for k in 1..=n {
            let t0 = at(k - 1);
            let t1 = at(k);
            let a = point(t0);
            let b = point(t1);
            let handle = tangent_intersection(a, tangent(t0), b, tangent(t1));
            p.add_quadratic_bezier_curve_to([handle[0], handle[1], 0.0], [b[0], b[1], 0.0], false)
                .unwrap();
        }
        ir.compile_path(&p, style(BLUE_B, 1.0, 3.0, 3.0), DrawKind::Stroke);
    }

    // ---- three overlapping translucent arcs: painter order under alpha.
    //
    // Built by fmn-geom's own `QuadPath::arc`, so the arc-density rule under
    // test is BN-09's, not a hand-rolled one — the spike measures the engines,
    // not a private approximation of a circle.
    for (i, color) in [RED_C, GREEN_C, YELLOW_C].into_iter().enumerate() {
        let start = std::f64::consts::PI * (0.15 + 0.10 * i as f64);
        let p = QuadPath::arc(
            start,
            std::f64::consts::PI * 1.4,
            h * 0.26,
            [w * (0.30 + 0.20 * i as f64), h * 0.50, 0.0],
            None,
        );
        ir.compile_path(&p, style(color, 0.55, 12.0, 12.0), DrawKind::Stroke);
    }

    // ---- tapered strokes: the arc-length width interpolation.
    for i in 0..6 {
        let mut p = QuadPath::default();
        let y = h * (0.14 + 0.145 * i as f64);
        p.start_new_path([w * 0.06, y, 0.0]);
        p.add_quadratic_bezier_curve_to([w * 0.16, y - h * 0.07, 0.0], [w * 0.26, y, 0.0], false)
            .unwrap();
        let taper = 2.0 + 2.0 * i as f32;
        ir.compile_path(&p, style(TEAL_C, 0.95, taper, 0.0), DrawKind::Stroke);
    }

    // ---- a sharp-jointed polyline: the nearest segment changes across pixels.
    {
        let mut p = QuadPath::default();
        let y0 = h * 0.80;
        p.start_new_path([w * 0.62, y0, 0.0]);
        for i in 0..7 {
            let x = w * (0.62 + 0.05 * (i + 1) as f64);
            let y = if i % 2 == 0 { y0 - h * 0.11 } else { y0 };
            p.add_line_to([x, y, 0.0], false).unwrap();
        }
        ir.compile_path(&p, style(MAROON_B, 1.0, 7.0, 7.0), DrawKind::Stroke);
    }

    // ---- a hairline thinner than the AA band: the sub-pixel regime.
    {
        let mut p = QuadPath::default();
        p.start_new_path([w * 0.05, h * 0.97, 0.0]);
        p.add_quadratic_bezier_curve_to([w * 0.5, h * 0.90, 0.0], [w * 0.95, h * 0.97, 0.0], false)
            .unwrap();
        ir.compile_path(&p, style(WHITE, 1.0, 0.4, 0.4), DrawKind::Stroke);
    }

    // ---- a heavy opaque underline, drawn last: proves painter order wins.
    {
        let mut p = QuadPath::default();
        p.start_new_path([w * 0.30, h * 0.06, 0.0]);
        p.add_line_to([w * 0.70, h * 0.06, 0.0], false).unwrap();
        ir.compile_path(&p, style(BLUE_D, 1.0, 9.0, 9.0), DrawKind::Stroke);
    }

    ir.bin();
    ir
}

/// How far a control handle may sit from its chord midpoint, as a multiple of
/// the chord length.
///
/// Any real curve fitter needs this clamp, and the spike learned why the hard
/// way: near a Lissajous turning point consecutive tangents are nearly
/// parallel, so their intersection runs away to hundreds of chord lengths. The
/// resulting quadratic is *algebraically* still an interpolant and
/// *numerically* a disaster — its coefficients span orders of magnitude, and
/// the f32 annex and the f64 CPU stop agreeing on a handful of pixels around
/// exactly those control points. Bounding the handle removes the pathology
/// from the content instead of papering over it in the kernel.
const MAX_HANDLE_BOW: f64 = 2.0;

/// Where the tangent at `a` meets the tangent at `b` — the quadratic control
/// point that makes the segment osculate the sampled curve — clamped to
/// [`MAX_HANDLE_BOW`] chord lengths from the chord midpoint.
///
/// Falls back to the chord midpoint when the tangents are parallel (an
/// inflection sampled symmetrically), which is the right answer there: the
/// osculating quadratic genuinely is the straight chord.
fn tangent_intersection(a: [f64; 2], da: [f64; 2], b: [f64; 2], db: [f64; 2]) -> [f64; 2] {
    let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
    let cross = da[0] * db[1] - da[1] * db[0];
    if cross.abs() < 1e-12 {
        return mid;
    }
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let chord = (dx * dx + dy * dy).sqrt();
    let s = (dx * db[1] - dy * db[0]) / cross;
    let h = [a[0] + da[0] * s, a[1] + da[1] * s];

    let bow = [h[0] - mid[0], h[1] - mid[1]];
    let bow_len = (bow[0] * bow[0] + bow[1] * bow[1]).sqrt();
    let limit = MAX_HANDLE_BOW * chord;
    if bow_len <= limit || bow_len <= 0.0 {
        return h;
    }
    let k = limit / bow_len;
    [mid[0] + bow[0] * k, mid[1] + bow[1] * k]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::PrimitiveHint;

    #[test]
    fn the_preview_frame_is_actually_nontrivial() {
        let ir = preview_frame();
        assert!(ir.paths.len() >= 25, "only {} paths", ir.paths.len());
        assert!(
            ir.segments.len() >= 150,
            "only {} segments",
            ir.segments.len()
        );
        assert!(ir.styles.len() >= 8, "only {} styles", ir.styles.len());
        assert!(
            !ir.tiles.draws.is_empty(),
            "nothing was binned, so nothing would render"
        );
    }

    #[test]
    fn both_primitive_hints_are_exercised() {
        let ir = preview_frame();
        assert!(
            ir.paths.iter().any(|p| p.hint == PrimitiveHint::Line),
            "no straight path — the capsule fast path is untested"
        );
        assert!(
            ir.paths.iter().any(|p| p.hint == PrimitiveHint::General),
            "no curved path — the cubic solve is untested"
        );
    }

    #[test]
    fn tapers_and_translucency_are_both_present() {
        let ir = preview_frame();
        assert!(
            ir.styles.iter().any(|s| s.width_start != s.width_end),
            "no tapered stroke — arc-length width interpolation is untested"
        );
        assert!(
            ir.styles.iter().any(|s| s.rgba[3] < 0.99),
            "no translucent stroke — painter-order compositing is untested"
        );
        assert!(
            ir.styles
                .iter()
                .any(|s| s.width_start < ANTI_ALIAS_WIDTH_PX as f32),
            "no sub-pixel hairline — the thin-stroke regime is untested"
        );
    }

    #[test]
    fn every_binned_draw_indexes_a_real_path() {
        let ir = preview_frame();
        for &d in &ir.tiles.draws {
            assert!((d as usize) < ir.paths.len(), "dangling draw index {d}");
        }
        for w in ir.tiles.offsets.windows(2) {
            assert!(w[0] <= w[1], "CSR offsets are not monotone");
        }
    }

    #[test]
    fn the_scene_is_tile_size_independent_in_its_geometry() {
        // Changing the tiling must change only the command lists, never the
        // geometry that produced them.
        let a = build(320, 180, 8);
        let b = build(320, 180, 20);
        assert_eq!(a.segments, b.segments);
        assert_eq!(a.paths, b.paths);
        assert_eq!(a.styles, b.styles);
        assert_ne!(a.tiles, b.tiles);
    }
}
