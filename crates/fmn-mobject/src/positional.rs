//! The structural/positional surface (§8.4, §1.1): bounding boxes with
//! automatic invalidation, the critical-point getters, and the full positional
//! API — all as [`Stage`] methods that recurse over a handle's family, exactly
//! as the Reference's `Mobject` methods recurse over `get_family()`.
//!
//! # Bounding boxes and invalidation
//!
//! Every positional operation is defined against the bounding box (§8.4), so the
//! box must be both correct and cheap. Ours is **lazily recomputed and keyed by
//! a subtree signature**: [`Stage::get_bounding_box`] folds the signature of
//! every family member's point-field revision and identity; if it matches the
//! cache the stored box is returned, otherwise the box is recomputed from the
//! `point` records in f64. Because a point write bumps the RecordBuffer's field
//! revision and a structural change alters the family list, the box invalidates
//! **automatically through any channel** — a raw `write` on the record buffer, a
//! positional transform, or an `attach`/`detach` — with no explicit dirty-flag
//! bookkeeping to get wrong. This is the §8.4 dirty-flag requirement expressed
//! as the D-20 lazy-revisioned-mirror pattern already used for render mirrors.
//!
//! Where the Reference transforms the cached box in place to avoid a recompute
//! (its `works_on_bounding_box` flag), we simply recompute on next read — the
//! result is identical (min/max commute with the affine point transform) and,
//! for a negative-scale factor, ours is the *more* correct box.
//!
//! # Numeric doctrine
//!
//! Points are stored as f32 records but all positional math is done in f64
//! (`Semantic`, §6.1); parity against the Reference is checked at loose f32
//! tolerance (§16.4).

use fmn_core::constants::{DOWN, FRAME_X_RADIUS, FRAME_Y_RADIUS, IN, LEFT, ORIGIN, OUT, RIGHT, UP};
use fmn_core::types::Vec3;

use crate::bbox::{BoundingBox, BoxAccum};
use crate::stage::{Mob, Stage};
use crate::uniforms::Uniforms;

/// A positional target: another mobject (positioned by its critical point) or a
/// literal point. Methods that accept "a mobject or a point" take
/// `impl Into<PosTarget>`, so both `stage.move_to(a, b, ORIGIN)` and
/// `stage.move_to(a, [1.0, 0.0, 0.0], ORIGIN)` read naturally.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PosTarget {
    /// Align against another mobject's bounding box.
    Mob(Mob),
    /// Align against a fixed point.
    Point(Vec3),
}

impl From<Mob> for PosTarget {
    fn from(m: Mob) -> Self {
        Self::Mob(m)
    }
}

impl From<Vec3> for PosTarget {
    fn from(p: Vec3) -> Self {
        Self::Point(p)
    }
}

// --- small f64 vector helpers (kept local; the geometry kernel's Vec3 math is
// pub(crate)-internal to fmn-geom and not a dependency here) ---

#[inline]
fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn scaled(a: Vec3, s: f64) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

#[inline]
fn neg(a: Vec3) -> Vec3 {
    [-a[0], -a[1], -a[2]]
}

#[inline]
fn hadamard(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] * b[0], a[1] * b[1], a[2] * b[2]]
}

/// NumPy `sign`: `-0.0` and `+0.0` both map to `0.0` (unlike `f64::signum`).
#[inline]
fn np_sign(a: Vec3) -> Vec3 {
    let s = |x: f64| {
        if x > 0.0 {
            1.0
        } else if x < 0.0 {
            -1.0
        } else {
            0.0
        }
    };
    [s(a[0]), s(a[1]), s(a[2])]
}

#[inline]
fn np_abs(a: Vec3) -> Vec3 {
    [a[0].abs(), a[1].abs(), a[2].abs()]
}

/// The Reference's per-scalar minimum scale factor (`scale`'s `min_scale_factor`).
const MIN_SCALE_FACTOR: f64 = 1e-8;

impl Stage {
    // ---------------------------------------------------------- bounding box

    /// A subtree signature: any point write (revision bump), resize, or
    /// structural change alters it, which is what drives cache invalidation.
    fn bbox_signature(&self, mob: Mob) -> u64 {
        // FNV-1a over the family. In-memory only; never serialized, so no
        // cross-platform determinism obligation (that is fmn-hash's job).
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut h = OFFSET;
        let mut mix = |v: u64| h = (h ^ v).wrapping_mul(PRIME);
        for m in self.family(mob) {
            mix(m.bits());
            if let Some(e) = self.get(m) {
                mix(e.buffer.field_revision("point").unwrap_or(0));
                mix(e.buffer.len() as u64);
            }
        }
        h
    }

