//! `QuadPath`: the shared-anchor quadratic-Bézier path model (§1.2, §7.1).
//!
//! `points = [a0, h0, a1, h1, a2, …]` — length odd when nonempty, curve *i*
//! is `points[2i..2i+3]`, and a subpath break is a null curve whose handle
//! sits exactly on its anchor. This layout is **API surface**: curve counts,
//! partial reveals, alignment, and user code indexing `points` all depend on
//! it, so every mutator here preserves it formally and the exact fixtures in
//! `tests/` lock it down. Semantics are ported from `VMobject`
//! (`3b1b/manim` @ `6199a00d`), computed in f64 per §6.1.
//!
//! Not here by design: the proportion/length layer (`point_from_proportion`,
//! `get_arc_length`) is fm-xci — true arc length under the original names —
//! and the error-bounded cubic→quadratic converter is fm-6cf.

use crate::GeomError;
use crate::bezier;
use crate::cubic;
use crate::scalar;
use crate::smoothing;
use crate::vec;
use fmn_core::constants::{DEG, OUT, PI, TAU};
use fmn_core::types::Vec3;

/// The Reference's `VMobject.tolerance_for_point_equality`.
pub const DEFAULT_TOLERANCE_FOR_POINT_EQUALITY: f64 = 1e-8;

/// The Reference's subpath-end disambiguation tolerance (its own comment:
/// "TODO, this is too unsystematic" — kept exactly, because subpath
/// decomposition is API behavior).
const SUBPATH_END_ATOL: f64 = 1e-4;

/// The angle below which `add_arc_to` degrades to a line.
const ARC_ANGLE_THRESHOLD: f64 = 1e-3;

/// Anchor modes for [`QuadPath::change_anchor_mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorMode {
    /// Handles at segment midpoints: straight polyline corners.
    Jagged,
    /// The local parabola-through-neighbors handle rule (no solver).
    ApproxSmooth,
    /// The spline solve: a smooth quadratic path through the anchors
    /// (may change the number of points).
    TrueSmooth,
}

/// A joined quadratic-Bézier path over the shared-anchor layout.
#[derive(Debug, Clone, PartialEq)]
pub struct QuadPath {
    points: Vec<Vec3>,
    tolerance_for_point_equality: f64,
    /// `VMobject.long_lines`: lines split into two quadratics instead of one.
    long_lines: bool,
    /// `VMobject.use_simple_quadratic_approx`: single-quad shortcut for
    /// shallow cubics in `add_cubic_bezier_curve_to`.
    use_simple_quadratic_approx: bool,
}

impl Default for QuadPath {
    fn default() -> Self {
        Self::new()
    }
}

impl QuadPath {
    #[must_use]
    pub fn new() -> Self {
        Self {
            points: Vec::new(),
            tolerance_for_point_equality: DEFAULT_TOLERANCE_FOR_POINT_EQUALITY,
            long_lines: false,
            use_simple_quadratic_approx: false,
        }
    }

    /// Build a path from raw shared-anchor points (length must be 0 or odd).
    pub fn from_points(points: Vec<Vec3>) -> Result<Self, GeomError> {
        let mut path = Self::new();
        path.set_points(points)?;
        Ok(path)
    }

    /// The arc primitive, under the one arc-density rule (BN-09): the
    /// unit-arc points scaled by `radius`, rotated by `start_angle`, and
    /// shifted to `arc_center`. `n_components` defaults to
    /// [`bezier::arc_n_components`].
    #[must_use]
    pub fn arc(
        start_angle: f64,
        angle: f64,
        radius: f64,
        arc_center: Vec3,
        n_components: Option<usize>,
    ) -> Self {
        let n = n_components.unwrap_or_else(|| bezier::arc_n_components(angle));
        let rot = vec::rotation_about_z(start_angle);
        let points = bezier::quadratic_points_for_arc(angle, n)
            .into_iter()
            .map(|p| {
                let rotated = [
                    rot[0][0] * p[0] + rot[0][1] * p[1],
                    rot[1][0] * p[0] + rot[1][1] * p[1],
                    p[2],
                ];
                vec::add(vec::scale(rotated, radius), arc_center)
            })
            .collect();
        Self {
            points,
            ..Self::new()
        }
    }

