//! Family and point alignment — the Transform mechanism's data plane
//! (§9.4, fm-cye), ported from the pinned Reference exactly:
//!
//! - `align_data_and_family` runs **`align_family` first, then
//!   `align_data`** (mobject.py:1741 — the order is semantics).
//! - `align_family` reconciles submobject *counts* recursively via
//!   `add_n_more_submobjects` (mobject.py:1757/1777): a childless mobject
//!   grows single-point copies of itself at its center; a mobject with
//!   children pads by emitting each child once plus invisible
//!   (opacity-zero) copies, distributed by the Reference's
//!   `repeat_indices = arange(target) * curr // target` rule.
//! - `align_points` dispatches by record schema: plain records
//!   null-align by `resize_preserving_order` (mobject.py:1751); vmobject
//!   records (marked by a `joint_angle` field) run the bezier-aware
//!   algorithm (vectorized_mobject.py:964): subpaths split, sorted
//!   descending by polyline length, missing subpaths synthesized by
//!   folding the largest back on itself, per-pair curve counts equalized
//!   by greedy longest-curve insertion, subpath breaks re-marked with a
//!   repeated anchor, and joint angles refreshed.
//!
//! `push_self_into_submobjects` exists in the Reference but has no call
//! site in the pinned tree; it is deliberately not ported.

use fmn_core::types::Vec3;
use fmn_geom::{DEFAULT_TOLERANCE_FOR_POINT_EQUALITY, QuadPath};

use crate::StageError;
use crate::stage::{Mob, Stage};

/// Euclidean distance between two points (local helper; fmn-geom's vector
/// utilities are crate-private).
fn dist(a: Vec3, b: Vec3) -> f64 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

/// Polyline length over the raw shared-anchor run — the Reference's
/// subpath sort key sums consecutive point distances, handles included.
fn polyline_length(points: &[Vec3]) -> f64 {
    points.windows(2).map(|w| dist(w[0], w[1])).sum()
}

/// Whether this entry's records are vmobject-shaped: the `joint_angle`
/// field is the schema marker (present in `RecordSchema::vmobject`, absent
/// from the base layout).
fn is_vmobject_schema(stage: &Stage, mob: Mob) -> Result<bool, StageError> {
    Ok(stage
        .try_get(mob)?
        .buffer
        .schema()
        .offset("joint_angle")
        .is_some())
}

fn read_points(stage: &Stage, mob: Mob) -> Result<Vec<Vec3>, StageError> {
    let column = stage
        .try_get(mob)?
        .buffer
        .read_column("point")
        .ok_or(StageError::SchemaMismatch)?;
    Ok(column
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| [f64::from(c[0]), f64::from(c[1]), f64::from(c[2])])
        .collect())
}

/// Resize the whole record run preserving order, then write the new point
/// run and (for vmobject records) refreshed joint angles.
fn write_points(stage: &mut Stage, mob: Mob, points: &[Vec3]) -> Result<(), StageError> {
    let angles: Option<Vec<f64>> = if is_vmobject_schema(stage, mob)? {
        let path = QuadPath::from_points(points.to_vec()).map_err(StageError::Geometry)?;
        Some(path.joint_angles())
    } else {
        None
    };
    let entry = stage.get_mut(mob).ok_or(StageError::StaleHandle)?;
    entry.buffer.resize_preserving_order(points.len());
    #[allow(clippy::cast_possible_truncation)]
    let flat: Vec<f32> = points
        .iter()
        .flat_map(|p| p.iter().map(|v| *v as f32))
        .collect();
    entry.buffer.write_range("point", 0, &flat);
    if let Some(angles) = angles {
        #[allow(clippy::cast_possible_truncation)]
        let flat: Vec<f32> = angles.iter().map(|a| *a as f32).collect();
        entry.buffer.write_range("joint_angle", 0, &flat);
    }
    Ok(())
}

impl Stage {
    /// Reference `is_aligned_with` (mobject.py:1731): equal record counts,
    /// equal submobject counts, recursively.
    #[must_use]
    pub fn is_aligned_with(&self, a: Mob, b: Mob) -> bool {
        let (Some(ea), Some(eb)) = (self.get(a), self.get(b)) else {
            return false;
        };
        ea.buffer.len() == eb.buffer.len()
            && ea.submobjects().len() == eb.submobjects().len()
            && ea
                .submobjects()
                .to_vec()
                .iter()
                .zip(eb.submobjects().to_vec().iter())
                .all(|(&sa, &sb)| self.is_aligned_with(sa, sb))
    }

