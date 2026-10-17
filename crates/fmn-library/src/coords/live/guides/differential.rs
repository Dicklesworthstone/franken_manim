//! Tangents and secant triangles use full affine vectors, not screen angles.
use super::{
    CoordinateFrame, CoordinateSystem, CoordsError, LiveCoordinateSystem, Vec3, add, dot, invalid,
    max_abs, record_point, scale, validate_style,
};
use crate::coords::EPSILON;
use crate::{Line, Style, VMobject, v_group};
use fmn_core::constants::{GREEN, RED, YELLOW};
use fmn_mobject::Stage;

fn unit(vector: Vec3) -> Result<Vec3, CoordsError> {
    let magnitude = max_abs(vector);
    if !vector.iter().all(|x| x.is_finite()) || magnitude == 0.0 {
        return Err(invalid("graph secant has no finite nonzero direction"));
    }
    let scaled = vector.map(|x| x / magnitude);
    let norm = dot(scaled, scaled).sqrt();
    Ok(scaled.map(|x| x / norm))
}
fn length(value: f64) -> Result<(), CoordsError> {
    if !value.is_finite() || value <= 0.0 || value > f64::from(f32::MAX) {
        Err(invalid(
            "line length must be positive and fit finite f32 records",
        ))
    } else {
        Ok(())
    }
}
fn line(start: Vec3, end: Vec3, style: Style) -> Result<VMobject, CoordsError> {
    Line::new(record_point(start)?, record_point(end)?)
        .style(style)
        .build()
        .map_err(|_| invalid("native tangent or secant line construction failed"))
}

impl CoordinateFrame {
    fn graph_pair(
        &self,
        x: f64,
        function: &dyn Fn(f64) -> f64,
        dx: f64,
    ) -> Result<([f64; 2], [f64; 2]), CoordsError> {
        let next = x + dx;
        if !x.is_finite() || !dx.is_finite() || !next.is_finite() || next == x {
            return Err(invalid(
                "secant inputs must be finite and x + dx must differ from x",
            ));
        }
        self.sampling_budget
            .ensure_total("graph secant samples", 2)?;
        let y = function(x);
        if !y.is_finite() {
            return Err(invalid("graph callback returned a non-finite height"));
        }
        let next_y = function(next);
        if !next_y.is_finite() {
            return Err(invalid("graph callback returned a non-finite height"));
        }
        Ok(([x, y], [next, next_y]))
    }

    fn secant_direction(&self, start: [f64; 2], end: [f64; 2]) -> Result<Vec3, CoordsError> {
        // Subtract in data space before applying the linear basis. Subtracting
        // two translated world points can erase a small derivative probe.
        unit(add(
            scale(self.basis[0], end[0] - start[0]),
            scale(self.basis[1], end[1] - start[1]),
        ))
    }

    /// Unit world-space secant direction from x to x+dx, including its z lane.
    /// This is a finite-difference estimate, not symbolic differentiation.
    /// Each input is sampled once after admission; negative dx reverses direction.
    pub fn tangent_direction(
        &self,
        x: f64,
        function: &dyn Fn(f64) -> f64,
        dx: f64,
    ) -> Result<Vec3, CoordsError> {
        let (start, end) = self.graph_pair(x, function, dx)?;
        self.secant_direction(start, end)
    }

    /// Angle of the world-XY projection of the finite-difference tangent.
    /// A tangent perpendicular to world XY has no such angle and is refused.
    pub fn angle_of_tangent_dx(
        &self,
        x: f64,
        function: &dyn Fn(f64) -> f64,
        dx: f64,
    ) -> Result<f64, CoordsError> {
        let d = self.tangent_direction(x, function, dx)?;
        if d[0] == 0.0 && d[1] == 0.0 {
            return Err(invalid("tangent has no world-XY projection"));
        }
        Ok(fmn_dmath::atan2(d[1], d[0]))
    }

    /// World-XY tangent angle using the ordinary EPSILON forward difference.
    pub fn angle_of_tangent(
        &self,
        x: f64,
        function: &dyn Fn(f64) -> f64,
    ) -> Result<f64, CoordsError> {
        self.angle_of_tangent_dx(x, function, EPSILON)
    }

    /// World dy/dx of the tangent, not the derivative in numeric chart units.
    /// A vertical or unrepresentable projected slope is a named refusal.
    pub fn slope_of_tangent(
        &self,
        x: f64,
        function: &dyn Fn(f64) -> f64,
    ) -> Result<f64, CoordsError> {
        let d = self.tangent_direction(x, function, EPSILON)?;
        let slope = d[1] / d[0];
        if !slope.is_finite() {
            return Err(invalid("tangent has no finite world-XY slope"));
        }
        Ok(slope)
    }

    /// Length-controlled 3D tangent centered on c2p(x, f(x)), with the ordinary
    /// EPSILON forward-difference sample and default Line style.
    pub fn get_tangent_line(
        &self,
        x: f64,
        function: &dyn Fn(f64) -> f64,
        line_length: f64,
    ) -> Result<VMobject, CoordsError> {
        self.tangent_line_styled(x, function, line_length, EPSILON, Style::default())
    }

