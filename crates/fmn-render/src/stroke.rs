//! §10.3's strokes: **true curve-distance**, round caps, arc-length width and
//! colour — replacing the Reference's ≤32-segment polyline ribbons.
//!
//! > True curve-distance strokes: exact/high-accuracy signed distance to the
//! > quadratic within conservative slabs; **round caps** on open ends;
//! > round/bevel/miter joins with a real miter limit plus a smooth "auto" join
//! > tuned in the look study; width and colour interpolated by **arc length**;
//! > `flat_stroke` vs camera-facing 3D construction both supported; the smoothstep
//! > AA profile at the familiar ~1.5 px weight.
//!
//! ## The stroke is one expression
//!
//! Everything above except the joins falls out of a single quantity — the
//! **signed excess**
//!
//! ```text
//! excess(p) = min over segments of ( distance(p, segment) − half_width(s) )
//! ```
//!
//! where `s` is the *arc length* at the nearest point. Coverage is that excess
//! through G0-2's measured antialiasing profile, and nothing else is needed:
//!
//! - **Round caps come free.** The distance to an open end is radial, so the
//!   level set is a semicircle without anyone asking for one. The Reference's
//!   butt caps were a ribbon-pipeline artifact; round is the deliberate choice.
//! - **Round joins come free.** Two segments sharing an anchor contribute two
//!   distance fields whose minimum is exactly a round join — no notch, no gap, no
//!   bookkeeping. G0-2's finding L6 is that this collapses the Reference's four
//!   joint types to one, and its calibration capture renders four identical rows
//!   on purpose.
//! - **Width by arc length is a change of what `s` means**, not a change of
//!   mechanism. Because the excess is taken *per segment* before the minimum, a
//!   stroke whose width varies is a union of variable-radius tubes rather than one
//!   tube of averaged radius — which is what makes a reparameterization of the
//!   curve leave the stroke identical, the metamorphic law §10.3's acceptance
//!   names.
//!
//! ## What is not free
//!
//! The `Miter`, `Bevel` and `NoJoint` overrides are real geometry the distance
//! field does not produce, and they land with [`JoinWedge`] rather than by
//! bending the distance field into shapes it does not have. Their ruling — which
//! numeric code draws which corner, and why the Reference's *names* cannot be
//! trusted — is ADR-0012.
//!
//! ## `flat_stroke` and camera-facing construction
//!
//! This module owns the affine distance kernel. Under its [`ScreenMap`], a width
//! measured in the object plane and one measured in screen pixels differ only
//! by the uniform scale, so both constructions intentionally collapse to the
//! same expression here. Perspective vectors take the camera-bound route in
//! [`crate::three_d::ThreeDJob`]: it derives separate flat and billboard
//! half-widths from world-space curve controls, then feeds the resulting
//! screen-space widths back into this distance kernel.
//! [`crate::engine::FrameJob`] rejects camera-dependent style fields so they
//! cannot silently render through the affine route.
//!
//! ## What the slabs are, and why there is no slab table
//!
//! §10.3 says "within conservative slabs", and the IR already retains what a slab
//! is made of: a segment's control-polygon hull contains its curve. The
//! width-dependent expansion is deliberately **not** retained, because the width
//! is a property of the *style* and two occurrences of one interned outline can
//! carry different widths — a retained slab would be invalidated by a restyle,
//! which is exactly the coupling §10.8's revision axes exist to avoid. So
//! [`segment_slab`] computes it per draw from retained data, and it is cheap: a
//! hull and an add.
//!
//! ## Prepared rejection and the SIMD ruling
//!
//! fm-4wt.3 found that the engine used only the union slab: every pixel inside
//! that union still ran the cubic nearest-point solve and both arc-length
//! evaluations for every segment. [`PreparedStroke`] now derives each segment's
//! style/placement-dependent slab and geometry-only total arc length once per
//! draw. The scalar kernel keeps segment order, rejects only provably
//! zero-coverage segments, and remains bit-identical to the unculled oracle
//! wherever coverage can change.
//!
//! On the committed 512×256 curved-chain benchmark, 8/32/64 segments fell from
//! 86.63/373.44/770.93 ms to 14.84/28.94/45.95 ms: 5.8×/12.9×/16.8× from
//! eliminating work. A governed x86-64-v3 `f64x4` prototype then evaluated four
//! slab admissions together while retaining scalar f64 cubic solves in original
//! order. Three SIMD sweeps measured 14.60–14.84/28.32–29.46/43.79–44.58 ms
//! versus scalar sweeps of 14.64–15.02/27.96–28.80/43.43–45.43 ms. That is
//! noise, not a repeatable tier speedup, so the packing route is deliberately
//! not retained.
//! The ill-conditioned distance solve remains f64 exactly as G0-8 ruled.

use crate::bin::ScreenMap;
use crate::table::{Segment, Style};

/// Convert a stroke width from manim's width units to screen pixels.
///
/// `STROKE_WIDTH_CONVERSION = 0.01` scene units per width unit
/// (`stroke/vert.glsl:21`), then scene units to pixels. G0-2 derived the pair: at
/// 1920×1080 with the default frame there are 135 px per scene unit, so one width
/// unit is 1.35 px and `DEFAULT_STROKE_WIDTH = 4.0` is 5.4 px.
///
/// The one conversion for every width in the renderer — §10.2's
/// `fill_border_width` included, since the Reference feeds that field into this
/// same stroke-width slot.
#[must_use]
pub fn width_px(width_units: f32, map: ScreenMap) -> f64 {
    f64::from(width_units) * fmn_core::constants::STROKE_WIDTH_CONVERSION * map.scale.abs()
}

/// Coverage from a signed excess, through G0-2's **measured** antialiasing
/// profile.
///
/// `stroke/frag.glsl` is `smoothstep(0.5, −0.5, |d|/aaw − hw/aaw)`, i.e.
/// `t = clamp(½ − excess/aaw, 0, 1)` then the Hermite `t²(3 − 2t)`, with
/// `anti_alias_width = 1.5` px (`vectorized_mobject.py:96`). G0-2 finding L1
/// measured that band at **1.560 px** with RMS 0.0031 against a declared 1.5 —
/// inside the capture's own 8-bit quantization — and ratified keeping both the
/// width and the curve.
///
/// The profile lives here because the stroke is where it was measured and where
/// it is used most; §10.4's adaptive-AA bead (fm-gmr) may relocate it, and
/// §10.2's inner border already calls it rather than restating it.
#[must_use]
pub fn aa_coverage(excess_px: f64, aa_width_px: f64) -> f64 {
    let aa = if aa_width_px > 0.0 { aa_width_px } else { 1e-8 };
    let t = (0.5 - excess_px / aa).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The half-width in pixels at normalized arc length `s`.
///
/// The IR's [`Style`] carries the two ends of the per-point `stroke_width`
/// column, which is how the Reference expresses a width ramp along a path, so the
/// ramp is linear in `s`. Interpolating the *width* and halving is not the same
/// as interpolating two half-widths, but it is the same to floating point and it
/// is the order the Reference uses.
#[must_use]
pub fn half_width_px(style: &Style, map: ScreenMap, s: f64) -> f64 {
    let s = s.clamp(0.0, 1.0) as f32;
    let w = style.stroke_width + (style.stroke_width_end - style.stroke_width) * s;
    0.5 * width_px(w, map)
}

/// The stroke colour at normalized arc length `s`.
///
/// Componentwise in the linear-light straight-alpha space the table stores
/// (§6.3, BN-04) — a colour interpolation happens where colour interpolation is
/// defined, not in an encoded space.
#[must_use]
pub fn stroke_rgba_at(style: &Style, s: f64) -> [f32; 4] {
    let s = s.clamp(0.0, 1.0) as f32;
    let mut out = [0.0f32; 4];
    for (k, o) in out.iter_mut().enumerate() {
        let a = style.stroke_rgba[k];
        let b = style.stroke_rgba_end[k];
        *o = a + (b - a) * s;
    }
    out
}

/// One segment's conservative screen-space slab: the curve's hull, grown by the
/// widest half-width the ramp can reach plus the antialiasing band.
///
/// Conservative in the safe direction — a slab that is too large costs a distance
/// evaluation, and there is no input for which one that is too small is anything
/// but a clipped stroke. It uses the **whole ramp's** maximum rather than the
/// width at this segment's own arc-length span, because the nearest point to a
/// pixel inside this slab can lie on a neighbouring segment.
#[must_use]
pub fn segment_slab(seg: &Segment, style: &Style, map: ScreenMap, translate: [f64; 2]) -> [f64; 4] {
    let to_px = |p: fmn_core::types::Vec3| {
        [
            map.origin[0] + p[0] * map.scale + translate[0],
            map.origin[1] + p[1] * map.scale + translate[1],
        ]
    };
    let pts = [to_px(seg.p0), to_px(seg.p1), to_px(seg.p2)];
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for q in pts {
        for k in 0..2 {
            lo[k] = lo[k].min(q[k]);
            hi[k] = hi[k].max(q[k]);
        }
    }
    let pad = max_stroke_reach_px(style, map);
    [lo[0] - pad, lo[1] - pad, hi[0] + pad, hi[1] + pad]
}

/// Maximum screen-space reach beyond the control-polygon hull.
///
/// Round and bevelled strokes reach one half-width beyond the centreline. An
/// admitted miter may reach [`MITER_LIMIT`] half-widths, and the binning and
/// engine slabs must include that tip before they are allowed to reject a
/// pixel. The antialiasing band is added in either case.
#[must_use]
pub fn max_stroke_reach_px(style: &Style, map: ScreenMap) -> f64 {
    let widest = 0.5
        * width_px(style.stroke_width, map)
            .max(width_px(style.stroke_width_end, map))
            .max(0.0);
    let geometric = if style.joint_type == fmn_mobject::JointType::Miter {
        MITER_LIMIT * widest
    } else {
        widest
    };
    geometric + f64::from(style.anti_alias_width).max(0.0)
}

/// One segment's style/placement-dependent stroke preparation.
///
/// `pub(crate)` so the frame arena ([`crate::arena`]) can hold a typed pool
/// of them; the engine's per-frame path bump-allocates there instead of
/// allocating a fresh `Vec` per draw (PG-6).
#[derive(Debug, Clone, Copy)]
pub(crate) struct PreparedSegment {
    slab: [f64; 4],
    total_arc_length: f64,
    line: bool,
}

/// Per-draw stroke data whose inputs are fixed before a tile is touched.
///
/// The retained [`Segment`] remains geometry-only. These values additionally
/// depend on style, placement, and camera scale, so they belong to the
/// per-frame draw derivation: one conservative slab and one total arc length
/// per segment. A pixel outside a segment slab cannot receive coverage from
/// that segment, which lets the engine avoid its cubic solve without changing a
/// visible bit.
///
/// The struct is a *view*: the segment storage lives in the caller's buffer —
/// the frame arena's typed pool in the engine, a plain `Vec` in tests — so the
/// per-frame path allocates nothing once the arena is warm.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PreparedStroke<'a> {
    segments: &'a [PreparedSegment],
    slab: [f64; 4],
}

