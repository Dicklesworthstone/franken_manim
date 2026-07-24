//! Polygons, rectangles, Bézier curves, and arrow tips (§12.1).
//!
//! Ports of `manimlib/mobject/geometry.py`'s `CubicBezier`, `Polygon`,
//! `Polyline`, `RegularPolygon`, `Triangle`, `ArrowTip`, `Rectangle`,
//! `Square`, and `RoundedRectangle`, plus `mobject/frame.py`'s screen
//! rectangles. Constructor point arrays are the compatibility surface —
//! user code indexes them — so the formulas are the Reference's, and
//! `tests/geometry_parity.rs` locks them against values generated from it.

use fmn_core::color::Srgb;
use fmn_core::constants::{
    BLACK, DEFAULT_MOBJECT_COLOR, DEG, DL, DR, FRAME_HEIGHT, FRAME_WIDTH, GREY_E, ORIGIN, OUT,
    RIGHT, UL, UR,
};
use fmn_core::types::Vec3;
use fmn_geom::{QuadPath, space_ops};
use fmn_mobject::Mobject;
use fmn_mobject::ShapeTag;

use crate::style::Style;
use crate::vmobject::VMobject;

/// The Reference's `DEFAULT_ARROW_TIP_LENGTH`.
pub const DEFAULT_ARROW_TIP_LENGTH: f64 = 0.35;
/// The Reference's `DEFAULT_ARROW_TIP_WIDTH`.
pub const DEFAULT_ARROW_TIP_WIDTH: f64 = 0.35;

/// `CubicBezier(a0, h0, h1, a1)`: one cubic, converted to the shared-anchor
/// quadratic model on the way in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CubicBezier {
    control: [Vec3; 4],
    style: Style,
}

impl CubicBezier {
    /// A cubic through the four control points.
    #[must_use]
    pub fn new(a0: Vec3, h0: Vec3, h1: Vec3, a1: Vec3) -> Self {
        Self {
            control: [a0, h0, h1, a1],
            style: Style::default(),
        }
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
        let [a0, h0, h1, a1] = self.control;
        let mut path = QuadPath::new();
        path.start_new_path(a0);
        let _ = path.add_cubic_bezier_curve_to(h0, h1, a1);
        VMobject::from_path(&path).with_style(self.style)
    }
}

impl From<CubicBezier> for Mobject {
    fn from(c: CubicBezier) -> Self {
        c.build().into()
    }
}

/// `Polygon(*vertices)`: a closed run of corner-joined segments.
#[derive(Debug, Clone, PartialEq)]
pub struct Polygon {
    vertices: Vec<Vec3>,
    style: Style,
    closed: bool,
}

impl Polygon {
    /// A closed polygon through the vertices (the Reference repeats the
    /// first vertex to close it).
    #[must_use]
    pub fn new(vertices: impl IntoIterator<Item = Vec3>) -> Self {
        Self {
            vertices: vertices.into_iter().collect(),
            style: Style::default(),
            closed: true,
        }
    }