    /// Recompute the box from the family's `point` records (in f64).
    fn compute_bounding_box(&self, mob: Mob) -> BoundingBox {
        let mut acc = BoxAccum::new();
        for m in self.family(mob) {
            if let Some(e) = self.get(m)
                && let Some(col) = e.buffer.read_column("point")
            {
                let (tris, _rem) = col.as_chunks::<3>();
                for tri in tris {
                    acc.push([f64::from(tri[0]), f64::from(tri[1]), f64::from(tri[2])]);
                }
            }
        }
        acc.finish()
    }

    /// The bounding box of `mob`'s whole family (§8.4). Lazily recomputed; see
    /// the module docs for the invalidation model.
    #[must_use]
    pub fn get_bounding_box(&self, mob: Mob) -> BoundingBox {
        let sig = self.bbox_signature(mob);
        let Some(e) = self.get(mob) else {
            return BoundingBox::ZERO;
        };
        let cached = {
            let c = e.bbox_cell().borrow();
            (c.signature == Some(sig)).then_some(c.value)
        };
        if let Some(v) = cached {
            return v;
        }
        let value = self.compute_bounding_box(mob);
        let mut c = e.bbox_cell().borrow_mut();
        c.signature = Some(sig);
        c.value = value;
        c.materializations += 1;
        value
    }

    /// How many times `mob`'s box has actually been recomputed — the laziness
    /// test hook (mirrors `MirrorSet::materializations`).
    #[must_use]
    pub fn bbox_materializations(&self, mob: Mob) -> u64 {
        self.get(mob)
            .map_or(0, |e| e.bbox_cell().borrow().materializations)
    }

    // ---------------------------------------------------- critical-point getters

    /// The critical point of `mob`'s box in a direction (Reference
    /// `get_bounding_box_point`).
    #[must_use]
    pub fn get_bounding_box_point(&self, mob: Mob, dir: Vec3) -> Vec3 {
        self.get_bounding_box(mob).point(dir)
    }

    /// The center of `mob`'s box.
    #[must_use]
    pub fn get_center(&self, mob: Mob) -> Vec3 {
        self.get_bounding_box(mob).center()
    }

    /// Alias of [`get_bounding_box_point`](Self::get_bounding_box_point).
    #[must_use]
    pub fn get_edge_center(&self, mob: Mob, dir: Vec3) -> Vec3 {
        self.get_bounding_box_point(mob, dir)
    }

    /// Alias of [`get_bounding_box_point`](Self::get_bounding_box_point).
    #[must_use]
    pub fn get_corner(&self, mob: Mob, dir: Vec3) -> Vec3 {
        self.get_bounding_box_point(mob, dir)
    }

    /// Left / right / top / bottom / zenith / nadir edge centers.
    #[must_use]
    pub fn get_left(&self, mob: Mob) -> Vec3 {
        self.get_edge_center(mob, LEFT)
    }
    /// See [`get_left`](Self::get_left).
    #[must_use]
    pub fn get_right(&self, mob: Mob) -> Vec3 {
        self.get_edge_center(mob, RIGHT)
    }
    /// See [`get_left`](Self::get_left).
    #[must_use]
    pub fn get_top(&self, mob: Mob) -> Vec3 {
        self.get_edge_center(mob, UP)
    }
    /// See [`get_left`](Self::get_left).
    #[must_use]
    pub fn get_bottom(&self, mob: Mob) -> Vec3 {
        self.get_edge_center(mob, DOWN)
    }
    /// See [`get_left`](Self::get_left).
    #[must_use]
    pub fn get_zenith(&self, mob: Mob) -> Vec3 {
        self.get_edge_center(mob, OUT)
    }
    /// See [`get_left`](Self::get_left).
    #[must_use]
    pub fn get_nadir(&self, mob: Mob) -> Vec3 {
        self.get_edge_center(mob, IN)
    }

