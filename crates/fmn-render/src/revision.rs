//! The seven independent revision axes (§10.8), and how they are read out of
//! Marionette.
//!
//! §10.8's retained plan turns on one sentence:
//!
//! > Every renderable resource carries **independent revisions** — topology,
//! > geometry, transform, style, visibility/order, image, camera-projection —
//! > and the distinctions pay directly: a color change does not regenerate curve
//! > coefficients; a translation does not recompute an object-space arc-length
//! > table; a camera move does not re-decode font outlines.
//!
//! The payoff is entirely in the *distinctions*, so this module's job is to make
//! them nameable ([`Axis`]), comparable ([`Revisions`]), and derivable from the
//! authoritative state without any renderable ever asking "did something
//! change?" — the question that has no cheap correct answer. It asks "is this
//! artifact's key still the key I built it under?", which is a comparison of
//! seven integers.
//!
//! ## Where the numbers come from, and where they do not
//!
//! Marionette carries per-field revision counters on the `RecordBuffer`
//! (`record.rs`) plus fm-7if's independent affine placement revision on every
//! entry. Five of the seven axes read directly off that machinery plus the
//! family and uniforms. Two are produced elsewhere:
//!
//! - **`Image` has no Marionette producer yet** — there are no image mobjects,
//!   so its revision remains an explicit input supplied by §12's
//!   `ImageMobject`.
//! - **`Camera` is produced by Lumen, not Marionette.** A [`Stage`] does not own
//!   the camera, so [`Revisions::read`] deliberately leaves that axis at zero.
//!   The capture boundary composes it with
//!   `with_camera(camera.revision())`; [`crate::Camera::revision`] covers frame,
//!   projection, resolution, sampling, background, and movable-light state.
//!
//! ## The counting rule
//!
//! Revisions are compared, never ordered: an artifact is stale iff any axis it
//! depends on differs from the axis value it was built at. Monotonicity is
//! therefore not required, and this matters — `RecordBuffer::swap_in` bumps
//! every counter past the old ones on a resize, and a *different* buffer
//! generation can legitimately restart. [`Revisions`] folds the storage identity
//! into the axes for exactly that reason.

use fmn_mobject::{Mob, Stage, Uniforms};

/// One of §10.8's seven revision axes.
///
/// Named rather than implicit so that a dependency can be *stated* — "the
/// arc-length table depends on `Geometry` alone" — and then tested, which is the
/// acceptance criterion this whole module exists to make checkable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Axis {
    /// Point count and family shape: the number of curves and who draws.
    Topology,
    /// Point positions in object space.
    Geometry,
    /// The object→world placement.
    Transform,
    /// Colours, widths, and the per-object uniforms that reach a shader.
    Style,
    /// Draw order and visibility: `z_index`, the scene list, batching.
    Order,
    /// Raster image content bound to a renderable.
    Image,
    /// The camera and its projection.
    CameraProjection,
}

impl Axis {
    /// Every axis, in declaration order — the order [`Revisions::get`] indexes.
    pub const ALL: [Axis; 7] = [
        Axis::Topology,
        Axis::Geometry,
        Axis::Transform,
        Axis::Style,
        Axis::Order,
        Axis::Image,
        Axis::CameraProjection,
    ];

    /// A short stable name, for traces and golden snapshots.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Axis::Topology => "topology",
            Axis::Geometry => "geometry",
            Axis::Transform => "transform",
            Axis::Style => "style",
            Axis::Order => "order",
            Axis::Image => "image",
            Axis::CameraProjection => "camera",
        }
    }
}

/// One renderable's revision vector: seven independent counters.
///
/// `Copy` and seven `u64`s wide, because it is compared once per resource per
/// frame and stored in every retained artifact. Equality is not the useful
/// operation — [`differs_on`] is, since an artifact depends on a *subset* of the
/// axes and must not be invalidated by the others.
///
/// [`differs_on`]: Revisions::differs_on
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Revisions {
    /// [`Axis::Topology`].
    pub topology: u64,
    /// [`Axis::Geometry`].
    pub geometry: u64,
    /// [`Axis::Transform`].
    pub transform: u64,
    /// [`Axis::Style`].
    pub style: u64,
    /// [`Axis::Order`].
    pub order: u64,
    /// [`Axis::Image`].
    pub image: u64,
    /// [`Axis::CameraProjection`].
    pub camera: u64,
}

