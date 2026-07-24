//! `VMobject` and its variants: the detached builder every library class
//! is made of (§12, Appendix A `mobject/types/vectorized_mobject`).
//!
//! Per G0-1's ratified §15.1 surface, a library class is a **value**:
//! chained by-value setters producing a plain struct that
//! `Stage::add(impl Into<Mobject>)` moves into the arena. [`VMobject`] is
//! that value for the vectorized family — points in the shared-anchor
//! layout, a [`Style`], the uniform inventory, the semantic
//! [`ShapeTag`] (§10.8), and detached children.
//!
//! Everything above it in this crate builds one of these and hands it to
//! `Stage::add`; nothing in the library reaches into the arena to finish a
//! half-constructed object.

use fmn_core::color::Srgb;
use fmn_core::constants::OUT;
use fmn_core::types::Vec3;
use fmn_geom::{QuadPath, space_ops};
use fmn_mobject::stage::{Mob, Stage};
use fmn_mobject::uniforms::{JointType, Uniforms};
use fmn_mobject::{Mobject, RecordBuffer, RecordSchema, ShapeTag};

use crate::style::Style;

/// A detached vectorized mobject.
#[derive(Debug, Clone, PartialEq)]
pub struct VMobject {
    points: Vec<Vec3>,
    style: Style,
    uniforms: Uniforms,
    shape: ShapeTag,
    submobjects: Vec<VMobject>,
}

impl Default for VMobject {
    fn default() -> Self {
        Self::new()
    }
}

impl VMobject {
    /// An empty vectorized mobject: no points, default style.
    #[must_use]
    pub fn new() -> Self {
        Self {
            points: Vec::new(),
            style: Style::default(),
            uniforms: Uniforms::default(),
            shape: ShapeTag::General,
            submobjects: Vec::new(),
        }
    }

    /// A vectorized mobject over an explicit shared-anchor point run.
    #[must_use]
    pub fn from_points(points: Vec<Vec3>) -> Self {
        Self::new().with_points(points)
    }

    /// A vectorized mobject over a built [`QuadPath`] — the usual route,
    /// since every geometry class draws its path with the Chisel API.
    #[must_use]
    pub fn from_path(path: &QuadPath) -> Self {
        Self::new().with_points(path.points().to_vec())
    }

    /// Replace the point run.
    #[must_use]
    pub fn with_points(mut self, points: Vec<Vec3>) -> Self {
        self.points = points;
        self
    }

    /// Replace the whole style.
    #[must_use]
    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Amend the style in place (`vmob.map_style(|s| s.color(RED))`).
    #[must_use]
    pub fn map_style(mut self, f: impl FnOnce(Style) -> Style) -> Self {
        self.style = f(self.style);
        self
    }

    /// The Reference's `color=`: stroke and fill together.
    #[must_use]
    pub fn with_color(self, color: Srgb) -> Self {
        self.map_style(|s| s.color(color))
    }

    /// Tag the semantic shape this builder produced (§10.8).
    #[must_use]
    pub fn with_shape(mut self, shape: ShapeTag) -> Self {
        self.shape = shape;
        self
    }

    /// Replace the uniform inventory.
    #[must_use]
    pub fn with_uniforms(mut self, uniforms: Uniforms) -> Self {
        self.uniforms = uniforms;
        self
    }

    /// Set the joint type uniform (`joint_type=`).
    #[must_use]
    pub fn with_joint_type(mut self, joint_type: JointType) -> Self {
        self.uniforms.joint_type = joint_type;
        self
    }

    /// Set the `stroke_behind` uniform.
    #[must_use]
    pub fn with_stroke_behind(mut self, behind: bool) -> Self {
        self.uniforms.stroke_behind = behind;
        self
    }

    /// Set the `flat_stroke` uniform.
    #[must_use]
    pub fn with_flat_stroke(mut self, flat: bool) -> Self {
        self.uniforms.flat_stroke = flat;
        self
    }

