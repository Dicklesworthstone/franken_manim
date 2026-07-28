//! The compiled render IR's tables (§10.8), and the interning that makes them
//! smaller than the scene.
//!
//! > Between Marionette's authoritative state and the engines sits a compiled,
//! > backend-neutral render IR — `PathTable`, `SegmentTable`, `StyleTable`,
//! > `InstanceTable`, `ImageTable`, and per-tile command lists — synchronized
//! > *lazily* from the RecordBuffer under §8.2's mirror rule, with
//! > backend-specific layouts derived from it.
//!
//! ## Backend-neutral means what it says
//!
//! Everything here is host structs in the semantic width (`f64`, §6.1). The flat
//! single-typed arrays a GPU wants are a **derived layout**, one derivation per
//! backend, not this representation reshaped — G0-8's finding F1, confirmed
//! against a real Metal pipeline before this table existed. Two consequences the
//! spike measured and this module honours:
//!
//! - **F1**: a device buffer binding is a typed pointer, so a derived layout is
//!   one flat array per table, each array a single scalar type.
//! - **F12**: fills and strokes read *different* style fields, so the device
//!   style row is per-stage. The interned table and its dedup index are shared;
//!   the layout is not.
//!
//! ## Interning is a semantics, not an optimization
//!
//! [`StyleTable`] dedups **by exact bits**, matching
//! `fmn_mobject::order::BatchKey`. §8.5 makes batching semantics — how many draw
//! calls a scene takes is observable — so a table that merged `0.1 + 0.2` with
//! `0.3` would quietly change a number the Parity Ledger has to explain.
//!
//! [`ShapeTable`] is the same mechanism applied to geometry, and it is where
//! §10.8's "glyphs and repeated shapes are interned" lives: an outline compiles
//! once, content-addressed, and each occurrence becomes an [`Instance`] carrying
//! a transform, a style and an order. For text-heavy mathematical scenes that is
//! where the duplication is — the same `x` glyph in a formula is one compiled
//! path and N instances — and the same mechanism serves dots, arrow tips,
//! repeated graph nodes and copied decorations.

use crate::hint::Hint;
use fmn_core::types::Vec3;
use fmn_geom::arclength::ArcLengthTable;
use fmn_geom::quadpath::QuadPath;
use fmn_hash::Digest;
use fmn_mobject::{BoundingBox, JointType, Mob, Uniforms};
use std::collections::HashMap;

/// One quadratic segment of a compiled path, in **object space**.
///
/// §10.8 says compiled paths retain "screen-independent coefficients", and this
/// is why: a camera move must not rebuild curve geometry. The screen-space `f32`
/// form a kernel reads is a derived layout keyed on
/// `(Geometry, Transform, CameraProjection)`; this is keyed on `Geometry` alone.
///
/// The normalized arc-length span `(s0, s1)` rides along because §10.3
/// interpolates stroke width and colour by arc length, and §7.3's arc-length
/// layer is transcendental-dense and branchy — the wrong shape for a per-pixel
/// kernel and the right shape for the host (G0-8 finding F3). The span is a
/// function of geometry alone, so it survives a restyle and a transform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    /// Start anchor.
    pub p0: Vec3,
    /// Control handle.
    pub p1: Vec3,
    /// End anchor.
    pub p2: Vec3,
    /// Normalized arc length at `t = 0`, over the whole path.
    pub s0: f64,
    /// Normalized arc length at `t = 1`.
    pub s1: f64,
}

