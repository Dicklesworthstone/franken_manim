//! The Line lineage (§12.1): `Line`, `DashedLine`, `TangentLine`,
//! `Elbow`, `Arrow`, `StrokeArrow`, and `Vector`.
//!
//! # Buffers and dashes by true length (BN-03)
//!
//! `Line(buff=…)` trims the segment at both ends. The Reference computes
//! that trim with `get_arc_length`, whose value for a `path_arc` line is a
//! closed form but whose `pointwise_become_partial` cut is in curve-index
//! space — consistent only because a straight line is one curve. Ours
//! trims by **true arc length** throughout, so a `path_arc` line's buffer
//! is the distance along the curve the caller asked for, and `DashedLine`
//! spaces its dashes evenly along the real path.

use fmn_core::color::Srgb;
use fmn_core::constants::{
    DEFAULT_LIGHT_COLOR, LEFT, MED_SMALL_BUFF, ORIGIN, OUT, PI, RIGHT, UP, UR,
};
use fmn_core::types::Vec3;
use fmn_geom::{ArcLengthTable, QuadPath, space_ops};
use fmn_mobject::{Mobject, ShapeTag};

use crate::poly::ArrowTip;
use crate::style::Style;
use crate::tip::{TipEnd, attach_tip};
use crate::vmobject::{VMobject, dashed_vmobject};

/// The Reference's `DEFAULT_DASH_LENGTH`.
pub const DEFAULT_DASH_LENGTH: f64 = 0.05;

/// `Line(start, end, buff, path_arc)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Line {
    start: Vec3,
    end: Vec3,
    buff: f64,
    path_arc: f64,
    style: Style,
}

impl Default for Line {
    fn default() -> Self {
        Self::new(LEFT, RIGHT)
    }
}

impl Line {
    /// A segment between two points (the Reference's defaults are
    /// `LEFT`–`RIGHT`).
    #[must_use]
    pub fn new(start: Vec3, end: Vec3) -> Self {
        Self {
            start,
            end,
            buff: 0.0,
            path_arc: 0.0,
            style: Style::default(),
        }
    }

    /// Distance trimmed from each end, along the path.
    #[must_use]
    pub fn buff(mut self, buff: f64) -> Self {
        self.buff = buff;
        self
    }

    /// Bend the segment into an arc subtending this angle.
    #[must_use]
    pub fn path_arc(mut self, path_arc: f64) -> Self {
        self.path_arc = path_arc;
        self
    }

    /// Set stroke and fill colour.
    #[must_use]
    pub fn color(mut self, color: Srgb) -> Self {
        self.style = self.style.color(color);
        self
    }

    /// Replace the style.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// The configured ends, before any buffer.
    #[must_use]
    pub fn ends(&self) -> (Vec3, Vec3) {
        (self.start, self.end)
    }

    /// Build the detached mobject.
    #[must_use]
    pub fn build(self) -> VMobject {
        let points = points_by_ends(self.start, self.end, self.buff, self.path_arc);
        VMobject::from_points(points)
            .with_style(self.style)
            .with_shape(ShapeTag::Line {
                start: self.start,
                end: self.end,
                path_arc: self.path_arc,
                buff: self.buff,
            })
    }
}

impl From<Line> for Mobject {
    fn from(l: Line) -> Self {
        l.build().into()
    }
}

/// The Reference's `set_points_by_ends`, with the buffer measured along
/// the path rather than across the chord.
#[must_use]
pub fn points_by_ends(start: Vec3, end: Vec3, buff: f64, path_arc: f64) -> Vec<Vec3> {
    let mut path = QuadPath::new();
    path.start_new_path(start);
    let _ = path.add_arc_to(end, path_arc, None);
    if buff <= 0.0 {
        return path.points().to_vec();
    }
    let length = path.get_arc_length();
    if length <= 0.0 {
        return path.points().to_vec();
    }
    let alpha = (buff / length).min(0.5);
    match partial_by_length(&path, alpha, 1.0 - alpha) {
        Some(points) => points,
        None => path.points().to_vec(),
    }
}