impl<'a> PreparedStroke<'a> {
    /// The view over caller-owned prepared segments and their aggregate slab.
    pub(crate) fn from_parts(segments: &'a [PreparedSegment], slab: [f64; 4]) -> Self {
        Self { segments, slab }
    }

    /// Derive every segment's slab and total arc length into `out`, returning
    /// the aggregate slab. Same loop, same order as the retained derivation —
    /// only the destination is the caller's.
    pub(crate) fn prepare_into(
        out: &mut impl crate::arena::Sink<PreparedSegment>,
        segments: &[Segment],
        style: &Style,
        map: ScreenMap,
        translate: [f64; 2],
        straight_segments: bool,
    ) -> [f64; 4] {
        let mut slab = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for segment in segments {
            let segment_slab = segment_slab(segment, style, map, translate);
            slab[0] = slab[0].min(segment_slab[0]);
            slab[1] = slab[1].min(segment_slab[1]);
            slab[2] = slab[2].max(segment_slab[2]);
            slab[3] = slab[3].max(segment_slab[3]);
            out.put(PreparedSegment {
                slab: segment_slab,
                total_arc_length: fmn_geom::arclength::quadratic_arc_length(
                    segment.p0, segment.p1, segment.p2,
                ),
                line: straight_segments,
            });
        }
        slab
    }

    #[must_use]
    pub(crate) fn slab(&self) -> [f64; 4] {
        self.slab
    }

    fn nearest(
        &self,
        segments: &[Segment],
        style: &Style,
        map: ScreenMap,
        translate: [f64; 2],
        p: [f64; 2],
    ) -> Option<(f64, f64)> {
        let scale = map.scale;
        if scale == 0.0 || segments.is_empty() || segments.len() != self.segments.len() {
            return None;
        }
        let obj = [
            (p[0] - map.origin[0] - translate[0]) / scale,
            (p[1] - map.origin[1] - translate[1]) / scale,
            0.0,
        ];
        let mut best = f64::INFINITY;
        let mut best_s = 0.0;
        let mut admitted = false;
        for (segment, prepared) in segments.iter().zip(self.segments.iter()) {
            if p[0] < prepared.slab[0]
                || p[0] > prepared.slab[2]
                || p[1] < prepared.slab[1]
                || p[1] > prepared.slab[3]
            {
                continue;
            }
            admitted = true;
            let (excess, s) = if prepared.line {
                line_segment_excess_and_s(segment, style, map, obj)
            } else {
                segment_excess_and_s(segment, prepared.total_arc_length, style, map, obj)
            };
            if excess < best {
                best = excess;
                best_s = s;
            }
        }
        admitted.then_some((best, best_s))
    }

    #[must_use]
    pub(crate) fn shade(
        &self,
        segments: &[Segment],
        joins: &[JoinWedge],
        style: &Style,
        map: ScreenMap,
        translate: [f64; 2],
        p: [f64; 2],
    ) -> (f64, f64) {
        if style.stroke_width <= 0.0 && style.stroke_width_end <= 0.0 {
            return (0.0, 0.0);
        }
        match self.nearest(segments, style, map, translate, p) {
            Some((round, s)) => (
                aa_coverage(
                    apply_joins(round, joins, style.joint_type, p),
                    f64::from(style.anti_alias_width),
                ),
                s,
            ),
            None => (0.0, 0.0),
        }
    }
}

/// Maximum screen-space Hausdorff error admitted by the line fast path.
///
/// One quarter of a milli-pixel is far below the raw half-float surface's
/// useful spatial resolution, while remaining explicit and invariant under
/// scene scale. Semantic `Line`/`Polyline` hints bypass the numeric test because
/// they are the authoritative construction identity; writable point mutation
/// invalidates that hint before the frame job is built.
#[cfg(any(feature = "metal", test))]
pub(crate) const LINE_APPROXIMATION_MAX_ERROR_PX: f64 = 1.0 / 4096.0;

/// Whether a segment may use capsule distance without a visible geometry move.
#[cfg(any(feature = "metal", test))]
pub(crate) fn line_approximation_admitted(
    segment: &Segment,
    map: ScreenMap,
    semantic_line: bool,
) -> bool {
    if semantic_line {
        return true;
    }
    let chord = [segment.p2[0] - segment.p0[0], segment.p2[1] - segment.p0[1]];
    let handle = [segment.p1[0] - segment.p0[0], segment.p1[1] - segment.p0[1]];
    let chord_squared = chord[0] * chord[0] + chord[1] * chord[1];
    if chord_squared == 0.0 {
        return handle[0] == 0.0 && handle[1] == 0.0;
    }
    let projection = (handle[0] * chord[0] + handle[1] * chord[1]) / chord_squared;
    if !(0.0..=1.0).contains(&projection) {
        return false;
    }
    let twice_area = (chord[0] * handle[1] - chord[1] * handle[0]).abs();
    let maximum_object_error = 0.5 * twice_area / chord_squared.sqrt();
    maximum_object_error * map.scale.abs() <= LINE_APPROXIMATION_MAX_ERROR_PX
}