/// The style a compiled path draws under.
///
/// **Per-path constants only.** The Reference stores `stroke_rgba`,
/// `stroke_width`, `fill_rgba` and `fill_border_width` *per point*, which is how
/// a gradient along a path is expressed, and this row carries the two endpoints
/// of that ramp rather than the ramp. That is a deliberate subset, not a
/// substitute: §10.2's interior colour field (arc-length boundary interpolation
/// with mean-value coordinates) is fm-5oi's to design and §10.3's stroke ramp is
/// fm-oac's, and inventing either here would pre-empt a decision with something
/// nobody reviewed. What this table owes them is the *interning*, the dedup
/// index, and the per-stage derivation — all of which are structural and none of
/// which change when the ramp arrives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// Stroke colour at arc-length 0, linear light, straight alpha.
    pub stroke_rgba: [f32; 4],
    /// Stroke colour at arc-length 1.
    pub stroke_rgba_end: [f32; 4],
    /// Stroke width at arc-length 0, object units.
    pub stroke_width: f32,
    /// Stroke width at arc-length 1.
    pub stroke_width_end: f32,
    /// Fill colour at the ramp's start.
    pub fill_rgba: [f32; 4],
    /// Fill colour at the ramp's end.
    pub fill_rgba_end: [f32; 4],
    /// §10.2's principled inner border stroke.
    pub fill_border_width: f32,
    /// The AA band, from the object's uniforms.
    pub anti_alias_width: f32,
    /// Stroke joint style.
    pub joint_type: JointType,
    /// Whether the stroke draws behind the fill (§8.5 batch key).
    pub stroke_behind: bool,
}

impl Style {
    /// The bit pattern this style interns under.
    ///
    /// Bitwise, matching `BatchKey`. The array width is asserted by
    /// construction: adding a field without widening it fails every test here
    /// rather than silently merging two distinct styles.
    fn bits(&self) -> [u32; 22] {
        let mut b = [0u32; 22];
        let scalars = [
            self.stroke_width,
            self.stroke_width_end,
            self.fill_border_width,
            self.anti_alias_width,
        ];
        for (i, v) in self
            .stroke_rgba
            .iter()
            .chain(self.stroke_rgba_end.iter())
            .chain(self.fill_rgba.iter())
            .chain(self.fill_rgba_end.iter())
            .chain(scalars.iter())
            .enumerate()
        {
            b[i] = v.to_bits();
        }
        // `to_code` is an `f64`, and `as u32` on its bit pattern would take the
        // *low* 32 bits — which are all zero for every small integer an `f64`
        // can hold, so all four joint types would have interned as one. Folding
        // the halves keeps the whole pattern in the key and survives a code that
        // stops being an integer.
        let joint = self.joint_type.to_code().to_bits();
        b[20] = ((joint >> 32) ^ (joint & 0xffff_ffff)) as u32;
        b[21] = u32::from(self.stroke_behind);
        b
    }

    /// The per-object constants a `Uniforms` contributes.
    #[must_use]
    pub fn with_uniforms(mut self, u: &Uniforms) -> Style {
        self.anti_alias_width = u.anti_alias_width as f32;
        self.joint_type = u.joint_type;
        self.stroke_behind = u.stroke_behind;
        self
    }
}

impl Default for Style {
    fn default() -> Self {
        let u = Uniforms::default();
        Style {
            stroke_rgba: [0.0; 4],
            stroke_rgba_end: [0.0; 4],
            stroke_width: 0.0,
            stroke_width_end: 0.0,
            fill_rgba: [0.0; 4],
            fill_rgba_end: [0.0; 4],
            fill_border_width: 0.0,
            anti_alias_width: u.anti_alias_width as f32,
            joint_type: u.joint_type,
            stroke_behind: u.stroke_behind,
        }
    }
}

/// Deduplicated styles, and the index that made them unique.
#[derive(Debug, Clone, Default)]
pub struct StyleTable {
    rows: Vec<Style>,
    index: HashMap<[u32; 22], u32>,
}

impl StyleTable {
    /// Intern `style`, returning its row.
    pub fn intern(&mut self, style: Style) -> u32 {
        let key = style.bits();
        if let Some(&i) = self.index.get(&key) {
            return i;
        }
        let i = self.rows.len() as u32;
        self.rows.push(style);
        self.index.insert(key, i);
        i
    }

    /// The interned rows, in insertion order.
    #[must_use]
    pub fn rows(&self) -> &[Style] {
        &self.rows
    }

    /// One row.
    #[must_use]
    pub fn get(&self, index: u32) -> Option<&Style> {
        self.rows.get(index as usize)
    }

    /// How many distinct styles the scene uses.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Is the table empty?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Forget every row.
    pub fn clear(&mut self) {
        self.rows.clear();
        self.index.clear();
    }
}