    // ------------------------------------------------------------- storage

    #[must_use]
    pub fn points(&self) -> &[Vec3] {
        &self.points
    }

    #[must_use]
    pub fn num_points(&self) -> usize {
        self.points.len()
    }

    #[must_use]
    pub fn has_points(&self) -> bool {
        !self.points.is_empty()
    }

    #[must_use]
    pub fn tolerance_for_point_equality(&self) -> f64 {
        self.tolerance_for_point_equality
    }

    pub fn set_tolerance_for_point_equality(&mut self, tolerance: f64) -> &mut Self {
        self.tolerance_for_point_equality = tolerance;
        self
    }

    pub fn set_long_lines(&mut self, long_lines: bool) -> &mut Self {
        self.long_lines = long_lines;
        self
    }

    pub fn set_use_simple_quadratic_approx(&mut self, value: bool) -> &mut Self {
        self.use_simple_quadratic_approx = value;
        self
    }

    /// Replace the points wholesale. The shared-anchor invariant is checked:
    /// the length must be 0 or odd.
    pub fn set_points(&mut self, points: Vec<Vec3>) -> Result<&mut Self, GeomError> {
        if !points.is_empty() && points.len().is_multiple_of(2) {
            return Err(GeomError::EvenPointCount { len: points.len() });
        }
        self.points = points;
        Ok(self)
    }

    /// Append points. Preserving oddness means appending an even count.
    pub fn append_points(&mut self, points: &[Vec3]) -> Result<&mut Self, GeomError> {
        if !points.len().is_multiple_of(2) {
            return Err(GeomError::EvenPointCount {
                len: self.points.len() + points.len(),
            });
        }
        if self.points.is_empty() && !points.is_empty() {
            return Err(GeomError::EmptyPath);
        }
        self.points.extend_from_slice(points);
        Ok(self)
    }

    pub fn clear_points(&mut self) -> &mut Self {
        self.points.clear();
        self
    }

    /// `set_anchors_and_handles`: interleave `anchors` and `handles`
    /// (`anchors.len() == handles.len() + 1`; empty anchors clears).
    pub fn set_anchors_and_handles(
        &mut self,
        anchors: &[Vec3],
        handles: &[Vec3],
    ) -> Result<&mut Self, GeomError> {
        if anchors.is_empty() {
            self.clear_points();
            return Ok(self);
        }
        if anchors.len() != handles.len() + 1 {
            return Err(GeomError::MismatchedAnchorsAndHandles {
                anchors: anchors.len(),
                handles: handles.len(),
            });
        }
        let mut points = Vec::with_capacity(2 * anchors.len() - 1);
        for i in 0..handles.len() {
            points.push(anchors[i]);
            points.push(handles[i]);
        }
        points.push(anchors[anchors.len() - 1]);
        self.points = points;
        Ok(self)
    }

    // ------------------------------------------------------- path building

    /// `start_new_path`: path ends are signaled by a handle sitting directly
    /// on top of the previous anchor.
    pub fn start_new_path(&mut self, point: Vec3) -> &mut Self {
        if let Some(&last) = self.points.last() {
            self.points.push(last);
            self.points.push(point);
        } else {
            self.points.push(point);
        }
        self
    }

    /// `add_line_to`. With `allow_null_line = false` a segment to a
    /// coincident point is silently skipped.
    pub fn add_line_to(
        &mut self,
        point: Vec3,
        allow_null_line: bool,
    ) -> Result<&mut Self, GeomError> {
        let last = self.last_point().ok_or(GeomError::EmptyPath)?;
        if !allow_null_line && self.consider_points_equal(last, point) {
            return Ok(self);
        }
        let n_alphas = if self.long_lines { 5 } else { 3 };
        let alphas = bezier::linspace(0.0, 1.0, n_alphas);
        for &alpha in &alphas[1..] {
            self.points.push(vec::lerp(last, point, alpha));
        }
        Ok(self)
    }