    /// `Polyline(*vertices)`: the same construction, left open.
    #[must_use]
    pub fn polyline(vertices: impl IntoIterator<Item = Vec3>) -> Self {
        Self {
            closed: false,
            ..Self::new(vertices)
        }
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

    /// The vertices as given.
    #[must_use]
    pub fn vertices(&self) -> &[Vec3] {
        &self.vertices
    }

    /// Build the detached mobject.
    #[must_use]
    pub fn build(self) -> VMobject {
        let mut corners = self.vertices.clone();
        if self.closed && !corners.is_empty() {
            corners.push(corners[0]);
        }
        let mut path = QuadPath::new();
        let _ = path.set_points_as_corners(&corners);
        VMobject::from_path(&path)
            .with_style(self.style)
            .with_shape(ShapeTag::Polyline {
                vertices: self.vertices.len(),
                closed: self.closed,
            })
    }

    /// Reference `Polygon.round_corners`: replace each corner with an arc
    /// of the given radius, tangent to both edges.
    ///
    /// `None` picks the Reference's default, a quarter of the shortest
    /// edge. A negative radius gives concave corners, as it does there.
    #[must_use]
    pub fn round_corners(self, radius: Option<f64>) -> VMobject {
        let style = self.style;
        let verts = self.vertices.clone();
        if verts.len() < 3 {
            return self.build();
        }
        let radius = radius.unwrap_or_else(|| {
            let min_edge = verts
                .windows(2)
                .filter(|w| space_ops::get_norm(sub(w[0], w[1])) > 1e-8)
                .map(|w| space_ops::get_norm(sub(w[0], w[1])))
                .fold(f64::INFINITY, f64::min);
            0.25 * min_edge
        });

        // One arc per corner, over the cyclic vertex triples.
        let n = verts.len();
        let mut arcs: Vec<Vec<Vec3>> = Vec::with_capacity(n);
        for i in 0..n {
            let v1 = verts[i];
            let v2 = verts[(i + 1) % n];
            let v3 = verts[(i + 2) % n];
            let vect1 = space_ops::normalize(sub(v2, v1));
            let vect2 = space_ops::normalize(sub(v3, v2));
            let angle = space_ops::angle_between_vectors(vect1, vect2);
            let cut_off_length = radius * (angle / 2.0).tan();
            let sign = (radius * space_ops::cross2d(vect1, vect2)).signum();
            let start = sub(v2, scale(vect1, cut_off_length));
            let end = add(v2, scale(vect2, cut_off_length));
            arcs.push(arc_between_points(start, end, sign * angle, Some(2)));
        }

        // The Reference loops starting with the last arc so the path
        // closes on the first corner.
        arcs.rotate_right(1);
        let mut path = QuadPath::new();
        for i in 0..n {
            let this = &arcs[i];
            let next = &arcs[(i + 1) % n];
            let _ = path.add_subpath(this);
            if let Some(&start) = next.first() {
                let _ = path.add_line_to(start, false);
            }
        }
        VMobject::from_path(&path).with_style(style)
    }
}

impl From<Polygon> for Mobject {
    fn from(p: Polygon) -> Self {
        p.build().into()
    }
}

/// The point run of an arc between two points subtending `angle` — the
/// construction `ArcBetweenPoints` and `round_corners` share.
pub(crate) fn arc_between_points(
    start: Vec3,
    end: Vec3,
    angle: f64,
    n_components: Option<usize>,
) -> Vec<Vec3> {
    if angle == 0.0 {
        let mut path = QuadPath::new();
        let _ = path.set_points_as_corners(&[start, end]);
        return path.points().to_vec();
    }
    let arc = QuadPath::arc(0.0, angle, 1.0, ORIGIN, n_components);
    VMobject::from_points(arc.points().to_vec())
        .put_start_and_end_on(start, end)
        .points()
        .to_vec()
}

/// `RegularPolygon(n, radius, start_angle)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RegularPolygon {
    n: usize,
    radius: f64,
    start_angle: Option<f64>,
    style: Style,
}

impl Default for RegularPolygon {
    fn default() -> Self {
        Self::new(6)
    }
}

impl RegularPolygon {
    /// An `n`-gon of unit circumradius.
    #[must_use]
    pub fn new(n: usize) -> Self {
        Self {
            n,
            radius: 1.0,
            start_angle: None,
            style: Style::default(),
        }
    }

    /// `Triangle()`: the Reference's `RegularPolygon(n=3)`.
    #[must_use]
    pub fn triangle() -> Self {
        Self::new(3)
    }

    /// Circumradius.
    #[must_use]
    pub fn radius(mut self, radius: f64) -> Self {
        self.radius = radius;
        self
    }