    /// Append a detached child (`VMobject.add`).
    #[must_use]
    pub fn with_child(mut self, child: VMobject) -> Self {
        self.submobjects.push(child);
        self
    }

    /// Append several detached children.
    #[must_use]
    pub fn with_children(mut self, children: impl IntoIterator<Item = VMobject>) -> Self {
        self.submobjects.extend(children);
        self
    }

    /// The point run.
    #[must_use]
    pub fn points(&self) -> &[Vec3] {
        &self.points
    }

    /// The style.
    #[must_use]
    pub fn style(&self) -> Style {
        self.style
    }

    /// The semantic shape tag.
    #[must_use]
    pub fn shape(&self) -> ShapeTag {
        self.shape
    }

    /// The detached children.
    #[must_use]
    pub fn children(&self) -> &[VMobject] {
        &self.submobjects
    }

    /// Read the point run back as a [`QuadPath`] — the builder-side
    /// equivalent of reading the arena's records, for classes that
    /// measure their own geometry while still detached (tips, dashes).
    ///
    /// # Errors
    /// [`fmn_geom::GeomError::EvenPointCount`] if the run is not in the
    /// shared-anchor layout.
    pub fn path(&self) -> Result<QuadPath, fmn_geom::GeomError> {
        QuadPath::from_points(self.points.clone())
    }

    /// Apply a point map (the detached form of `apply_points_function`
    /// with no pivot) to this mobject and its detached children.
    ///
    /// Any semantic shape tag is dropped: the caller is by definition no
    /// longer building the shape the tag names. A class that *knows* the
    /// map preserves its shape re-tags afterwards.
    #[must_use]
    pub fn map_points(mut self, f: impl Fn(Vec3) -> Vec3 + Copy) -> Self {
        for p in &mut self.points {
            *p = f(*p);
        }
        self.submobjects = self
            .submobjects
            .into_iter()
            .map(|child| child.map_points(f))
            .collect();
        self.shape = ShapeTag::General;
        self
    }

    /// Shift every point (including children's).
    #[must_use]
    pub fn shifted(self, offset: Vec3) -> Self {
        let shape = self.shape;
        self.map_points(|p| [p[0] + offset[0], p[1] + offset[1], p[2] + offset[2]])
            .with_shape(shifted_tag(shape, offset))
    }

    /// The `(min, max)` corners of the family's points, or `None` when
    /// there are none — the detached form of the bounding box the
    /// positional API works against.
    #[must_use]
    pub fn extent(&self) -> Option<(Vec3, Vec3)> {
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        let mut any = false;
        self.visit_points(&mut |p| {
            any = true;
            for k in 0..3 {
                min[k] = min[k].min(p[k]);
                max[k] = max[k].max(p[k]);
            }
        });
        any.then_some((min, max))
    }

    fn visit_points(&self, f: &mut impl FnMut(Vec3)) {
        for p in &self.points {
            f(*p);
        }
        for child in &self.submobjects {
            child.visit_points(f);
        }
    }

    /// The family's extent along one axis (Reference `length_over_dim`).
    #[must_use]
    pub fn length_over_dim(&self, dim: usize) -> f64 {
        self.extent().map_or(0.0, |(min, max)| max[dim] - min[dim])
    }

    /// The centre of the family's extent (Reference `get_center`).
    #[must_use]
    pub fn center_point(&self) -> Vec3 {
        self.extent().map_or([0.0; 3], |(min, max)| {
            [
                0.5 * (min[0] + max[0]),
                0.5 * (min[1] + max[1]),
                0.5 * (min[2] + max[2]),
            ]
        })
    }

    /// Scale about a pivot (Reference `scale(factor, about_point=…)`).
    #[must_use]
    pub fn scaled_about(self, factor: f64, about: Vec3) -> Self {
        self.map_points(move |p| {
            [
                about[0] + (p[0] - about[0]) * factor,
                about[1] + (p[1] - about[1]) * factor,
                about[2] + (p[2] - about[2]) * factor,
            ]
        })
    }