/// The true-length restriction of a path to `[a, b]`, as a point run.
fn partial_by_length(path: &QuadPath, a: f64, b: f64) -> Option<Vec<Vec3>> {
    let n_curves = path.num_curves();
    if n_curves == 0 {
        return None;
    }
    let table = ArcLengthTable::for_path(path);
    let to_index = |alpha: f64| -> f64 {
        match table.curve_and_t_at(path, alpha.clamp(0.0, 1.0)) {
            Some((curve, t)) => (curve as f64 + t) / n_curves as f64,
            None => alpha,
        }
    };
    QuadPath::partial_points(path.points(), to_index(a), to_index(b)).map(|(points, _, _)| points)
}

/// Reference `Line.get_arc_length`.
///
/// Appendix C ruling **C-3**: the Reference's version only corrects for
/// curvature when `path_arc > 0`, so a negative `path_arc` — a line bent
/// the other way — reports its chord length instead of its arc length.
/// Ours is the true arc length for every `path_arc`, which is what the
/// name says and what dashes, tips, and `MoveAlongPath` all consume
/// (BN-03).
#[must_use]
pub fn line_arc_length(shape: &VMobject) -> f64 {
    shape.path().map(|p| p.get_arc_length()).unwrap_or(0.0)
}

/// Reference `Line.get_vector`.
#[must_use]
pub fn line_vector(shape: &VMobject) -> Vec3 {
    let points = shape.points();
    match (points.first(), points.last()) {
        (Some(a), Some(b)) => sub(*b, *a),
        _ => ORIGIN,
    }
}

/// Reference `Line.get_unit_vector`.
#[must_use]
pub fn line_unit_vector(shape: &VMobject) -> Vec3 {
    space_ops::normalize(line_vector(shape))
}

/// Reference `Line.get_angle`.
#[must_use]
pub fn line_angle(shape: &VMobject) -> f64 {
    space_ops::angle_of_vector(line_vector(shape))
}

/// Reference `Line.get_slope`.
#[must_use]
pub fn line_slope(shape: &VMobject) -> f64 {
    line_angle(shape).tan()
}

/// Reference `Line.get_projection`: the projection of a point onto the
/// line through the segment's ends.
#[must_use]
pub fn line_projection(shape: &VMobject, point: Vec3) -> Vec3 {
    let unit = line_unit_vector(shape);
    let start = shape.points().first().copied().unwrap_or(ORIGIN);
    add(start, scale(unit, space_ops::dot(sub(point, start), unit)))
}

/// `DashedLine(start, end, dash_length, positive_space_ratio)`.
///
/// The dash count is the Reference's — `ceil(length / (dash_length /
/// positive_space_ratio))` — and the placement is by true length.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DashedLine {
    line: Line,
    dash_length: f64,
    positive_space_ratio: f64,
}

impl DashedLine {
    /// A dashed segment at the Reference's default dash length.
    #[must_use]
    pub fn new(start: Vec3, end: Vec3) -> Self {
        Self {
            line: Line::new(start, end),
            dash_length: DEFAULT_DASH_LENGTH,
            positive_space_ratio: 0.5,
        }
    }

    /// Length of each dash.
    #[must_use]
    pub fn dash_length(mut self, length: f64) -> Self {
        self.dash_length = length;
        self
    }

    /// Fraction of each period that is drawn.
    #[must_use]
    pub fn positive_space_ratio(mut self, ratio: f64) -> Self {
        self.positive_space_ratio = ratio;
        self
    }

    /// Bend the segment.
    #[must_use]
    pub fn path_arc(mut self, path_arc: f64) -> Self {
        self.line = self.line.path_arc(path_arc);
        self
    }

    /// Set stroke and fill colour.
    #[must_use]
    pub fn color(mut self, color: Srgb) -> Self {
        self.line = self.line.color(color);
        self
    }