    /// Angle of the first vertex.
    ///
    /// The default is the Reference's `(n % 2) * 90°`: **odd** `n` starts
    /// at 90° and even `n` at 0°, so a triangle points up and a hexagon
    /// has a vertex on the `+x` axis. (`RegularPolygon`'s docstring in the
    /// Reference says the opposite; its code, which is what every scene
    /// actually sees, says this.)
    #[must_use]
    pub fn start_angle(mut self, angle: f64) -> Self {
        self.start_angle = Some(angle);
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

    /// The vertices this polygon is built from.
    #[must_use]
    pub fn corner_points(&self) -> Vec<Vec3> {
        let start_angle = self.start_angle.unwrap_or((self.n % 2) as f64 * 90.0 * DEG);
        let start_vect = space_ops::rotate_vector(scale(RIGHT, self.radius), start_angle, OUT);
        space_ops::compass_directions(self.n, start_vect)
    }

    /// Build the detached mobject.
    #[must_use]
    pub fn build(self) -> VMobject {
        Polygon::new(self.corner_points()).style(self.style).build()
    }
}

impl From<RegularPolygon> for Mobject {
    fn from(p: RegularPolygon) -> Self {
        p.build().into()
    }
}

/// How an [`ArrowTip`] is shaped (the Reference's `tip_style` code).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TipStyle {
    /// `0`: a plain triangle.
    #[default]
    Triangle,
    /// `1`: a triangle with its base pulled in, so the tip reads sharper.
    InnerSmooth,
    /// `2`: a dot.
    Dot,
}

/// `ArrowTip(angle, width, length, tip_style)`: the head every tipped
/// mobject attaches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArrowTip {
    angle: f64,
    width: f64,
    length: f64,
    tip_style: TipStyle,
    style: Style,
}

impl Default for ArrowTip {
    fn default() -> Self {
        Self::new()
    }
}

impl ArrowTip {
    /// A tip at the Reference's default dimensions, pointing right.
    #[must_use]
    pub fn new() -> Self {
        Self {
            angle: 0.0,
            width: DEFAULT_ARROW_TIP_WIDTH,
            length: DEFAULT_ARROW_TIP_LENGTH,
            tip_style: TipStyle::Triangle,
            // The Reference's tip_config: filled, unstroked.
            style: Style::default()
                .fill(DEFAULT_MOBJECT_COLOR, 1.0)
                .stroke(BLACK, 0.0, 1.0),
        }
    }

    /// Direction the tip points.
    #[must_use]
    pub fn angle(mut self, angle: f64) -> Self {
        self.angle = angle;
        self
    }

    /// Width across the base.
    #[must_use]
    pub fn width(mut self, width: f64) -> Self {
        self.width = width;
        self
    }

    /// Length from base to point.
    #[must_use]
    pub fn length(mut self, length: f64) -> Self {
        self.length = length;
        self
    }

    /// Tip shape.
    #[must_use]
    pub fn tip_style(mut self, tip_style: TipStyle) -> Self {
        self.tip_style = tip_style;
        self
    }

    /// Set fill colour (a tip is a filled shape).
    #[must_use]
    pub fn color(mut self, color: Srgb) -> Self {
        self.style = self.style.fill(color, self.style.fill_opacity);
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
        let base = RegularPolygon::new(3)
            .start_angle(0.0)
            .style(self.style)
            .build();
        let sized = base
            .with_height(self.width, false)
            .with_width(self.length, true);
        let shaped = match self.tip_style {
            TipStyle::Triangle => sized,
            TipStyle::InnerSmooth => {
                let mut points = sized.with_height(self.length * 0.9, true).points().to_vec();
                if let Some(p) = points.get_mut(4) {
                    p[0] += 0.6 * self.length;
                }
                VMobject::from_points(points).with_style(self.style)
            }
            TipStyle::Dot => {
                let dot = crate::arc::Dot::new()
                    .style(self.style)
                    .build()
                    .with_width(self.length / 2.0, false);
                VMobject::from_points(dot.points().to_vec()).with_style(self.style)
            }
        };
        let center = shaped.center_point();
        shaped.rotated_about(self.angle, OUT, center)
    }
}