    /// Explicit-dx and styled tangent. Length is measured in world units.
    /// Request, style and three-point budget are checked before callbacks;
    /// geometry is built only after both finite samples have been observed.
    pub fn tangent_line_styled(
        &self,
        x: f64,
        function: &dyn Fn(f64) -> f64,
        line_length: f64,
        dx: f64,
        style: Style,
    ) -> Result<VMobject, CoordsError> {
        length(line_length)?;
        validate_style(style)?;
        self.sampling_budget.ensure_total("graph tangent line", 3)?;
        let (start, end) = self.graph_pair(x, function, dx)?;
        let center = record_point(self.c2p(&start))?;
        let half = scale(self.secant_direction(start, end)?, line_length / 2.0);
        line(add(center, scale(half, -1.0)), add(center, half), style)
    }

    /// A secant slope diagram with children [numeric dx, numeric dy, secant].
    /// The run is YELLOW, rise GREEN and secant RED; all are native lines.
    /// On sheared or tilted axes, the two legs follow the chart basis, not
    /// world horizontal/vertical. None keeps the exact graph-point chord;
    /// Some(length) extends the secant symmetrically about its chord midpoint.
    pub fn get_secant_slope_group(
        &self,
        x: f64,
        function: &dyn Fn(f64) -> f64,
        dx: f64,
        secant_length: Option<f64>,
    ) -> Result<VMobject, CoordsError> {
        if let Some(value) = secant_length {
            length(value)?;
        }
        self.sampling_budget
            .ensure_total("graph secant slope group", 9)?;
        let (start, end) = self.graph_pair(x, function, dx)?;
        let p0 = record_point(self.c2p(&start))?;
        let corner = record_point(self.c2p(&[end[0], start[1]]))?;
        let p1 = record_point(self.c2p(&end))?;
        let (s0, s1) = if let Some(value) = secant_length {
            let middle = p0.map(|v| v * 0.5);
            let middle = add(middle, scale(p1, 0.5));
            let half = scale(self.secant_direction(start, end)?, value / 2.0);
            (add(middle, scale(half, -1.0)), add(middle, half))
        } else {
            (p0, p1)
        };
        Ok(v_group([
            line(p0, corner, Style::default().stroke(YELLOW, 2.0, 1.0))?,
            line(corner, p1, Style::default().stroke(GREEN, 2.0, 1.0))?,
            line(s0, s1, Style::default().stroke(RED, 2.0, 1.0))?,
        ]))
    }
}

impl LiveCoordinateSystem {
    /// Finite-difference direction in the current affine frame; no XY flattening.
    pub fn tangent_direction(
        &self,
        stage: &Stage,
        x: f64,
        function: &dyn Fn(f64) -> f64,
        dx: f64,
    ) -> Result<Vec3, CoordsError> {
        self.snapshot(stage)?.tangent_direction(x, function, dx)
    }

    /// World-XY tangent angle, with an explicit numeric probe increment.
    pub fn angle_of_tangent_dx(
        &self,
        stage: &Stage,
        x: f64,
        function: &dyn Fn(f64) -> f64,
        dx: f64,
    ) -> Result<f64, CoordsError> {
        self.snapshot(stage)?.angle_of_tangent_dx(x, function, dx)
    }

    /// World-XY tangent angle with the ordinary EPSILON probe increment.
    pub fn angle_of_tangent(
        &self,
        stage: &Stage,
        x: f64,
        function: &dyn Fn(f64) -> f64,
    ) -> Result<f64, CoordsError> {
        self.snapshot(stage)?.angle_of_tangent(x, function)
    }

    /// Finite world-XY tangent slope from one current chart snapshot.
    pub fn slope_of_tangent(
        &self,
        stage: &Stage,
        x: f64,
        function: &dyn Fn(f64) -> f64,
    ) -> Result<f64, CoordsError> {
        self.snapshot(stage)?.slope_of_tangent(x, function)
    }

    /// Build a world-length tangent on the current axes, including rotated 3D charts.
    pub fn get_tangent_line(
        &self,
        stage: &Stage,
        x: f64,
        function: &dyn Fn(f64) -> f64,
        line_length: f64,
    ) -> Result<VMobject, CoordsError> {
        self.snapshot(stage)?
            .get_tangent_line(x, function, line_length)
    }

    /// Explicit probe increment and style; validation precedes authored callbacks.
    pub fn tangent_line_styled(
        &self,
        stage: &Stage,
        x: f64,
        function: &dyn Fn(f64) -> f64,
        line_length: f64,
        dx: f64,
        style: Style,
    ) -> Result<VMobject, CoordsError> {
        self.snapshot(stage)?
            .tangent_line_styled(x, function, line_length, dx, style)
    }

    /// Build numeric run/rise and their secant from a single current snapshot.
    pub fn get_secant_slope_group(
        &self,
        stage: &Stage,
        x: f64,
        function: &dyn Fn(f64) -> f64,
        dx: f64,
        secant_length: Option<f64>,
    ) -> Result<VMobject, CoordsError> {
        self.snapshot(stage)?
            .get_secant_slope_group(x, function, dx, secant_length)
    }
}