    /// Stretch one axis about a pivot (Reference `stretch`).
    #[must_use]
    pub fn stretched_about(self, factor: f64, dim: usize, about: Vec3) -> Self {
        self.map_points(move |mut p| {
            p[dim] = about[dim] + (p[dim] - about[dim]) * factor;
            p
        })
    }

    /// Reference `rescale_to_fit`: resize along `dim` to `length`, about
    /// the family's own centre. A zero extent along that axis is left
    /// alone (the Reference's early return), never divided by.
    #[must_use]
    pub fn rescaled_to_fit(self, length: f64, dim: usize, stretch: bool) -> Self {
        let old = self.length_over_dim(dim);
        if old == 0.0 {
            return self;
        }
        let about = self.center_point();
        let factor = length / old;
        if stretch {
            self.stretched_about(factor, dim, about)
        } else {
            self.scaled_about(factor, about)
        }
    }

    /// Reference `set_width(width, stretch=…)`.
    #[must_use]
    pub fn with_width(self, width: f64, stretch: bool) -> Self {
        self.rescaled_to_fit(width, 0, stretch)
    }

    /// Reference `set_height(height, stretch=…)`.
    #[must_use]
    pub fn with_height(self, height: f64, stretch: bool) -> Self {
        self.rescaled_to_fit(height, 1, stretch)
    }

    /// Rotate about a pivot and axis (Reference `rotate`).
    #[must_use]
    pub fn rotated_about(self, angle: f64, axis: Vec3, about: Vec3) -> Self {
        let m = fmn_geom::rotation_matrix(angle, axis);
        self.map_points(move |p| {
            let v = [p[0] - about[0], p[1] - about[1], p[2] - about[2]];
            [
                about[0] + m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
                about[1] + m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
                about[2] + m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
            ]
        })
    }

    /// Move the family's centre to `point` (Reference `move_to`).
    #[must_use]
    pub fn moved_to(self, point: Vec3) -> Self {
        let c = self.center_point();
        self.shifted([point[0] - c[0], point[1] - c[1], point[2] - c[2]])
    }

