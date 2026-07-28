//! The retained render plan: lazy synchronization from Marionette (§10.8, §8.2).
//!
//! > … synchronized *lazily* from the RecordBuffer under §8.2's mirror rule …
//!
//! ## What "lazily" has to mean to be worth anything
//!
//! Not "recompute a bit less". The plan holds compiled outlines across frames
//! and, for a renderable whose declared axes have not moved, **never reads its
//! points at all**. That is the difference between a cache and a retained plan:
//! a cache still has to look at the data to discover it is unchanged, and
//! looking at the data is most of the cost — reading a glyph's points, hashing
//! them, splitting curves, measuring arc length.
//!
//! So the sync loop's inner question is a comparison of seven integers
//! ([`crate::revision`]), and everything expensive sits behind it. The cheap
//! things that genuinely must happen every frame — the painter-order instance
//! list — are rebuilt every frame, because an instance is an index, an offset
//! and two more indices, and pretending otherwise would buy nothing and cost
//! correctness.
//!
//! ## The observable
//!
//! [`SyncStats`] exists because "untouched resources never recompile" is a claim
//! about work *not* done, which is untestable unless the work counts itself.
//! `MirrorSet::materializations` and `CachedArcLength::rebuilds` are the same
//! idea one layer down, and the same reason.

use crate::hint::Hint;
use crate::revision::{Axis, Dependency, Revisions};
use crate::table::{Instance, Segment, ShapeTable, Style, StyleTable, compile_shape, shape_digest};
use fmn_core::types::Vec3;
use fmn_geom::quadpath::QuadPath;
use fmn_hash::{Digest, Sha256};
use fmn_mobject::{Mob, RecordBuffer, Stage};
use std::collections::HashMap;

/// The axes a compiled outline depends on.
///
/// Geometry because it *is* the geometry, topology because a point count or a
/// family change reshapes it. Deliberately **not** style, order or camera: a
/// recolour, a `z_index` bump and a pan must all leave a compiled outline alone,
/// which is the §10.8 promise this constant makes checkable.
pub const SHAPE_AXES: [Axis; 3] = [Axis::Topology, Axis::Geometry, Axis::Transform];

/// The axes a style row depends on.
pub const STYLE_AXES: [Axis; 1] = [Axis::Style];

/// Record fields the compiled outline actually observes today.
///
/// Keep this consumption list exact: a field-scoped view of user metadata must
/// not turn into object-wide recompilation merely because both share a record.
const SHAPE_VIEW_FIELDS: [&str; 1] = ["point"];

/// Record fields [`read_style`] observes.
const STYLE_VIEW_FIELDS: [&str; 4] = [
    "stroke_rgba",
    "stroke_width",
    "fill_rgba",
    "fill_border_width",
];

fn writable_view_affects_any(buffer: &RecordBuffer, fields: &[&str]) -> bool {
    fields
        .iter()
        .any(|field| buffer.writable_view_affects(field))
}

/// What one [`RenderPlan::sync`] did, and — more usefully — did not do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncStats {
    /// Renderables the draw plan presented.
    pub visited: usize,
    /// Outlines compiled from points this sync.
    pub shapes_compiled: usize,
    /// Renderables whose outline was reused without their points being read.
    pub shapes_reused: usize,
    /// Outlines that interned onto an existing compiled shape — the instancing
    /// win, counted separately from a reuse because it is a different mechanism:
    /// this one is two *different* mobjects sharing one outline.
    pub shapes_shared: usize,
    /// Style rows recomputed this sync.
    pub styles_rebuilt: usize,
    /// Retained entries dropped because their mobject left the draw plan.
    pub dropped: usize,
}

/// One renderable's retained compilation.
#[derive(Debug, Clone)]
struct Retained {
    shape_dep: Dependency,
    style_dep: Dependency,
    shape: u32,
    style: u32,
    /// The instance offset, kept so a reuse needs no point read.
    offset: Vec3,
}