    /// `add_quadratic_bezier_curve_to`. A handle coincident with the last
    /// anchor would read as a subpath break, so it is nudged to the segment
    /// midpoint, exactly as the Reference does.
    pub fn add_quadratic_bezier_curve_to(
        &mut self,
        handle: Vec3,
        anchor: Vec3,
        allow_null_curve: bool,
    ) -> Result<&mut Self, GeomError> {
        let last = self.last_point().ok_or(GeomError::EmptyPath)?;
        if !allow_null_curve && self.consider_points_equal(last, anchor) {
            return Ok(self);
        }
        let handle = if self.consider_points_equal(handle, last) {
            vec::midpoint(handle, anchor)
        } else {
            handle
        };
        self.points.push(handle);
        self.points.push(anchor);
        Ok(self)
    }

    /// `add_cubic_bezier_curve_to`: reduce the cubic and append it. Note the
    /// Reference's own caveat: the shallow-angle shortcut assumes points on
    /// the xy-plane.
    pub fn add_cubic_bezier_curve_to(
        &mut self,
        handle1: Vec3,
        handle2: Vec3,
        anchor: Vec3,
    ) -> Result<&mut Self, GeomError> {
        let last = self.last_point().ok_or(GeomError::EmptyPath)?;
        let v1 = vec::sub(handle1, last);
        let v2 = vec::sub(anchor, handle2);
        let angle = vec::angle_between_vectors(v1, v2);
        let mut quad_approx: Vec<Vec3> = if self.use_simple_quadratic_approx && angle < 45.0 * DEG {
            vec![
                last,
                vec::find_intersection(last, v1, anchor, vec::scale(v2, -1.0), 1e-5),
                anchor,
            ]
        } else {
            let approx = cubic::quadratic_approximation_of_cubic(last, handle1, handle2, anchor);
            let mut approx = approx.to_vec();
            if self.consider_points_equal(approx[3], approx[4]) {
                // Avoid degenerate handles (duplicate points).
                approx[3] = vec::midpoint(approx[2], approx[3]);
            }
            approx
        };
        if self.consider_points_equal(quad_approx[1], last) {
            // Prevent the subpath from accidentally being marked closed.
            quad_approx[1] = vec::midpoint(quad_approx[1], quad_approx[2]);
        }
        self.points.extend_from_slice(&quad_approx[1..]);
        Ok(self)
    }

    /// `add_smooth_curve_to`: continue with the reflection of the last
    /// handle, or a plain line when a new subpath was just started.
    pub fn add_smooth_curve_to(&mut self, point: Vec3) -> Result<&mut Self, GeomError> {
        if self.has_new_path_started() {
            self.add_line_to(point, true)
        } else {
            let handle = self
                .reflection_of_last_handle()
                .ok_or(GeomError::EmptyPath)?;
            self.add_quadratic_bezier_curve_to(handle, point, true)
        }
    }

    /// `add_smooth_cubic_curve_to`.
    pub fn add_smooth_cubic_curve_to(
        &mut self,
        handle: Vec3,
        point: Vec3,
    ) -> Result<&mut Self, GeomError> {
        if self.points.is_empty() {
            return Err(GeomError::EmptyPath);
        }
        let new_handle = if self.num_points() == 1 {
            handle
        } else {
            // Reflection is well-defined here: at least two points exist.
            let n = self.points.len();
            vec::sub(vec::scale(self.points[n - 1], 2.0), self.points[n - 2])
        };
        self.add_cubic_bezier_curve_to(new_handle, handle, point)
    }