fn line_segment_excess_and_s(
    segment: &Segment,
    style: &Style,
    map: ScreenMap,
    object_point: fmn_core::types::Vec3,
) -> (f64, f64) {
    let chord = [
        segment.p2[0] - segment.p0[0],
        segment.p2[1] - segment.p0[1],
        segment.p2[2] - segment.p0[2],
    ];
    let delta = [
        object_point[0] - segment.p0[0],
        object_point[1] - segment.p0[1],
        object_point[2] - segment.p0[2],
    ];
    let denominator = dot3(chord, chord);
    let t = if denominator > 0.0 {
        (dot3(delta, chord) / denominator).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let nearest_delta = [
        delta[0] - chord[0] * t,
        delta[1] - chord[1] * t,
        delta[2] - chord[2] * t,
    ];
    let s = segment.s0 + (segment.s1 - segment.s0) * t;
    let distance = dot3(nearest_delta, nearest_delta).sqrt();
    (distance * map.scale.abs() - half_width_px(style, map, s), s)
}

fn dot3(a: fmn_core::types::Vec3, b: fmn_core::types::Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The stroke's **signed excess** at a screen point, in pixels: negative inside
/// the stroke, zero on its silhouette, positive outside.
///
/// The single quantity the stroke is made of. Queried in **object space** and
/// scaled, rather than by projecting the segments: the map is a uniform scale plus
/// a translation, so it preserves which point is nearest and multiplies the
/// distance by the scale — and doing it this way means the renderer holds no
/// second copy of the geometry (D-04).
///
/// The excess is formed **per segment before the minimum**, which is what makes a
/// variable-width stroke a union of variable-radius tubes rather than one tube of
/// some averaged radius. Taking `min(distance) − half_width(s_of_that_min)`
/// instead would use one segment's width at another segment's nearest point and
/// would give a different, wrong silhouette wherever the ramp is steep.
///
/// `None` for a path with no segments or a degenerate map.
#[must_use]
pub fn stroke_excess_px(
    segments: &[Segment],
    style: &Style,
    map: ScreenMap,
    translate: [f64; 2],
    p: [f64; 2],
) -> Option<f64> {
    stroke_nearest(segments, style, map, translate, p).map(|(excess, _)| excess)
}

/// [`stroke_excess_px`], plus the **arc-length coordinate of the winning
/// segment's nearest point**.
///
/// The coordinate is not a diagnostic: [`stroke_rgba_at`] and
/// [`half_width_px`] are both functions of it, and §10.3 says width and colour
/// interpolate by the *same* arc length. Nothing produced it until an engine
/// needed to shade a pixel, so the ramp existed with no way to evaluate it —
/// which is why this is the primitive and [`stroke_excess_px`] is the
/// projection, rather than the loop being written twice and kept in agreement
/// by hand.
///
/// Ties go to the **earliest segment in the shape's own segment order**, which
/// is a property of the geometry rather than of the sync that compiled it
/// (ADR-0013): `excess < best` keeps the incumbent, so the winner is
/// order-determined only where two segments are exactly equidistant, and that
/// order is the outline's.
///
/// `None` for a path with no segments or a degenerate map.
#[must_use]
pub fn stroke_nearest(
    segments: &[Segment],
    style: &Style,
    map: ScreenMap,
    translate: [f64; 2],
    p: [f64; 2],
) -> Option<(f64, f64)> {
    let scale = map.scale;
    if scale == 0.0 || segments.is_empty() {
        return None;
    }
    let obj = [
        (p[0] - map.origin[0] - translate[0]) / scale,
        (p[1] - map.origin[1] - translate[1]) / scale,
        0.0,
    ];
    let mut best = f64::INFINITY;
    let mut best_s = 0.0;
    for g in segments {
        let total = fmn_geom::arclength::quadratic_arc_length(g.p0, g.p1, g.p2);
        let (excess, s) = segment_excess_and_s(g, total, style, map, obj);
        if excess < best {
            best = excess;
            best_s = s;
        }
    }
    Some((best, best_s))
}

fn segment_excess_and_s(
    segment: &Segment,
    total_arc_length: f64,
    style: &Style,
    map: ScreenMap,
    object_point: fmn_core::types::Vec3,
) -> (f64, f64) {
    let near =
        fmn_geom::distance::nearest_on_quadratic(segment.p0, segment.p1, segment.p2, object_point);
    // Arc length at the nearest point, not the parameter: the ramp is
    // arc-length-parameterized (BN-03), and `t` and `s` differ by exactly the
    // amount that note exists to talk about.
    let frac = if total_arc_length > 0.0 {
        let sub =
            fmn_geom::bezier::partial_quadratic(&[segment.p0, segment.p1, segment.p2], 0.0, near.t);
        (fmn_geom::arclength::quadratic_arc_length(sub[0], sub[1], sub[2]) / total_arc_length)
            .clamp(0.0, 1.0)
    } else {
        0.0
    };
    let s = segment.s0 + (segment.s1 - segment.s0) * frac;
    let excess = near.distance * map.scale.abs() - half_width_px(style, map, s);
    (excess, s)
}

/// The stroke's coverage **and** its ramp coordinate at a screen point — the
/// engine's per-pixel stroke shading call.
///
/// [`stroke_coverage_with_joins`] is this with the coordinate dropped. Both
/// numbers come from one pass over the segments, because computing the coverage
/// and then re-solving for the colour would evaluate every nearest-point solve
/// twice and could disagree with itself on a tie.
#[must_use]
pub fn stroke_shade(
    segments: &[Segment],
    joins: &[JoinWedge],
    style: &Style,
    map: ScreenMap,
    translate: [f64; 2],
    p: [f64; 2],
) -> (f64, f64) {
    if style.stroke_width <= 0.0 && style.stroke_width_end <= 0.0 {
        return (0.0, 0.0);
    }
    match stroke_nearest(segments, style, map, translate, p) {
        Some((round, s)) => (
            aa_coverage(
                apply_joins(round, joins, style.joint_type, p),
                f64::from(style.anti_alias_width),
            ),
            s,
        ),
        None => (0.0, 0.0),
    }
}

/// The stroke's coverage at a screen point, in `[0, 1]`.
///
/// [`stroke_excess_px`] through [`aa_coverage`]. Round caps and round joins are
/// properties of the excess, so they need no argument and cannot be switched off
/// by accident; the explicit joint overrides are [`JoinWedge`]'s.
#[must_use]
pub fn stroke_coverage(
    segments: &[Segment],
    style: &Style,
    map: ScreenMap,
    translate: [f64; 2],
    p: [f64; 2],
) -> f64 {
    // A zero-width ramp draws nothing, and it is worth short-circuiting rather
    // than letting the AA band paint a hairline where the author asked for
    // nothing at all.
    if style.stroke_width <= 0.0 && style.stroke_width_end <= 0.0 {
        return 0.0;
    }
    match stroke_excess_px(segments, style, map, translate, p) {
        Some(excess) => aa_coverage(excess, f64::from(style.anti_alias_width)),
        None => 0.0,
    }
}

// ------------------------------------------------------------------- the joins

/// The Reference's **de facto** miter limit, in half-widths.
///
/// There is no `miter_limit` constant in the Reference to copy. G0-2 finding L6
/// derived it: `auto` crosses over at `cos θ = −0.8` (θ = 143.13°), so its worst
/// admissible extension is `|shift| = tan(71.565°) = 3.0`, i.e.
/// `√(1 + 9) = 3.1623` half-widths of miter length. That is the number §10.3 asked
/// this bead to adopt.
pub const MITER_LIMIT: f64 = 3.162_277_660_168_379_5;

/// One interior anchor's join geometry, in screen pixels.
///
/// The **outer** wedge at a corner: the region whose nearest point on the path is
/// the shared anchor itself. Everything a non-round join needs is here, and
/// nothing a round join needs is — a round join is what the distance field
/// already produces, so [`join_wedges`] returns nothing for it and the round path
/// is untouched by construction rather than by care.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JoinWedge {
    /// The shared anchor.
    pub anchor: [f64; 2],
    /// Unit tangent arriving at the anchor.
    pub t_in: [f64; 2],
    /// Unit tangent leaving it.
    pub t_out: [f64; 2],
    /// Half-width in pixels at this anchor.
    pub half_width: f64,
    /// Unit bisector pointing **into** the wedge.
    pub bisector: [f64; 2],
    /// Outward normal of the incoming segment's edge, pointing into the wedge.
    pub n_in: [f64; 2],
    /// Outward normal of the outgoing segment's edge.
    pub n_out: [f64; 2],
}

fn dot2(a: [f64; 2], b: [f64; 2]) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}

fn norm2(a: [f64; 2]) -> f64 {
    dot2(a, a).sqrt()
}

fn unit2(a: [f64; 2]) -> Option<[f64; 2]> {
    let n = norm2(a);
    if n <= 0.0 {
        None
    } else {
        Some([a[0] / n, a[1] / n])
    }
}

impl JoinWedge {
    /// Is this point in the outer wedge — i.e. is the anchor its nearest point on
    /// the path?
    ///
    /// Inclusive on both half-planes on purpose: the anchor itself must belong to
    /// its own wedge, or a pixel centre landing exactly on a corner would fall
    /// through every region.
    #[must_use]
    pub fn contains(&self, p: [f64; 2]) -> bool {
        let d = [p[0] - self.anchor[0], p[1] - self.anchor[1]];
        dot2(d, self.t_in) >= 0.0 && dot2(d, self.t_out) <= 0.0
    }

    /// The miter length in half-widths: `1 / cos φ`, with `φ` the half-angle
    /// between an edge normal and the bisector.
    ///
    /// `∞` for a reversal (a 180° cusp), where the two edges are antiparallel and
    /// no finite point is on both offset lines — which is precisely the case
    /// [`MITER_LIMIT`] exists to catch.
    #[must_use]
    pub fn miter_ratio(&self) -> f64 {
        let c = dot2(self.n_in, self.bisector);
        if c <= 0.0 { f64::INFINITY } else { 1.0 / c }
    }

    /// The bevel's signed excess: distance past the chord joining the two offset
    /// points.
    ///
    /// The chord sits at `half_width · cos φ` from the anchor along the bisector,
    /// because it passes through `anchor + half_width · n_in`.
    #[must_use]
    pub fn bevel_excess(&self, p: [f64; 2]) -> f64 {
        let d = [p[0] - self.anchor[0], p[1] - self.anchor[1]];
        dot2(d, self.bisector) - self.half_width * dot2(self.n_in, self.bisector)
    }

    /// The miter's signed excess: the intersection of the two offset half-planes.
    #[must_use]
    pub fn miter_excess(&self, p: [f64; 2]) -> f64 {
        let d = [p[0] - self.anchor[0], p[1] - self.anchor[1]];
        (dot2(d, self.n_in) - self.half_width).max(dot2(d, self.n_out) - self.half_width)
    }
}