/// The compiled, backend-neutral render IR for a scene, retained across frames.
#[derive(Debug, Clone)]
pub struct RenderPlan {
    segments: Vec<Segment>,
    styles: StyleTable,
    shapes: ShapeTable,
    retained: HashMap<Mob, Retained>,
    stats: SyncStats,
    geometry_key: GeometryIdentity,
    plan_key: PlanIdentity,
}

/// Collision-resistant identity of the compiled geometry tables.
///
/// This is deliberately separate from [`PlanIdentity`]: a recolour or painter
/// reorder does not invalidate geometry-only artifacts such as
/// [`crate::fill::MonoTable`]. The zero value is reserved for an unbuilt
/// artifact and is never emitted by the SHA-256 construction below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct GeometryIdentity(Digest);

impl Default for GeometryIdentity {
    fn default() -> Self {
        Self(Digest::from_bytes([0; 32]))
    }
}

/// Collision-resistant identity of every pixel-deciding row in a render plan.
///
/// Unlike an allocation address or a local generation counter, a content
/// identity lets independently compiled but identical plans share derived
/// artifacts safely, while reordered or stale instance lists cannot alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PlanIdentity(Digest);

impl Default for PlanIdentity {
    fn default() -> Self {
        Self(Digest::from_bytes([0; 32]))
    }
}

impl Default for RenderPlan {
    fn default() -> Self {
        let mut plan = Self {
            segments: Vec::new(),
            styles: StyleTable::default(),
            shapes: ShapeTable::default(),
            retained: HashMap::new(),
            stats: SyncStats::default(),
            geometry_key: GeometryIdentity::default(),
            plan_key: PlanIdentity::default(),
        };
        plan.refresh_identities();
        plan
    }
}

/// Allocation-free canonical input to the in-tree SHA-256 implementation.
///
/// These identities are process-local guards rather than durable documents, so
/// they do not ride the size-limited serialization envelope. They preserve
/// exact float bits (matching `StyleTable`'s equality) and domain-separate every
/// sequence, giving the same collision resistance as the content-addressed
/// cache without making large scenes fail merely because an integrity check was
/// requested.
struct IdentityHasher(Sha256);

impl IdentityHasher {
    fn new(domain: &[u8]) -> Self {
        let mut hash = Sha256::new();
        hash.update(domain);
        Self(hash)
    }

    fn bytes(&mut self, value: &[u8]) {
        self.0.update(value);
    }

    fn bool(&mut self, value: bool) {
        self.bytes(&[u8::from(value)]);
    }

    fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes(&value.to_le_bytes());
    }

    fn f32(&mut self, value: f32) {
        self.u32(value.to_bits());
    }

    fn f64(&mut self, value: f64) {
        self.u64(value.to_bits());
    }

    fn digest(&mut self, value: Digest) {
        self.bytes(value.as_bytes());
    }

    fn finish(self) -> Digest {
        self.0.finalize()
    }
}

impl RenderPlan {
    /// An empty plan.
    #[must_use]
    pub fn new() -> RenderPlan {
        RenderPlan::default()
    }