/// A compiled outline: geometry that many instances share.
///
/// The unit §10.8 interns. Its identity is the **content digest of the object-
/// space points**, not the mobject that produced it, which is exactly what makes
/// two copies of one glyph — or two `Dot`s, or a hundred arrow tips — collapse
/// to one compiled path.
///
/// **Geometry here is shape-local**: translated so the outline's first anchor
/// sits at the origin, which is the same normalization [`shape_digest`] hashes
/// under. That is not tidiness, it is what makes interning *correct*. Store the
/// points where the first mobject happened to be and every later instance
/// sharing the outline inherits that position — the interning still dedups, and
/// the picture is wrong. An instance's placement is [`Instance::offset`], and it
/// is the only thing that says where the outline goes.
#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    /// The content address of the object-space points.
    pub digest: Digest,
    /// The segment range this shape owns in [`crate::plan::RenderPlan::segments`]
    /// — §10.8's `SegmentTable`, which is one flat table for the whole scene.
    pub first_segment: u32,
    /// How many segments.
    pub segment_count: u32,
    /// Object-space bounds — the control-polygon hull, which contains the curve.
    pub bounds: BoundingBox,
    /// The kernel route (§10.8's primitive hint).
    pub hint: Hint,
    /// Total object-space arc length, and the per-curve table §7.3 owns.
    pub arc_length: ArcLengthTable,
    /// Segment indices, **relative to `first_segment`**, at which a subpath
    /// begins. Always starts with `0` for a shape that has any segments.
    ///
    /// fmn-geom marks a subpath break with a null curve, and [`compile_shape`]
    /// drops those — a null curve is not a drawn piece. Dropping them without
    /// recording *where* they were loses the only thing they carry, and §10.2's
    /// fill is the consumer that needs it: a fill closes each subpath, so it has
    /// to know where one ends and the next begins. Recovering the boundaries
    /// from anchor continuity (`p2 != p0`) is almost right, and silently wrong
    /// exactly when one subpath begins where the previous one ended — a letter
    /// `o` whose counter is drawn starting from the point the outer ring closed
    /// at would fill as one ring with a chord through it.
    pub subpath_starts: Vec<u32>,
}

/// One occurrence of a [`Shape`]: transform, style, clip, order.
///
/// §10.8's word is "instance", and the fields are its list. An instance is
/// deliberately small — an index, a placement and two more indices — because the
/// entire point of interning is that this is what a repeated glyph costs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Instance {
    /// Which compiled shape.
    pub shape: u32,
    /// Which style row.
    pub style: u32,
    /// The mobject this occurrence came from, for the inspector and the journal.
    pub mob: Mob,
    /// Object→world placement.
    ///
    /// A translation today, because that is all Marionette can express without
    /// the transform channel (fm-7if): every other transform is baked into the
    /// points, which is why they hash differently and do not intern. A full
    /// matrix here would be a field nothing can fill.
    pub offset: Vec3,
    /// Painter order: position in the draw sequence.
    pub order: u32,
    /// A fold of this occurrence's seven revision axes at sync time.
    ///
    /// Carried on the instance rather than looked up because the tile cache's
    /// key (§10.8) is per *tile*, and a tile knows its commands rather than
    /// their mobjects. One `u64` per occurrence is the cheapest thing that makes
    /// the key complete.
    pub revisions: u64,
    /// Whether a **writable live view** exists on this occurrence's buffer.
    ///
    /// §8.2: "while a *writable* live view exists, the affected object … is
    /// conservatively invalidated each frame — a live view never silently
    /// receives weaker semantics to buy speed." A view can mutate points with no
    /// Stage method called and therefore no revision bumped, so this is not
    /// foldable into `revisions`: it *poisons* the tile key instead, and the
    /// difference is that a poisoned tile can never hit however many frames pass.
    pub volatile: bool,
    /// Whether writable point storage makes the compiled primitive hint unsafe.
    ///
    /// This is narrower than [`Instance::volatile`]: a field-scoped colour view
    /// poisons cached pixels but cannot invalidate a geometry hint. Binning and
    /// the frame compiler use this bit to take their general, fail-closed paths.
    pub hint_unsafe: bool,
}

/// Interned outlines and their occurrences.
///
/// The `InstanceTable` of §10.8, plus the shape table it indexes into. They live
/// in one type because an instance without its shape is not a thing, and because
/// the dedup statistic — N occurrences, M compiled shapes — is the number this
/// mechanism exists to move and should be readable in one place.
#[derive(Debug, Clone, Default)]
pub struct ShapeTable {
    shapes: Vec<Shape>,
    by_digest: HashMap<Digest, u32>,
    instances: Vec<Instance>,
}