    /// Reference `align_family` (mobject.py:1757): pad the smaller side's
    /// submobject count with `add_n_more_submobjects`, then recurse over
    /// the zipped children.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`].
    pub fn align_family(&mut self, a: Mob, b: Mob) -> Result<(), StageError> {
        let n1 = self.try_get(a)?.submobjects().len();
        let n2 = self.try_get(b)?.submobjects().len();
        if n1 != n2 {
            self.add_n_more_submobjects(a, n2.saturating_sub(n1))?;
            self.add_n_more_submobjects(b, n1.saturating_sub(n2))?;
        }
        let ca = self.try_get(a)?.submobjects().to_vec();
        let cb = self.try_get(b)?.submobjects().to_vec();
        for (&sa, &sb) in ca.iter().zip(cb.iter()) {
            self.align_family(sa, sb)?;
        }
        Ok(())
    }

    /// Reference `add_n_more_submobjects` (mobject.py:1777). Childless:
    /// `n` single-point copies of `mob` at its center. Otherwise each
    /// existing child is kept and padded with invisible (opacity-zero)
    /// copies of itself, counts distributed by
    /// `repeat_indices = arange(curr + n) * curr // (curr + n)`.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`].
    pub fn add_n_more_submobjects(&mut self, mob: Mob, n: usize) -> Result<(), StageError> {
        if n == 0 {
            return Ok(());
        }
        let children = self.try_get(mob)?.submobjects().to_vec();
        let curr = children.len();
        if curr == 0 {
            let center = self.get_center(mob);
            for _ in 0..n {
                let copy = self.copy_family(mob)?;
                write_points(self, copy, &[center])?;
                self.attach(mob, copy).expect("fresh leaf copy is acyclic");
            }
            return Ok(());
        }
        let target = curr + n;
        let mut split_factors = vec![0usize; curr];
        for i in 0..target {
            split_factors[i * curr / target] += 1;
        }
        let mut new_children: Vec<Mob> = Vec::with_capacity(target);
        for (&child, &sf) in children.iter().zip(split_factors.iter()) {
            new_children.push(child);
            for _ in 1..sf {
                let ghost = self.copy_family(child)?;
                self.set_family_opacity_zero(ghost);
                new_children.push(ghost);
            }
        }
        for &child in &children {
            self.detach(mob, child);
        }
        for &child in &new_children {
            self.attach(mob, child).expect("padding copies are acyclic");
        }
        Ok(())
    }

    /// Reference `invisible_copy`'s `set_opacity(0)`: zero the alpha lane
    /// of every `*rgba` field across the whole family.
    fn set_family_opacity_zero(&mut self, mob: Mob) {
        for member in self.family(mob) {
            let Some(entry) = self.get_mut(member) else {
                continue;
            };
            let fields: Vec<String> = entry
                .buffer
                .schema()
                .fields()
                .iter()
                .filter(|f| f.name.ends_with("rgba"))
                .map(|f| f.name.clone())
                .collect();
            for field in fields {
                if let Some(mut column) = entry.buffer.read_column(&field) {
                    for alpha in column.iter_mut().skip(3).step_by(4) {
                        *alpha = 0.0;
                    }
                    entry.buffer.write_range(&field, 0, &column);
                }
            }
        }
    }

    /// Reference `align_data` (mobject.py:1746): zip the two families and
    /// align each pair's points. (Run [`Stage::align_family`] first —
    /// `align_data_and_family` does — so the zip covers both sides.)
    ///
    /// # Errors
    /// [`StageError::StaleHandle`], [`StageError::SchemaMismatch`] on a
    /// vmobject/base mixed pair, [`StageError::Geometry`] on a malformed
    /// point run.
    pub fn align_data(&mut self, a: Mob, b: Mob) -> Result<(), StageError> {
        let fa = self.family(a);
        let fb = self.family(b);
        for (&ma, &mb) in fa.iter().zip(fb.iter()) {
            self.align_points(ma, mb)?;
        }
        Ok(())
    }

    /// Reference `align_data_and_family` (mobject.py:1741): family first,
    /// then data — the order is semantics.
    ///
    /// # Errors
    /// As [`Stage::align_family`] and [`Stage::align_data`].
    pub fn align_data_and_family(&mut self, a: Mob, b: Mob) -> Result<(), StageError> {
        self.align_family(a, b)?;
        self.align_data(a, b)
    }