    /// Replace the style.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.line = self.line.style(style);
        self
    }

    /// The Reference's `calculate_num_dashes`, over the true length.
    #[must_use]
    pub fn num_dashes(&self) -> usize {
        if self.positive_space_ratio <= 0.0 || self.dash_length <= 0.0 {
            return 1;
        }
        let full_period = self.dash_length / self.positive_space_ratio;
        let length = line_arc_length(&self.line.build());
        ((length / full_period).ceil() as usize).max(1)
    }

    /// Build the detached mobject: a group of dashes, with no path of its
    /// own (the Reference clears the parent's points too).
    #[must_use]
    pub fn build(self) -> VMobject {
        let source = self.line.build();
        let n = self.num_dashes();
        dashed_vmobject(&source, n, self.positive_space_ratio, 0.0)
    }
}

impl From<DashedLine> for Mobject {
    fn from(d: DashedLine) -> Self {
        d.build().into()
    }
}

/// `TangentLine(vmob, alpha, length, d_alpha)`: a segment tangent to a
/// path at the given proportion.
///
/// `alpha` is a **true-length** proportion (BN-03), so "tangent at the
/// halfway point" means half way along the curve, not half way through its
/// curve list.
#[must_use]
pub fn tangent_line(
    source: &VMobject,
    alpha: f64,
    length: f64,
    d_alpha: f64,
    style: Style,
) -> VMobject {
    let Ok(path) = source.path() else {
        return VMobject::new().with_style(style);
    };
    let a1 = (alpha - d_alpha).clamp(0.0, 1.0);
    let a2 = (alpha + d_alpha).clamp(0.0, 1.0);
    let (Some(p1), Some(p2)) = (
        path.point_from_proportion(a1),
        path.point_from_proportion(a2),
    ) else {
        return VMobject::new().with_style(style);
    };
    let line = Line::new(p1, p2).style(style).build();
    let current = space_ops::get_norm(sub(p2, p1));
    if current == 0.0 {
        return line;
    }
    let center = line.center_point();
    line.scaled_about(length / current, center)
}

/// `Elbow(width, angle)`: the L-shaped corner mark.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Elbow {
    width: f64,
    angle: f64,
    style: Style,
}

impl Default for Elbow {
    fn default() -> Self {
        Self::new()
    }
}

impl Elbow {
    /// The Reference's default elbow: 0.2 wide, unrotated.
    #[must_use]
    pub fn new() -> Self {
        Self {
            width: 0.2,
            angle: 0.0,
            style: Style::default(),
        }
    }

    /// Width of the corner mark.
    #[must_use]
    pub fn width(mut self, width: f64) -> Self {
        self.width = width;
        self
    }

    /// Rotation about the origin.
    #[must_use]
    pub fn angle(mut self, angle: f64) -> Self {
        self.angle = angle;
        self
    }

    /// Set stroke and fill colour.
    #[must_use]
    pub fn color(mut self, color: Srgb) -> Self {
        self.style = self.style.color(color);
        self
    }

    /// Replace the style.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Build the detached mobject.
    #[must_use]
    pub fn build(self) -> VMobject {
        let mut path = QuadPath::new();
        let _ = path.set_points_as_corners(&[UP, UR, RIGHT]);
        let built = VMobject::from_path(&path).with_style(self.style);
        // The Reference sizes and rotates about the ORIGIN, not the
        // centre, so the corner stays where it was put.
        let current = built.length_over_dim(0);
        let sized = if current == 0.0 {
            built
        } else {
            built.scaled_about(self.width / current, ORIGIN)
        };
        sized.rotated_about(self.angle, OUT, ORIGIN)
    }
}

impl From<Elbow> for Mobject {
    fn from(e: Elbow) -> Self {
        e.build().into()
    }
}

/// `Arrow(start, end, buff, path_arc, …)`: a filled arrow whose shaft
/// tapers into its head.
///
/// The Reference builds the whole outline — shaft edges, head, and the
/// return path — as one filled polygon whose thickness is derived from
/// `thickness`, `tip_width_ratio`, and the two ratio caps. That
/// construction is kept: an `Arrow` is one filled path, not a stroked line
/// with a triangle glued on (that is `StrokeArrow`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arrow {
    start: Vec3,
    end: Vec3,
    buff: f64,
    path_arc: f64,
    thickness: f64,
    tip_width_ratio: f64,
    tip_angle: f64,
    max_tip_length_to_length_ratio: f64,
    max_width_to_length_ratio: f64,
    style: Style,
}

