//! Projection guides and checked graph queries in a single affine chart.
use super::{
    CoordinateFrame, CoordinateFrameError, CoordinateSystem, CoordsError, LiveCoordinateSystem,
    Vec3, add, dot, max_abs, scale, sub,
};
use crate::{DashedLine, Style, VMobject};
use fmn_core::constants::GREY_A;
use fmn_mobject::Stage;

fn invalid(reason: &'static str) -> CoordsError {
    CoordinateFrameError::InvalidGeometry(reason).into()
}

pub(super) fn record_point(point: Vec3) -> Result<Vec3, CoordsError> {
    if point
        .iter()
        .all(|x| x.is_finite() && x.abs() <= f64::from(f32::MAX))
    {
        Ok(point)
    } else {
        Err(invalid("guide points must fit finite f32 records"))
    }
}

pub(super) fn validate_style(style: Style) -> Result<(), CoordsError> {
    for color in [style.fill_color, style.stroke_color] {
        if ![color.r, color.g, color.b].iter().all(|x| x.is_finite()) {
            return Err(invalid("guide colors must be finite"));
        }
    }
    for opacity in [style.fill_opacity, style.stroke_opacity] {
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err(invalid("guide opacity must lie in [0, 1]"));
        }
    }
    for width in [style.stroke_width, style.fill_border_width] {
        if !width.is_finite() || width < 0.0 || width > f64::from(f32::MAX) {
            return Err(invalid(
                "guide widths must fit nonnegative finite f32 records",
            ));
        }
    }
    Ok(())
}

impl CoordinateFrame {
    /// The closest world-space point on an infinite numeric axis.
    ///
    /// This is Euclidean projection, as in NumberLine::projection, not setting
    /// the other numeric coordinates to zero. Those operations differ under
    /// shear. Any of a 3D chart's three axes can be selected. The query accepts
    /// points outside the chart plane and never clamps to the drawn axis range.
    pub fn project_to_axis(&self, index: usize, point: Vec3) -> Result<Vec3, CoordsError> {
        let axis = self
            .basis
            .get(index)
            .filter(|_| index < self.dimension())
            .ok_or_else(|| invalid("axis index is outside the coordinate frame"))?;
        record_point(point)?;
        // Scale before taking dot products: squaring an unnormalized, tiny or
        // huge axis would underflow or overflow despite a valid affine chart.
        let axis = axis.map(|x| x / max_abs(*axis));
        let distance = dot(sub(point, self.origin), axis) / dot(axis, axis);
        record_point(add(self.origin, scale(axis, distance)))
    }

    /// Dashed perpendicular guide from axis `index` to a world-space point,
    /// using the familiar GREY_A, width-2 default. On a skew chart this is
    /// perpendicular to the named axis rather than parallel to another axis.
    pub fn get_line_from_axis_to_point(
        &self,
        index: usize,
        point: Vec3,
    ) -> Result<VMobject, CoordsError> {
        self.line_from_axis_to_point_styled(index, point, Style::default().stroke(GREY_A, 2.0, 1.0))
    }

    /// Styled guide with the native DashedLine spacing and resource refusals.
    /// The chart's point budget is checked before allocating the dash family.
    pub fn line_from_axis_to_point_styled(
        &self,
        index: usize,
        point: Vec3,
        style: Style,
    ) -> Result<VMobject, CoordsError> {
        validate_style(style)?;
        let start = self.project_to_axis(index, point)?;
        let line = DashedLine::new(start, point).style(style);
        let points = line
            .num_dashes()?
            .checked_mul(3)
            .ok_or_else(|| invalid("guide point count overflow"))?;
        self.sampling_budget
            .ensure_total("coordinate projection guide", points)?;
        Ok(line.build()?)
    }

    /// Perpendicular guide from the x-axis, even after out-of-plane rotation.
    pub fn get_v_line(&self, point: Vec3) -> Result<VMobject, CoordsError> {
        self.get_line_from_axis_to_point(0, point)
    }

    /// Perpendicular guide from the y-axis in this frame.
    pub fn get_h_line(&self, point: Vec3) -> Result<VMobject, CoordsError> {
        self.get_line_from_axis_to_point(1, point)
    }