    /// Synchronize against `stage`, at camera revision `camera`.
    ///
    /// Walks the scene's draw plan — which is `fmn_mobject`'s pure function of
    /// scene state, deliberately uncached there so that this cache can be
    /// checked against it — and for each renderable either reuses its retained
    /// compilation or rebuilds exactly the part whose axes moved.
    pub fn sync(&mut self, stage: &Stage, camera: u64) -> SyncStats {
        let mut stats = SyncStats::default();
        let plan = stage.draw_plan();

        // Instances are painter order, and painter order is a property of the
        // frame rather than of any object, so the list is rebuilt every frame.
        // The *shapes* it indexes are what persist.
        self.shapes.clear_instances();

        let mut seen: Vec<Mob> = Vec::with_capacity(plan.items().len());

        for (order, item) in plan.items().iter().enumerate() {
            let mob = item.mob;
            let Some(now) = Revisions::read(stage, mob).map(|r| r.with_camera(camera)) else {
                continue;
            };
            stats.visited += 1;
            seen.push(mob);

            let previous = self.retained.get(&mob).cloned();
            let (hint_unsafe, style_unsafe) = stage.get(mob).map_or((false, false), |entry| {
                (
                    writable_view_affects_any(&entry.buffer, &SHAPE_VIEW_FIELDS),
                    writable_view_affects_any(&entry.buffer, &STYLE_VIEW_FIELDS),
                )
            });

            // --- geometry: the expensive half, and the one that must be skipped.
            let (shape, offset) = match &previous {
                Some(r) if !hint_unsafe && !r.shape_dep.is_stale(&now) => {
                    stats.shapes_reused += 1;
                    (r.shape, r.offset)
                }
                _ => {
                    let Some(points) = stage.get_points(mob) else {
                        continue;
                    };
                    if points.is_empty() {
                        continue;
                    }
                    // Shape-local, matching the normalization `shape_digest`
                    // hashes under: the outline is translated so its first
                    // anchor sits at the origin, and `offset` is what places it
                    // back. Compiling absolute points would still dedup — and
                    // would put every later copy of a glyph wherever the first
                    // one happened to be.
                    let offset = points[0];
                    let local: Vec<Vec3> = points
                        .iter()
                        .map(|p| [p[0] - offset[0], p[1] - offset[1], p[2] - offset[2]])
                        .collect();
                    let digest = shape_digest(&points);
                    let before = self.shapes.shapes().len();
                    let first_segment = self.segments.len() as u32;
                    let hint =
                        Hint::of(stage, mob).translated([-offset[0], -offset[1], -offset[2]]);
                    let segments_out = &mut self.segments;
                    let index = self.shapes.intern_shape(digest, || {
                        let path =
                            QuadPath::from_points(local).unwrap_or_else(|_| QuadPath::default());
                        let (shape, mut segs) = compile_shape(digest, &path, hint, first_segment);
                        segments_out.append(&mut segs);
                        shape
                    });
                    if self.shapes.shapes().len() > before {
                        stats.shapes_compiled += 1;
                    } else {
                        // Interned onto an outline some other mobject already
                        // compiled: §10.8's instancing, paying for itself.
                        stats.shapes_shared += 1;
                    }
                    (index, offset)
                }
            };

            // --- style: the cheap half, gated on its own axis so a moved point
            // does not re-intern a colour.
            let style = match &previous {
                Some(r) if !style_unsafe && !r.style_dep.is_stale(&now) => r.style,
                _ => {
                    stats.styles_rebuilt += 1;
                    let row = read_style(stage, mob);
                    self.styles.intern(row)
                }
            };

            // §8.2's conservative rule, read fresh every sync: a writable live
            // view can mutate points with no Stage method called and therefore
            // no revision bumped, so it cannot be folded into `revisions` — it
            // has to be a separate flag that poisons any cache downstream.
            let volatile = hint_unsafe || style_unsafe;

            self.shapes.push_instance(Instance {
                shape,
                style,
                mob,
                offset,
                order: order as u32,
                revisions: now.fold(),
                volatile,
                hint_unsafe,
            });

            self.retained.insert(
                mob,
                Retained {
                    shape_dep: Dependency::new(now, &SHAPE_AXES),
                    style_dep: Dependency::new(now, &STYLE_AXES),
                    shape,
                    style,
                    offset,
                },
            );
        }

        // Drop what left the scene. Compiled outlines are *not* dropped with it:
        // a mobject removed and re-added within a frame — which `Scene.add` does
        // on every re-add, since it removes first — must not pay to recompile,
        // and an interned outline is shared, so it is not this mobject's to free.
        let live: std::collections::HashSet<Mob> = seen.into_iter().collect();
        let before = self.retained.len();
        self.retained.retain(|m, _| live.contains(m));
        stats.dropped = before - self.retained.len();

        self.stats = stats;
        // Shape tables are append-only, so their identity can stay cached when
        // every outline was reused or interned onto an existing row. The
        // painter plan is rebuilt every sync and is hashed once alongside that
        // O(instances) work.
        if stats.shapes_compiled > 0 {
            self.geometry_key = self.compute_geometry_identity();
        }
        self.plan_key = self.compute_identity(self.geometry_key);
        stats
    }

    /// The segment table.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// The interned style table.
    #[must_use]
    pub fn styles(&self) -> &StyleTable {
        &self.styles
    }