/// The Reference's `Arrow.tickness_multiplier` (its spelling).
const THICKNESS_MULTIPLIER: f64 = 0.015;

impl Arrow {
    /// An arrow between two points, at the Reference's defaults.
    #[must_use]
    pub fn new(start: Vec3, end: Vec3) -> Self {
        Self {
            start,
            end,
            buff: MED_SMALL_BUFF,
            path_arc: 0.0,
            thickness: 3.0,
            tip_width_ratio: 5.0,
            tip_angle: PI / 3.0,
            max_tip_length_to_length_ratio: 0.5,
            max_width_to_length_ratio: 0.1,
            style: Style::default().fill(DEFAULT_LIGHT_COLOR, 1.0).stroke(
                DEFAULT_LIGHT_COLOR,
                0.0,
                1.0,
            ),
        }
    }

    /// `Vector(direction)`: an arrow from the origin, with no buffer.
    #[must_use]
    pub fn vector(direction: Vec3) -> Self {
        Self::new(ORIGIN, direction).buff(0.0)
    }

    /// Distance trimmed from each end.
    #[must_use]
    pub fn buff(mut self, buff: f64) -> Self {
        self.buff = buff;
        self
    }

    /// Bend the arrow into an arc.
    #[must_use]
    pub fn path_arc(mut self, path_arc: f64) -> Self {
        self.path_arc = path_arc;
        self
    }

    /// Shaft thickness.
    #[must_use]
    pub fn thickness(mut self, thickness: f64) -> Self {
        self.thickness = thickness;
        self
    }

    /// Ratio of head width to shaft width.
    #[must_use]
    pub fn tip_width_ratio(mut self, ratio: f64) -> Self {
        self.tip_width_ratio = ratio;
        self
    }

    /// Head half-angle.
    #[must_use]
    pub fn tip_angle(mut self, angle: f64) -> Self {
        self.tip_angle = angle;
        self
    }

    /// Set fill and stroke colour.
    #[must_use]
    pub fn color(mut self, color: Srgb) -> Self {
        self.style = self.style.color(color);
        self
    }

    /// Replace the style.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// The Reference's `get_key_dimensions`: shaft width, head width, and
    /// head length, each capped against the arrow's own length.
    #[must_use]
    pub fn key_dimensions(&self, length: f64) -> (f64, f64, f64) {
        let mut width = self.thickness * THICKNESS_MULTIPLIER;
        let w_ratio = fdiv(self.max_width_to_length_ratio, fdiv(width, length));
        if w_ratio < 1.0 {
            width *= w_ratio;
        }
        let mut tip_width = self.tip_width_ratio * width;
        let mut tip_length = tip_width / (2.0 * (self.tip_angle / 2.0).tan());
        let t_ratio = fdiv(
            self.max_tip_length_to_length_ratio,
            fdiv(tip_length, length),
        );
        if t_ratio < 1.0 {
            tip_length *= t_ratio;
            tip_width *= t_ratio;
        }
        (width, tip_width, tip_length)
    }