impl From<ArrowTip> for Mobject {
    fn from(t: ArrowTip) -> Self {
        t.build().into()
    }
}

/// The tip's base: the Reference's `point_from_proportion(0.5)`, which for
/// a triangle is the midpoint of the back edge.
#[must_use]
pub fn tip_base(tip: &VMobject) -> Vec3 {
    tip.path()
        .ok()
        .and_then(|p| p.quick_point_from_proportion(0.5))
        .unwrap_or(ORIGIN)
}

/// The tip's point: the Reference's `get_points()[0]`.
#[must_use]
pub fn tip_point(tip: &VMobject) -> Vec3 {
    tip.points().first().copied().unwrap_or(ORIGIN)
}

/// The vector from base to point.
#[must_use]
pub fn tip_vector(tip: &VMobject) -> Vec3 {
    sub(tip_point(tip), tip_base(tip))
}

/// The direction the tip points, as an angle.
#[must_use]
pub fn tip_angle(tip: &VMobject) -> f64 {
    space_ops::angle_of_vector(tip_vector(tip))
}

/// `Rectangle(width, height)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rectangle {
    width: f64,
    height: f64,
    corner_radius: Option<f64>,
    style: Style,
}

impl Default for Rectangle {
    fn default() -> Self {
        Self::new()
    }
}

impl Rectangle {
    /// The Reference's default rectangle, 4 by 2.
    #[must_use]
    pub fn new() -> Self {
        Self {
            width: 4.0,
            height: 2.0,
            corner_radius: None,
            style: Style::default(),
        }
    }

    /// `Square(side_length)`.
    #[must_use]
    pub fn square(side_length: f64) -> Self {
        Self::new().width(side_length).height(side_length)
    }

    /// Full width.
    #[must_use]
    pub fn width(mut self, width: f64) -> Self {
        self.width = width;
        self
    }

    /// Full height.
    #[must_use]
    pub fn height(mut self, height: f64) -> Self {
        self.height = height;
        self
    }

    /// `RoundedRectangle(corner_radius=…)`.
    #[must_use]
    pub fn corner_radius(mut self, radius: f64) -> Self {
        self.corner_radius = Some(radius);
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
        // The Reference builds the unit square UR, UL, DL, DR and stretches.
        let corners = Polygon::new([UR, UL, DL, DR]).style(self.style);
        match self.corner_radius {
            None => corners
                .build()
                .with_width(self.width, true)
                .with_height(self.height, true)
                .with_shape(ShapeTag::Rect {
                    center: ORIGIN,
                    width: self.width,
                    height: self.height,
                }),
            Some(radius) => {
                // round_corners runs on the *stretched* rectangle, exactly
                // as the Reference's RoundedRectangle does, so the corner
                // arcs stay circular rather than being stretched into
                // ellipses.
                let stretched: Vec<Vec3> = [UR, UL, DL, DR]
                    .iter()
                    .map(|p| [p[0] * self.width / 2.0, p[1] * self.height / 2.0, p[2]])
                    .collect();
                Polygon::new(stretched)
                    .style(self.style)
                    .round_corners(Some(radius))
                    .with_shape(ShapeTag::RoundedRect {
                        center: ORIGIN,
                        width: self.width,
                        height: self.height,
                        corner_radius: radius,
                    })
            }
        }
    }
}

impl From<Rectangle> for Mobject {
    fn from(r: Rectangle) -> Self {
        r.build().into()
    }
}

/// `ScreenRectangle(aspect_ratio, height)` (`mobject/frame.py`): a
/// rectangle of the frame's aspect ratio.
#[must_use]
pub fn screen_rectangle(aspect_ratio: f64, height: f64) -> Rectangle {
    Rectangle::new().width(aspect_ratio * height).height(height)
}

/// `FullScreenRectangle`: the whole camera frame, at the Reference's
/// default 1.01 overshoot so no seam shows at the edge.
#[must_use]
pub fn full_screen_rectangle() -> Rectangle {
    Rectangle::new()
        .width(FRAME_WIDTH * 1.01)
        .height(FRAME_HEIGHT * 1.01)
}