    /// `add_arc_to`: an arc subtending `angle` from the current end to
    /// `point`. `n_components` defaults to the one arc-density rule (BN-09);
    /// the Reference's `ceil(8·|θ|/TAU)` here was one of the three retired
    /// conventions.
    pub fn add_arc_to(
        &mut self,
        point: Vec3,
        angle: f64,
        n_components: Option<usize>,
    ) -> Result<&mut Self, GeomError> {
        let last = self.last_point().ok_or(GeomError::EmptyPath)?;
        if angle.abs() < ARC_ANGLE_THRESHOLD {
            return self.add_line_to(point, true);
        }
        let n = n_components.unwrap_or_else(|| bezier::arc_n_components(angle));
        let mut arc_points = bezier::quadratic_points_for_arc(angle, n);
        let target_vect = vec::sub(point, last);
        let curr_vect = vec::sub(arc_points[arc_points.len() - 1], arc_points[0]);
        let rot = vec::rotation_between_vectors(curr_vect, target_vect);
        let scale_factor = vec::norm(target_vect) / vec::norm(curr_vect);
        for p in arc_points.iter_mut() {
            // Reference: `arc_points @ R.T` = R · p per point.
            *p = vec::scale(vec::mul_point_mat(*p, &vec::transpose(&rot)), scale_factor);
        }
        let offset = vec::sub(last, arc_points[0]);
        for p in arc_points.iter_mut() {
            *p = vec::add(*p, offset);
        }
        self.points.extend_from_slice(&arc_points[1..]);
        Ok(self)
    }

    /// `add_points_as_corners`.
    pub fn add_points_as_corners(&mut self, points: &[Vec3]) -> Result<&mut Self, GeomError> {
        for &p in points {
            self.add_line_to(p, true)?;
        }
        Ok(self)
    }

    /// `set_points_as_corners`: anchors with midpoint handles.
    pub fn set_points_as_corners(&mut self, points: &[Vec3]) -> Result<&mut Self, GeomError> {
        let handles: Vec<Vec3> = points
            .windows(2)
            .map(|w| vec::midpoint(w[0], w[1]))
            .collect();
        self.set_anchors_and_handles(points, &handles)
    }

    /// `set_points_smoothly`.
    pub fn set_points_smoothly(
        &mut self,
        points: &[Vec3],
        approx: bool,
    ) -> Result<&mut Self, GeomError> {
        self.set_points_as_corners(points)?;
        self.make_smooth(approx)?;
        Ok(self)
    }

    /// `add_subpath`: splice a shared-anchor point run in as a new subpath
    /// (or a continuation when it starts at the current end).
    pub fn add_subpath(&mut self, points: &[Vec3]) -> Result<&mut Self, GeomError> {
        if points.len().is_multiple_of(2) && !points.is_empty() {
            return Err(GeomError::EvenPointCount { len: points.len() });
        }
        if points.is_empty() {
            return Ok(self);
        }
        if !self.has_points() {
            self.points = points.to_vec();
            return Ok(self);
        }
        if !self.consider_points_equal(points[0], self.points[self.points.len() - 1]) {
            self.start_new_path(points[0]);
        }
        self.points.extend_from_slice(&points[1..]);
        Ok(self)
    }

    // ------------------------------------------------------------- queries

    /// `consider_points_equal`: strict per-component comparison against the
    /// path's tolerance.
    #[must_use]
    pub fn consider_points_equal(&self, p0: Vec3, p1: Vec3) -> bool {
        (0..3).all(|i| (p1[i] - p0[i]).abs() < self.tolerance_for_point_equality)
    }

    /// `has_new_path_started`: a one-point path, or a trailing break marker.
    #[must_use]
    pub fn has_new_path_started(&self) -> bool {
        match self.points.len() {
            0 => false,
            1 => true,
            n => self.consider_points_equal(self.points[n - 3], self.points[n - 2]),
        }
    }

    #[must_use]
    pub fn last_point(&self) -> Option<Vec3> {
        self.points.last().copied()
    }

    /// `get_reflection_of_last_handle`: `2·points[-1] − points[-2]`.
    #[must_use]
    pub fn reflection_of_last_handle(&self) -> Option<Vec3> {
        let n = self.points.len();
        if n < 2 {
            return None;
        }
        Some(vec::sub(
            vec::scale(self.points[n - 1], 2.0),
            self.points[n - 2],
        ))
    }

    /// `close_path`: join the end of the last subpath back to its start.
    pub fn close_path(&mut self, smooth: bool) -> Result<&mut Self, GeomError> {
        if self.is_closed() {
            return Ok(self);
        }
        let start = self.last_subpath_start().ok_or(GeomError::EmptyPath)?;
        if smooth {
            self.add_smooth_curve_to(start)
        } else {
            self.add_line_to(start, true)
        }
    }