impl Revisions {
    /// The value on one axis.
    #[must_use]
    pub fn get(&self, axis: Axis) -> u64 {
        match axis {
            Axis::Topology => self.topology,
            Axis::Geometry => self.geometry,
            Axis::Transform => self.transform,
            Axis::Style => self.style,
            Axis::Order => self.order,
            Axis::Image => self.image,
            Axis::CameraProjection => self.camera,
        }
    }

    /// Does any axis in `axes` differ between these two vectors?
    ///
    /// The one question a retained artifact ever asks. Passing the *exact* set
    /// an artifact depends on is what makes §10.8's promises true: give the
    /// arc-length table `[Geometry]` and a recolour cannot touch it, whatever
    /// else moved.
    #[must_use]
    pub fn differs_on(&self, other: &Revisions, axes: &[Axis]) -> bool {
        axes.iter().any(|&a| self.get(a) != other.get(a))
    }

    /// Which axes differ, in [`Axis::ALL`] order — for traces, and for the tests
    /// that assert a mutation touched *exactly* one axis.
    #[must_use]
    pub fn diff(&self, other: &Revisions) -> Vec<Axis> {
        Axis::ALL
            .into_iter()
            .filter(|&a| self.get(a) != other.get(a))
            .collect()
    }

    /// Read the five axes a `Stage` can answer for one mobject.
    ///
    /// [`Axis::Image`] and [`Axis::CameraProjection`] are left at their current
    /// values because neither is owned by Marionette. The capture boundary
    /// composes them with [`Revisions::with_image`] and
    /// `with_camera(camera.revision())`.
    /// Returns `None` for a stale or foreign handle, which is not an error: a
    /// deleted mobject has no revisions, and the plan drops it.
    #[must_use]
    pub fn read(stage: &Stage, mob: Mob) -> Option<Revisions> {
        let entry = stage.try_get(mob).ok()?;
        let buffer = &entry.buffer;

        // Fold the storage identity into every axis. A resize swaps the whole
        // generation and restarts nothing — but a *snapshot restore* can install
        // an older generation whose counters are lower than the ones an artifact
        // was built at, and comparison-not-ordering only saves us if the
        // identity is in the key.
        let generation = buffer.storage_id() as u64;

        let field = |name: &str| buffer.field_revision(name).unwrap_or(0);

        // Topology: how many records, and how many objects draw beneath this
        // one. Both change the *shape* of the compiled path without changing any
        // coordinate, which is exactly the distinction the axis exists for.
        let topology = mix(&[
            generation,
            buffer.len() as u64,
            entry.submobjects().len() as u64,
        ]);

        // Geometry: point positions, plus the two fields Marionette derives from
        // them. `joint_angle` is refreshed by `set_points` and `base_normal` is a
        // function of the points, so treating either as style would let a
        // recolour appear to move a curve.
        let geometry = mix(&[
            generation,
            field("point"),
            field("joint_angle"),
            field("base_normal"),
        ]);
        let transform = mix(&[generation, entry.placement_revision()]);

        // Style: everything that reaches a shader without moving a vertex,
        // including the per-object uniforms (§8.5 makes those part of the batch
        // key, so they are style by construction).
        let style = mix(&[
            generation,
            field("stroke_rgba"),
            field("stroke_width"),
            field("fill_rgba"),
            field("fill_border_width"),
            uniform_key(entry.uniforms()),
        ]);

        // Order: where this object sits in the painter's sequence.
        let order = mix(&[
            generation,
            stage.z_index(mob) as i64 as u64,
            entry.parents().len() as u64,
        ]);

        Some(Revisions {
            topology,
            geometry,
            transform,
            style,
            order,
            image: 0,
            camera: 0,
        })
    }

    /// Fold all seven axes into one value.
    ///
    /// For consumers that key on *any* change rather than on a declared subset —
    /// the tile cache, whose §10.8 key is "resource revisions" without
    /// qualification, because a tile's bytes can depend on any axis.
    #[must_use]
    pub fn fold(&self) -> u64 {
        mix(&[
            self.topology,
            self.geometry,
            self.transform,
            self.style,
            self.order,
            self.image,
            self.camera,
        ])
    }

    /// With [`Axis::Image`] set.
    #[must_use]
    pub fn with_image(mut self, revision: u64) -> Revisions {
        self.image = revision;
        self
    }