/// The join wedges a path needs for the given joint type.
///
/// Empty for [`fmn_mobject::JointType::Auto`] and
/// [`fmn_mobject::JointType::NoJoint`], both of which render
/// the round join the distance field already produces (ADR-0012). Empty is not a
/// shortcut: it is what makes the round path bit-identical whether or not the
/// join machinery exists.
///
/// `subpath_starts` is [`crate::table::Shape::subpath_starts`], relative to this
/// slice. Joins are formed only *within* a subpath, and only between segments
/// that actually share an anchor — plus the wrap join of a closed subpath, which
/// is a corner like any other and would otherwise be the one place a closed
/// outline showed a round join under a miter setting.
#[must_use]
pub fn join_wedges(
    segments: &[Segment],
    subpath_starts: &[u32],
    style: &Style,
    map: ScreenMap,
    translate: [f64; 2],
) -> Vec<JoinWedge> {
    let mut out = Vec::new();
    let mut pairs = crate::arena::Pool::default();
    join_wedges_into(
        &mut out,
        &mut pairs,
        segments,
        subpath_starts,
        style,
        map,
        translate,
    );
    out
}

/// [`join_wedges`] into caller-owned storage — the engine's per-frame path.
///
/// `out` receives the wedges in exactly the order [`join_wedges`] produces
/// them; `pairs` is per-subpath corner-index scratch, cleared on every use,
/// so the arena's pooled copy allocates only until the widest subpath has
/// been seen. Identical arithmetic; only the destinations differ.
pub(crate) fn join_wedges_into(
    out: &mut impl crate::arena::Sink<JoinWedge>,
    pairs: &mut crate::arena::Pool<(usize, usize)>,
    segments: &[Segment],
    subpath_starts: &[u32],
    style: &Style,
    map: ScreenMap,
    translate: [f64; 2],
) {
    if matches!(
        style.joint_type,
        fmn_mobject::JointType::Auto | fmn_mobject::JointType::NoJoint
    ) {
        return;
    }
    let to_px = |p: fmn_core::types::Vec3| {
        [
            map.origin[0] + p[0] * map.scale + translate[0],
            map.origin[1] + p[1] * map.scale + translate[1],
        ]
    };
    // A quadratic's tangents are `2(p1 − p0)` and `2(p2 − p1)`; a coincident
    // handle leaves one of them zero, so the chord stands in.
    let tan_in = |g: &Segment| {
        let a = to_px(g.p1);
        let b = to_px(g.p2);
        unit2([b[0] - a[0], b[1] - a[1]]).or_else(|| {
            let z = to_px(g.p0);
            unit2([b[0] - z[0], b[1] - z[1]])
        })
    };
    let tan_out = |g: &Segment| {
        let a = to_px(g.p0);
        let b = to_px(g.p1);
        unit2([b[0] - a[0], b[1] - a[1]]).or_else(|| {
            let z = to_px(g.p2);
            unit2([z[0] - a[0], z[1] - a[1]])
        })
    };

    for (k, &start) in subpath_starts.iter().enumerate() {
        let end = subpath_starts
            .get(k + 1)
            .copied()
            .unwrap_or(segments.len() as u32);
        let (a, b) = (
            (start as usize).min(segments.len()),
            (end as usize).min(segments.len()),
        );
        if b <= a {
            continue;
        }
        let sub = &segments[a..b];
        // Interior corners, then the wrap corner if the subpath closes.
        pairs.clear();
        for i in 0..sub.len().saturating_sub(1) {
            pairs.put((i, i + 1));
        }
        let first = sub[0];
        let last = sub[sub.len() - 1];
        if sub.len() > 1 && to_px(last.p2) == to_px(first.p0) {
            pairs.put((sub.len() - 1, 0));
        }
        for &(i, j) in pairs.iter() {
            let (Some(t_in), Some(t_out)) = (tan_in(&sub[i]), tan_out(&sub[j])) else {
                continue;
            };
            // The wedge's bisector points along `t_in − t_out`. A smooth join
            // leaves that vector zero and has no wedge to draw.
            let Some(bisector) = unit2([t_in[0] - t_out[0], t_in[1] - t_out[1]]) else {
                continue;
            };
            let pick = |t: [f64; 2]| {
                let cand = [-t[1], t[0]];
                if dot2(cand, bisector) > 0.0 {
                    cand
                } else {
                    [t[1], -t[0]]
                }
            };
            out.put(JoinWedge {
                anchor: to_px(sub[i].p2),
                t_in,
                t_out,
                half_width: half_width_px(style, map, sub[i].s1),
                bisector,
                n_in: pick(t_in),
                n_out: pick(t_out),
            });
        }
    }
}

/// Apply the joint override to a round excess at one point.
///
/// The round join is the baseline and the override *edits* it, which is why this
/// takes an excess rather than producing one:
///
/// - **Bevel** trims. Its region is a subset of the round join's, so the edit is
///   `max` with the chord's half-plane — coverage can only be removed, and only
///   inside the wedge.
/// - **Miter** extends. Its region contains the round join's near the tip, so the
///   edit is `min` with the two offset half-planes. Over [`MITER_LIMIT`] it falls
///   back to the bevel, which is the classical behaviour and the one G0-2's
///   measured limit was derived for.
///
/// Which numeric code selects which corner is ADR-0012's ruling: **our names mean
/// what they say**, so `Bevel` cuts flat and `Miter` comes to a point. The
/// Reference's constants are swapped relative to the geometry its own shader
/// produces (G0-2 L6), and reproducing a misnamed constant is not compatibility.
#[must_use]
pub fn apply_joins(
    round_excess: f64,
    joins: &[JoinWedge],
    joint: fmn_mobject::JointType,
    p: [f64; 2],
) -> f64 {
    use fmn_mobject::JointType;
    if joins.is_empty() {
        return round_excess;
    }
    let mut excess = round_excess;
    for w in joins {
        if !w.contains(p) {
            continue;
        }
        match joint {
            JointType::Bevel => excess = excess.max(w.bevel_excess(p)),
            JointType::Miter => {
                if w.miter_ratio() > MITER_LIMIT {
                    excess = excess.max(w.bevel_excess(p));
                } else {
                    excess = excess.min(w.miter_excess(p));
                }
            }
            JointType::Auto | JointType::NoJoint => {}
        }
    }
    excess
}