    fn last_subpath_start(&self) -> Option<Vec3> {
        if self.points.is_empty() {
            return None;
        }
        let ends = self.subpath_end_indices();
        let index = if ends.len() == 1 {
            0
        } else {
            ends[ends.len() - 2] + 2
        };
        Some(self.points[index])
    }

    /// `is_closed`: whether the last subpath's end returns to its start
    /// (within the point-equality tolerance). An empty path is not closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        match self.last_subpath_start() {
            Some(start) => self.consider_points_equal(start, self.points[self.points.len() - 1]),
            None => false,
        }
    }

    #[must_use]
    pub fn num_curves(&self) -> usize {
        self.points.len() / 2
    }

    /// Curve *i* = `points[2i..2i+3]`.
    #[must_use]
    pub fn nth_curve_points(&self, n: usize) -> Option<[Vec3; 3]> {
        if n >= self.num_curves() {
            return None;
        }
        Some([
            self.points[2 * n],
            self.points[2 * n + 1],
            self.points[2 * n + 2],
        ])
    }

    /// Evaluate curve `n` at parameter `t`.
    #[must_use]
    pub fn nth_curve_point(&self, n: usize, t: f64) -> Option<Vec3> {
        self.nth_curve_points(n)
            .map(|[p0, p1, p2]| bezier::quadratic_point(p0, p1, p2, t))
    }

    /// Iterator over the curve triples.
    pub fn bezier_tuples(&self) -> impl Iterator<Item = [Vec3; 3]> + '_ {
        (0..self.num_curves()).map(|i| {
            [
                self.points[2 * i],
                self.points[2 * i + 1],
                self.points[2 * i + 2],
            ]
        })
    }

    #[must_use]
    pub fn anchors(&self) -> Vec<Vec3> {
        self.points.iter().step_by(2).copied().collect()
    }

    #[must_use]
    pub fn start_anchors(&self) -> Vec<Vec3> {
        if self.points.is_empty() {
            return Vec::new();
        }
        self.points[..self.points.len() - 1]
            .iter()
            .step_by(2)
            .copied()
            .collect()
    }

    #[must_use]
    pub fn end_anchors(&self) -> Vec<Vec3> {
        if self.points.len() < 3 {
            return Vec::new();
        }
        self.points[2..].iter().step_by(2).copied().collect()
    }

    /// `get_subpath_end_indices`: anchor indices ending each subpath. An
    /// anchor ends a subpath when its following handle sits exactly on top
    /// of it *and* the following anchor is genuinely distinct (beyond the
    /// Reference's fixed 1e-4), disambiguating breaks from runs of null
    /// curves. The final point index is always an end.
    #[must_use]
    pub fn subpath_end_indices(&self) -> Vec<usize> {
        if self.points.is_empty() {
            return Vec::new();
        }
        let mut ends = Vec::new();
        for i in 0..self.num_curves() {
            let a0 = self.points[2 * i];
            let h = self.points[2 * i + 1];
            let a1 = self.points[2 * i + 2];
            let is_break = a0 == h && (0..3).any(|k| (h[k] - a1[k]).abs() > SUBPATH_END_ATOL);
            if is_break {
                ends.push(2 * i);
            }
        }
        ends.push(self.points.len() - 1);
        ends
    }

    /// The subpaths as point slices (each in shared-anchor layout).
    #[must_use]
    pub fn subpaths(&self) -> Vec<&[Vec3]> {
        if self.points.is_empty() {
            return Vec::new();
        }
        let ends = self.subpath_end_indices();
        let mut out = Vec::with_capacity(ends.len());
        let mut start = 0usize;
        for &end in &ends {
            out.push(&self.points[start..=end]);
            start = end + 2;
        }
        out
    }

    /// `get_points_without_null_curves`.
    #[must_use]
    pub fn points_without_null_curves(&self, atol: f64) -> Vec<Vec3> {
        if self.points.is_empty() {
            return Vec::new();
        }
        let mut out = vec![self.points[0]];
        for tup in self.bezier_tuples() {
            if vec::norm(vec::sub(tup[1], tup[0])) > atol
                || vec::norm(vec::sub(tup[2], tup[0])) > atol
            {
                out.push(tup[1]);
                out.push(tup[2]);
            }
        }
        out
    }

    // -------------------------------------------------------- subdivision

    /// `subdivide_curves_by_condition`: split each curve into `f(curve) + 1`
    /// equal-parameter pieces.
    pub fn subdivide_curves_by_condition(
        &mut self,
        tuple_to_subdivisions: impl Fn([Vec3; 3]) -> usize,
    ) -> &mut Self {
        if !self.has_points() {
            return self;
        }
        let mut new_points = vec![self.points[0]];
        for tup in self.bezier_tuples().collect::<Vec<_>>() {
            let n_divisions = tuple_to_subdivisions(tup);
            if n_divisions > 0 {
                let alphas = bezier::linspace(0.0, 1.0, n_divisions + 2);
                for pair in alphas.windows(2) {
                    let sub = bezier::partial_quadratic(&tup, pair[0], pair[1]);
                    new_points.push(sub[1]);
                    new_points.push(sub[2]);
                }
            } else {
                new_points.push(tup[1]);
                new_points.push(tup[2]);
            }
        }
        self.points = new_points;
        self
    }

    /// `subdivide_sharp_curves`.
    pub fn subdivide_sharp_curves(&mut self, angle_threshold: f64) -> &mut Self {
        self.subdivide_curves_by_condition(|[b0, b1, b2]| {
            let angle = vec::angle_between_vectors(vec::sub(b1, b0), vec::sub(b2, b1));
            (angle / angle_threshold) as usize
        })
    }

    /// `insert_n_curves_to_point_list`: distribute `n` extra curves over the
    /// longest curves (null curves never split), preserving the traced shape.
    #[must_use]
    pub fn insert_n_curves_to_point_list(n: usize, points: &[Vec3], tolerance: f64) -> Vec<Vec3> {
        if points.len() == 1 {
            return vec![points[0]; 2 * n + 1];
        }
        let tuples: Vec<[Vec3; 3]> = (0..points.len().saturating_sub(1) / 2)
            .map(|i| [points[2 * i], points[2 * i + 1], points[2 * i + 2]])
            .collect();
        let mut norms: Vec<f64> = tuples
            .iter()
            .map(|tup| {
                if vec::norm(vec::sub(tup[1], tup[0])) < tolerance {
                    0.0
                } else {
                    vec::norm(vec::sub(tup[2], tup[0]))
                }
            })
            .collect();
        let mut ipc = vec![0usize; tuples.len()];
        for _ in 0..n {
            // argmax (first index wins ties, like np.argmax).
            let mut index = 0;
            for (i, &value) in norms.iter().enumerate() {
                if value > norms[index] {
                    index = i;
                }
            }
            ipc[index] += 1;
            norms[index] *= ipc[index] as f64 / (ipc[index] + 1) as f64;
        }
        let mut new_points = vec![points[0]];
        for (tup, &n_inserts) in tuples.iter().zip(ipc.iter()) {
            let alphas = bezier::linspace(0.0, 1.0, n_inserts + 2);
            for pair in alphas.windows(2) {
                let sub = bezier::partial_quadratic(tup, pair[0], pair[1]);
                new_points.push(sub[1]);
                new_points.push(sub[2]);
            }
        }
        new_points
    }

    /// `insert_n_curves`.
    pub fn insert_n_curves(&mut self, n: usize) -> &mut Self {
        if self.num_curves() > 0 {
            self.points = Self::insert_n_curves_to_point_list(
                n,
                &self.points,
                self.tolerance_for_point_equality,
            );
        }
        self
    }

    // ------------------------------------------------------- anchor modes

    /// `is_smooth`: all anchor-joint angles under `angle_tol`
    /// (the Reference's default is 1°).
    #[must_use]
    pub fn is_smooth(&self, angle_tol: f64) -> bool {
        self.joint_angles()
            .iter()
            .step_by(2)
            .all(|a| a.abs() < angle_tol)
    }

    /// `change_anchor_mode`: recompute every subpath's handles under the
    /// given mode, preserving the break markers, with the Reference's exact
    /// post-fixes for handles that land on an anchor.
    pub fn change_anchor_mode(&mut self, mode: AnchorMode) -> Result<&mut Self, GeomError> {
        if self.points.is_empty() {
            return Ok(self);
        }
        let subpaths: Vec<Vec<Vec3>> = self.subpaths().into_iter().map(|s| s.to_vec()).collect();
        self.clear_points();
        for subpath in subpaths {
            let anchors: Vec<Vec3> = subpath.iter().step_by(2).copied().collect();
            let mut new_subpath = subpath.clone();
            match mode {
                AnchorMode::Jagged => {
                    for (slot, pair) in new_subpath
                        .iter_mut()
                        .skip(1)
                        .step_by(2)
                        .zip(anchors.windows(2))
                    {
                        *slot = vec::midpoint(pair[0], pair[1]);
                    }
                }
                AnchorMode::ApproxSmooth => {
                    let handles = smoothing::approx_smooth_quadratic_handles(&anchors);
                    for (slot, handle) in new_subpath.iter_mut().skip(1).step_by(2).zip(handles) {
                        *slot = handle;
                    }
                }
                AnchorMode::TrueSmooth => {
                    new_subpath = smoothing::smooth_quadratic_path(&anchors)?;
                }
            }
            // Shift any handle that ended up exactly on top of the previous
            // anchor (a false break marker), then any that landed on the
            // following anchor (a degenerate handle).
            for i in (1..new_subpath.len()).step_by(2) {
                if new_subpath[i - 1] == new_subpath[i] {
                    new_subpath[i] = vec::midpoint(new_subpath[i - 1], new_subpath[i + 1]);
                }
                if new_subpath[i] == new_subpath[i + 1] {
                    new_subpath[i] = vec::midpoint(new_subpath[i - 1], new_subpath[i + 1]);
                }
            }
            self.add_subpath(&new_subpath)?;
        }
        Ok(self)
    }

    /// `make_smooth`: `approx = true` keeps the point count (approx handles),
    /// `false` runs the true spline solve. Already-smooth paths are left
    /// untouched, matching the Reference.
    pub fn make_smooth(&mut self, approx: bool) -> Result<&mut Self, GeomError> {
        if self.is_smooth(1.0 * DEG) {
            return Ok(self);
        }
        let mode = if approx {
            AnchorMode::ApproxSmooth
        } else {
            AnchorMode::TrueSmooth
        };
        self.change_anchor_mode(mode)
    }

    /// `make_jagged`.
    pub fn make_jagged(&mut self) -> Result<&mut Self, GeomError> {
        self.change_anchor_mode(AnchorMode::Jagged)
    }

    // ------------------------------------------------ normals and joints

    /// `get_area_vector`: the right-hand-rule area vector of the anchor
    /// polygon(s).
    #[must_use]
    pub fn area_vector(&self) -> Vec3 {
        if !self.has_points() {
            return [0.0; 3];
        }
        let mut area = [0.0; 3];
        for subpath in self.subpaths() {
            let anchors: Vec<Vec3> = subpath.iter().step_by(2).copied().collect();
            if anchors.is_empty() {
                continue;
            }
            for i in 0..anchors.len() {
                let p0 = anchors[i];
                let p1 = anchors[(i + 1) % anchors.len()];
                area[0] += 0.5 * (p0[1] + p1[1]) * (p1[2] - p0[2]);
                area[1] += 0.5 * (p0[2] + p1[2]) * (p1[0] - p0[0]);
                area[2] += 0.5 * (p0[0] + p1[0]) * (p1[1] - p0[1]);
            }
        }
        area
    }

    /// `get_unit_normal`.
    #[must_use]
    pub fn unit_normal(&self) -> Vec3 {
        if self.num_points() < 3 {
            return OUT;
        }
        let area_vect = self.area_vector();
        let area = vec::norm(area_vect);
        if area > 0.0 {
            vec::scale(area_vect, 1.0 / area)
        } else {
            vec::unit_normal_from(
                vec::sub(self.points[1], self.points[0]),
                vec::sub(self.points[2], self.points[1]),
                1e-6,
            )
        }
    }

    /// `get_joint_angles`: per-point signed turn angles between the tangent
    /// into and out of each vertex, in the plane of the path's unit normal.
    /// Closed subpaths join their seam tangents; open ends get zero turn.
    ///
    /// Computed fresh on every call; revisioned caching arrives with the
    /// render mirrors (§8.2 / §10.8), never here.
    #[must_use]
    pub fn joint_angles(&self) -> Vec<f64> {
        let n = self.points.len();
        if n < 3 {
            return vec![0.0; n];
        }
        let rot = vec::rotation_between_vectors(OUT, self.unit_normal());
        let points: Vec<Vec3> = self
            .points
            .iter()
            .map(|p| vec::mul_point_mat(*p, &rot))
            .collect();

        // Tangent vectors into and out of each vertex.
        let mut v_in = vec![[0.0f64; 3]; n];
        let mut v_out = vec![[0.0f64; 3]; n];
        for i in 0..(n - 1) / 2 {
            let a0_to_h = vec::sub(points[2 * i + 1], points[2 * i]);
            let h_to_a1 = vec::sub(points[2 * i + 2], points[2 * i + 1]);
            v_in[2 * i + 1] = a0_to_h;
            v_in[2 * i + 2] = h_to_a1;
            v_out[2 * i] = a0_to_h;
            v_out[2 * i + 1] = h_to_a1;
        }

        // Join up closed loops; mark unclosed path ends as straight-through.
        let ends = self.subpath_end_indices();
        let mut start = 0usize;
        for &end in &ends {
            if start != end {
                if points[start] == points[end] {
                    v_in[start] = v_out[end - 1];
                    v_out[end] = v_in[start + 1];
                } else {
                    v_in[start] = v_out[start];
                    v_out[end] = v_in[end];
                }
            }
            start = end + 2;
        }

        (0..n)
            .map(|i| {
                let angle_in = scalar::atan2(v_in[i][1], v_in[i][0]);
                let angle_out = scalar::atan2(v_out[i][1], v_out[i][0]);
                let mut diff = angle_out - angle_in;
                if diff < -PI {
                    diff += TAU;
                }
                if diff > PI {
                    diff -= TAU;
                }
                diff
            })
            .collect()
    }

    /// `reverse_points`: reverse the trace direction, repositioning break
    /// markers so subpath decomposition survives, exactly as the Reference
    /// does.
    pub fn reverse_points(&mut self) -> &mut Self {
        if !self.has_points() {
            return self;
        }
        let ends = self.subpath_end_indices();
        for &end in &ends[..ends.len().saturating_sub(1)] {
            self.points[end + 1] = self.points[end + 2];
        }
        self.points.reverse();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc_constructor_uses_bn09_density() {
        let quarter = QuadPath::arc(0.0, TAU / 4.0, 1.0, [0.0; 3], None);
        assert_eq!(quarter.num_curves(), 4);
        let full = QuadPath::arc(0.0, TAU, 2.0, [1.0, 0.0, 0.0], None);
        assert_eq!(full.num_curves(), 16);
        // Anchors sit on the circle of radius 2 about (1, 0, 0).
        for anchor in full.anchors() {
            let r = vec::norm(vec::sub(anchor, [1.0, 0.0, 0.0]));
            assert!((r - 2.0).abs() < 1e-12);
        }
    }

    #[test]
    fn joint_angles_zero_for_straight_line() {
        let mut path = QuadPath::new();
        path.start_new_path([0.0; 3]);
        path.add_line_to([1.0, 0.0, 0.0], true).unwrap();
        path.add_line_to([2.0, 0.0, 0.0], true).unwrap();
        for angle in path.joint_angles() {
            assert!(angle.abs() < 1e-12);
        }
    }
}