    /// Build the detached mobject.
    #[must_use]
    pub fn build(self) -> VMobject {
        let vect = sub(self.end, self.start);
        let length = space_ops::get_norm(vect).max(1e-8);
        let unit = space_ops::normalize(vect);
        let (width, tip_width, tip_length) = self.key_dimensions(length - self.buff);

        // Trim the ends by the buffer, and — for a curved arrow — rotate
        // them about the arc's centre, as the Reference does.
        let (start, end, path_arc) = if self.path_arc == 0.0 {
            (
                add(self.start, scale(unit, self.buff)),
                sub(self.end, scale(unit, self.buff)),
                0.0,
            )
        } else {
            let r = length / 2.0 / (self.path_arc / 2.0).sin();
            let midpoint = scale(add(self.start, self.end), 0.5);
            let center = add(
                midpoint,
                scale(
                    space_ops::rotate_vector(scale(vect, 0.5), PI / 2.0, OUT),
                    1.0 / (self.path_arc / 2.0).tan(),
                ),
            );
            let start = add(
                center,
                space_ops::rotate_vector(sub(self.start, center), self.buff / r, OUT),
            );
            let end = add(
                center,
                space_ops::rotate_vector(sub(self.end, center), -self.buff / r, OUT),
            );
            (
                start,
                end,
                self.path_arc - (2.0 * self.buff + tip_length) / r,
            )
        };

        let vect = sub(end, start);
        let length = space_ops::get_norm(vect);

        // The outline, drawn pointing left at the origin as the Reference
        // does, then rotated and shifted onto the real ends.
        let (points1, points2) = if path_arc == 0.0 {
            let base: Vec<Vec3> = [RIGHT, scale(RIGHT, 0.5), ORIGIN]
                .iter()
                .map(|p| add(scale(*p, length - tip_length), scale(UP, width / 2.0)))
                .collect();
            let mut mirrored: Vec<Vec3> = base.iter().rev().copied().collect();
            for p in &mut mirrored {
                *p = sub(*p, scale(UP, width));
            }
            (base, mirrored)
        } else {
            let r = length / 2.0 / (path_arc / 2.0).sin();
            let arc: Vec<Vec3> = QuadPath::arc(0.0, path_arc, 1.0, ORIGIN, None)
                .points()
                .to_vec();
            let outer: Vec<Vec3> = arc.iter().map(|p| scale(*p, r + width / 2.0)).collect();
            let inner: Vec<Vec3> = arc
                .iter()
                .rev()
                .map(|p| scale(*p, r - width / 2.0))
                .collect();
            let rot = space_ops::rotation_matrix_transpose(PI / 2.0 - path_arc, OUT);
            let place = |pts: Vec<Vec3>| -> Vec<Vec3> {
                pts.into_iter()
                    .map(|p| {
                        let q = mul_point_mat(p, &rot);
                        [q[0], q[1] - r, q[2]]
                    })
                    .collect()
            };
            (place(outer), place(inner))
        };

        let mut path = QuadPath::new();
        let _ = path.set_points(points1.clone());
        let _ = path.add_line_to(scale(UP, tip_width / 2.0), true);
        let _ = path.add_line_to(scale(LEFT, tip_length), true);
        // The Reference records where the arrow's point landed: its
        // `get_end` is that vertex, not the path's last point (which is
        // back on the shaft, where the outline closes).
        let tip_index = path.num_points() - 1;
        let _ = path.add_line_to(scale(UP, -tip_width / 2.0), true);
        if let Some(&first) = points2.first() {
            let _ = path.add_line_to(first, true);
        }
        let _ = path.add_subpath(&points2);
        if let Some(&first) = points1.first() {
            let _ = path.add_line_to(first, true);
        }

        let built = VMobject::from_path(&path).with_style(self.style);
        let current = arrow_vector(&built, tip_index);
        let center = built.center_point();
        let turned = built.rotated_about(
            space_ops::angle_of_vector(vect) - space_ops::angle_of_vector(current),
            OUT,
            center,
        );
        // The out-of-plane tilt, which is a no-op for a planar arrow and
        // the whole story for one pointing out of the screen.
        let unit = space_ops::normalize(vect);
        let axis = space_ops::rotate_vector(
            space_ops::normalize(arrow_vector(&turned, tip_index)),
            -PI / 2.0,
            OUT,
        );
        let center = turned.center_point();
        let tilted = turned.rotated_about(PI / 2.0 - unit[2].clamp(-1.0, 1.0).acos(), axis, center);

        let shifted = tilted
            .clone()
            .shifted(sub(start, arrow_start(&tilted, tip_index)));
        shifted.with_shape(ShapeTag::Line {
            start: self.start,
            end: self.end,
            path_arc: self.path_arc,
            buff: self.buff,
        })
    }
}

impl From<Arrow> for Mobject {
    fn from(a: Arrow) -> Self {
        a.build().into()
    }
}

/// `StrokeArrow(start, end, …)`: a stroked line whose width tapers into a
/// separate tip, rather than one filled outline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrokeArrow {
    line: Line,
    tip_width_ratio: f64,
    tip_len_to_width: f64,
    max_tip_length_to_length_ratio: f64,
}