impl ShapeTable {
    /// Intern an outline by its content digest.
    ///
    /// `build` runs only on a miss — which is the whole mechanism: compiling a
    /// glyph outline means decoding it, splitting it, and measuring its arc
    /// length, and doing that once per occurrence is what §10.8 calls "the bulk
    /// of geometry duplication".
    pub fn intern_shape(&mut self, digest: Digest, build: impl FnOnce() -> Shape) -> u32 {
        if let Some(&i) = self.by_digest.get(&digest) {
            return i;
        }
        let i = self.shapes.len() as u32;
        self.shapes.push(build());
        self.by_digest.insert(digest, i);
        i
    }

    /// Record an occurrence, returning its index.
    pub fn push_instance(&mut self, instance: Instance) -> u32 {
        self.instances.push(instance);
        (self.instances.len() - 1) as u32
    }

    /// The compiled shapes.
    #[must_use]
    pub fn shapes(&self) -> &[Shape] {
        &self.shapes
    }

    /// The occurrences, in painter order.
    #[must_use]
    pub fn instances(&self) -> &[Instance] {
        &self.instances
    }

    /// One shape.
    #[must_use]
    pub fn shape(&self, index: u32) -> Option<&Shape> {
        self.shapes.get(index as usize)
    }

    /// How many occurrences share one compiled shape, on average.
    ///
    /// The number §10.8's interning exists to raise, and the one an IR golden
    /// snapshot should carry: it is `1.0` for a scene of distinct paths and `N`
    /// for `N` copies of a glyph.
    #[must_use]
    pub fn instances_per_shape(&self) -> f64 {
        if self.shapes.is_empty() {
            return 0.0;
        }
        self.instances.len() as f64 / self.shapes.len() as f64
    }

    /// Forget the occurrences, keeping the compiled shapes.
    ///
    /// The per-frame half of the retained plan: painter order is a property of
    /// the frame, so the instance list is rebuilt every sync while the outlines
    /// it indexes persist. Clearing both would make interning worthless.
    pub fn clear_instances(&mut self) {
        self.instances.clear();
    }

    /// Forget everything.
    pub fn clear(&mut self) {
        self.shapes.clear();
        self.by_digest.clear();
        self.instances.clear();
    }
}

/// The content address of an outline: object-space points, canonically hashed.
///
/// Uses `fmn-hash`'s versioned envelope rather than an ad-hoc byte dump, so the
/// digest is stable across platforms and versions — floats are canonicalized at
/// the boundary (`-0.0 → +0.0`, one canonical NaN), which matters here because
/// two glyphs that differ only in the sign of a zero are the same outline and
/// must intern together.
///
/// **Position is excluded**: the points are hashed relative to the first anchor,
/// so two copies of one glyph at different places share a compiled shape and
/// differ only in their [`Instance::offset`]. That is the entire mechanism —
/// without it, interning would find nothing in a real formula.
#[must_use]
pub fn shape_digest(points: &[Vec3]) -> Digest {
    const SCHEMA: fmn_hash::Schema = fmn_hash::Schema::new(*b"FMNR", 1, 1, 0);
    let origin = points.first().copied().unwrap_or([0.0; 3]);
    let mut w = fmn_hash::Writer::new(SCHEMA);
    w.put_u64(points.len() as u64);
    for p in points {
        for k in 0..3 {
            w.put_f64(p[k] - origin[k]);
        }
    }
    w.finish()
        .map(|bytes| fmn_hash::sha256(&bytes))
        .unwrap_or_else(|_| fmn_hash::sha256(&[]))
}