/// `FullScreenFadeRectangle`: the full-screen rectangle as a dark,
/// unstroked veil.
#[must_use]
pub fn full_screen_fade_rectangle(opacity: f64) -> Rectangle {
    full_screen_rectangle().style(
        Style::default()
            .fill(GREY_E, opacity)
            .stroke(BLACK, 0.0, 1.0),
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_core::constants::{PI, TAU};

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn polygon_closes_and_counts_its_corners() {
        let tri = Polygon::new([[-3.0, 0.0, 0.0], [3.0, 0.0, 0.0], [0.0, 3.0, 0.0]]).build();
        // Three edges, each a straight quadratic: 2*3 + 1 points.
        assert_eq!(tri.points().len(), 7);
        assert_eq!(tri.points()[0], [-3.0, 0.0, 0.0]);
        assert_eq!(*tri.points().last().unwrap(), [-3.0, 0.0, 0.0]);
        assert!(matches!(
            tri.shape(),
            ShapeTag::Polyline {
                vertices: 3,
                closed: true
            }
        ));
    }

    #[test]
    fn polyline_stays_open() {
        let line = Polygon::polyline([[0.0; 3], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]]).build();
        assert_eq!(line.points().len(), 5);
        assert_ne!(line.points()[0], *line.points().last().unwrap());
    }

    #[test]
    fn regular_polygon_vertices_are_on_the_circumcircle() {
        for n in [3usize, 4, 5, 6, 8] {
            let poly = RegularPolygon::new(n).radius(2.0);
            let verts = poly.corner_points();
            assert_eq!(verts.len(), n);
            for v in &verts {
                assert!(close(space_ops::get_norm(*v), 2.0), "{n}-gon: {v:?}");
            }
            // Equal turning between successive vertices.
            let step = space_ops::angle_between_vectors(verts[0], verts[1]);
            assert!(close(step, TAU / n as f64), "{n}-gon step {step}");
        }
    }

    #[test]
    fn regular_polygon_default_start_angle_follows_parity() {
        // `(n % 2) * 90°`: odd n starts at 90° (a triangle points up),
        // even n at 0° (a hexagon has a vertex on +x).
        let tri = RegularPolygon::new(3).corner_points();
        assert!(
            close(tri[0][0], 0.0) && close(tri[0][1], 1.0),
            "{:?}",
            tri[0]
        );
        let hex = RegularPolygon::new(6).corner_points();
        assert!(
            close(hex[0][0], 1.0) && close(hex[0][1], 0.0),
            "{:?}",
            hex[0]
        );
        // An explicit angle overrides the parity rule.
        let turned = RegularPolygon::new(3).start_angle(0.0).corner_points();
        assert!(close(turned[0][0], 1.0) && close(turned[0][1], 0.0));
    }

    #[test]
    fn rectangle_has_the_requested_dimensions() {
        let rect = Rectangle::new().width(3.0).height(5.0).build();
        assert!(close(rect.length_over_dim(0), 3.0));
        assert!(close(rect.length_over_dim(1), 5.0));
        assert!(close(rect.center_point()[0], 0.0));
        assert!(matches!(rect.shape(), ShapeTag::Rect { .. }));

        let square = Rectangle::square(2.0).build();
        assert!(close(square.length_over_dim(0), 2.0));
        assert!(close(square.length_over_dim(1), 2.0));
    }

    #[test]
    fn rounded_rectangle_keeps_its_extent_and_rounds_its_corners() {
        let radius = 0.5;
        let rect = Rectangle::new()
            .width(4.0)
            .height(2.0)
            .corner_radius(radius)
            .build();
        assert!(
            close(rect.length_over_dim(0), 4.0),
            "{}",
            rect.length_over_dim(0)
        );
        assert!(close(rect.length_over_dim(1), 2.0));
        assert!(matches!(rect.shape(), ShapeTag::RoundedRect { .. }));
        // No point sits in the cut-off square at a corner: that is what
        // "rounded" means.
        for p in rect.points() {
            let dx = 2.0 - p[0].abs();
            let dy = 1.0 - p[1].abs();
            assert!(
                dx > -1e-9 && dy > -1e-9,
                "point outside the rectangle: {p:?}"
            );
        }
    }

    #[test]
    fn round_corners_defaults_to_a_quarter_of_the_shortest_edge() {
        let poly = Polygon::new([[0.0; 3], [4.0, 0.0, 0.0], [4.0, 2.0, 0.0], [0.0, 2.0, 0.0]]);
        let rounded = poly.round_corners(None);
        assert!(rounded.points().len() > 9, "corners became arcs");
        // The default radius is 0.5 here (shortest edge 2), so the corner
        // at the origin is cut back by 0.5 in both directions.
        let has_cut = rounded
            .points()
            .iter()
            .any(|p| close(p[0], 0.5) && close(p[1], 0.0));
        assert!(has_cut, "expected a cut-back corner point");
    }

    #[test]
    fn degenerate_polygons_do_not_panic() {
        assert_eq!(Polygon::new([]).build().points().len(), 0);
        let single = Polygon::new([[1.0, 1.0, 0.0]]).build();
        assert!(single.points().len() <= 3);
        // Fewer than three vertices: round_corners has nothing to round.
        let two = Polygon::new([[0.0; 3], [1.0, 0.0, 0.0]]).round_corners(None);
        assert!(!two.points().is_empty());
    }

    #[test]
    fn arrow_tip_dimensions_and_geometry() {
        let tip = ArrowTip::new().width(0.35).length(0.35).build();
        assert!(
            close(tip.length_over_dim(0), 0.35),
            "{}",
            tip.length_over_dim(0)
        );
        assert!(close(tip.length_over_dim(1), 0.35));
        // The tip points right by default, and the base is behind it.
        let vector = tip_vector(&tip);
        assert!(vector[0] > 0.0, "{vector:?}");
        assert!(close(tip_angle(&tip), 0.0));
        // Rotating the tip rotates its vector.
        let turned = ArrowTip::new().angle(PI / 2.0).build();
        assert!(
            close(tip_angle(&turned), PI / 2.0),
            "{}",
            tip_angle(&turned)
        );
    }

    #[test]
    fn arrow_tip_styles_differ() {
        let triangle = ArrowTip::new().build();
        let smooth = ArrowTip::new().tip_style(TipStyle::InnerSmooth).build();
        let dot = ArrowTip::new().tip_style(TipStyle::Dot).build();
        assert_ne!(triangle.points(), smooth.points());
        assert!(
            dot.points().len() > triangle.points().len(),
            "a dot is an arc"
        );
    }

    #[test]
    fn cubic_bezier_lands_on_its_anchors() {
        let curve =
            CubicBezier::new([0.0; 3], [1.0, 2.0, 0.0], [3.0, 2.0, 0.0], [4.0, 0.0, 0.0]).build();
        assert_eq!(curve.points()[0], [0.0; 3]);
        assert_eq!(*curve.points().last().unwrap(), [4.0, 0.0, 0.0]);
    }

    #[test]
    fn frame_rectangles_match_the_frame() {
        let full = full_screen_rectangle().build();
        assert!(close(full.length_over_dim(0), FRAME_WIDTH * 1.01));
        assert!(close(full.length_over_dim(1), FRAME_HEIGHT * 1.01));
        let screen = screen_rectangle(16.0 / 9.0, 4.0).build();
        assert!(close(screen.length_over_dim(1), 4.0));
        assert!(close(screen.length_over_dim(0), 4.0 * 16.0 / 9.0));
        let fade = full_screen_fade_rectangle(0.7).build();
        assert!(close(fade.style().fill_opacity, 0.7));
        assert!(close(fade.style().stroke_width, 0.0));
    }
}