impl StrokeArrow {
    /// A stroke arrow at the Reference's defaults (width 5, buff 0.25).
    #[must_use]
    pub fn new(start: Vec3, end: Vec3) -> Self {
        Self {
            line: Line::new(start, end)
                .buff(0.25)
                .style(Style::default().stroke(DEFAULT_LIGHT_COLOR, 5.0, 1.0)),
            tip_width_ratio: 5.0,
            tip_len_to_width: 0.0075,
            max_tip_length_to_length_ratio: 0.3,
        }
    }

    /// Bend the arrow.
    #[must_use]
    pub fn path_arc(mut self, path_arc: f64) -> Self {
        self.line = self.line.path_arc(path_arc);
        self
    }

    /// Set the stroke colour.
    #[must_use]
    pub fn color(mut self, color: Srgb) -> Self {
        self.line = self.line.color(color);
        self
    }

    /// Replace the style.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.line = self.line.style(style);
        self
    }

    /// Build the detached mobject: the shaft, with a tip sized from the
    /// stroke width exactly as the Reference's `insert_tip_anchor` does.
    #[must_use]
    pub fn build(self) -> VMobject {
        let shaft = self.line.build();
        let style = shaft.style();
        let arc_len = line_arc_length(&shaft);
        let tip_len = style.stroke_width * self.tip_width_ratio * self.tip_len_to_width;
        let tip_length = if arc_len <= 0.0 {
            tip_len
        } else if tip_len >= self.max_tip_length_to_length_ratio * arc_len {
            self.max_tip_length_to_length_ratio * arc_len
        } else {
            tip_len
        };
        let tip_width = self.tip_width_ratio * style.stroke_width * self.tip_len_to_width * 2.0;
        attach_tip(
            shaft,
            ArrowTip::new()
                .length(tip_length)
                .width(tip_width.max(tip_length))
                .color(style.stroke_color),
            TipEnd::End,
        )
    }
}

impl From<StrokeArrow> for Mobject {
    fn from(a: StrokeArrow) -> Self {
        a.build().into()
    }
}

/// Reference `Arrow.get_start`: the midpoint of the shaft's back edge —
/// `0.5 * (points[0] + points[-3])`, the two corners the outline starts
/// and ends at.
#[must_use]
pub fn arrow_start(arrow: &VMobject, _tip_index: usize) -> Vec3 {
    let p = arrow.points();
    if p.len() < 3 {
        return ORIGIN;
    }
    scale(add(p[0], p[p.len() - 3]), 0.5)
}

/// Reference `Arrow.get_end`: the vertex the head points at.
#[must_use]
pub fn arrow_end(arrow: &VMobject, tip_index: usize) -> Vec3 {
    arrow.points().get(tip_index).copied().unwrap_or(ORIGIN)
}

fn arrow_vector(arrow: &VMobject, tip_index: usize) -> Vec3 {
    sub(arrow_end(arrow, tip_index), arrow_start(arrow, tip_index))
}

