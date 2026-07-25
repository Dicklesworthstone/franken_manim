//! The CPU reference engine for the stroke-SDF + AA-resolve stage.
//!
//! This stands in for §10.1's **certified CPU engine**: it defines what the
//! stage *means*, and the Metal engine is measured against it under §16.3's
//! visual-equivalence budget. It is written to the §10.5 parallelism contract
//! even though the spike runs it single-threaded — tiles are write-disjoint,
//! composition order inside a tile is the command-list order and nothing else,
//! and no accumulation depends on how the tiles were scheduled — because an
//! engine that would need restructuring to become deterministic is not a
//! reference.
//!
//! It is deliberately *not* optimized. §17's order of operations is
//! instrument → eliminate → pipeline → vectorize → offload, and this spike is
//! the offload rung's feasibility probe, not its performance claim. Timings
//! taken here measure the shape of the work, not the CPU engine's eventual
//! speed, and the report says so.

use crate::ir::{PrimitiveHint, RenderIr};
use crate::sdf;

/// A rendered surface in linear-light f32 RGBA, straight alpha.
#[derive(Debug, Clone, PartialEq)]
pub struct Surface {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` linear-light components.
    pub pixels: Vec<f32>,
}

impl Surface {
    /// A surface cleared to `background`.
    pub fn cleared(width: u32, height: u32, background: [f32; 4]) -> Surface {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            pixels.extend_from_slice(&background);
        }
        Surface {
            width,
            height,
            pixels,
        }
    }

    /// The linear RGBA at `(x, y)`.
    pub fn get(&self, x: u32, y: u32) -> [f32; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }

    /// Encode to sRGB RGBA8 through fmn-frame's certified transfer function —
    /// the same encode the real output pipeline performs, so the PNG a reviewer
    /// looks at is not a second, private colour path.
    pub fn to_srgb8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len());
        for px in self.pixels.as_chunks::<4>().0 {
            for c in &px[..3] {
                out.push(fmn_frame::transfer::quantize8(
                    fmn_frame::transfer::srgb_encode((*c as f64).clamp(0.0, 1.0)),
                ));
            }
            // Alpha is linear by definition — it is a coverage fraction, not a
            // light intensity, so it never goes through the transfer function.
            out.push(fmn_frame::transfer::quantize8(
                (px[3] as f64).clamp(0.0, 1.0),
            ));
        }
        out
    }
}

/// Which arithmetic the CPU engine evaluates the stage in.
///
/// [`Precision::Reference`] is the semantic definition. [`Precision::AnnexF32`]
/// runs the identical algorithm in `f32` — the annex's arithmetic without the
/// annex's hardware — so "how much of the Metal engine's divergence is just
/// f32?" is a question CI can answer on a machine with no GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precision {
    /// `f64` throughout: what the stage means.
    Reference,
    /// `f32` throughout the distance solve, mirroring the shader.
    AnnexF32,
}

/// Render `ir` on the CPU in the reference precision.
pub fn render(ir: &RenderIr) -> Surface {
    render_at(ir, Precision::Reference)
}

/// Render `ir` on the CPU at an explicit [`Precision`].
pub fn render_at(ir: &RenderIr, precision: Precision) -> Surface {
    let mut surface = Surface::cleared(ir.grid.width, ir.grid.height, ir.background);
    let cols = ir.grid.cols();
    let tile = ir.grid.tile;

    for t in 0..ir.grid.count() {
        let tx = (t as u32 % cols) * tile;
        let ty = (t as u32 / cols) * tile;
        let lo = ir.tiles.offsets[t] as usize;
        let hi = ir.tiles.offsets[t + 1] as usize;
        if lo == hi {
            continue;
        }
        for py in ty..(ty + tile).min(ir.grid.height) {
            for px in tx..(tx + tile).min(ir.grid.width) {
                // Pixel centre, matching the shader's `(gid + 0.5)`.
                let p = [px as f64 + 0.5, py as f64 + 0.5];
                let mut acc = surface.get(px, py).map(|c| c as f64);
                for &draw in &ir.tiles.draws[lo..hi] {
                    let path = &ir.paths[draw as usize];
                    if p[0] < path.slab[0] as f64
                        || p[0] > path.slab[2] as f64
                        || p[1] < path.slab[1] as f64
                        || p[1] > path.slab[3] as f64
                    {
                        continue;
                    }
                    let style = &ir.styles[path.style as usize];
                    let (alpha, _) = shade(
                        ir,
                        path.first_segment,
                        path.segment_count,
                        path.hint,
                        style,
                        p,
                        precision,
                    );
                    if alpha <= 0.0 {
                        continue;
                    }
                    let src = [
                        style.rgba[0] as f64,
                        style.rgba[1] as f64,
                        style.rgba[2] as f64,
                        style.rgba[3] as f64 * alpha,
                    ];
                    acc = sdf::over(src, acc);
                }
                let i = ((py * surface.width + px) * 4) as usize;
                for (dst, src) in surface.pixels[i..i + 4].iter_mut().zip(acc) {
                    *dst = src as f32;
                }
            }
        }
    }
    surface
}

/// Coverage of one path at one pixel, and the arc-length coordinate where the
/// nearest point sat (returned so tests can pin the width interpolation).
///
/// The loop is the shader's loop: nearest over all segments, arc-length
/// coordinate from the winning segment's span, width lerped along it, then the
/// kept AA profile. Taking the width at the *nearest* point rather than
/// per-segment is what makes a tapered stroke continuous across a joint.
fn shade(
    ir: &RenderIr,
    first: u32,
    count: u32,
    hint: PrimitiveHint,
    style: &crate::ir::Style,
    p: [f64; 2],
    precision: Precision,
) -> (f64, f64) {
    let mut best_d = f64::INFINITY;
    let mut best_s = 0.0;
    for i in first..(first + count) {
        let seg = &ir.segments[i as usize];
        let p0 = [seg.p0[0] as f64, seg.p0[1] as f64];
        let p1 = [seg.p1[0] as f64, seg.p1[1] as f64];
        let p2 = [seg.p2[0] as f64, seg.p2[1] as f64];
        let (d, t) = match (hint, precision) {
            (PrimitiveHint::Line, _) => distance_to_capsule(p, p0, p2),
            (PrimitiveHint::General, Precision::Reference) => {
                sdf::distance_to_quadratic(p, p0, p1, p2)
            }
            (PrimitiveHint::General, Precision::AnnexF32) => {
                let n = |v: [f64; 2]| [v[0] as f32, v[1] as f32];
                let (d, t) = sdf::distance_to_quadratic_f32(n(p), n(p0), n(p1), n(p2));
                (d as f64, t as f64)
            }
        };
        if d < best_d {
            best_d = d;
            best_s = seg.s0 as f64 + (seg.s1 - seg.s0) as f64 * t;
        }
    }
    let width = style.width_start as f64 + (style.width_end - style.width_start) as f64 * best_s;
    let alpha = sdf::coverage(best_d, 0.5 * width, style.aa_width as f64);
    (alpha, best_s)
}

/// The `PrimitiveHint::Line` fast path: distance to a segment, and the
/// normalized position along it.
///
/// Exact for a straight quadratic (`ir::is_straight` only tags a path `Line`
/// when the quadratic term vanishes), so taking this path can only remove work,
/// never change the answer — which is the property that lets §10.8's hints be
/// an optimization rather than a second semantics.
fn distance_to_capsule(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> (f64, f64) {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let ap = [p[0] - a[0], p[1] - a[1]];
    let denom = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if denom <= 0.0 {
        0.0
    } else {
        ((ap[0] * ab[0] + ap[1] * ab[1]) / denom).clamp(0.0, 1.0)
    };
    let d = [ap[0] - ab[0] * t, ap[1] - ab[1] * t];
    ((d[0] * d[0] + d[1] * d[1]).sqrt(), t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Style, TileGrid};
    use fmn_geom::quadpath::QuadPath;

    fn line_ir(width_start: f32, width_end: f32) -> RenderIr {
        let mut path = QuadPath::default();
        path.start_new_path([8.0, 32.0, 0.0]);
        path.add_line_to([56.0, 32.0, 0.0], false).unwrap();
        let mut ir = RenderIr::new(
            TileGrid {
                width: 64,
                height: 64,
                tile: 16,
            },
            [0.0, 0.0, 0.0, 1.0],
        );
        ir.compile_path(
            &path,
            Style {
                rgba: [1.0, 1.0, 1.0, 1.0],
                width_start,
                width_end,
                aa_width: sdf::ANTI_ALIAS_WIDTH_PX as f32,
            },
        )
        .unwrap();
        ir.bin();
        ir
    }

    #[test]
    fn the_f32_transcription_tracks_the_reference_except_where_it_cannot() {
        // The controlled experiment behind the G0-8 equivalence numbers: same
        // algorithm, same geometry, only the scalar width changes. Whatever
        // this reports is the floor under the Metal engine's divergence, and it
        // is reproducible on any machine.
        let ir = crate::scene::preview_frame();
        let a = render_at(&ir, Precision::Reference);
        let b = render_at(&ir, Precision::AnnexF32);
        let d = crate::compare::diverge(&a, &b);
        // Overwhelmingly identical...
        assert!(
            d.differing_fraction() < 1e-3,
            "f32 alone should barely move the picture: {}",
            d.summary()
        );
        // ...but NOT identical, and pretending otherwise would be the lie this
        // spike exists to prevent. If this ever passes with zero divergence,
        // the transcription has stopped tracking the shader.
        assert!(
            d.max_abs > 0.0,
            "f32 and f64 agreeing bit-for-bit means the f32 path is not running"
        );
    }

    #[test]
    fn a_stroke_covers_its_core_and_clears_far_away() {
        let ir = line_ir(6.0, 6.0);
        let s = render(&ir);
        // Dead centre of a 6 px stroke: fully covered.
        assert_eq!(s.get(32, 32)[0], 1.0);
        // 10 px away vertically: nothing.
        assert_eq!(s.get(32, 42)[0], 0.0);
        // Background survives where no path is binned.
        assert_eq!(s.get(2, 2), [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn the_aa_band_is_where_the_kept_profile_says_it_is() {
        let ir = line_ir(6.0, 6.0);
        let s = render(&ir);
        // Stroke centre y = 32.0; pixel centres sit at y + 0.5. Half width 3,
        // AA band 1.5, so the band spans d ∈ [2.25, 3.75]: opaque below it,
        // clear above it, strictly decreasing inside.
        let at = |py: u32| s.get(32, py)[0];
        assert_eq!(at(33), 1.0, "d = 1.5 is inside the core");
        assert!(at(34) > 0.0 && at(34) < 1.0, "d = 2.5 must be partial");
        assert!(
            at(35) > 0.0 && at(35) < at(34),
            "d = 3.5 must be fainter still"
        );
        assert_eq!(at(36), 0.0, "d = 4.5 is beyond the band");
    }

    #[test]
    fn a_tapered_stroke_is_thinner_where_the_taper_says() {
        // Width 8 at the start, 0 at the end: the left end is fat, the right
        // end vanishes. This is the arc-length interpolation under test.
        let ir = line_ir(8.0, 0.0);
        let s = render(&ir);
        let thickness = |px: u32| (0..64).filter(|&py| s.get(px, py)[0] > 0.5).count();
        let left = thickness(12);
        let right = thickness(52);
        assert!(
            left > right,
            "left {left} should be thicker than right {right}"
        );
        assert!(left >= 6, "left end should be near 8 px thick, got {left}");
        assert!(
            right <= 2,
            "right end should have nearly vanished, got {right}"
        );
    }

    #[test]
    fn the_line_hint_and_the_general_solver_agree() {
        // Same geometry, once tagged Line and once forced through the general
        // quadratic path: the hint must be an optimization, not a semantics.
        let mut ir = line_ir(6.0, 6.0);
        let hinted = render(&ir);
        assert_eq!(ir.paths[0].hint, PrimitiveHint::Line);
        ir.paths[0].hint = PrimitiveHint::General;
        let general = render(&ir);
        for (a, b) in hinted.pixels.iter().zip(general.pixels.iter()) {
            assert!(
                (a - b).abs() <= 1e-6,
                "hint changed the picture: {a} vs {b}"
            );
        }
    }

    #[test]
    fn painter_order_decides_which_colour_wins() {
        let mut ir = RenderIr::new(
            TileGrid {
                width: 32,
                height: 32,
                tile: 16,
            },
            [0.0, 0.0, 0.0, 1.0],
        );
        for rgba in [[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]] {
            let mut p = QuadPath::default();
            p.start_new_path([4.0, 16.0, 0.0]);
            p.add_line_to([28.0, 16.0, 0.0], false).unwrap();
            ir.compile_path(
                &p,
                Style {
                    rgba,
                    width_start: 8.0,
                    width_end: 8.0,
                    aa_width: 1.5,
                },
            )
            .unwrap();
        }
        ir.bin();
        let s = render(&ir);
        // Green was drawn last, so green wins — and it must be *exactly* green,
        // not a blend, because both strokes are fully opaque there.
        assert_eq!(s.get(16, 16), [0.0, 1.0, 0.0, 1.0]);
    }

    #[test]
    fn tiling_is_invisible_in_the_output() {
        // The load-bearing property of the whole tiled design: tile size is a
        // scheduling choice that cannot change a pixel.
        let make = |tile: u32| {
            let mut path = QuadPath::default();
            path.start_new_path([6.0, 6.0, 0.0]);
            path.add_quadratic_bezier_curve_to([32.0, 60.0, 0.0], [58.0, 6.0, 0.0], false)
                .unwrap();
            let mut ir = RenderIr::new(
                TileGrid {
                    width: 64,
                    height: 64,
                    tile,
                },
                [0.05, 0.05, 0.08, 1.0],
            );
            ir.compile_path(
                &path,
                Style {
                    rgba: [0.9, 0.7, 0.1, 1.0],
                    width_start: 5.0,
                    width_end: 5.0,
                    aa_width: 1.5,
                },
            )
            .unwrap();
            ir.bin();
            render(&ir)
        };
        let a = make(8);
        let b = make(16);
        let c = make(64);
        assert_eq!(a.pixels, b.pixels);
        assert_eq!(b.pixels, c.pixels);
    }

    #[test]
    fn srgb_encoding_round_trips_the_extremes() {
        let s = Surface::cleared(2, 1, [0.0, 1.0, 0.5, 1.0]);
        let bytes = s.to_srgb8();
        assert_eq!(bytes[0], 0);
        assert_eq!(bytes[1], 255);
        assert_eq!(bytes[3], 255);
        // 0.5 linear is ~188 in sRGB, emphatically not 128.
        assert!(bytes[2] > 180 && bytes[2] < 195, "got {}", bytes[2]);
    }
}