    /// Reference `put_start_and_end_on` for a detached builder: scale,
    /// turn, and shift so the first point lands on `start` and the last on
    /// `end`.
    ///
    /// Returns `self` unchanged when the current endpoints coincide —
    /// there is no such transform, and the Reference raises here. Classes
    /// that can rebuild instead (`Line`) do that rather than call this.
    #[must_use]
    pub fn put_start_and_end_on(self, start: Vec3, end: Vec3) -> Self {
        let (Some(&curr_start), Some(&curr_end)) = (self.points.first(), self.points.last()) else {
            return self;
        };
        let curr_vect = sub(curr_end, curr_start);
        if curr_vect == [0.0; 3] {
            return self;
        }
        let target_vect = sub(end, start);
        let scale = space_ops::get_norm(target_vect) / space_ops::get_norm(curr_vect);
        let scaled = self.scaled_about(scale, curr_start);
        let center = scaled.center_point();
        let turned = scaled.rotated_about(
            space_ops::angle_of_vector(target_vect) - space_ops::angle_of_vector(curr_vect),
            OUT,
            center,
        );
        let curr_xy = space_ops::get_norm([curr_vect[0], curr_vect[1], 0.0]);
        let target_xy = space_ops::get_norm([target_vect[0], target_vect[1], 0.0]);
        let center = turned.center_point();
        let tilted = turned.rotated_about(
            space_ops::angle_of_vector([curr_xy, curr_vect[2], 0.0])
                - space_ops::angle_of_vector([target_xy, target_vect[2], 0.0]),
            [-target_vect[1], target_vect[0], 0.0],
            center,
        );
        let now_start = tilted.points.first().copied().unwrap_or(start);
        tilted.shifted(sub(start, now_start))
    }
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// Translate a tag's geometric payload — the one transform a detached
/// builder performs often enough (`arc_center`, `Dot(point)`) to be worth
/// keeping the hint through.
fn shifted_tag(tag: ShapeTag, offset: Vec3) -> ShapeTag {
    let moved = |p: Vec3| [p[0] + offset[0], p[1] + offset[1], p[2] + offset[2]];
    match tag {
        ShapeTag::General | ShapeTag::Polyline { .. } => tag,
        ShapeTag::Line {
            start,
            end,
            path_arc,
            buff,
        } => ShapeTag::Line {
            start: moved(start),
            end: moved(end),
            path_arc,
            buff,
        },
        ShapeTag::Arc {
            center,
            radius,
            start_angle,
            angle,
        } => ShapeTag::Arc {
            center: moved(center),
            radius,
            start_angle,
            angle,
        },
        ShapeTag::Circle { center, radius } => ShapeTag::Circle {
            center: moved(center),
            radius,
        },
        ShapeTag::Dot { center, radius } => ShapeTag::Dot {
            center: moved(center),
            radius,
        },
        ShapeTag::Rect {
            center,
            width,
            height,
        } => ShapeTag::Rect {
            center: moved(center),
            width,
            height,
        },
        ShapeTag::RoundedRect {
            center,
            width,
            height,
            corner_radius,
        } => ShapeTag::RoundedRect {
            center: moved(center),
            width,
            height,
            corner_radius,
        },
    }
}

impl From<VMobject> for Mobject {
    fn from(v: VMobject) -> Self {
        let VMobject {
            points,
            style,
            uniforms,
            shape,
            submobjects,
        } = v;

        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len());
        #[allow(clippy::cast_possible_truncation)]
        let flat: Vec<f32> = points
            .iter()
            .flat_map(|p| p.iter().map(|v| *v as f32))
            .collect();
        buffer.write_range("point", 0, &flat);
        // Joint angles are a function of the path, so they are written
        // with it rather than left for a later refresh to remember.
        if let Ok(path) = QuadPath::from_points(points) {
            #[allow(clippy::cast_possible_truncation)]
            let angles: Vec<f32> = path.joint_angles().iter().map(|a| *a as f32).collect();
            buffer.write_range("joint_angle", 0, &angles);
        }
        style.write(&mut buffer);

        Mobject {
            buffer,
            uniforms,
            shape,
            submobjects: submobjects.into_iter().map(Mobject::from).collect(),
        }
    }
}

/// `VGroup`: a vectorized mobject with no geometry of its own, holding
/// children (Appendix A `types/vectorized_mobject`).
///
/// The Reference's `VGroup` refuses non-VMobject members; here that is the
/// type system's job, since [`VMobject`] is the only thing it accepts.
#[must_use]
pub fn v_group(children: impl IntoIterator<Item = VMobject>) -> VMobject {
    VMobject::new().with_children(children)
}

/// `VectorizedPoint`: a single location that behaves like a mobject.
///
/// The Reference gives it four copies of the same point so it survives
/// path arithmetic (`VectorizedPoint.__init__` sets `[location] * 4`);
/// ours keeps the shared-anchor invariant instead, which needs an odd
/// count, so it is three — one null curve, the degenerate path §7.1
/// specifies. The observable behaviour (a zero-extent mobject at
/// `location`, with `get_start == get_end == location`) is identical.
#[must_use]
pub fn vectorized_point(location: Vec3) -> VMobject {
    VMobject::from_points(vec![location; 3])
        .map_style(|s| s.stroke(s.stroke_color, 0.0, 0.0).fill(s.fill_color, 0.0))
}