    /// The interned shapes and their painter-ordered instances.
    #[must_use]
    pub fn shapes(&self) -> &ShapeTable {
        &self.shapes
    }

    /// What the last [`sync`] did.
    ///
    /// [`sync`]: RenderPlan::sync
    #[must_use]
    pub fn stats(&self) -> SyncStats {
        self.stats
    }

    /// Identity of the shape-indexed geometry consumed by derived fill tables.
    pub(crate) fn geometry_identity(&self) -> GeometryIdentity {
        self.geometry_key
    }

    /// Identity of geometry, styles, and the painter-ordered instance list.
    pub(crate) fn identity(&self) -> PlanIdentity {
        self.plan_key
    }

    /// Refresh both cached identities once after a synchronization.
    fn refresh_identities(&mut self) {
        let geometry = self.compute_geometry_identity();
        let plan = self.compute_identity(geometry);
        self.geometry_key = geometry;
        self.plan_key = plan;
    }

    fn compute_geometry_identity(&self) -> GeometryIdentity {
        let mut hash = IdentityHasher::new(b"fmn-render/geometry-identity/v1");

        hash.u64(self.segments.len() as u64);
        for segment in &self.segments {
            for point in [segment.p0, segment.p1, segment.p2] {
                for component in point {
                    hash.f64(component);
                }
            }
            hash.f64(segment.s0);
            hash.f64(segment.s1);
        }

        let shapes = self.shapes.shapes();
        hash.u64(shapes.len() as u64);
        for shape in shapes {
            hash.digest(shape.digest);
            hash.u32(shape.first_segment);
            hash.u32(shape.segment_count);
            for point in [shape.bounds.min, shape.bounds.mid, shape.bounds.max] {
                for component in point {
                    hash.f64(component);
                }
            }
            hash_hint(&mut hash, shape.hint);
            hash.u64(shape.arc_length.curve_lengths().len() as u64);
            for &length in shape.arc_length.curve_lengths() {
                hash.f64(length);
            }
            hash.u64(shape.subpath_starts.len() as u64);
            for &start in &shape.subpath_starts {
                hash.u32(start);
            }
        }

        GeometryIdentity(hash.finish())
    }

    fn compute_identity(&self, geometry: GeometryIdentity) -> PlanIdentity {
        let mut hash = IdentityHasher::new(b"fmn-render/plan-identity/v1");
        hash.digest(geometry.0);

        let instances = self.shapes.instances();
        hash.u64(instances.len() as u64);
        for instance in instances {
            hash.u32(instance.shape);
            hash.u32(instance.style);
            match self.styles.get(instance.style) {
                Some(style) => {
                    hash.bool(true);
                    for &component in style
                        .stroke_rgba
                        .iter()
                        .chain(style.stroke_rgba_end.iter())
                        .chain(style.fill_rgba.iter())
                        .chain(style.fill_rgba_end.iter())
                    {
                        hash.f32(component);
                    }
                    hash.f32(style.stroke_width);
                    hash.f32(style.stroke_width_end);
                    hash.f32(style.fill_border_width);
                    hash.f32(style.anti_alias_width);
                    hash.f64(style.joint_type.to_code());
                    hash.bool(style.stroke_behind);
                }
                None => hash.bool(false),
            }
            for component in instance.offset {
                hash.f64(component);
            }
            hash.u32(instance.order);
            // A point view disables hint-derived tile classification. Binning
            // built on the other side of this transition is therefore stale
            // even when the point bytes themselves have not moved.
            hash.bool(instance.hint_unsafe);
        }

        PlanIdentity(hash.finish())
    }
}