    /// Checked `c2p(x, function(x))`, with one callback invocation. A 3D chart
    /// uses numeric z = 0. Invalid x or a budget below one point is refused
    /// before invoking authored code. No Stage state is mutated.
    pub fn try_input_to_graph_point(
        &self,
        x: f64,
        function: &dyn Fn(f64) -> f64,
    ) -> Result<Vec3, CoordsError> {
        if !x.is_finite() {
            return Err(CoordinateFrameError::NonFiniteCoordinates.into());
        }
        self.sampling_budget.ensure_total("graph point query", 1)?;
        let y = function(x);
        if !y.is_finite() {
            return Err(CoordinateFrameError::NonFiniteCoordinates.into());
        }
        record_point(self.c2p(&[x, y]))
    }

    /// Dashed x-axis projection to `c2p(x, function(x))`. The callback runs
    /// once, only after the input and minimum output budget are admitted.
    pub fn get_v_line_to_graph(
        &self,
        x: f64,
        function: &dyn Fn(f64) -> f64,
    ) -> Result<VMobject, CoordsError> {
        self.sampling_budget
            .ensure_total("coordinate projection guide", 3)?;
        self.get_v_line(self.try_input_to_graph_point(x, function)?)
    }

    /// Dashed y-axis projection to a graph point; see `get_v_line_to_graph`.
    pub fn get_h_line_to_graph(
        &self,
        x: f64,
        function: &dyn Fn(f64) -> f64,
    ) -> Result<VMobject, CoordsError> {
        self.sampling_budget
            .ensure_total("coordinate projection guide", 3)?;
        self.get_h_line(self.try_input_to_graph_point(x, function)?)
    }
}

impl LiveCoordinateSystem {
    /// Closest point on the current world-space numeric axis. Stale or
    /// non-affine axes are refused by the same live snapshot contract as c2p.
    pub fn project_to_axis(
        &self,
        stage: &Stage,
        index: usize,
        point: Vec3,
    ) -> Result<Vec3, CoordsError> {
        self.snapshot(stage)?.project_to_axis(index, point)
    }

    /// Build a projection guide from one snapshot of the current live axes.
    pub fn get_line_from_axis_to_point(
        &self,
        stage: &Stage,
        index: usize,
        point: Vec3,
    ) -> Result<VMobject, CoordsError> {
        self.snapshot(stage)?
            .get_line_from_axis_to_point(index, point)
    }

    /// Styled variant using the current axes without moving or copying them.
    pub fn line_from_axis_to_point_styled(
        &self,
        stage: &Stage,
        index: usize,
        point: Vec3,
        style: Style,
    ) -> Result<VMobject, CoordsError> {
        self.snapshot(stage)?
            .line_from_axis_to_point_styled(index, point, style)
    }

    /// Dashed guide from the current x-axis to a world-space point.
    pub fn get_v_line(&self, stage: &Stage, point: Vec3) -> Result<VMobject, CoordsError> {
        self.snapshot(stage)?.get_v_line(point)
    }

    /// Dashed guide from the current y-axis to a world-space point.
    pub fn get_h_line(&self, stage: &Stage, point: Vec3) -> Result<VMobject, CoordsError> {
        self.snapshot(stage)?.get_h_line(point)
    }

    /// Checked graph point in the current frame. A failed snapshot never
    /// invokes the callback. Use a CoordinateFrame snapshot to batch queries.
    pub fn input_to_graph_point(
        &self,
        stage: &Stage,
        x: f64,
        function: &dyn Fn(f64) -> f64,
    ) -> Result<Vec3, CoordsError> {
        self.snapshot(stage)?.try_input_to_graph_point(x, function)
    }

    /// Alias of the checked live graph-point query.
    pub fn i2gp(
        &self,
        stage: &Stage,
        x: f64,
        function: &dyn Fn(f64) -> f64,
    ) -> Result<Vec3, CoordsError> {
        self.input_to_graph_point(stage, x, function)
    }

    /// Project the graph point onto the current x-axis. Both endpoints use
    /// the same snapshot, including when invoked by an animation updater.
    pub fn get_v_line_to_graph(
        &self,
        stage: &Stage,
        x: f64,
        function: &dyn Fn(f64) -> f64,
    ) -> Result<VMobject, CoordsError> {
        self.snapshot(stage)?.get_v_line_to_graph(x, function)
    }

    /// Project a graph point onto the current y-axis, from one live snapshot.
    pub fn get_h_line_to_graph(
        &self,
        stage: &Stage,
        x: f64,
        function: &dyn Fn(f64) -> f64,
    ) -> Result<VMobject, CoordsError> {
        self.snapshot(stage)?.get_h_line_to_graph(x, function)
    }
}