/// `CurvesAsSubmobjects`: one child per Bézier curve of the source.
///
/// Used by the passing-flash animations, which reveal a path curve by
/// curve. Style is inherited by every piece.
#[must_use]
pub fn curves_as_submobjects(source: &VMobject) -> VMobject {
    let mut group = VMobject::new().with_style(source.style());
    if let Ok(path) = source.path() {
        for tuple in path.bezier_tuples() {
            group =
                group.with_child(VMobject::from_points(tuple.to_vec()).with_style(source.style()));
        }
    }
    group
}

/// `DashedVMobject`: a source path cut into `num_dashes` dashes.
///
/// **The dashes are placed by true arc length** (BN-03's dash corollary):
/// the Reference walks `pointwise_become_partial` in *curve-index* space,
/// so on a path whose curves differ in length — every arc, every smoothed
/// curve — its dashes bunch up where the curves are short. Ours cut at
/// equal true-length proportions, so a dashed circle has evenly spaced
/// dashes, which is what the name always promised.
///
/// `positive_space_ratio` is the fraction of each period that is dash;
/// `dash_offset` shifts the pattern along the path, both as in the
/// Reference.
#[must_use]
pub fn dashed_vmobject(
    source: &VMobject,
    num_dashes: usize,
    positive_space_ratio: f64,
    dash_offset: f64,
) -> VMobject {
    let mut group = VMobject::new().with_style(source.style());
    if num_dashes == 0 {
        return group;
    }
    let Ok(path) = source.path() else {
        return group;
    };
    if !path.has_points() {
        return group;
    }

    // The Reference's period arithmetic (vectorized_mobject.py), kept: the
    // dashes tile [0, 1] with `positive_space_ratio` of each period drawn.
    let n = num_dashes as f64;
    let full_period = 1.0 / n;
    let dash_len = full_period * positive_space_ratio;
    let table = fmn_geom::ArcLengthTable::for_path(&path);
    for i in 0..num_dashes {
        let alpha = i as f64 * full_period + dash_offset * full_period;
        let (a, b) = (alpha, alpha + dash_len);
        if let Some(points) = subpath_by_length(&path, &table, a.rem_euclid(1.0), b) {
            group = group.with_child(VMobject::from_points(points).with_style(source.style()));
        }
    }
    group
}

/// The true-length restriction of `path` to `[a, b]`, as a point run.
///
/// Both ends are converted from length proportion to the *curve-index*
/// proportion `QuadPath::partial_points` speaks, so the cut lands where
/// the arc length says it should.
fn subpath_by_length(
    path: &QuadPath,
    table: &fmn_geom::ArcLengthTable,
    a: f64,
    b: f64,
) -> Option<Vec<Vec3>> {
    let n_curves = path.num_curves();
    if n_curves == 0 {
        return None;
    }
    let index_alpha = |alpha: f64| -> f64 {
        let clamped = alpha.clamp(0.0, 1.0);
        match table.curve_and_t_at(path, clamped) {
            Some((curve, t)) => (curve as f64 + t) / n_curves as f64,
            None => clamped,
        }
    };
    let (ia, ib) = (index_alpha(a), index_alpha(b.min(1.0)));
    if ib <= ia {
        return None;
    }
    QuadPath::partial_points(path.points(), ia, ib).map(|(points, _, _)| points)
}

/// `VHighlight`: concentric copies of a source at decreasing opacity, the
/// Reference's cheap glow.
///
/// `n_layers` copies are scaled outward from the source's own centre; the
/// outermost is the faintest. The Reference's defaults are 5 layers, a
/// maximum outward stroke of 5, and opacities fading to zero.
#[must_use]
pub fn v_highlight(
    source: &VMobject,
    n_layers: usize,
    max_stroke_addition: f64,
    color: Srgb,
) -> VMobject {
    let mut group = VMobject::new();
    if n_layers == 0 {
        return group;
    }
    for i in 0..n_layers {
        let t = (i + 1) as f64 / n_layers as f64;
        let width = max_stroke_addition * t;
        let opacity = 1.0 - t;
        group = group.with_child(
            VMobject::from_points(source.points().to_vec())
                .map_style(|s| s.stroke(color, width, opacity).fill(color, 0.0)),
        );
    }
    group
}