/// Feed a primitive hint's full payload, not merely its diagnostic name.
///
/// The parameters participate in specialized fill and containment kernels, so
/// hashing only the enum discriminant would let two differently placed circles
/// or rectangles share a binning that was built for the other one.
fn hash_hint(hash: &mut IdentityHasher, hint: Hint) {
    fn point(hash: &mut IdentityHasher, tag: u8, center: [f64; 3]) {
        hash.bytes(&[tag]);
        for component in center {
            hash.f64(component);
        }
    }

    match hint {
        Hint::General => hash.bytes(&[0]),
        Hint::Line => hash.bytes(&[1]),
        Hint::Polyline { closed } => {
            hash.bytes(&[2]);
            hash.bool(closed);
        }
        Hint::Arc {
            center,
            radius,
            start_angle,
            angle,
        } => {
            point(hash, 3, center);
            hash.f64(radius);
            hash.f64(start_angle);
            hash.f64(angle);
        }
        Hint::Circle { center, radius } => {
            point(hash, 4, center);
            hash.f64(radius);
        }
        Hint::Dot { center, radius } => {
            point(hash, 5, center);
            hash.f64(radius);
        }
        Hint::Rect {
            center,
            width,
            height,
        } => {
            point(hash, 6, center);
            hash.f64(width);
            hash.f64(height);
        }
        Hint::RoundedRect {
            center,
            width,
            height,
            corner_radius,
        } => {
            point(hash, 7, center);
            hash.f64(width);
            hash.f64(height);
            hash.f64(corner_radius);
        }
    }
}

/// Decode one sRGB-encoded colour record into the linear light the IR stores.
///
/// **This is BN-04's "render boundary", and it had no implementation.** The
/// record buffer holds what manim holds — sRGB-encoded components, because
/// `mobject.data` is API surface — while [`Style`] documents linear light and
/// every consumer already assumes it: `fill_rgba_at` and `stroke_rgba_at` both
/// say a ramp "happens where colour interpolation is defined, not in an encoded
/// space". Between the writer and those readers the decode simply did not exist,
/// so a mid-tone would have rendered at its encoded value — visibly wrong, and
/// wrong in the direction that looks like a lighting choice rather than a bug.
///
/// Alpha does not decode. It is a coverage fraction, not a light intensity, and
/// gamma-encoding it is the mistake `fmn_frame::transfer` names on the way out.
///
/// The decode routes through [`fmn_frame::transfer::srgb_decode`] rather than
/// `fmn_core::color::srgb_eotf`: the two compute the same function, but the
/// former rides `fmn_dmath::pow` and the latter rides `std::powf`, and ADR-0010's
/// first binding property is that fmn-dmath owns every transcendental on the
/// certified path. It is decoded **once per interned style row**, not per pixel.
fn decode_rgba(rgba: [f32; 4]) -> [f32; 4] {
    [
        fmn_frame::transfer::srgb_decode(f64::from(rgba[0])) as f32,
        fmn_frame::transfer::srgb_decode(f64::from(rgba[1])) as f32,
        fmn_frame::transfer::srgb_decode(f64::from(rgba[2])) as f32,
        rgba[3],
    ]
}