    /// With [`Axis::CameraProjection`] set.
    #[must_use]
    pub fn with_camera(mut self, revision: u64) -> Revisions {
        self.camera = revision;
        self
    }
}

/// A retained artifact's staleness key: the value it was built at, plus the
/// axes it actually depends on.
///
/// The type exists so a dependency is declared once, at construction, and cannot
/// drift from the check — the failure mode being an artifact that compares the
/// wrong axes and is either rebuilt every frame or never.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    built_at: Revisions,
    axes: Vec<Axis>,
}

impl Dependency {
    /// Declare that an artifact built at `built_at` depends on `axes`.
    #[must_use]
    pub fn new(built_at: Revisions, axes: &[Axis]) -> Dependency {
        let mut axes = axes.to_vec();
        axes.sort_unstable();
        axes.dedup();
        Dependency { built_at, axes }
    }

    /// The axes this artifact depends on.
    #[must_use]
    pub fn axes(&self) -> &[Axis] {
        &self.axes
    }

    /// The revision vector the artifact was built at.
    #[must_use]
    pub fn built_at(&self) -> &Revisions {
        &self.built_at
    }

    /// Is the artifact stale against `now`?
    #[must_use]
    pub fn is_stale(&self, now: &Revisions) -> bool {
        self.built_at.differs_on(now, &self.axes)
    }

    /// Record that the artifact has been rebuilt at `now`.
    pub fn refresh(&mut self, now: Revisions) {
        self.built_at = now;
    }
}

/// Fold a list of counters into one, order-sensitively.
///
/// Shared with the tile cache (`crate::cache`), which folds the same way for the
/// same reason: it needs a cheap, stable, order-sensitive function of a handful
/// of integers, not a content address.
///
/// FNV-1a over the little-endian bytes. It needs to be a *function*, cheap, and
/// stable across runs and platforms — not cryptographic: a collision costs a
/// missed rebuild only if two different states hash equal, and the inputs are
/// monotone counters from one process rather than adversarial input. fmn-hash is
/// the right tool for content addressing (§6.7) and the wrong one for a
/// per-resource per-frame comparison.
pub(crate) fn mix(values: &[u64]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for v in values {
        for b in v.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(PRIME);
        }
    }
    h
}