/// Add a built [`VMobject`] to a stage and return its handle — sugar for
/// `stage.add(vmob)` that reads the way the README's examples do.
pub fn add_to(stage: &mut Stage, vmob: VMobject) -> Mob {
    stage.add(vmob)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arc::Circle;
    use crate::style::VStyle;
    use fmn_core::constants::{RED, TAU};

    #[test]
    fn a_built_vmobject_carries_points_style_uniforms_and_shape() {
        let mut stage = Stage::new();
        let mob = stage.add(
            VMobject::from_points(vec![[0.0; 3], [1.0, 1.0, 0.0], [2.0, 0.0, 0.0]])
                .with_color(RED)
                .with_joint_type(JointType::Bevel)
                .with_shape(ShapeTag::Polyline {
                    vertices: 2,
                    closed: false,
                }),
        );
        assert_eq!(stage.get_points(mob).unwrap().len(), 3);
        // Colours round-trip through f32 records (§6.1), so this is an
        // f32-tolerance comparison, not a bit-for-bit one.
        let stroke = stage.get_stroke_color(mob).unwrap();
        assert!(
            (stroke.r - RED.r).abs() < 1e-6
                && (stroke.g - RED.g).abs() < 1e-6
                && (stroke.b - RED.b).abs() < 1e-6,
            "{stroke:?}"
        );
        assert_eq!(
            stage.get(mob).unwrap().uniforms().joint_type,
            JointType::Bevel
        );
        assert!(matches!(
            stage.primitive_hint(mob),
            Some(ShapeTag::Polyline { vertices: 2, .. })
        ));
    }

    #[test]
    fn joint_angles_are_written_with_the_path() {
        let mut stage = Stage::new();
        let mob = stage.add(VMobject::from_points(vec![
            [0.0; 3],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 1.0, 0.0],
            [2.0, 2.0, 0.0],
        ]));
        let angles = stage
            .get(mob)
            .unwrap()
            .buffer
            .read_column("joint_angle")
            .unwrap();
        assert_eq!(angles.len(), 5);
        // The corner at index 2 turns; the ends do not.
        assert!(angles[2].abs() > 1e-3, "corner joint angle {}", angles[2]);
        assert!(angles[0].abs() < 1e-6);
    }

    #[test]
    fn children_enter_the_arena_as_family() {
        let mut stage = Stage::new();
        let group = stage.add(v_group([
            VMobject::from_points(vec![[0.0; 3], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]]),
            VMobject::from_points(vec![[0.0; 3], [0.0, 1.0, 0.0], [0.0, 2.0, 0.0]]),
        ]));
        assert_eq!(stage.get(group).unwrap().submobjects().len(), 2);
        assert_eq!(stage.family(group).len(), 3);
    }

    #[test]
    fn vectorized_point_is_a_degenerate_path_at_its_location() {
        let mut stage = Stage::new();
        let mob = stage.add(vectorized_point([1.0, 2.0, 3.0]));
        assert_eq!(stage.get_start(mob), Some([1.0, 2.0, 3.0]));
        assert_eq!(stage.get_end(mob), Some([1.0, 2.0, 3.0]));
        let bbox = stage.get_bounding_box(mob);
        assert_eq!(bbox.width(), 0.0);
        assert_eq!(bbox.height(), 0.0);
        assert!(!stage.has_stroke(mob), "a point draws nothing by itself");
    }

    #[test]
    fn curves_as_submobjects_splits_by_curve() {
        let circle = Circle::new().build();
        let pieces = curves_as_submobjects(&circle);
        assert_eq!(
            pieces.children().len(),
            circle.path().unwrap().num_curves(),
            "one child per Bezier"
        );
        for piece in pieces.children() {
            assert_eq!(piece.points().len(), 3);
        }
    }

    #[test]
    fn dashes_are_evenly_spaced_by_true_length() {
        // On a circle every curve is the same length, so index-space and
        // length-space agree and the dashes are evenly spaced either way.
        let circle = Circle::new().radius(1.0).build();
        let dashed = dashed_vmobject(&circle, 8, 0.5, 0.0);
        assert_eq!(dashed.children().len(), 8);
        let lengths: Vec<f64> = dashed
            .children()
            .iter()
            .map(|d| d.path().unwrap().get_arc_length())
            .collect();
        let first = lengths[0];
        for l in &lengths {
            assert!((l - first).abs() < 1e-6, "dash lengths differ: {lengths:?}");
        }
        // Half of the circumference is drawn, at ratio 0.5.
        let total: f64 = lengths.iter().sum();
        assert!(
            (total - TAU / 2.0).abs() < 1e-3,
            "drawn length {total} vs {}",
            TAU / 2.0
        );
    }

    #[test]
    fn dashes_on_an_uneven_path_still_measure_equal() {
        // A path whose curves differ wildly in length: index-space dashes
        // would come out uneven, true-length dashes do not (BN-03).
        let points = vec![
            [0.0, 0.0, 0.0],
            [0.05, 0.0, 0.0],
            [0.1, 0.0, 0.0],
            [3.0, 0.0, 0.0],
            [6.0, 0.0, 0.0],
        ];
        let source = VMobject::from_points(points);
        let dashed = dashed_vmobject(&source, 4, 0.5, 0.0);
        assert_eq!(dashed.children().len(), 4);
        let lengths: Vec<f64> = dashed
            .children()
            .iter()
            .map(|d| d.path().unwrap().get_arc_length())
            .collect();
        let first = lengths[0];
        for l in &lengths {
            assert!((l - first).abs() < 1e-6, "dash lengths differ: {lengths:?}");
        }
    }

    #[test]
    fn zero_dashes_and_empty_sources_are_defined() {
        let circle = Circle::new().build();
        assert!(dashed_vmobject(&circle, 0, 0.5, 0.0).children().is_empty());
        let empty = VMobject::new();
        assert!(dashed_vmobject(&empty, 4, 0.5, 0.0).children().is_empty());
        assert!(curves_as_submobjects(&empty).children().is_empty());
        assert!(v_highlight(&empty, 0, 5.0, RED).children().is_empty());
    }

    #[test]
    fn highlight_layers_fade_outward() {
        let circle = Circle::new().build();
        let glow = v_highlight(&circle, 5, 5.0, RED);
        assert_eq!(glow.children().len(), 5);
        let widths: Vec<f64> = glow
            .children()
            .iter()
            .map(|c| c.style().stroke_width)
            .collect();
        assert!(widths.windows(2).all(|w| w[0] < w[1]), "{widths:?}");
        let opacities: Vec<f64> = glow
            .children()
            .iter()
            .map(|c| c.style().stroke_opacity)
            .collect();
        assert!(opacities.windows(2).all(|o| o[0] > o[1]), "{opacities:?}");
    }

    #[test]
    fn map_points_drops_the_tag_but_shifted_keeps_it() {
        let circle = Circle::new().radius(2.0).build();
        assert!(matches!(circle.shape(), ShapeTag::Circle { .. }));
        let squashed = circle.clone().map_points(|p| [p[0], p[1] * 0.5, p[2]]);
        assert_eq!(squashed.shape(), ShapeTag::General);
        let moved = circle.shifted([1.0, 0.0, 0.0]);
        match moved.shape() {
            ShapeTag::Circle { center, radius } => {
                assert!((center[0] - 1.0).abs() < 1e-12);
                assert!((radius - 2.0).abs() < 1e-12);
            }
            other => panic!("shift lost the circle: {other:?}"),
        }
    }
}