/// Read one renderable's style row out of its record buffer.
///
/// The Reference stores these per point; this reads the ramp's endpoints, which
/// is the subset [`Style`] documents. A point-less mobject yields the default
/// row rather than an error — a group with no geometry of its own is normal, and
/// it simply contributes no instance.
fn read_style(stage: &Stage, mob: Mob) -> Style {
    let mut row = Style::default();
    let Some(entry) = stage.get(mob) else {
        return row;
    };
    let buffer = &entry.buffer;
    let n = buffer.len();
    if n == 0 {
        return row.with_uniforms(entry.uniforms());
    }
    let last = n - 1;
    let rgba = |index: usize, field: &str| -> [f32; 4] {
        buffer
            .read(index, field)
            .and_then(|v| <[f32; 4]>::try_from(v.as_slice()).ok())
            .unwrap_or([0.0; 4])
    };
    let scalar = |index: usize, field: &str| -> f32 {
        buffer
            .read(index, field)
            .and_then(|v| v.first().copied())
            .unwrap_or(0.0)
    };
    row.stroke_rgba = decode_rgba(rgba(0, "stroke_rgba"));
    row.stroke_rgba_end = decode_rgba(rgba(last, "stroke_rgba"));
    row.fill_rgba = decode_rgba(rgba(0, "fill_rgba"));
    row.fill_rgba_end = decode_rgba(rgba(last, "fill_rgba"));
    row.stroke_width = scalar(0, "stroke_width");
    row.stroke_width_end = scalar(last, "stroke_width");
    row.fill_border_width = scalar(0, "fill_border_width");
    row.with_uniforms(entry.uniforms())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_mobject::{Mobject, RecordBuffer, RecordSchema};

    fn vmobject(points: &[[f64; 3]]) -> Mobject {
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len());
        for (i, p) in points.iter().enumerate() {
            buffer.write(i, "point", &[p[0] as f32, p[1] as f32, p[2] as f32]);
            buffer.write(i, "stroke_rgba", &[1.0, 1.0, 1.0, 1.0]);
            buffer.write(i, "stroke_width", &[2.0]);
        }
        Mobject::from_buffer(buffer)
    }

    fn tri_at(dx: f64, dy: f64) -> Mobject {
        vmobject(&[
            [dx, dy, 0.0],
            [dx + 1.0, dy, 0.0],
            [dx + 2.0, dy, 0.0],
            [dx + 2.0, dy + 0.5, 0.0],
            [dx + 2.0, dy + 1.0, 0.0],
        ])
    }

    fn staged(n: usize) -> (Stage, Vec<Mob>) {
        let mut stage = Stage::new();
        let mut mobs = Vec::new();
        for i in 0..n {
            let mob = stage.add(tri_at(i as f64 * 10.0, 0.0));
            stage.add_to_scene(mob).expect("live");
            mobs.push(mob);
        }
        (stage, mobs)
    }

    #[test]
    fn a_first_sync_compiles_and_a_second_reads_no_points() {
        // The claim the whole module exists for. Not "recompiles less" —
        // *nothing* is compiled on the second pass, and the points are never
        // touched.
        let (stage, _) = staged(3);
        let mut plan = RenderPlan::new();

        let first = plan.sync(&stage, 0);
        assert_eq!(first.visited, 3);
        assert_eq!(first.shapes_compiled, 1, "three copies of one outline");
        assert_eq!(first.shapes_shared, 2);
        assert_eq!(first.shapes_reused, 0);

        let second = plan.sync(&stage, 0);
        assert_eq!(second.visited, 3);
        assert_eq!(second.shapes_compiled, 0);
        assert_eq!(second.shapes_shared, 0);
        assert_eq!(second.shapes_reused, 3);
        assert_eq!(second.styles_rebuilt, 0);
    }

    #[test]
    fn writable_views_refresh_exactly_the_render_fields_they_expose() {
        let (mut stage, mobs) = staged(1);
        let mob = mobs[0];
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);

        let point_view = stage
            .get_mut(mob)
            .expect("live")
            .buffer
            .export_field_view("point", true)
            .expect("point field");
        for _ in 0..2 {
            let stats = plan.sync(&stage, 0);
            assert_eq!(stats.shapes_reused, 0);
            assert_eq!(
                stats.shapes_shared, 1,
                "unchanged bytes may re-intern, but must be observed"
            );
            assert_eq!(stats.styles_rebuilt, 0);
            let instance = plan.shapes().instances()[0];
            assert!(instance.volatile);
            assert!(instance.hint_unsafe);
        }
        drop(point_view);

        // Detaching the writable point view conservatively advances its field
        // revision, so the final state is observed once more before ordinary
        // reuse resumes.
        let detached = plan.sync(&stage, 0);
        assert_eq!(detached.shapes_reused, 0);
        assert_eq!(detached.shapes_shared, 1);
        assert!(!plan.shapes().instances()[0].hint_unsafe);
        assert_eq!(plan.sync(&stage, 0).shapes_reused, 1);

        let style_view = stage
            .get_mut(mob)
            .expect("live")
            .buffer
            .export_field_view("fill_rgba", true)
            .expect("fill field");
        for _ in 0..2 {
            let stats = plan.sync(&stage, 0);
            assert_eq!(stats.shapes_reused, 1);
            assert_eq!(stats.styles_rebuilt, 1);
            let instance = plan.shapes().instances()[0];
            assert!(instance.volatile);
            assert!(
                !instance.hint_unsafe,
                "a colour view cannot invalidate geometry"
            );
        }
        drop(style_view);

        let whole_view = stage.get_mut(mob).expect("live").buffer.export_view(true);
        let stats = plan.sync(&stage, 0);
        assert_eq!(stats.shapes_reused, 0);
        assert_eq!(stats.shapes_shared, 1);
        assert_eq!(stats.styles_rebuilt, 1);
        assert!(plan.shapes().instances()[0].hint_unsafe);
        drop(whole_view);
    }

    #[test]
    fn a_writable_custom_field_view_does_not_invalidate_render_state() {
        let schema = RecordSchema::new(
            &[
                ("point", 3),
                ("stroke_rgba", 4),
                ("stroke_width", 1),
                ("joint_angle", 1),
                ("fill_rgba", 4),
                ("base_normal", 3),
                ("fill_border_width", 1),
                ("user_metadata", 1),
            ],
            &["point"],
            &["point"],
        );
        let points = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 0.5, 0.0],
            [2.0, 1.0, 0.0],
        ];
        let mut buffer = RecordBuffer::new(schema, points.len());
        for (i, point) in points.iter().enumerate() {
            buffer.write(i, "point", point);
            buffer.write(i, "stroke_rgba", &[1.0; 4]);
            buffer.write(i, "stroke_width", &[2.0]);
        }
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::from_buffer(buffer));
        stage.add_to_scene(mob).expect("live");
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);

        let metadata_view = stage
            .get_mut(mob)
            .expect("live")
            .buffer
            .export_field_view("user_metadata", true)
            .expect("custom field");
        let stats = plan.sync(&stage, 0);
        assert_eq!(stats.shapes_reused, 1);
        assert_eq!(stats.styles_rebuilt, 0);
        assert!(!plan.shapes().instances()[0].volatile);
        drop(metadata_view);
    }

    #[test]
    fn n_copies_of_one_outline_are_one_compiled_path_and_n_instances() {
        // §10.8's instancing dedup, stated as the acceptance criterion does.
        let (stage, _) = staged(8);
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);
        assert_eq!(plan.shapes().shapes().len(), 1);
        assert_eq!(plan.shapes().instances().len(), 8);
        assert!((plan.shapes().instances_per_shape() - 8.0).abs() < 1e-12);
        // And the segment table holds one outline's worth, not eight.
        assert_eq!(plan.segments().len(), 2);
    }

    #[test]
    fn a_recolour_rebuilds_the_style_and_not_the_geometry() {
        // The §10.8 headline: "a color change does not regenerate curve
        // coefficients".
        let (mut stage, mobs) = staged(2);
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);

        stage
            .get_mut(mobs[0])
            .expect("live")
            .buffer
            .write(0, "stroke_rgba", &[1.0, 0.0, 0.0, 1.0]);

        let after = plan.sync(&stage, 0);
        assert_eq!(after.shapes_compiled, 0, "no outline may recompile");
        assert_eq!(after.shapes_reused, 2);
        assert_eq!(after.styles_rebuilt, 1, "exactly the one that changed");
        assert_eq!(plan.styles().len(), 2, "and it interned as a new row");
    }

    #[test]
    fn a_camera_move_rebuilds_nothing() {
        // "a camera move does not re-decode font outlines" — object-space
        // geometry cannot depend on the camera, so the axis must not reach it.
        let (stage, _) = staged(4);
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);
        let after = plan.sync(&stage, 99);
        assert_eq!(after.shapes_compiled, 0);
        assert_eq!(after.styles_rebuilt, 0);
        assert_eq!(after.shapes_reused, 4);
    }

    #[test]
    fn moving_one_point_recompiles_exactly_one_outline() {
        let (mut stage, mobs) = staged(3);
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);

        stage
            .get_mut(mobs[1])
            .expect("live")
            .buffer
            .write(2, "point", &[500.0, 500.0, 0.0]);

        let after = plan.sync(&stage, 0);
        assert_eq!(after.shapes_reused, 2, "the untouched two are reused");
        assert_eq!(
            after.shapes_compiled + after.shapes_shared,
            1,
            "and exactly one is recompiled"
        );
        assert_eq!(
            plan.shapes().shapes().len(),
            2,
            "a second outline exists now"
        );
    }

    #[test]
    fn a_z_index_change_reuses_every_outline_but_reorders_the_instances() {
        // Order is its own axis: the geometry cannot care, and the instance list
        // must.
        let (mut stage, mobs) = staged(3);
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);
        let before: Vec<Mob> = plan.shapes().instances().iter().map(|i| i.mob).collect();

        stage.set_z_index(mobs[0], 10, false);
        stage.add_to_scene(mobs[0]).expect("live");

        let after = plan.sync(&stage, 0);
        assert_eq!(after.shapes_compiled, 0);
        assert_eq!(after.shapes_reused, 3);
        let now: Vec<Mob> = plan.shapes().instances().iter().map(|i| i.mob).collect();
        assert_ne!(before, now, "raising z_index must move it in painter order");
        assert_eq!(now.last(), Some(&mobs[0]), "and put it on top");
    }

    #[test]
    fn leaving_the_scene_drops_the_retained_entry() {
        let (mut stage, mobs) = staged(3);
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);

        stage.remove_from_scene(mobs[2]);
        let after = plan.sync(&stage, 0);
        assert_eq!(after.visited, 2);
        assert_eq!(after.dropped, 1);
        assert_eq!(plan.shapes().instances().len(), 2);
    }

    #[test]
    fn instances_carry_painter_order_and_the_offset_that_places_them() {
        let (stage, mobs) = staged(3);
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);
        let instances = plan.shapes().instances();
        assert_eq!(instances.len(), 3);
        for (i, inst) in instances.iter().enumerate() {
            assert_eq!(inst.order, i as u32);
            assert_eq!(inst.mob, mobs[i]);
            // The outline is shared, so the placement is what distinguishes the
            // three — which is the whole reason position is excluded from the
            // shape digest.
            assert!((inst.offset[0] - i as f64 * 10.0).abs() < 1e-9);
        }
        assert_eq!(instances[0].shape, instances[2].shape);
    }

    #[test]
    fn interned_indices_are_stable_across_syncs() {
        // The property the tile cache's key rests on (`crate::cache`): shape and
        // style indices come from append-only interning, so a retained plan
        // keeps them stable and adding an object cannot renumber the ones
        // already there. A plan rebuilt each frame reshuffles them, and every
        // tile key in the frame moves for a reason that has nothing to do with
        // the tile — which is exactly what the cache's first test harness did.
        let (mut stage, mobs) = staged(2);
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);
        let shapes: Vec<u32> = plan.shapes().instances().iter().map(|i| i.shape).collect();
        let styles: Vec<u32> = plan.shapes().instances().iter().map(|i| i.style).collect();

        // A newcomer *behind* the others, so it lands first in the draw sequence
        // and shifts every instance index.
        let newcomer = stage.add(vmobject(&[
            [0.0, 0.0, 0.0],
            [3.0, 1.0, 0.0],
            [6.0, 0.0, 0.0],
        ]));
        stage.set_z_index(newcomer, -5, false);
        stage.add_to_scene(newcomer).expect("live");
        plan.sync(&stage, 0);

        let instances = plan.shapes().instances();
        assert_eq!(instances.len(), 3);
        assert_eq!(instances[0].mob, newcomer, "the newcomer draws first");
        for (k, mob) in mobs.iter().enumerate() {
            let inst = instances
                .iter()
                .find(|i| i.mob == *mob)
                .expect("still in the scene");
            assert_eq!(inst.shape, shapes[k], "shape index moved for {mob:?}");
            assert_eq!(inst.style, styles[k], "style index moved for {mob:?}");
        }
    }

    #[test]
    fn a_point_less_group_contributes_no_instance() {
        // A `Group` with children but no geometry of its own is normal, and it
        // must not appear as a degenerate compiled path.
        let mut stage = Stage::new();
        let group = stage.add(Mobject::new());
        stage.add_to_scene(group).expect("live");
        let mut plan = RenderPlan::new();
        let stats = plan.sync(&stage, 0);
        assert_eq!(stats.shapes_compiled, 0);
        assert_eq!(plan.shapes().instances().len(), 0);
    }
}