/// Build a [`Shape`] from an object-space path.
///
/// The compile step §10.8 wants to happen once per *outline* rather than once
/// per occurrence: segments, hull bounds, the arc-length table, and the hint.
#[must_use]
pub fn compile_shape(
    digest: Digest,
    path: &QuadPath,
    hint: Hint,
    first_segment: u32,
) -> (Shape, Vec<Segment>) {
    let arc_length = ArcLengthTable::for_path(path);
    let lengths = arc_length.curve_lengths();

    // A subpath break is **a handle sitting on top of the previous anchor**,
    // which is fmn-geom's convention and the Reference's — not a zero-length
    // curve. The two coincide only when the next subpath happens to begin where
    // the last one ended, so testing length finds an annulus whose rings are
    // concentric and misses every glyph counter. Asking `QuadPath` rather than
    // re-deriving the predicate also keeps the Reference's 1e-4 disambiguation
    // tolerance in the one place that owns it.
    let breaks: std::collections::HashSet<usize> = path
        .subpath_end_indices()
        .into_iter()
        // The last entry is the path's own end, not a break; it is the only one
        // that can be an odd index, and it never names a curve start.
        .filter(|&i| i.is_multiple_of(2) && i + 2 < path.points().len())
        .collect();

    // The ramp normalizes over the length actually drawn. A dropped break has
    // real length, and leaving it in the denominator would stop every stroke's
    // width and colour ramp short of its endpoint. Summing the kept curves in
    // curve order is bit-identical to `ArcLengthTable::total()` for any path
    // with nothing to drop, which is every single-subpath shape.
    let mut total = 0.0f64;
    for i in 0..path.num_curves() {
        let len = lengths.get(i).copied().unwrap_or(0.0);
        if len > 0.0 && !breaks.contains(&(2 * i)) {
            total += len;
        }
    }

    let mut segments = Vec::with_capacity(path.num_curves());
    let mut subpath_starts: Vec<u32> = Vec::new();
    let mut opens_subpath = true;
    let mut acc = 0.0f64;
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];

    for i in 0..path.num_curves() {
        let Some([p0, p1, p2]) = path.nth_curve_points(i) else {
            continue;
        };
        let len = lengths.get(i).copied().unwrap_or(0.0);
        if breaks.contains(&(2 * i)) {
            // Not a drawn piece: dropping it is what keeps a stroked annulus
            // from growing a radial spoke between its two rings.
            opens_subpath = true;
            continue;
        }
        if len <= 0.0 {
            // A true null curve (`p0 == p1 == p2`) draws nothing and, per
            // `subpath_end_indices`' own disambiguation, does not open a
            // subpath either — a run of them is padding, not a boundary.
            continue;
        }
        if opens_subpath {
            subpath_starts.push(segments.len() as u32);
            opens_subpath = false;
        }
        let s0 = if total > 0.0 { acc / total } else { 0.0 };
        acc += len;
        let s1 = if total > 0.0 { acc / total } else { 0.0 };
        for p in [p0, p1, p2] {
            for k in 0..3 {
                min[k] = min[k].min(p[k]);
                max[k] = max[k].max(p[k]);
            }
        }
        segments.push(Segment { p0, p1, p2, s0, s1 });
    }

    let bounds = if segments.is_empty() {
        BoundingBox::ZERO
    } else {
        BoundingBox {
            min,
            mid: [
                0.5 * (min[0] + max[0]),
                0.5 * (min[1] + max[1]),
                0.5 * (min[2] + max[2]),
            ],
            max,
        }
    };

    let count = segments.len() as u32;
    (
        Shape {
            digest,
            first_segment,
            segment_count: count,
            bounds,
            hint,
            arc_length,
            subpath_starts,
        },
        segments,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_core::constants::TAU;

    fn style_with(width: f32) -> Style {
        Style {
            stroke_width: width,
            ..Style::default()
        }
    }

    fn tri() -> QuadPath {
        let mut p = QuadPath::default();
        p.start_new_path([0.0, 0.0, 0.0]);
        p.add_line_to([2.0, 0.0, 0.0], false).unwrap();
        p.add_line_to([1.0, 1.0, 0.0], false).unwrap();
        p.add_line_to([0.0, 0.0, 0.0], false).unwrap();
        p
    }

    #[test]
    fn styles_intern_by_bits_and_never_merge_distinct_ones() {
        let mut t = StyleTable::default();
        let a = t.intern(style_with(2.0));
        let b = t.intern(style_with(2.0));
        assert_eq!(a, b);
        assert_eq!(t.len(), 1);
        // One ulp apart is two styles: §8.5 makes batching semantics, so a
        // table that merged these would change how many draw calls a scene
        // takes. `2.0 + f32::EPSILON` would NOT do — EPSILON is the ulp at 1.0,
        // and at 2.0 it rounds straight back, which is how this test first
        // passed against a key that could not tell them apart.
        let c = t.intern(style_with(f32::from_bits(2.0f32.to_bits() + 1)));
        assert_ne!(c, a);
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn every_style_field_participates_in_the_key() {
        // The failure this guards is a field added to Style and forgotten in
        // `bits`, which silently merges two distinct styles forever after.
        let base = Style::default();
        let variants: Vec<Style> = vec![
            Style {
                stroke_rgba: [1.0, 0.0, 0.0, 1.0],
                ..base
            },
            Style {
                stroke_rgba_end: [1.0, 0.0, 0.0, 1.0],
                ..base
            },
            Style {
                stroke_width: 1.0,
                ..base
            },
            Style {
                stroke_width_end: 1.0,
                ..base
            },
            Style {
                fill_rgba: [0.0, 1.0, 0.0, 1.0],
                ..base
            },
            Style {
                fill_rgba_end: [0.0, 1.0, 0.0, 1.0],
                ..base
            },
            Style {
                fill_border_width: 1.0,
                ..base
            },
            Style {
                anti_alias_width: 2.0,
                ..base
            },
            Style {
                joint_type: JointType::Miter,
                ..base
            },
            Style {
                stroke_behind: true,
                ..base
            },
        ];

        let mut t = StyleTable::default();
        let baseline = t.intern(base);
        for (i, v) in variants.iter().enumerate() {
            assert_ne!(
                t.intern(*v),
                baseline,
                "variant {i} interned as the default style — a field is missing from the key"
            );
        }
        assert_eq!(t.len(), variants.len() + 1);
    }

    #[test]
    fn uniforms_reach_the_style_row() {
        let u = Uniforms {
            anti_alias_width: 2.5,
            joint_type: JointType::Bevel,
            stroke_behind: true,
            ..Uniforms::default()
        };
        let s = Style::default().with_uniforms(&u);
        assert_eq!(s.anti_alias_width, 2.5);
        assert_eq!(s.joint_type, JointType::Bevel);
        assert!(s.stroke_behind);
    }

    #[test]
    fn one_outline_at_two_places_is_one_compiled_shape() {
        // The instancing claim, at its smallest: N copies of a glyph must
        // produce 1 compiled path and N instances.
        let points: Vec<Vec3> = tri().points().to_vec();
        let shifted: Vec<Vec3> = points
            .iter()
            .map(|p| [p[0] + 100.0, p[1] - 7.5, p[2]])
            .collect();
        assert_eq!(
            shape_digest(&points),
            shape_digest(&shifted),
            "position must not be part of an outline's identity"
        );

        // A real handle, because Instance carries one and a synthetic Mob is
        // not constructible from outside Marionette — which is the point of a
        // generational handle.
        let mut stage = fmn_mobject::Stage::new();
        let mob = stage.add(fmn_mobject::Mobject::new());

        let mut table = ShapeTable::default();
        let mut compiles = 0;
        for (i, pts) in [points.clone(), shifted.clone(), points.clone()]
            .into_iter()
            .enumerate()
        {
            let digest = shape_digest(&pts);
            let shape = table.intern_shape(digest, || {
                compiles += 1;
                let path = QuadPath::from_points(pts.clone()).expect("valid path");
                compile_shape(digest, &path, Hint::Line, 0).0
            });
            table.push_instance(Instance {
                shape,
                style: 0,
                mob,
                offset: [pts[0][0], pts[0][1], pts[0][2]],
                order: i as u32,
                revisions: 0,
                volatile: false,
                hint_unsafe: false,
            });
        }
        assert_eq!(compiles, 1, "the outline must compile once");
        assert_eq!(table.shapes().len(), 1);
        assert_eq!(table.instances().len(), 3);
        assert!((table.instances_per_shape() - 3.0).abs() < 1e-12);
    }

    #[test]
    fn a_different_outline_is_a_different_shape() {
        let a: Vec<Vec3> = tri().points().to_vec();
        let mut b = a.clone();
        b[2] = [2.0, 0.5, 0.0];
        assert_ne!(shape_digest(&a), shape_digest(&b));
    }

    #[test]
    fn a_signed_zero_does_not_split_an_outline() {
        // fmn-hash canonicalizes floats at the boundary. Two glyphs differing
        // only in the sign of a zero are one outline, and interning them
        // separately would silently halve the mechanism's value on real text.
        let a: Vec<Vec3> = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]];
        let b: Vec<Vec3> = vec![[0.0, -0.0, 0.0], [1.0, -0.0, 0.0], [2.0, -0.0, 0.0]];
        assert_eq!(shape_digest(&a), shape_digest(&b));
    }

    #[test]
    fn a_subpath_that_begins_elsewhere_is_still_a_subpath() {
        // fmn-geom signals a subpath break with **a handle sitting on top of the
        // previous anchor** (`QuadPath::subpath_end_indices`, and the Reference's
        // own convention). A break to a *different* location therefore has
        // nonzero length, so testing for a zero-length curve finds only the
        // special case where the next subpath happens to start where the last
        // one ended.
        //
        // Two consequences, both visible: an annulus or a glyph counter compiles
        // as ONE subpath, which §10.2 says makes it "fill as one ring with a
        // chord through it"; and the break itself survives into the segment list,
        // so a stroked annulus grows a radial spoke from its outer ring to its
        // inner one.
        let outer = QuadPath::arc(0.0, TAU, 1.0, [0.0, 0.0, 0.0], Some(8));
        let inner = QuadPath::arc(0.0, -TAU, 0.4, [0.0, 0.0, 0.0], Some(8));
        let mut ring = QuadPath::new();
        ring.add_subpath(outer.points()).expect("outer");
        ring.add_subpath(inner.points()).expect("inner");

        // fmn-geom itself sees two subpaths; the compiler must agree with it.
        assert_eq!(ring.subpaths().len(), 2, "the fixture is not a ring");

        let (shape, segments) = compile_shape(shape_digest(ring.points()), &ring, Hint::General, 0);
        assert_eq!(
            shape.subpath_starts.len(),
            2,
            "the compiled shape lost a subpath boundary: {:?}",
            shape.subpath_starts
        );
        // And the break is not a drawn piece.
        assert_eq!(
            segments.len(),
            16,
            "the subpath break survived as a drawn segment"
        );
        for s in &segments {
            assert!(
                s.p0 != s.p1 || s.p1 == s.p2,
                "a break curve (handle on the anchor) is in the segment list"
            );
        }
    }

    #[test]
    fn compiling_a_shape_partitions_arc_length_and_hulls_the_curve() {
        let path = tri();
        let digest = shape_digest(path.points());
        let (shape, segments) = compile_shape(digest, &path, Hint::Line, 7);
        assert_eq!(shape.first_segment, 7);
        assert_eq!(shape.segment_count, segments.len() as u32);
        assert_eq!(segments.len(), 3);

        // The spans partition [0, 1] in order.
        assert_eq!(segments[0].s0, 0.0);
        assert!((segments[segments.len() - 1].s1 - 1.0).abs() < 1e-12);
        for w in segments.windows(2) {
            assert_eq!(w[0].s1, w[1].s0);
        }
        // Hull bounds contain every control point, hence the curve.
        assert_eq!(shape.bounds.min, [0.0, 0.0, 0.0]);
        assert_eq!(shape.bounds.max, [2.0, 1.0, 0.0]);
        assert_eq!(shape.bounds.mid, [1.0, 0.5, 0.0]);
        // And the arc length is the shared §7.3 artifact, not a private one:
        // the triangle's perimeter, 2 + 2*sqrt(2).
        let want = 2.0 + 2.0 * std::f64::consts::SQRT_2;
        assert!(
            (shape.arc_length.total() - want).abs() < 1e-9,
            "{} vs {want}",
            shape.arc_length.total()
        );
    }

    #[test]
    fn a_path_with_no_curves_compiles_to_nothing_rather_than_a_degenerate_shape() {
        let mut p = QuadPath::default();
        p.start_new_path([1.0, 1.0, 0.0]);
        let (shape, segments) = compile_shape(shape_digest(p.points()), &p, Hint::General, 0);
        assert!(segments.is_empty());
        assert_eq!(shape.segment_count, 0);
        assert_eq!(shape.bounds, BoundingBox::ZERO);
    }
}