/// The uniforms' contribution to the style axis.
///
/// Floats fold in **by bits**, matching `fmn_mobject::order::BatchKey`'s bitwise
/// comparison. §8.5 makes batching semantics, so a style key that merged
/// `0.1 + 0.2` with `0.3` would silently change how many draw calls a scene
/// takes — a difference the Parity Ledger would have to explain.
fn uniform_key(u: &Uniforms) -> u64 {
    let mut parts: Vec<u64> = Vec::with_capacity(24);
    parts.push(u.is_fixed_in_frame.to_bits());
    parts.extend(u.shading.iter().map(|v| v.to_bits()));
    for plane in &u.clip_planes {
        parts.extend(plane.iter().map(|v| v.to_bits()));
    }
    parts.push(u.anti_alias_width.to_bits());
    parts.push(u.joint_type.to_code() as u64);
    parts.push(u.flat_stroke as u64);
    parts.push(u.scale_stroke_with_zoom as u64);
    parts.push(u.stroke_behind as u64);
    parts.push(u.depth_test as u64);
    parts.push(u.use_winding_fill as u64);
    mix(&parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_mobject::{Mobject, RecordBuffer, RecordSchema, Stage};

    /// A stage with one VMobject-shaped mobject holding a triangle.
    fn vmobject(points: &[[f64; 3]]) -> Mobject {
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len());
        for (i, p) in points.iter().enumerate() {
            buffer.write(i, "point", &[p[0] as f32, p[1] as f32, p[2] as f32]);
        }
        Mobject::from_buffer(buffer)
    }

    fn one_mobject() -> (Stage, Mob) {
        let mut stage = Stage::new();
        let mob = stage.add(vmobject(&[
            [0.0, 0.0, 0.0],
            [1.0, 0.5, 0.0],
            [2.0, 0.0, 0.0],
            [1.0, -0.5, 0.0],
            [0.0, 0.0, 0.0],
        ]));
        stage.add_to_scene(mob).expect("live handle");
        (stage, mob)
    }

    fn read(stage: &Stage, mob: Mob) -> Revisions {
        Revisions::read(stage, mob).expect("live handle")
    }

    #[test]
    fn every_axis_is_named_exactly_once() {
        let mut names: Vec<&str> = Axis::ALL.iter().map(|a| a.name()).collect();
        names.sort_unstable();
        let unique = {
            let mut n = names.clone();
            n.dedup();
            n
        };
        assert_eq!(names, unique, "axis names must be distinct");
        assert_eq!(Axis::ALL.len(), 7, "§10.8 names seven axes");
    }

    #[test]
    fn a_recolour_touches_style_and_nothing_else() {
        // The headline promise of §10.8: "a color change does not regenerate
        // curve coefficients".
        let (mut stage, mob) = one_mobject();
        let before = read(&stage, mob);
        let entry = stage.get_mut(mob).expect("live");
        entry
            .buffer
            .write(0, "stroke_rgba", &[1.0, 0.0, 0.0, 1.0])
            .then_some(())
            .expect("field exists");
        let after = read(&stage, mob);
        assert_eq!(
            after.diff(&before),
            vec![Axis::Style],
            "a recolour must touch the style axis alone"
        );
    }

    #[test]
    fn moving_an_object_space_point_touches_geometry_alone() {
        let (mut stage, mob) = one_mobject();
        let before = read(&stage, mob);
        let entry = stage.get_mut(mob).expect("live");
        entry
            .buffer
            .write(1, "point", &[1.0, 0.75, 0.0])
            .then_some(())
            .expect("field exists");
        let after = read(&stage, mob);
        assert_eq!(after.diff(&before), vec![Axis::Geometry]);
    }

    #[test]
    fn translating_touches_transform_alone() {
        let (mut stage, mob) = one_mobject();
        let before = read(&stage, mob);
        let point_revision = stage.get(mob).expect("live").buffer.field_revision("point");
        stage.shift(mob, [3.0, -2.0, 0.5]);
        let after = read(&stage, mob);
        assert_eq!(after.diff(&before), vec![Axis::Transform]);
        assert_eq!(
            stage.get(mob).expect("live").buffer.field_revision("point"),
            point_revision,
            "placement must not masquerade as object-space geometry"
        );
    }

    #[test]
    fn a_uniform_change_is_a_style_change() {
        // §8.5 puts uniforms in the batch key, so they are style by
        // construction — and a stroke that starts drawing behind its fill must
        // not look like a geometry edit.
        let (mut stage, mob) = one_mobject();
        let before = read(&stage, mob);
        stage
            .get_mut(mob)
            .expect("live")
            .uniforms_mut()
            .stroke_behind = true;
        let after = read(&stage, mob);
        assert_eq!(after.diff(&before), vec![Axis::Style]);
    }

    #[test]
    fn a_uniform_float_changing_by_one_bit_is_a_style_change() {
        // Bitwise, matching BatchKey. If this ever folds two distinct widths
        // together, batching silently changes and §16's ledger has to explain it.
        let (mut stage, mob) = one_mobject();
        let before = read(&stage, mob);
        stage
            .get_mut(mob)
            .expect("live")
            .uniforms_mut()
            .anti_alias_width = 1.5 + f64::EPSILON;
        let after = read(&stage, mob);
        assert_eq!(after.diff(&before), vec![Axis::Style]);
    }

    #[test]
    fn a_z_index_change_touches_order_alone() {
        let (mut stage, mob) = one_mobject();
        let before = read(&stage, mob);
        stage.set_z_index(mob, 3, false);
        let after = read(&stage, mob);
        assert_eq!(after.diff(&before), vec![Axis::Order]);
    }

    #[test]
    fn a_resize_invalidates_every_axis_and_that_is_correct() {
        // The one mutation that legitimately moves everything, and it is worth
        // stating rather than discovering. `RecordBuffer::swap_in` — every
        // resize and every `assign_from` — installs a new storage generation and
        // bumps every field counter past the old ones (§8.2: views detach, the
        // mirror fully resyncs).
        //
        // That is not a coarseness this module should route around. A resize
        // renumbers records, so *every* per-record derived table is invalid
        // afterwards, including the style table: the same colours now live at
        // different indices. Reporting a resize as "topology only" would be
        // precise and wrong.
        let (mut stage, mob) = one_mobject();
        let before = read(&stage, mob);
        stage.get_mut(mob).expect("live").buffer.resize(9);
        let after = read(&stage, mob);
        assert_eq!(
            after.diff(&before),
            vec![
                Axis::Topology,
                Axis::Geometry,
                Axis::Transform,
                Axis::Style,
                Axis::Order
            ],
            "a resize must stale every axis a Stage derives"
        );
        // The two axes Marionette does not produce stay exactly where the caller
        // left them — a resize is not a camera move.
        assert_eq!(after.image, before.image);
        assert_eq!(after.camera, before.camera);
    }

    #[test]
    fn adding_a_submobject_is_a_topology_change() {
        let (mut stage, mob) = one_mobject();
        let child = stage.add(vmobject(&[[0.0, 0.0, 0.0]]));
        let before = read(&stage, mob);
        stage.attach(mob, child).expect("acyclic");
        let after = read(&stage, mob);
        assert!(after.diff(&before).contains(&Axis::Topology));
    }

    #[test]
    fn reading_a_stale_handle_yields_nothing_rather_than_a_panic() {
        let (mut stage, mob) = one_mobject();
        stage.delete(mob).expect("live handle");
        assert!(Revisions::read(&stage, mob).is_none());
    }

    #[test]
    fn a_dependency_ignores_the_axes_it_did_not_declare() {
        // The load-bearing property: an artifact keyed on geometry alone must
        // survive a recolour, whatever else the frame did.
        let (mut stage, mob) = one_mobject();
        let built_at = read(&stage, mob);
        let mut dep = Dependency::new(built_at, &[Axis::Geometry]);
        assert!(!dep.is_stale(&built_at));

        let entry = stage.get_mut(mob).expect("live");
        entry.buffer.write(0, "fill_rgba", &[0.0, 1.0, 0.0, 1.0]);
        let recoloured = read(&stage, mob);
        assert_ne!(recoloured.style, built_at.style, "the recolour did happen");
        assert!(
            !dep.is_stale(&recoloured),
            "style must not stale a geometry key"
        );

        let entry = stage.get_mut(mob).expect("live");
        entry.buffer.write(2, "point", &[2.5, 0.0, 0.0]);
        let moved = read(&stage, mob);
        assert!(dep.is_stale(&moved), "geometry must stale a geometry key");

        dep.refresh(moved);
        assert!(!dep.is_stale(&moved));

        stage.shift(mob, [4.0, 0.0, 0.0]);
        let translated = read(&stage, mob);
        assert!(
            !dep.is_stale(&translated),
            "placement must not stale a geometry-only artifact"
        );
    }

    #[test]
    fn image_is_external_and_camera_uses_the_lumen_revision() {
        use crate::Camera;

        // Neither producer belongs to Marionette. A plausible zero would be a
        // lie — a camera move that never bumps the axis is a stale frame.
        let (stage, mob) = one_mobject();
        let base = read(&stage, mob);
        assert_eq!(base.image, 0);
        assert_eq!(base.camera, 0);
        let mut camera = Camera::default();
        let composed = base.with_image(7).with_camera(camera.revision());
        assert_eq!(
            composed.diff(&base),
            vec![Axis::Image, Axis::CameraProjection]
        );
        // And composing them leaves the derived axes exactly alone.
        assert_eq!(composed.geometry, base.geometry);
        assert_eq!(composed.style, base.style);

        camera
            .frame_mut()
            .set_center([1.0, 0.0, 0.0])
            .expect("finite camera center");
        let moved = base.with_image(7).with_camera(camera.revision());
        assert_eq!(moved.diff(&composed), vec![Axis::CameraProjection]);
    }

    #[test]
    fn a_snapshot_restore_is_visible_even_though_counters_may_go_backwards() {
        // Revisions are compared, never ordered. A restore can install a buffer
        // generation whose counters are *lower* than the ones an artifact was
        // built at, so the storage identity is folded into every axis.
        let (mut stage, mob) = one_mobject();
        let snapshot = stage.snapshot();
        let at_snapshot = read(&stage, mob);

        let entry = stage.get_mut(mob).expect("live");
        entry.buffer.write(1, "point", &[9.0, 9.0, 0.0]);
        let moved = read(&stage, mob);
        assert_ne!(moved.geometry, at_snapshot.geometry);

        stage.restore(&snapshot);
        let restored = read(&stage, mob);
        let dep = Dependency::new(moved, &[Axis::Geometry]);
        assert!(
            dep.is_stale(&restored),
            "an artifact built after the snapshot must be stale once it is undone"
        );
    }
}