/// The stroke's coverage at a screen point, with the joint override applied.
///
/// [`stroke_coverage`] is this with no joins, and for
/// [`JointType::Auto`](fmn_mobject::JointType::Auto) the two are the same number:
/// [`join_wedges`] returns nothing, so nothing is edited.
#[must_use]
pub fn stroke_coverage_with_joins(
    segments: &[Segment],
    joins: &[JoinWedge],
    style: &Style,
    map: ScreenMap,
    translate: [f64; 2],
    p: [f64; 2],
) -> f64 {
    stroke_shade(segments, joins, style, map, translate, p).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hint::Hint;
    use crate::table::{compile_shape, shape_digest};
    use fmn_core::types::Vec3;
    use fmn_geom::quadpath::QuadPath;

    fn unit() -> ScreenMap {
        ScreenMap {
            scale: 1.0,
            origin: [0.0, 0.0],
        }
    }

    fn segs_of(path: &QuadPath) -> Vec<Segment> {
        compile_shape(shape_digest(path.points()), path, Hint::General, 0).1
    }

    /// Own the prepared-segment backing for a [`PreparedStroke`] view.
    fn prepared(
        segments: &[Segment],
        style: &Style,
        map: ScreenMap,
        translate: [f64; 2],
        straight_segments: bool,
    ) -> (Vec<PreparedSegment>, [f64; 4]) {
        let mut backing = Vec::new();
        let slab = PreparedStroke::prepare_into(
            &mut backing,
            segments,
            style,
            map,
            translate,
            straight_segments,
        );
        (backing, slab)
    }

    /// A style whose stroke is `width` units wide, constant, with the calibrated
    /// AA band.
    fn flat_stroke_style(width: f32) -> Style {
        Style {
            stroke_width: width,
            stroke_width_end: width,
            stroke_rgba: [1.0, 1.0, 1.0, 1.0],
            stroke_rgba_end: [1.0, 1.0, 1.0, 1.0],
            anti_alias_width: 1.5,
            ..Style::default()
        }
    }

    /// A horizontal line from `(0,0)` to `(len,0)`, as one straight quadratic.
    fn line(len: f64) -> QuadPath {
        let mut p = QuadPath::default();
        p.start_new_path([0.0, 0.0, 0.0]);
        p.add_line_to([len, 0.0, 0.0], false).unwrap();
        p
    }

    #[test]
    fn the_line_route_is_bounded_and_rejects_curves_and_overshoots() {
        let segment = |p0, p1, p2| Segment {
            p0,
            p1,
            p2,
            s0: 0.0,
            s1: 1.0,
        };
        let map = ScreenMap {
            scale: 60.0,
            origin: [0.0, 0.0],
        };
        let quantized_line = segment(
            [0.0, 0.0, 0.0],
            [0.009_999_990_463_256_836, 0.199_999_928_474_426_27, 0.0],
            [0.019_999_980_926_513_672, 0.399_999_976_158_142_1, 0.0],
        );
        assert!(line_approximation_admitted(&quantized_line, map, false));
        assert!(
            line_approximation_admitted(
                &segment([0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [2.0, 0.0, 0.0],),
                map,
                true,
            ),
            "a valid semantic line hint is authoritative"
        );
        assert!(!line_approximation_admitted(
            &segment([0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [2.0, 0.0, 0.0],),
            map,
            false,
        ));
        assert!(!line_approximation_admitted(
            &segment([0.0, 0.0, 0.0], [3.0, 0.0, 0.0], [2.0, 0.0, 0.0],),
            map,
            false,
        ));
    }

    #[test]
    fn a_quantized_polyline_uses_capsule_distance() {
        let segments = [
            Segment {
                p0: [0.0, 0.0, 0.0],
                p1: [0.009_999_990_463_256_836, 0.199_999_928_474_426_27, 0.0],
                p2: [0.019_999_980_926_513_672, 0.399_999_976_158_142_1, 0.0],
                s0: 0.0,
                s1: 0.5,
            },
            Segment {
                p0: [0.019_999_980_926_513_672, 0.399_999_976_158_142_1, 0.0],
                p1: [0.029_999_971_389_770_508, 0.199_999_928_474_426_27, 0.0],
                p2: [0.040_000_021_457_672_12, 0.0, 0.0],
                s0: 0.5,
                s1: 0.95,
            },
        ];
        let map = ScreenMap {
            scale: 60.0,
            origin: [160.0, 90.0],
        };
        let style = flat_stroke_style(14.0);
        let translate = [40.8, -84.0];
        let (backing, slab) = prepared(&segments, &style, map, translate, true);
        let prepared = PreparedStroke::from_parts(&backing, slab);
        let (coverage, _) = prepared.shade(&segments, &[], &style, map, translate, [206.5, 10.5]);
        assert!(coverage > 0.99, "{coverage}");
    }

    #[test]
    fn the_width_conversion_is_the_references_own() {
        let map = ScreenMap {
            scale: 135.0,
            origin: [0.0, 0.0],
        };
        assert!((width_px(1.0, map) - 1.35).abs() < 1e-12);
        assert!(
            (width_px(4.0, map) - 5.4).abs() < 1e-12,
            "DEFAULT_STROKE_WIDTH"
        );
        assert_eq!(width_px(0.0, map), 0.0);
        // And it is the same function §10.2's inner border uses.
        assert_eq!(width_px(0.5, map), crate::fill::border_width_px(0.5, map));
    }

    #[test]
    fn the_aa_profile_is_the_measured_smoothstep() {
        // G0-2 L1, pinned at the points that identify the curve rather than
        // merely sample it.
        let aa = 1.5;
        assert_eq!(aa_coverage(-aa, aa), 1.0, "well inside");
        assert!(
            (aa_coverage(0.0, aa) - 0.5).abs() < 1e-12,
            "on the silhouette"
        );
        assert_eq!(aa_coverage(aa, aa), 0.0, "beyond the band");
        // Hermite, not linear: at a quarter of the band the linear answer would
        // be 0.75 and the Hermite is 0.84375.
        let quarter = aa_coverage(-0.25 * aa, aa);
        assert!((quarter - 0.843_75).abs() < 1e-12, "{quarter}");
        // Monotone, and symmetric about the silhouette.
        let mut last = 1.0;
        for k in 0..=20 {
            let e = -aa + 2.0 * aa * f64::from(k) / 20.0;
            let c = aa_coverage(e, aa);
            assert!(c <= last + 1e-12, "not monotone at {e}");
            last = c;
        }
        for k in 1..=10 {
            let e = aa * f64::from(k) / 20.0;
            assert!(
                (aa_coverage(-e, aa) + aa_coverage(e, aa) - 1.0).abs() < 1e-12,
                "not symmetric at {e}"
            );
        }
    }

    #[test]
    fn a_straight_stroke_has_the_width_it_was_asked_for() {
        // The simplest silhouette check: a 100-unit-wide stroke at scale 1 is
        // 1 px of half-width, so coverage is 1/2 exactly one pixel off the axis.
        let path = line(40.0);
        let segs = segs_of(&path);
        let style = flat_stroke_style(200.0); // 2 px wide, 1 px half-width
        let map = unit();
        assert!((half_width_px(&style, map, 0.0) - 1.0).abs() < 1e-12);

        // On the axis: fully covered.
        assert_eq!(
            stroke_coverage(&segs, &style, map, [0.0, 0.0], [20.0, 0.0]),
            1.0
        );
        // On the silhouette: half.
        let edge = stroke_coverage(&segs, &style, map, [0.0, 0.0], [20.0, 1.0]);
        assert!((edge - 0.5).abs() < 1e-9, "{edge}");
        // Beyond the band: nothing.
        assert_eq!(
            stroke_coverage(&segs, &style, map, [0.0, 0.0], [20.0, 1.0 + 1.5]),
            0.0
        );
    }

    #[test]
    fn open_ends_are_round_caps_and_not_butt_caps() {
        // The distance to an open end is radial, so the level set is a semicircle
        // with nobody asking for one. A butt cap would read zero coverage
        // immediately past the endpoint; a round cap reads the same value at the
        // same distance in every direction.
        let path = line(40.0);
        let segs = segs_of(&path);
        let style = flat_stroke_style(400.0); // 4 px wide, 2 px half-width
        let map = unit();
        let end = [40.0, 0.0];

        // Straight out past the end, at half the half-width: covered.
        assert_eq!(
            stroke_coverage(&segs, &style, map, [0.0, 0.0], [end[0] + 1.0, end[1]]),
            1.0,
            "a butt cap would be empty here"
        );
        // The cap is a circle: every direction at radius 2 sits on the silhouette.
        for k in 0..12 {
            let a = std::f64::consts::TAU * f64::from(k) / 12.0;
            // Only the outward half is the cap; the inward half is the stroke body.
            if a.cos() <= 0.0 {
                continue;
            }
            let p = [end[0] + 2.0 * a.cos(), end[1] + 2.0 * a.sin()];
            let c = stroke_coverage(&segs, &style, map, [0.0, 0.0], p);
            assert!(
                (c - 0.5).abs() < 1e-9,
                "cap is not circular at angle {}: {c}",
                a.to_degrees()
            );
        }
    }

    #[test]
    fn a_corner_is_a_round_join_with_no_notch_and_no_gap() {
        // G0-2 L6: two segments sharing an anchor contribute two distance fields
        // whose minimum is exactly a round join. The failure modes this excludes
        // are a NOTCH (coverage dipping at the outer corner) and a GAP (coverage
        // dipping anywhere on the join arc).
        let mut p = QuadPath::default();
        p.start_new_path([0.0, 20.0, 0.0]);
        p.add_line_to([0.0, 0.0, 0.0], false).unwrap();
        p.add_line_to([20.0, 0.0, 0.0], false).unwrap();
        let segs = segs_of(&p);
        let style = flat_stroke_style(600.0); // 6 px wide, 3 px half-width
        let map = unit();

        // The outer corner is the third quadrant from the vertex. Sweep the join
        // arc at 3 px: every sample must sit on the silhouette, so the outer
        // boundary is a circular arc rather than a mitre or a cut.
        for k in 0..=16 {
            let a = std::f64::consts::PI + std::f64::consts::FRAC_PI_2 * f64::from(k) / 16.0;
            let q = [3.0 * a.cos(), 3.0 * a.sin()];
            let c = stroke_coverage(&segs, &style, map, [0.0, 0.0], q);
            assert!(
                (c - 0.5).abs() < 1e-9,
                "join is not round at {} deg: {c}",
                a.to_degrees()
            );
        }
        // And just inside that arc there is no gap.
        for k in 0..=16 {
            let a = std::f64::consts::PI + std::f64::consts::FRAC_PI_2 * f64::from(k) / 16.0;
            let q = [2.0 * a.cos(), 2.0 * a.sin()];
            assert_eq!(
                stroke_coverage(&segs, &style, map, [0.0, 0.0], q),
                1.0,
                "gap in the join at {} deg",
                a.to_degrees()
            );
        }
    }

    #[test]
    fn reparameterizing_a_curve_leaves_the_stroke_identical() {
        // §10.3's metamorphic law, and the reason width interpolates by ARC
        // LENGTH. A de Casteljau split changes the point array and the
        // parameterization; it does not change the curve, so it must not change
        // one pixel of the stroke. A parameter-space width ramp fails this.
        let mut path = QuadPath::default();
        path.start_new_path([0.0, 0.0, 0.0]);
        path.add_quadratic_bezier_curve_to([15.0, 25.0, 0.0], [30.0, 0.0, 0.0], false)
            .unwrap();

        let mut split = QuadPath::default();
        for i in 0..path.num_curves() {
            let [p0, p1, p2] = path.nth_curve_points(i).unwrap();
            let mid = |a: Vec3, b: Vec3| [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0, 0.0];
            let q0 = mid(p0, p1);
            let q1 = mid(p1, p2);
            let r = mid(q0, q1);
            if i == 0 {
                split.start_new_path(p0);
            }
            split.add_quadratic_bezier_curve_to(q0, r, false).unwrap();
            split.add_quadratic_bezier_curve_to(q1, p2, false).unwrap();
        }

        let a = segs_of(&path);
        let b = segs_of(&split);
        assert!(b.len() > a.len(), "the subdivision must add segments");

        // A steep width ramp, so a parameter-space interpolation would be
        // visibly different rather than subtly.
        let style = Style {
            stroke_width: 100.0,
            stroke_width_end: 800.0,
            anti_alias_width: 1.5,
            ..Style::default()
        };
        let map = unit();
        let mut worst = 0.0f64;
        for y in -6..30 {
            for x in -6..36 {
                let p = [f64::from(x) + 0.5, f64::from(y) + 0.5];
                let ca = stroke_coverage(&a, &style, map, [0.0, 0.0], p);
                let cb = stroke_coverage(&b, &style, map, [0.0, 0.0], p);
                worst = worst.max((ca - cb).abs());
            }
        }
        assert!(
            worst < 1e-6,
            "stroke drift under reparameterization: {worst}"
        );
    }

    #[test]
    fn the_width_ramp_is_taken_per_segment_before_the_minimum() {
        // The ordering claim in `stroke_excess_px`. On a path whose width ramps
        // steeply, a pixel near the seam between two segments is nearest to one of
        // them, and the silhouette there must use THAT segment's width. Computing
        // min(distance) first and then looking up a width would use the wrong
        // one.
        let mut p = QuadPath::default();
        p.start_new_path([0.0, 0.0, 0.0]);
        p.add_line_to([20.0, 0.0, 0.0], false).unwrap();
        p.add_line_to([40.0, 0.0, 0.0], false).unwrap();
        let segs = segs_of(&p);
        assert_eq!(segs.len(), 2);
        let style = Style {
            stroke_width: 200.0,     // 2 px wide at s=0 -> half-width 1
            stroke_width_end: 800.0, // 8 px wide at s=1 -> half-width 4
            anti_alias_width: 1.5,
            ..Style::default()
        };
        let map = unit();
        // At the start the half-width is 1; at the end it is 4; at the midpoint
        // (the shared anchor, s = 1/2) it is 2.5. Each is checked on its own
        // silhouette.
        for (x, hw) in [(0.0f64, 1.0f64), (20.0, 2.5), (40.0, 4.0)] {
            let c = stroke_coverage(&segs, &style, map, [0.0, 0.0], [x, hw]);
            assert!(
                (c - 0.5).abs() < 0.05,
                "at x={x} the silhouette should sit at y={hw}, got coverage {c}"
            );
        }
    }

    #[test]
    fn the_colour_ramp_is_linear_in_arc_length() {
        let style = Style {
            stroke_rgba: [0.0, 0.25, 1.0, 0.5],
            stroke_rgba_end: [1.0, 0.75, 0.0, 1.0],
            ..Style::default()
        };
        assert_eq!(stroke_rgba_at(&style, 0.0), style.stroke_rgba);
        assert_eq!(stroke_rgba_at(&style, 1.0), style.stroke_rgba_end);
        let mid = stroke_rgba_at(&style, 0.5);
        for (k, ((got, a), b)) in mid
            .iter()
            .zip(&style.stroke_rgba)
            .zip(&style.stroke_rgba_end)
            .enumerate()
        {
            assert!((got - 0.5 * (a + b)).abs() < 1e-6, "channel {k}");
        }
        // Clamped, not extrapolated.
        assert_eq!(stroke_rgba_at(&style, -2.0), style.stroke_rgba);
        assert_eq!(stroke_rgba_at(&style, 3.0), style.stroke_rgba_end);
    }

    #[test]
    fn the_slab_contains_the_stroke_it_bounds() {
        // Conservative means conservative: every pixel with any coverage must be
        // inside the slab, or the rejection would clip a stroke. Checked by
        // sweeping a window larger than the slab.
        let mut p = QuadPath::default();
        p.start_new_path([2.0, 3.0, 0.0]);
        p.add_quadratic_bezier_curve_to([10.0, 18.0, 0.0], [24.0, 5.0, 0.0], false)
            .unwrap();
        let segs = segs_of(&p);
        let style = flat_stroke_style(500.0); // 5 px wide
        let map = ScreenMap {
            scale: 2.0,
            origin: [5.0, 7.0],
        };
        let translate = [11.0, -3.0];
        let slab = segment_slab(&segs[0], &style, map, translate);
        for y in -30..90 {
            for x in -30..90 {
                let q = [f64::from(x) + 0.5, f64::from(y) + 0.5];
                if stroke_coverage(&segs, &style, map, translate, q) <= 0.0 {
                    continue;
                }
                assert!(
                    q[0] >= slab[0] && q[0] <= slab[2] && q[1] >= slab[1] && q[1] <= slab[3],
                    "covered pixel {q:?} outside slab {slab:?}"
                );
            }
        }
    }

    #[test]
    fn the_miter_tip_is_inside_the_conservative_slab() {
        // A shallow V whose admitted miter reaches more than one half-width
        // beyond the hull along y. Growing only by the round body clips this
        // point in both the binner and the engine's explicit slab reject.
        let mut path = QuadPath::default();
        path.start_new_path([-20.0, 40.0, 0.0]);
        path.add_line_to([0.0, 0.0, 0.0], false).unwrap();
        path.add_line_to([20.0, 40.0, 0.0], false).unwrap();
        let (shape, segments) = compile_shape(shape_digest(path.points()), &path, Hint::General, 0);
        let style = Style {
            joint_type: JointType::Miter,
            ..flat_stroke_style(2000.0)
        };
        let joins = join_wedges(&segments, &shape.subpath_starts, &style, unit(), [0.0, 0.0]);
        assert_eq!(joins.len(), 1);
        let join = joins[0];
        assert!(
            (1.0..MITER_LIMIT).contains(&join.miter_ratio()),
            "fixture must exercise an admitted extension: {}",
            join.miter_ratio()
        );
        let probe_distance = 0.9 * join.miter_ratio() * join.half_width;
        let probe = [
            join.anchor[0] + probe_distance * join.bisector[0],
            join.anchor[1] + probe_distance * join.bisector[1],
        ];
        assert_eq!(
            stroke_coverage_with_joins(&segments, &joins, &style, unit(), [0.0, 0.0], probe),
            1.0,
            "fixture probe must be inside the miter"
        );
        let round_pad = join.half_width + f64::from(style.anti_alias_width);
        assert!(
            probe[1] < -round_pad,
            "the probe must expose the old half-width-only slab"
        );

        let slab = segments
            .iter()
            .map(|segment| segment_slab(segment, &style, unit(), [0.0, 0.0]))
            .fold(
                [
                    f64::INFINITY,
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                    f64::NEG_INFINITY,
                ],
                |mut union, segment| {
                    union[0] = union[0].min(segment[0]);
                    union[1] = union[1].min(segment[1]);
                    union[2] = union[2].max(segment[2]);
                    union[3] = union[3].max(segment[3]);
                    union
                },
            );
        assert!(
            probe[0] >= slab[0]
                && probe[0] <= slab[2]
                && probe[1] >= slab[1]
                && probe[1] <= slab[3],
            "covered miter point {probe:?} outside slab {slab:?}"
        );
    }

    #[test]
    fn prepared_stroke_rejection_is_bit_exact_where_coverage_can_change() {
        let mut path = QuadPath::default();
        path.start_new_path([-28.0, -3.0, 0.0]);
        path.add_quadratic_bezier_curve_to([-20.0, 24.0, 0.0], [-10.0, 1.0, 0.0], false)
            .unwrap();
        path.add_quadratic_bezier_curve_to([0.0, -22.0, 0.0], [11.0, 4.0, 0.0], false)
            .unwrap();
        path.add_quadratic_bezier_curve_to([26.0, 4.0, 0.0], [18.0, -2.0, 0.0], false)
            .unwrap();
        path.add_quadratic_bezier_curve_to([24.0, -2.0 + 1e-12, 0.0], [30.0, -2.0, 0.0], false)
            .unwrap();
        let (shape, segments) = compile_shape(shape_digest(path.points()), &path, Hint::General, 0);
        let translate = [13.0, -9.0];

        for map in [
            ScreenMap {
                scale: 1.75,
                origin: [7.0, -4.0],
            },
            ScreenMap {
                scale: -1.75,
                origin: [7.0, -4.0],
            },
        ] {
            for joint_type in [JointType::Auto, JointType::Bevel, JointType::Miter] {
                let style = Style {
                    stroke_width: 300.0,
                    stroke_width_end: 900.0,
                    joint_type,
                    ..flat_stroke_style(300.0)
                };
                let joins = join_wedges(&segments, &shape.subpath_starts, &style, map, translate);
                let (backing, slab) = prepared(&segments, &style, map, translate, false);
                let prepared = PreparedStroke::from_parts(&backing, slab);
                for y in -80..80 {
                    for x in -80..120 {
                        let point = [f64::from(x) + 0.25, f64::from(y) + 0.75];
                        let scalar = stroke_shade(&segments, &joins, &style, map, translate, point);
                        let culled =
                            prepared.shade(&segments, &joins, &style, map, translate, point);
                        assert_eq!(
                            culled.0.to_bits(),
                            scalar.0.to_bits(),
                            "{joint_type:?} scale {} coverage at {point:?}",
                            map.scale
                        );
                        if scalar.0 > 0.0 {
                            assert_eq!(
                                culled.1.to_bits(),
                                scalar.1.to_bits(),
                                "{joint_type:?} scale {} ramp coordinate at {point:?}",
                                map.scale
                            );
                        }
                    }
                }

                let first_slab = prepared.segments[0].slab;
                let near_first = [
                    0.5 * (first_slab[0] + first_slab[2]),
                    0.5 * (first_slab[1] + first_slab[3]),
                ];
                assert!(
                    prepared
                        .segments
                        .iter()
                        .filter(|segment| {
                            near_first[0] >= segment.slab[0]
                                && near_first[0] <= segment.slab[2]
                                && near_first[1] >= segment.slab[1]
                                && near_first[1] <= segment.slab[3]
                        })
                        .count()
                        < segments.len(),
                    "the corpus must exercise a rejected segment"
                );
            }
        }
    }

    #[test]
    fn the_degenerate_corpus_produces_no_stroke_rather_than_a_panic() {
        let map = unit();
        let style = flat_stroke_style(200.0);
        // No segments.
        assert_eq!(
            stroke_excess_px(&[], &style, map, [0.0, 0.0], [1.0, 1.0]),
            None
        );
        assert_eq!(
            stroke_coverage(&[], &style, map, [0.0, 0.0], [1.0, 1.0]),
            0.0
        );
        let (backing, slab) = prepared(&[], &style, map, [0.0, 0.0], false);
        assert_eq!(
            PreparedStroke::from_parts(&backing, slab).shade(
                &[],
                &[],
                &style,
                map,
                [0.0, 0.0],
                [1.0, 1.0]
            ),
            (0.0, 0.0)
        );
        // A degenerate map has no pixels to measure in.
        let path = line(10.0);
        let segs = segs_of(&path);
        let degenerate_map = ScreenMap {
            scale: 0.0,
            origin: [0.0, 0.0],
        };
        assert_eq!(
            stroke_excess_px(&segs, &style, degenerate_map, [0.0, 0.0], [1.0, 1.0]),
            None
        );
        let (backing, slab) = prepared(&segs, &style, degenerate_map, [0.0, 0.0], false);
        assert_eq!(
            PreparedStroke::from_parts(&backing, slab).shade(
                &segs,
                &[],
                &style,
                degenerate_map,
                [0.0, 0.0],
                [1.0, 1.0]
            ),
            (0.0, 0.0)
        );
        // A zero width draws nothing at all, not a hairline from the AA band.
        let zero = Style {
            stroke_width: 0.0,
            stroke_width_end: 0.0,
            anti_alias_width: 1.5,
            ..Style::default()
        };
        assert_eq!(
            stroke_coverage(&segs, &zero, map, [0.0, 0.0], [5.0, 0.0]),
            0.0
        );
        // A cusp: the handle beyond an endpoint, so the distance solve's roots
        // collide. It must still produce a finite, in-range coverage.
        let mut cusp = QuadPath::default();
        cusp.start_new_path([0.0, 0.0, 0.0]);
        cusp.add_quadratic_bezier_curve_to([20.0, 0.0, 0.0], [0.0, 0.0, 0.0], true)
            .unwrap();
        let cusp_segs = segs_of(&cusp);
        let (cusp_backing, cusp_slab) = prepared(&cusp_segs, &style, map, [0.0, 0.0], false);
        let prepared_cusp = PreparedStroke::from_parts(&cusp_backing, cusp_slab);
        for x in 0..12 {
            let point = [f64::from(x), 0.5];
            let c = stroke_coverage(&cusp_segs, &style, map, [0.0, 0.0], point);
            assert!(
                (0.0..=1.0).contains(&c) && c.is_finite(),
                "cusp at x={x}: {c}"
            );
            assert_eq!(
                prepared_cusp
                    .shade(&cusp_segs, &[], &style, map, [0.0, 0.0], point)
                    .0
                    .to_bits(),
                c.to_bits(),
                "prepared cusp at x={x}"
            );
        }
    }

    // ------------------------------------------------------------- the joins

    use fmn_mobject::JointType;

    /// A right-angle corner: in along +x to the origin, out along +y.
    fn corner() -> (Vec<Segment>, Vec<u32>) {
        let mut p = QuadPath::default();
        p.start_new_path([-20.0, 0.0, 0.0]);
        p.add_line_to([0.0, 0.0, 0.0], false).unwrap();
        p.add_line_to([0.0, 20.0, 0.0], false).unwrap();
        let (shape, segs) = compile_shape(shape_digest(p.points()), &p, Hint::General, 0);
        (segs, shape.subpath_starts)
    }

    #[test]
    fn round_and_no_joint_produce_no_wedges_at_all() {
        // What makes the round path bit-identical whether or not the join
        // machinery exists: for the two round settings there is nothing to edit.
        let (segs, starts) = corner();
        for joint in [JointType::Auto, JointType::NoJoint] {
            let style = Style {
                joint_type: joint,
                ..flat_stroke_style(600.0)
            };
            let joins = join_wedges(&segs, &starts, &style, unit(), [0.0, 0.0]);
            assert!(joins.is_empty(), "{joint:?} must need no wedge");
            // And the coverage is identical to the join-free path, pixel for
            // pixel, not merely close.
            for y in -8..8 {
                for x in -8..8 {
                    let p = [f64::from(x) + 0.5, f64::from(y) + 0.5];
                    assert_eq!(
                        stroke_coverage_with_joins(&segs, &joins, &style, unit(), [0.0, 0.0], p),
                        stroke_coverage(&segs, &style, unit(), [0.0, 0.0], p),
                        "{joint:?} at {p:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_wedge_geometry_is_the_corner_it_describes() {
        // The right-angle corner, checked against hand arithmetic: the wedge is
        // the fourth quadrant, the bisector points into it, the offset normals are
        // the two edges' outward normals, and the miter ratio is sec(45 deg).
        let (segs, starts) = corner();
        let style = Style {
            joint_type: JointType::Miter,
            ..flat_stroke_style(600.0) // 6 px wide -> 3 px half-width
        };
        let joins = join_wedges(&segs, &starts, &style, unit(), [0.0, 0.0]);
        assert_eq!(joins.len(), 1, "one interior corner");
        let w = joins[0];
        assert_eq!(w.anchor, [0.0, 0.0]);
        assert!((w.half_width - 3.0).abs() < 1e-12);
        assert!((w.t_in[0] - 1.0).abs() < 1e-12 && w.t_in[1].abs() < 1e-12);
        assert!(w.t_out[0].abs() < 1e-12 && (w.t_out[1] - 1.0).abs() < 1e-12);
        let r = 0.5f64.sqrt();
        assert!((w.bisector[0] - r).abs() < 1e-12 && (w.bisector[1] + r).abs() < 1e-12);
        assert!(w.n_in[0].abs() < 1e-12 && (w.n_in[1] + 1.0).abs() < 1e-12);
        assert!((w.n_out[0] - 1.0).abs() < 1e-12 && w.n_out[1].abs() < 1e-12);
        assert!(
            (w.miter_ratio() - std::f64::consts::SQRT_2).abs() < 1e-12,
            "sec(45 deg): {}",
            w.miter_ratio()
        );

        // The wedge is the outer quadrant and nothing else.
        assert!(w.contains([1.0, -1.0]), "outer");
        assert!(!w.contains([-1.0, 1.0]), "inner");
        assert!(!w.contains([1.0, 1.0]), "ahead of the outgoing segment");
        assert!(!w.contains([-1.0, -1.0]), "behind the incoming one");
        assert!(
            w.contains([0.0, 0.0]),
            "the anchor belongs to its own wedge"
        );
    }

    #[test]
    fn bevel_trims_the_round_corner_and_miter_extends_it() {
        // The two edits, at the one point that tells them apart: the corner tip,
        // straight out along the bisector.
        // A 20 px stroke, so the three silhouettes — bevel chord at 0.707 hw,
        // round arc at 1.0 hw, miter tip at 1.414 hw — are each further apart than
        // the 1.5 px antialiasing band. At hw = 3 they overlap and every probe
        // reads a blend of two joins, which is how this test first failed.
        let (segs, starts) = corner();
        let hw = 10.0;
        let mk = |joint| Style {
            joint_type: joint,
            ..flat_stroke_style(2000.0)
        };
        let map = unit();
        let tip = |k: f64| {
            let r = 0.5f64.sqrt();
            [k * hw * r, -k * hw * r]
        };

        let round_style = mk(JointType::Auto);
        let round_joins = join_wedges(&segs, &starts, &round_style, map, [0.0, 0.0]);
        let bevel_style = mk(JointType::Bevel);
        let bevel_joins = join_wedges(&segs, &starts, &bevel_style, map, [0.0, 0.0]);
        let miter_style = mk(JointType::Miter);
        let miter_joins = join_wedges(&segs, &starts, &miter_style, map, [0.0, 0.0]);
        assert_eq!(bevel_joins.len(), 1);
        assert_eq!(miter_joins.len(), 1);

        let cov = |style: &Style, joins: &[JoinWedge], p| {
            stroke_coverage_with_joins(&segs, joins, style, map, [0.0, 0.0], p)
        };

        // Half a half-width in: inside all three, so the joins agree where they
        // are supposed to.
        let deep = tip(0.5);
        assert_eq!(cov(&round_style, &round_joins, deep), 1.0);
        assert_eq!(cov(&bevel_style, &bevel_joins, deep), 1.0);
        assert_eq!(cov(&miter_style, &miter_joins, deep), 1.0);

        // At 0.9 of a half-width: inside the round join and the miter, and OUTSIDE
        // the bevel, whose chord sits at hw*cos(45 deg) = 0.707 hw. That is the
        // trim.
        let inner = tip(0.9);
        assert_eq!(cov(&round_style, &round_joins, inner), 1.0);
        assert_eq!(cov(&miter_style, &miter_joins, inner), 1.0);
        assert_eq!(
            cov(&bevel_style, &bevel_joins, inner),
            0.0,
            "bevel must have trimmed the round corner"
        );

        // At 1.2 half-widths the round join has ended and the miter has not: the
        // miter tip reaches sqrt(2) = 1.414 hw. That is the extension.
        let outer = tip(1.2);
        assert_eq!(cov(&round_style, &round_joins, outer), 0.0);
        assert_eq!(cov(&bevel_style, &bevel_joins, outer), 0.0);
        assert_eq!(
            cov(&miter_style, &miter_joins, outer),
            1.0,
            "the miter must reach past the round join"
        );

        // The miter's own tip is its silhouette.
        let point = tip(std::f64::consts::SQRT_2);
        let at_tip = cov(&miter_style, &miter_joins, point);
        assert!((at_tip - 0.5).abs() < 1e-9, "miter tip coverage {at_tip}");
        assert_eq!(
            cov(&miter_style, &miter_joins, tip(1.9)),
            0.0,
            "and past it"
        );
    }

    #[test]
    fn a_join_edit_never_leaves_its_own_wedge() {
        // The containment guarantee. Away from the corner all three joint types
        // must agree pixel for pixel, or a joint setting would be changing the
        // stroke's body.
        let (segs, starts) = corner();
        let map = unit();
        let styles: Vec<Style> = [JointType::Auto, JointType::Bevel, JointType::Miter]
            .into_iter()
            .map(|joint| Style {
                joint_type: joint,
                ..flat_stroke_style(600.0)
            })
            .collect();
        let joins: Vec<Vec<JoinWedge>> = styles
            .iter()
            .map(|s| join_wedges(&segs, &starts, s, map, [0.0, 0.0]))
            .collect();
        for y in -24..24 {
            for x in -24..24 {
                let p = [f64::from(x) + 0.5, f64::from(y) + 0.5];
                // Outside the outer quadrant of the corner, nothing may differ.
                if joins[1][0].contains(p) {
                    continue;
                }
                let base =
                    stroke_coverage_with_joins(&segs, &joins[0], &styles[0], map, [0.0, 0.0], p);
                for k in 1..3 {
                    assert_eq!(
                        stroke_coverage_with_joins(
                            &segs,
                            &joins[k],
                            &styles[k],
                            map,
                            [0.0, 0.0],
                            p
                        ),
                        base,
                        "{:?} changed the body at {p:?}",
                        styles[k].joint_type
                    );
                }
            }
        }
    }

    #[test]
    fn a_reversal_exceeds_the_miter_limit_and_falls_back_to_bevel() {
        // A 180-degree turn has no finite miter point, so the limit is not a
        // tuning parameter here — it is the only thing standing between the
        // request and a spike of infinite length.
        let mut p = QuadPath::default();
        p.start_new_path([-20.0, 0.0, 0.0]);
        p.add_line_to([0.0, 0.0, 0.0], false).unwrap();
        p.add_line_to([-20.0, 0.0, 0.0], true).unwrap();
        let (shape, segs) = compile_shape(shape_digest(p.points()), &p, Hint::General, 0);
        let map = unit();
        let style = Style {
            joint_type: JointType::Miter,
            ..flat_stroke_style(600.0)
        };
        let joins = join_wedges(&segs, &shape.subpath_starts, &style, map, [0.0, 0.0]);
        if let Some(w) = joins.first() {
            assert!(
                w.miter_ratio() > MITER_LIMIT,
                "a reversal must exceed the limit: {}",
                w.miter_ratio()
            );
        }
        // Whatever the degenerate geometry, nothing may extend past the limit.
        for k in 1..=12 {
            let x = 3.0 * f64::from(k) / 4.0;
            let c = stroke_coverage_with_joins(&segs, &joins, &style, map, [0.0, 0.0], [x, 0.0]);
            assert!((0.0..=1.0).contains(&c) && c.is_finite());
            if x > MITER_LIMIT * 3.0 + 1.5 {
                assert_eq!(c, 0.0, "the limit did not hold at x={x}");
            }
        }
    }

    #[test]
    fn the_miter_limit_is_the_measured_one() {
        // G0-2 L6's derivation, reproduced from both ends.
        //
        // The Reference's `auto` join crosses over at `cos_angle = -0.8`, where
        // `cos_angle` is the cosine of the angle between the two TANGENTS. The
        // interior angle at the vertex is its supplement, so `cos psi = +0.8`, and
        // the classical miter length is `1 / sin(psi/2)`:
        //   psi = acos(0.8) = 36.87 deg, sin(psi/2) = 0.31623, 1/that = 3.16228.
        // The shader's own arithmetic agrees by a different route: the worst
        // admissible |shift| is tan(psi_turn/2) = tan(71.565 deg) = 3 half-widths
        // along the tangent, one half-width off it, so the tip is sqrt(1+9) away.
        assert!((MITER_LIMIT - 10.0f64.sqrt()).abs() < 1e-15);
        let psi = 0.8f64.acos();
        let predicted = 1.0 / (psi / 2.0).sin();
        assert!(
            (MITER_LIMIT - predicted).abs() < 1e-9,
            "{MITER_LIMIT} vs {predicted}"
        );
        // And it is the ratio this module's own wedge reports for that angle: a
        // corner whose tangents meet at cos = -0.8 must sit exactly at the limit.
        let turn = (-0.8f64).acos();
        let mut p = QuadPath::default();
        p.start_new_path([-20.0, 0.0, 0.0]);
        p.add_line_to([0.0, 0.0, 0.0], false).unwrap();
        p.add_line_to([20.0 * turn.cos(), 20.0 * turn.sin(), 0.0], false)
            .unwrap();
        let (shape, segs) = compile_shape(shape_digest(p.points()), &p, Hint::General, 0);
        let style = Style {
            joint_type: JointType::Miter,
            ..flat_stroke_style(600.0)
        };
        let joins = join_wedges(&segs, &shape.subpath_starts, &style, unit(), [0.0, 0.0]);
        assert_eq!(joins.len(), 1);
        assert!(
            (joins[0].miter_ratio() - MITER_LIMIT).abs() < 1e-9,
            "the wedge reports {} at the crossover angle",
            joins[0].miter_ratio()
        );
    }

    #[test]
    fn a_smooth_join_needs_no_wedge_and_a_closed_path_gets_its_wrap_corner() {
        // Two segments meeting tangentially have no corner to draw, and a wedge
        // there would be a zero-area region with an ill-defined bisector.
        let mut smooth = QuadPath::default();
        smooth.start_new_path([0.0, 0.0, 0.0]);
        smooth.add_line_to([10.0, 0.0, 0.0], false).unwrap();
        smooth.add_line_to([20.0, 0.0, 0.0], false).unwrap();
        let (sh, sg) = compile_shape(shape_digest(smooth.points()), &smooth, Hint::General, 0);
        let style = Style {
            joint_type: JointType::Miter,
            ..flat_stroke_style(400.0)
        };
        assert!(
            join_wedges(&sg, &sh.subpath_starts, &style, unit(), [0.0, 0.0]).is_empty(),
            "a tangential join has no wedge"
        );

        // A closed triangle has three corners, and the third is the wrap — the one
        // place a closed outline would otherwise keep a round join under a miter
        // setting.
        let mut tri = QuadPath::default();
        tri.start_new_path([0.0, 0.0, 0.0]);
        tri.add_line_to([20.0, 0.0, 0.0], false).unwrap();
        tri.add_line_to([10.0, 16.0, 0.0], false).unwrap();
        tri.add_line_to([0.0, 0.0, 0.0], false).unwrap();
        let (sh, sg) = compile_shape(shape_digest(tri.points()), &tri, Hint::General, 0);
        assert_eq!(sg.len(), 3);
        assert_eq!(
            join_wedges(&sg, &sh.subpath_starts, &style, unit(), [0.0, 0.0]).len(),
            3,
            "two interior corners plus the wrap"
        );
    }

    #[test]
    fn a_stroke_follows_its_occurrence() {
        let path = line(20.0);
        let segs = segs_of(&path);
        let style = flat_stroke_style(300.0);
        let map = ScreenMap {
            scale: 3.0,
            origin: [4.0, 9.0],
        };
        let here = stroke_coverage(&segs, &style, map, [0.0, 0.0], [4.0 + 30.0, 9.0]);
        let there = stroke_coverage(&segs, &style, map, [60.0, -21.0], [4.0 + 90.0, -12.0]);
        assert_eq!(here, there);
        assert_eq!(here, 1.0, "on the axis, so fully covered");
    }
}