    /// Reference `align_points`: base records null-align by proportional
    /// resize (mobject.py:1751); vmobject pairs run the bezier-aware
    /// subpath algorithm (vectorized_mobject.py:964). A mixed pair is a
    /// typed refusal — the Reference would crash on one.
    ///
    /// # Errors
    /// [`StageError::StaleHandle`], [`StageError::SchemaMismatch`],
    /// [`StageError::Geometry`].
    pub fn align_points(&mut self, a: Mob, b: Mob) -> Result<(), StageError> {
        match (is_vmobject_schema(self, a)?, is_vmobject_schema(self, b)?) {
            (true, true) => self.align_points_vmobject(a, b),
            (false, false) => {
                let la = self.try_get(a)?.buffer.len();
                let lb = self.try_get(b)?.buffer.len();
                let max_len = la.max(lb);
                self.get_mut(a)
                    .expect("checked live")
                    .buffer
                    .resize_preserving_order(max_len);
                self.get_mut(b)
                    .expect("checked live")
                    .buffer
                    .resize_preserving_order(max_len);
                Ok(())
            }
            _ => Err(StageError::SchemaMismatch),
        }
    }

    /// vectorized_mobject.py:964, step for step.
    fn align_points_vmobject(&mut self, a: Mob, b: Mob) -> Result<(), StageError> {
        let mut pa = read_points(self, a)?;
        let mut pb = read_points(self, b)?;
        if pa.len() == pb.len() {
            // Equal counts: refresh joint angles only.
            write_points(self, a, &pa)?;
            write_points(self, b, &pb)?;
            return Ok(());
        }
        // No points → one point at the center (start_new_path(get_center())).
        if pa.is_empty() {
            pa = vec![self.get_center(a)];
        }
        if pb.is_empty() {
            pb = vec![self.get_center(b)];
        }

        let path_a = QuadPath::from_points(pa).map_err(StageError::Geometry)?;
        let path_b = QuadPath::from_points(pb).map_err(StageError::Geometry)?;
        let mut subpaths1: Vec<Vec<Vec3>> = path_a
            .subpaths()
            .into_iter()
            .map(<[Vec3]>::to_vec)
            .collect();
        let mut subpaths2: Vec<Vec<Vec3>> = path_b
            .subpaths()
            .into_iter()
            .map(<[Vec3]>::to_vec)
            .collect();
        for subpaths in [&mut subpaths1, &mut subpaths2] {
            let mut keyed: Vec<(f64, Vec<Vec3>)> = subpaths
                .drain(..)
                .map(|sp| (polyline_length(&sp), sp))
                .collect();
            // Descending by length; stable, like Python's list.sort.
            keyed.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal));
            *subpaths = keyed.into_iter().map(|(_, sp)| sp).collect();
        }
        let n_subpaths = subpaths1.len().max(subpaths2.len());

        // Missing subpaths fold the largest back on itself:
        // vstack([sp0[:-1], sp0[::-1]]) — a degenerate zero-area run.
        let get_nth = |list: &[Vec<Vec3>], n: usize| -> Vec<Vec3> {
            if n >= list.len() {
                let sp0 = &list[0];
                let mut folded = sp0[..sp0.len() - 1].to_vec();
                folded.extend(sp0.iter().rev().copied());
                folded
            } else {
                list[n].clone()
            }
        };

        let mut new_points1: Vec<Vec3> = Vec::new();
        let mut new_points2: Vec<Vec3> = Vec::new();
        for n in 0..n_subpaths {
            let sp1 = get_nth(&subpaths1, n);
            let sp2 = get_nth(&subpaths2, n);
            let diff1 = sp2.len().saturating_sub(sp1.len()) / 2;
            let diff2 = sp1.len().saturating_sub(sp2.len()) / 2;
            let sp1 = QuadPath::insert_n_curves_to_point_list(
                diff1,
                &sp1,
                DEFAULT_TOLERANCE_FOR_POINT_EQUALITY,
            );
            let sp2 = QuadPath::insert_n_curves_to_point_list(
                diff2,
                &sp2,
                DEFAULT_TOLERANCE_FOR_POINT_EQUALITY,
            );
            if n > 0 {
                // Intermediate anchor marking the subpath break.
                new_points1.push(*new_points1.last().expect("prior subpath emitted"));
                new_points2.push(*new_points2.last().expect("prior subpath emitted"));
            }
            new_points1.extend(sp1);
            new_points2.extend(sp2);
        }

        write_points(self, a, &new_points1)?;
        write_points(self, b, &new_points2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polyline_length_sums_consecutive_gaps() {
        let pts = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 2.0, 0.0]];
        assert!((polyline_length(&pts) - 3.0).abs() < 1e-12);
    }
}