/// The Reference's `fdiv`: division with a defined answer at zero.
fn fdiv(a: f64, b: f64) -> f64 {
    if b == 0.0 { f64::INFINITY } else { a / b }
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: Vec3, s: f64) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn mul_point_mat(p: Vec3, m: &[[f64; 3]; 3]) -> Vec3 {
    [
        p[0] * m[0][0] + p[1] * m[1][0] + p[2] * m[2][0],
        p[0] * m[0][1] + p[1] * m[1][1] + p[2] * m[2][1],
        p[0] * m[0][2] + p[1] * m[1][2] + p[2] * m[2][2],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_core::constants::TAU;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    fn close_vec(a: Vec3, b: Vec3, tol: f64) -> bool {
        (0..3).all(|k| (a[k] - b[k]).abs() < tol)
    }

    #[test]
    fn a_plain_line_runs_between_its_ends() {
        let line = Line::new([-1.0, 0.0, 0.0], [2.0, 4.0, 0.0]).build();
        assert!(close_vec(line.points()[0], [-1.0, 0.0, 0.0], 1e-12));
        assert!(close_vec(
            *line.points().last().unwrap(),
            [2.0, 4.0, 0.0],
            1e-12
        ));
        assert!(close(line_arc_length(&line), 5.0, 1e-9));
        assert!(close(line_angle(&line), (4.0f64).atan2(3.0), 1e-12));
        assert!(matches!(line.shape(), ShapeTag::Line { path_arc, .. } if path_arc == 0.0));
    }

    #[test]
    fn the_buffer_is_measured_along_the_path() {
        let line = Line::new([0.0; 3], [10.0, 0.0, 0.0]).buff(2.0).build();
        assert!(close_vec(line.points()[0], [2.0, 0.0, 0.0], 1e-6));
        assert!(close_vec(
            *line.points().last().unwrap(),
            [8.0, 0.0, 0.0],
            1e-6
        ));
        assert!(close(line_arc_length(&line), 6.0, 1e-6));

        // On a curved line the buffer is still a distance along the curve,
        // not across the chord (BN-03).
        let curved = Line::new([0.0; 3], [4.0, 0.0, 0.0])
            .path_arc(TAU / 4.0)
            .buff(1.0)
            .build();
        let full = Line::new([0.0; 3], [4.0, 0.0, 0.0])
            .path_arc(TAU / 4.0)
            .build();
        assert!(
            close(line_arc_length(&full) - line_arc_length(&curved), 2.0, 1e-3),
            "trimmed {} from {}",
            line_arc_length(&full) - line_arc_length(&curved),
            line_arc_length(&full)
        );
    }

    #[test]
    fn a_path_arc_line_is_longer_than_its_chord_both_ways() {
        // C-3: the Reference only corrects for positive path_arc, so a
        // negatively bent line reported its chord. Ours reports the arc.
        let chord = 4.0;
        for arc in [TAU / 4.0, -TAU / 4.0] {
            let line = Line::new([0.0; 3], [chord, 0.0, 0.0]).path_arc(arc).build();
            let length = line_arc_length(&line);
            assert!(
                length > chord + 0.1,
                "path_arc {arc} reported length {length}"
            );
        }
    }

    #[test]
    fn line_queries_read_the_built_geometry() {
        let line = Line::new([0.0; 3], [3.0, 4.0, 0.0]).build();
        assert!(close_vec(line_vector(&line), [3.0, 4.0, 0.0], 1e-12));
        assert!(close_vec(line_unit_vector(&line), [0.6, 0.8, 0.0], 1e-12));
        assert!(close(line_slope(&line), 4.0 / 3.0, 1e-12));
        let proj = line_projection(&line, [0.0, 5.0, 0.0]);
        assert!(close_vec(proj, [2.4, 3.2, 0.0], 1e-9), "{proj:?}");
    }

    #[test]
    fn dashed_line_counts_and_spaces_its_dashes() {
        let dashed = DashedLine::new([0.0; 3], [1.0, 0.0, 0.0])
            .dash_length(0.05)
            .positive_space_ratio(0.5);
        // length 1, full period 0.1 → 10 dashes.
        assert_eq!(dashed.num_dashes(), 10);
        let built = dashed.build();
        assert_eq!(built.children().len(), 10);
        assert!(built.points().is_empty(), "the parent keeps no path");
        let lengths: Vec<f64> = built
            .children()
            .iter()
            .map(|d| d.path().unwrap().get_arc_length())
            .collect();
        for l in &lengths {
            assert!(close(*l, 0.05, 1e-6), "{lengths:?}");
        }
    }

    #[test]
    fn dashes_on_a_curved_line_are_evenly_spaced() {
        let built = DashedLine::new([0.0; 3], [4.0, 0.0, 0.0])
            .path_arc(TAU / 3.0)
            .dash_length(0.2)
            .build();
        let lengths: Vec<f64> = built
            .children()
            .iter()
            .map(|d| d.path().unwrap().get_arc_length())
            .collect();
        let first = lengths[0];
        for l in &lengths {
            assert!((l - first).abs() < 1e-6, "uneven dashes: {lengths:?}");
        }
    }

    #[test]
    fn elbow_sizes_and_rotates_about_the_origin() {
        let elbow = Elbow::new().width(2.0).build();
        assert!(close(elbow.length_over_dim(0), 2.0, 1e-12));
        // The corner stays anchored: the far end is on the x axis.
        assert!(close(elbow.points().last().unwrap()[1], 0.0, 1e-12));
        let turned = Elbow::new().width(2.0).angle(PI / 2.0).build();
        assert!(close(turned.length_over_dim(1), 2.0, 1e-9));
    }

    #[test]
    fn tangent_line_is_tangent_and_the_right_length() {
        let circle = crate::arc::Circle::new().radius(3.0).build();
        let tangent = tangent_line(&circle, 1.0 / 3.0, 6.0, 1e-6, Style::default());
        assert!(close(line_arc_length(&tangent), 6.0, 1e-6));
        // Perpendicular to the radius at the touch point — to within the
        // arc approximation, not beyond it. A `Circle` is 16 quadratic
        // components (BN-09), and a quadratic's tangent wobbles about the
        // true circle's by ~1e-3 rad between anchors; the secant this
        // construction takes is faithful to that path, so the tolerance
        // is the path's own, not an ideal circle's.
        let touch = circle
            .path()
            .unwrap()
            .point_from_proportion(1.0 / 3.0)
            .unwrap();
        let radial = sub(touch, circle.center_point());
        let along = line_unit_vector(&tangent);
        let cosine = space_ops::dot(radial, along) / space_ops::get_norm(radial);
        assert!(cosine.abs() < 5e-3, "not tangent: cos = {cosine}");
        // The touch point is on the segment, so it really is a tangent
        // *line* and not merely a parallel one.
        let offset = space_ops::get_norm(sub(line_projection(&tangent, touch), touch));
        assert!(offset < 1e-6, "touch point {offset} off the line");
    }

    #[test]
    fn arrow_spans_its_ends_and_caps_its_head() {
        let arrow = Arrow::new([0.0; 3], [4.0, 0.0, 0.0]).buff(0.0).build();
        let extent = arrow.length_over_dim(0);
        assert!(close(extent, 4.0, 1e-6), "arrow spans {extent}");
        assert!(
            arrow.style().fill_opacity > 0.0,
            "an Arrow is a filled shape"
        );
        // A short arrow has its head capped by max_tip_length_to_length_ratio.
        let short = Arrow::new([0.0; 3], [0.2, 0.0, 0.0]).buff(0.0);
        let (_, _, tip_length) = short.key_dimensions(0.2);
        assert!(tip_length <= 0.5 * 0.2 + 1e-12, "tip length {tip_length}");
    }

    #[test]
    fn vector_starts_at_the_origin() {
        let v = Arrow::vector([2.0, 2.0, 0.0]).build();
        // The shaft's back edge straddles the start point by half its
        // width, so the extent begins a shaft-half-width behind the
        // origin and no further.
        let (width, _, _) = Arrow::vector([2.0, 2.0, 0.0]).key_dimensions(2.0 * 2.0f64.sqrt());
        let (min, max) = v.extent().unwrap();
        assert!(
            min[0] >= -width && min[1] >= -width,
            "extent starts at {min:?}"
        );
        assert!(
            max[0] <= 2.0 + width && max[1] <= 2.0 + width,
            "extent ends at {max:?}"
        );
    }

    #[test]
    fn stroke_arrow_carries_a_tip_child() {
        let arrow = StrokeArrow::new([0.0; 3], [4.0, 0.0, 0.0]).build();
        assert_eq!(arrow.children().len(), 1, "shaft plus one tip");
        assert!(!arrow.points().is_empty(), "the shaft keeps its path");
    }

    #[test]
    fn degenerate_lines_are_defined() {
        let zero = Line::new([1.0, 1.0, 0.0], [1.0, 1.0, 0.0]).build();
        assert!(close(line_arc_length(&zero), 0.0, 1e-12));
        assert_eq!(line_unit_vector(&zero), ORIGIN, "no direction to report");
        // A buffer longer than the line collapses to its midpoint, not to
        // a reversed segment.
        let over = Line::new([0.0; 3], [1.0, 0.0, 0.0]).buff(10.0).build();
        assert!(line_arc_length(&over) < 1e-6, "{}", line_arc_length(&over));
    }
}