    /// The coordinate along `dim` at the box's critical point in `dir`.
    #[must_use]
    pub fn get_coord(&self, mob: Mob, dim: usize, dir: Vec3) -> f64 {
        self.get_bounding_box_point(mob, dir)[dim]
    }
    /// x / y / z at the box center (`get_coord` with `ORIGIN`).
    #[must_use]
    pub fn get_x(&self, mob: Mob) -> f64 {
        self.get_coord(mob, 0, ORIGIN)
    }
    /// See [`get_x`](Self::get_x).
    #[must_use]
    pub fn get_y(&self, mob: Mob) -> f64 {
        self.get_coord(mob, 1, ORIGIN)
    }
    /// See [`get_x`](Self::get_x).
    #[must_use]
    pub fn get_z(&self, mob: Mob) -> f64 {
        self.get_coord(mob, 2, ORIGIN)
    }

    /// Extent of `mob` along an axis.
    #[must_use]
    pub fn length_over_dim(&self, mob: Mob, dim: usize) -> f64 {
        self.get_bounding_box(mob).length_over_dim(dim)
    }
    /// Width (x extent).
    #[must_use]
    pub fn get_width(&self, mob: Mob) -> f64 {
        self.length_over_dim(mob, 0)
    }
    /// Height (y extent).
    #[must_use]
    pub fn get_height(&self, mob: Mob) -> f64 {
        self.length_over_dim(mob, 1)
    }
    /// Depth (z extent).
    #[must_use]
    pub fn get_depth(&self, mob: Mob) -> f64 {
        self.length_over_dim(mob, 2)
    }

    /// The first `point` record (Reference `get_start`), or `None` if empty.
    #[must_use]
    pub fn get_start(&self, mob: Mob) -> Option<Vec3> {
        let e = self.get(mob)?;
        let v = e.buffer.read(0, "point")?;
        Some([f64::from(v[0]), f64::from(v[1]), f64::from(v[2])])
    }

    /// The last `point` record (Reference `get_end`), or `None` if empty.
    #[must_use]
    pub fn get_end(&self, mob: Mob) -> Option<Vec3> {
        let e = self.get(mob)?;
        let n = e.buffer.len();
        let v = e.buffer.read(n.checked_sub(1)?, "point")?;
        Some([f64::from(v[0]), f64::from(v[1]), f64::from(v[2])])
    }

    // ----------------------------------------------------- the transform core

    /// Resolve the pivot the Reference's `apply_points_function` uses: an
    /// explicit `about_point` wins; else `about_edge` (if given) resolves to the
    /// box's critical point there; else there is no pivot (transform in place).
    fn resolve_pivot(
        &self,
        mob: Mob,
        about_point: Option<Vec3>,
        about_edge: Option<Vec3>,
    ) -> Option<Vec3> {
        match (about_point, about_edge) {
            (Some(p), _) => Some(p),
            (None, Some(e)) => Some(self.get_bounding_box_point(mob, e)),
            (None, None) => None,
        }
    }

    /// Apply `f` to every point of `mob`'s family, optionally about `pivot`
    /// (`f(p - pivot) + pivot`). The bounding box invalidates automatically via
    /// the record revision bump.
    fn transform_points<F: Fn(Vec3) -> Vec3>(&mut self, mob: Mob, pivot: Option<Vec3>, f: F) {
        for m in self.family(mob) {
            let col = match self.get(m).and_then(|e| e.buffer.read_column("point")) {
                Some(c) if !c.is_empty() => c,
                _ => continue,
            };
            let mut out = Vec::with_capacity(col.len());
            let (tris, _rem) = col.as_chunks::<3>();
            for tri in tris {
                let p = [f64::from(tri[0]), f64::from(tri[1]), f64::from(tri[2])];
                let q = match pivot {
                    Some(pv) => add(f(sub(p, pv)), pv),
                    None => f(p),
                };
                out.push(q[0] as f32);
                out.push(q[1] as f32);
                out.push(q[2] as f32);
            }
            if let Some(e) = self.get_mut(m) {
                e.buffer.write_range("point", 0, &out);
            }
        }
    }

    /// The Reference's `apply_points_function` made public: apply `f` to
    /// every family point about the resolved pivot (an explicit
    /// `about_point` wins; else the box point at `about_edge`; both `None`
    /// transforms in place). `Mobject.apply_function`'s default is
    /// `about_point = ORIGIN` — pass `Some(ORIGIN)` for that surface.
    pub fn apply_points_function(
        &mut self,
        mob: Mob,
        f: impl Fn(Vec3) -> Vec3,
        about_point: Option<Vec3>,
        about_edge: Option<Vec3>,
    ) -> &mut Self {
        let pivot = self.resolve_pivot(mob, about_point, about_edge);
        self.transform_points(mob, pivot, f);
        self
    }

    /// Reference `rotate` (mobject.py): rotate the family about `axis` by
    /// `angle`, pivoting on `about_point` (or the box point at
    /// `about_edge`; both `None` defaults to the box center, the
    /// Reference's `about_edge = ORIGIN`).
    pub fn rotate(
        &mut self,
        mob: Mob,
        angle: f64,
        axis: Vec3,
        about_point: Option<Vec3>,
        about_edge: Option<Vec3>,
    ) -> &mut Self {
        let m = fmn_geom::rotation_matrix(angle, axis);
        let edge = if about_point.is_none() && about_edge.is_none() {
            Some(ORIGIN)
        } else {
            about_edge
        };
        let pivot = self.resolve_pivot(mob, about_point, edge);
        self.transform_points(mob, pivot, move |p| {
            [
                m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2],
                m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2],
                m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2],
            ]
        });
        self
    }

    /// Reference `match_points` (mobject.py:311): resize `mob`'s records
    /// to `source`'s count (order-preserving) and copy every pointlike
    /// column. Applies member-for-member is the *caller's* concern — this
    /// is the single-entry rule, exactly the Reference's.
    pub fn match_points(&mut self, mob: Mob, source: Mob) {
        let (len, columns): (usize, Vec<(String, Vec<f32>)>) = match self.get(source) {
            Some(entry) => (
                entry.buffer.len(),
                entry
                    .buffer
                    .schema()
                    .pointlike_keys()
                    .iter()
                    .filter_map(|k| entry.buffer.read_column(k).map(|c| (k.clone(), c)))
                    .collect(),
            ),
            None => return,
        };
        let Some(entry) = self.get_mut(mob) else {
            return;
        };
        if entry.buffer.len() != len {
            entry.buffer.resize_preserving_order(len);
        }
        for (field, column) in columns {
            entry.buffer.write_range(&field, 0, &column);
        }
    }

    // ---------------------------------------------------------- positional API

    /// Shift every point by `vector`.
    pub fn shift(&mut self, mob: Mob, vector: Vec3) -> &mut Self {
        self.transform_points(mob, None, |p| add(p, vector));
        self
    }

    /// Scale about the box center (Reference default `about_edge=ORIGIN`).
    pub fn scale(&mut self, mob: Mob, factor: f64) -> &mut Self {
        self.scale_about(mob, factor, None, Some(ORIGIN))
    }

    /// Scale about an explicit `about_point` or `about_edge` (the Reference's
    /// full `scale` surface). `factor` is clamped to `MIN_SCALE_FACTOR`.
    pub fn scale_about(
        &mut self,
        mob: Mob,
        factor: f64,
        about_point: Option<Vec3>,
        about_edge: Option<Vec3>,
    ) -> &mut Self {
        let factor = factor.max(MIN_SCALE_FACTOR);
        let pivot = self.resolve_pivot(mob, about_point, about_edge);
        self.transform_points(mob, pivot, move |p| scaled(p, factor));
        self
    }

    /// Stretch along one axis about the box center.
    pub fn stretch(&mut self, mob: Mob, factor: f64, dim: usize) -> &mut Self {
        self.stretch_about(mob, factor, dim, None, Some(ORIGIN))
    }

    /// Stretch along one axis about an explicit pivot.
    pub fn stretch_about(
        &mut self,
        mob: Mob,
        factor: f64,
        dim: usize,
        about_point: Option<Vec3>,
        about_edge: Option<Vec3>,
    ) -> &mut Self {
        let pivot = self.resolve_pivot(mob, about_point, about_edge);
        self.transform_points(mob, pivot, move |mut p| {
            p[dim] *= factor;
            p
        });
        self
    }

    /// Move the box center to the origin.
    pub fn center(&mut self, mob: Mob) -> &mut Self {
        let c = self.get_center(mob);
        self.shift(mob, neg(c))
    }

    /// Align a side/corner to the frame border with `buff` (Reference
    /// `align_on_border`, the engine of `to_edge`/`to_corner`).
    fn align_on_border(&mut self, mob: Mob, dir: Vec3, buff: f64) -> &mut Self {
        let radius = [FRAME_X_RADIUS, FRAME_Y_RADIUS, 0.0];
        let s = np_sign(dir);
        let target = hadamard(s, radius);
        let point_to_align = self.get_bounding_box_point(mob, dir);
        let mut shift_val = sub(sub(target, point_to_align), scaled(dir, buff));
        // Zero out axes the direction does not touch.
        shift_val = hadamard(shift_val, np_abs(s));
        self.shift(mob, shift_val)
    }

    /// Move `mob` against the frame edge `edge` with buffer `buff`.
    pub fn to_edge(&mut self, mob: Mob, edge: Vec3, buff: f64) -> &mut Self {
        self.align_on_border(mob, edge, buff)
    }

    /// Move `mob` into the frame corner `corner` with buffer `buff`.
    pub fn to_corner(&mut self, mob: Mob, corner: Vec3, buff: f64) -> &mut Self {
        self.align_on_border(mob, corner, buff)
    }

    /// Position `mob` next to a target, on the `direction` side, `buff` away,
    /// aligning along `aligned_edge` (Reference `next_to`, `coor_mask = 1`).
    pub fn next_to(
        &mut self,
        mob: Mob,
        target: impl Into<PosTarget>,
        direction: Vec3,
        buff: f64,
        aligned_edge: Vec3,
    ) -> &mut Self {
        let target_point = match target.into() {
            PosTarget::Mob(m) => self.get_bounding_box_point(m, add(aligned_edge, direction)),
            PosTarget::Point(p) => p,
        };
        let point_to_align = self.get_bounding_box_point(mob, sub(aligned_edge, direction));
        self.shift(
            mob,
            add(sub(target_point, point_to_align), scaled(direction, buff)),
        )
    }

    /// Move `mob` so its critical point at `aligned_edge` coincides with the
    /// target's (Reference `move_to`, `coor_mask = 1`).
    pub fn move_to(
        &mut self,
        mob: Mob,
        target: impl Into<PosTarget>,
        aligned_edge: Vec3,
    ) -> &mut Self {
        let target_point = match target.into() {
            PosTarget::Mob(m) => self.get_bounding_box_point(m, aligned_edge),
            PosTarget::Point(p) => p,
        };
        let point_to_align = self.get_bounding_box_point(mob, aligned_edge);
        self.shift(mob, sub(target_point, point_to_align))
    }

    /// Align `mob` to a target along the nonzero components of `direction`
    /// (Reference `align_to`).
    pub fn align_to(
        &mut self,
        mob: Mob,
        target: impl Into<PosTarget>,
        direction: Vec3,
    ) -> &mut Self {
        let point = match target.into() {
            PosTarget::Mob(m) => self.get_bounding_box_point(m, direction),
            PosTarget::Point(p) => p,
        };
        for dim in 0..3 {
            if direction[dim] != 0.0 {
                self.set_coord(mob, point[dim], dim, direction);
            }
        }
        self
    }

    /// Set the coordinate along `dim` (at the box's critical point in `dir`) to
    /// `value` by shifting (Reference `set_coord`).
    pub fn set_coord(&mut self, mob: Mob, value: f64, dim: usize, dir: Vec3) -> &mut Self {
        let curr = self.get_coord(mob, dim, dir);
        let mut shift_vect = [0.0; 3];
        shift_vect[dim] = value - curr;
        self.shift(mob, shift_vect)
    }

    /// Set the x/y/z of the box center.
    pub fn set_x(&mut self, mob: Mob, x: f64) -> &mut Self {
        self.set_coord(mob, x, 0, ORIGIN)
    }
    /// See [`set_x`](Self::set_x).
    pub fn set_y(&mut self, mob: Mob, y: f64) -> &mut Self {
        self.set_coord(mob, y, 1, ORIGIN)
    }
    /// See [`set_x`](Self::set_x).
    pub fn set_z(&mut self, mob: Mob, z: f64) -> &mut Self {
        self.set_coord(mob, z, 2, ORIGIN)
    }

    /// Rescale so the extent along `dim` becomes `length`, by scaling (uniform)
    /// or stretching (single-axis). No-op if the current extent is zero.
    pub fn rescale_to_fit(
        &mut self,
        mob: Mob,
        length: f64,
        dim: usize,
        stretch: bool,
    ) -> &mut Self {
        let old = self.length_over_dim(mob, dim);
        if old == 0.0 {
            return self;
        }
        if stretch {
            self.stretch(mob, length / old, dim)
        } else {
            self.scale(mob, length / old)
        }
    }

    /// Set the width (uniform scale unless `stretch`).
    pub fn set_width(&mut self, mob: Mob, width: f64, stretch: bool) -> &mut Self {
        self.rescale_to_fit(mob, width, 0, stretch)
    }
    /// Set the height (uniform scale unless `stretch`).
    pub fn set_height(&mut self, mob: Mob, height: f64, stretch: bool) -> &mut Self {
        self.rescale_to_fit(mob, height, 1, stretch)
    }
    /// Set the depth (uniform scale unless `stretch`).
    pub fn set_depth(&mut self, mob: Mob, depth: f64, stretch: bool) -> &mut Self {
        self.rescale_to_fit(mob, depth, 2, stretch)
    }
    /// Stretch (single-axis) to fit a width.
    pub fn stretch_to_fit_width(&mut self, mob: Mob, width: f64) -> &mut Self {
        self.rescale_to_fit(mob, width, 0, true)
    }
    /// Stretch (single-axis) to fit a height.
    pub fn stretch_to_fit_height(&mut self, mob: Mob, height: f64) -> &mut Self {
        self.rescale_to_fit(mob, height, 1, true)
    }
    /// Stretch (single-axis) to fit a depth.
    pub fn stretch_to_fit_depth(&mut self, mob: Mob, depth: f64) -> &mut Self {
        self.rescale_to_fit(mob, depth, 2, true)
    }

    /// Match another mobject's extent along `dim` (uniform scale).
    pub fn match_dim_size(&mut self, mob: Mob, other: Mob, dim: usize) -> &mut Self {
        let length = self.length_over_dim(other, dim);
        self.rescale_to_fit(mob, length, dim, false)
    }
    /// Match another mobject's width.
    pub fn match_width(&mut self, mob: Mob, other: Mob) -> &mut Self {
        self.match_dim_size(mob, other, 0)
    }
    /// Match another mobject's height.
    pub fn match_height(&mut self, mob: Mob, other: Mob) -> &mut Self {
        self.match_dim_size(mob, other, 1)
    }
    /// Match another mobject's depth.
    pub fn match_depth(&mut self, mob: Mob, other: Mob) -> &mut Self {
        self.match_dim_size(mob, other, 2)
    }
    /// Match another mobject's coordinate along `dim` (at critical point `dir`).
    pub fn match_coord(&mut self, mob: Mob, other: Mob, dim: usize, dir: Vec3) -> &mut Self {
        let coord = self.get_coord(other, dim, dir);
        self.set_coord(mob, coord, dim, dir)
    }
    /// Match another mobject's x.
    pub fn match_x(&mut self, mob: Mob, other: Mob) -> &mut Self {
        self.match_coord(mob, other, 0, ORIGIN)
    }
    /// Match another mobject's y.
    pub fn match_y(&mut self, mob: Mob, other: Mob) -> &mut Self {
        self.match_coord(mob, other, 1, ORIGIN)
    }
    /// Match another mobject's z.
    pub fn match_z(&mut self, mob: Mob, other: Mob) -> &mut Self {
        self.match_coord(mob, other, 2, ORIGIN)
    }

    /// Lay out `mob`'s direct submobjects in a line, each `next_to` the last
    /// along `direction` with buffer `buff`; recenter the group if `center`
    /// (Reference `arrange`).
    pub fn arrange(&mut self, mob: Mob, direction: Vec3, buff: f64, center: bool) -> &mut Self {
        let subs = self
            .get(mob)
            .map(|e| e.submobjects().to_vec())
            .unwrap_or_default();
        for pair in subs.windows(2) {
            self.next_to(pair[1], PosTarget::Mob(pair[0]), direction, buff, ORIGIN);
        }
        if center {
            self.center(mob);
        }
        self
    }

    /// Lay out `mob`'s direct submobjects in a grid of `n_rows × n_cols`, each
    /// cell `x_unit`/`y_unit` apart (the widest/tallest submobject plus the
    /// buffer), filling rows first, then recentering (Reference
    /// `arrange_in_grid`, the common path).
    pub fn arrange_in_grid(
        &mut self,
        mob: Mob,
        n_rows: usize,
        n_cols: usize,
        h_buff: f64,
        v_buff: f64,
        aligned_edge: Vec3,
    ) -> &mut Self {
        let subs = self
            .get(mob)
            .map(|e| e.submobjects().to_vec())
            .unwrap_or_default();
        if subs.is_empty() || n_cols == 0 {
            return self;
        }
        let max_w = subs
            .iter()
            .map(|&s| self.get_width(s))
            .fold(0.0_f64, f64::max);
        let max_h = subs
            .iter()
            .map(|&s| self.get_height(s))
            .fold(0.0_f64, f64::max);
        let x_unit = h_buff + max_w;
        let y_unit = v_buff + max_h;
        let _ = n_rows; // rows are implied by fill-rows-first over n_cols
        for (index, &s) in subs.iter().enumerate() {
            let x = (index % n_cols) as f64;
            let y = (index / n_cols) as f64;
            self.move_to(s, ORIGIN, aligned_edge);
            self.shift(s, add(scaled(RIGHT, x * x_unit), scaled(DOWN, y * y_unit)));
        }
        self.center(mob)
    }

    // ------------------------------------------------------------- uniforms

    /// Read `mob`'s uniform inventory.
    #[must_use]
    pub fn uniforms(&self, mob: Mob) -> Option<&Uniforms> {
        self.get(mob).map(crate::stage::Entry::uniforms)
    }

    /// Mutable access to `mob`'s uniform inventory (scene code writes
    /// `mobject.uniforms` directly — the future Python bridge surface).
    pub fn uniforms_mut(&mut self, mob: Mob) -> Option<&mut Uniforms> {
        self.get_mut(mob).map(crate::stage::Entry::uniforms_mut)
    }

    /// Apply a mutation to the uniform inventory of `mob` (and its family if
    /// `recurse`), matching the Reference's family-recursing `set_uniform`.
    fn edit_uniforms(&mut self, mob: Mob, recurse: bool, f: impl Fn(&mut Uniforms)) -> &mut Self {
        let targets = if recurse { self.family(mob) } else { vec![mob] };
        for m in targets {
            if let Some(e) = self.get_mut(m) {
                f(e.uniforms_mut());
            }
        }
        self
    }

    /// Set the anti-alias width across the family (Reference
    /// `set_anti_alias_width`).
    pub fn set_anti_alias_width(&mut self, mob: Mob, width: f64, recurse: bool) -> &mut Self {
        self.edit_uniforms(mob, recurse, |u| u.anti_alias_width = width)
    }
    /// Set the `[reflectiveness, gloss, shadow]` shading triple.
    pub fn set_shading(&mut self, mob: Mob, shading: Vec3, recurse: bool) -> &mut Self {
        self.edit_uniforms(mob, recurse, |u| u.shading = shading)
    }
    /// Set the flat-stroke flag.
    pub fn set_flat_stroke(&mut self, mob: Mob, flat: bool, recurse: bool) -> &mut Self {
        self.edit_uniforms(mob, recurse, |u| u.flat_stroke = flat)
    }
    /// Set whether the stroke scales with zoom.
    pub fn set_scale_stroke_with_zoom(
        &mut self,
        mob: Mob,
        value: bool,
        recurse: bool,
    ) -> &mut Self {
        self.edit_uniforms(mob, recurse, |u| u.scale_stroke_with_zoom = value)
    }
    /// Set whether the stroke draws behind the fill.
    pub fn set_stroke_behind(&mut self, mob: Mob, behind: bool, recurse: bool) -> &mut Self {
        self.edit_uniforms(mob, recurse, |u| u.stroke_behind = behind)
    }
    /// Set the `is_fixed_in_frame` float mix (kept camera model).
    pub fn set_fixed_in_frame(&mut self, mob: Mob, value: f64, recurse: bool) -> &mut Self {
        self.edit_uniforms(mob, recurse, |u| u.is_fixed_in_frame = value)
    }
    /// C-7: accepted no-op. Records the flag across the family but changes no
    /// rendered bits.
    pub fn use_winding_fill(&mut self, mob: Mob, value: bool, recurse: bool) -> &mut Self {
        self.edit_uniforms(mob, recurse, |u| {
            u.use_winding_fill(value);
        })
    }

    /// C-2 / BN-07: read the **correct** uniform (not `flat_stroke`, as the
    /// Reference's buggy `get_scale_stroke_with_zoom` does).
    #[must_use]
    pub fn get_scale_stroke_with_zoom(&self, mob: Mob) -> bool {
        self.get(mob)
            .is_some_and(|e| e.uniforms().get_scale_stroke_with_zoom())
    }
}
