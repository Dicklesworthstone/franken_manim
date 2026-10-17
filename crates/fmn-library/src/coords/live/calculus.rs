//! Calculus geometry in a frozen affine chart, shared by live chart handles.
//!
//! Bin boundaries use Atlas' existing BN-20 law. Integral regions clip the
//! drawn quadratic paths in data coordinates, not in curve-index space.

use super::{
    CoordinateFrame, CoordinateFrameError, CoordinateSystem, CoordsError, LiveCoordinateSystem,
    Vec3,
};
use crate::coords::{RiemannConfig, riemann_bins};
use crate::graphs::SamplingError;
use crate::poly::Polygon;
use crate::style::Style;
use crate::vmobject::{VMobject, v_group};
use fmn_core::color::{Srgb, color_gradient};
use fmn_core::constants::{BLUE, GREEN};
use fmn_geom::{QuadPath, bezier};
use fmn_mobject::Stage;

fn invalid(reason: &'static str) -> CoordsError {
    CoordinateFrameError::InvalidGeometry(reason).into()
}
fn record_point(point: Vec3) -> Result<Vec3, CoordsError> {
    if point
        .iter()
        .all(|x| x.is_finite() && x.abs() <= f64::from(f32::MAX))
    {
        Ok(point)
    } else {
        Err(invalid("points must fit finite f32 records"))
    }
}
fn validate_fill(color: Srgb, opacity: f64) -> Result<(), CoordsError> {
    if ![color.r, color.g, color.b].iter().all(|v| v.is_finite()) {
        return Err(invalid("fill colors must be finite"));
    }
    if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
        return Err(invalid("fill opacity must lie in [0, 1]"));
    }
    Ok(())
}

impl CoordinateFrame {
    /// Riemann bins in this affine chart, with BN-20's clipped final bin and
    /// the ordinary BLUE/GREEN gradient and signed-area styling.
    ///
    /// The complete bin request and style are admitted before the callback is
    /// invoked. Non-finite heights or unrepresentable corners are errors, not
    /// partial geometry. Effects performed by the callback are not rolled back.
    pub fn get_riemann_rectangles(
        &self,
        function: &dyn Fn(f64) -> f64,
        x_range: Option<[f64; 2]>,
        dx: Option<f64>,
        input_sample_type: &str,
    ) -> Result<VMobject, CoordsError> {
        self.riemann_rectangles_styled(
            function,
            x_range,
            dx,
            input_sample_type,
            &RiemannConfig::default(),
        )
    }

    /// Styled bins whose four corners share the same frozen c2p. Negative
    /// height is decided in data coordinates, so reflection, shear and
    /// out-of-plane rotation cannot reverse the sign convention.
    pub fn riemann_rectangles_styled(
        &self,
        function: &dyn Fn(f64) -> f64,
        x_range: Option<[f64; 2]>,
        dx: Option<f64>,
        input_sample_type: &str,
        config: &RiemannConfig,
    ) -> Result<VMobject, CoordsError> {
        if !matches!(input_sample_type, "left" | "right" | "center") {
            return Err(CoordsError::InvalidSampleType(input_sample_type.to_owned()));
        }
        validate_fill(config.stroke_color, config.fill_opacity)?;
        validate_fill(config.negative_color, config.fill_opacity)?;
        for &color in &config.colors {
            validate_fill(color, config.fill_opacity)?;
        }
        if !config.stroke_width.is_finite() || config.stroke_width < 0.0 {
            return Err(invalid("stroke width must be finite and nonnegative"));
        }
        let [start, stop] = x_range.unwrap_or([self.ranges[0][0], self.ranges[0][1]]);
        let bins = riemann_bins(
            start,
            stop,
            dx.unwrap_or(self.ranges[0][2]),
            self.sampling_budget,
        )?;
        let gradient = if config.colors.len() >= 2 {
            color_gradient(&config.colors, bins.len())
        } else {
            vec![config.colors.first().copied().unwrap_or(BLUE); bins.len()]
        };
        let mut rectangles = Vec::new();
        rectangles
            .try_reserve_exact(bins.len())
            .map_err(|_| SamplingError::AllocationFailed {
                context: "live Riemann rectangles",
                samples: bins.len(),
            })?;
        for (index, (left, right)) in bins.into_iter().enumerate() {
            let sample = match input_sample_type {
                "right" => right,
                "center" => left + (right - left) / 2.0,
                _ => left,
            };
            let height = function(sample);
            if !height.is_finite() {
                return Err(invalid("Riemann callback returned a non-finite height"));
            }
            let fill = if height < 0.0 && config.show_signed_area {
                config.negative_color
            } else {
                gradient.get(index).copied().unwrap_or(GREEN)
            };
            let corners = [
                record_point(self.c2p(&[left, 0.0]))?,
                record_point(self.c2p(&[right, 0.0]))?,
                record_point(self.c2p(&[right, height]))?,
                record_point(self.c2p(&[left, height]))?,
            ];
            rectangles.push(
                Polygon::new(corners)
                    .style(
                        Style::default()
                            .stroke(config.stroke_color, config.stroke_width, 1.0)
                            .fill(fill, config.fill_opacity),
                    )
                    .build()
                    .with_stroke_behind(config.stroke_behind),
            );
        }
        Ok(v_group(rectangles))
    }

    /// Shade the currently drawn graph down to numeric y = 0 (and z = 0).
    ///
    /// No analytic callback or x-range metadata is needed. Bounds are actual
    /// data coordinates: nonuniform sampling, reversed paths and disconnected
    /// components are supported. Each component closes independently, leaving
    /// discontinuity gaps empty. Bounds intersect the drawn domain. General
    /// foldbacks are refused instead of choosing a branch.
    pub fn get_area_under_graph(
        &self,
        graph: &VMobject,
        x_range: Option<[f64; 2]>,
    ) -> Result<VMobject, CoordsError> {
        self.area_under_graph_styled(graph, x_range, BLUE, 0.5)
    }

    /// Styled integral region from the source's own quadratic point run.
    /// Child-bearing groups must be flattened explicitly. Input and output
    /// point counts share this chart's sampling budget. The source geometry,
    /// styles and Stage are never modified.
    pub fn area_under_graph_styled(
        &self,
        graph: &VMobject,
        x_range: Option<[f64; 2]>,
        color: Srgb,
        opacity: f64,
    ) -> Result<VMobject, CoordsError> {
        validate_fill(color, opacity)?;
        if let Some([left, right]) = x_range
            && (!left.is_finite() || !right.is_finite() || right < left)
        {
            return Err(invalid("area bounds must be finite and nondecreasing"));
        }
        if !graph.children().is_empty() {
            return Err(invalid(
                "area requires a graph path, not a child-bearing group",
            ));
        }
        self.sampling_budget
            .ensure_total("graph area input", graph.points().len())?;
        for &point in graph.points() {
            record_point(point)?;
        }
        let path = graph
            .path()
            .map_err(|_| invalid("area requires a shared-anchor quadratic path"))?;
        let mut result = QuadPath::new();
        let style = Style::default().stroke_width(0.0).fill(color, opacity);
        if x_range.is_some_and(|[left, right]| left == right) {
            return Ok(VMobject::new().with_style(style));
        }
        for component in path.subpaths() {
            if component.len() < 3 {
                continue;
            }
            let mut points = component.to_vec();
            let mut xs = points.iter().map(|&p| self.p2c(p)[0]).collect::<Vec<_>>();
            for &point in &points {
                let coords = self.p2c(point);
                let plane = self.c2p(&coords[..2]);
                let tolerance = 8.0
                    * f64::from(f32::EPSILON)
                    * (1.0 + super::max_abs(point) + super::max_abs(self.origin));
                if !super::finite(coords)
                    || !super::finite(plane)
                    || (0..3).any(|axis| (point[axis] - plane[axis]).abs() > tolerance)
                {
                    return Err(invalid("graph must lie in the chart's x/y plane"));
                }
            }
            if xs.iter().any(|x| !x.is_finite()) {
                return Err(invalid("graph inverse coordinates are not finite"));
            }
            if xs[0] > xs[xs.len() - 1] {
                points.reverse();
                xs.reverse();
            }
            let tolerance =
                8.0 * f64::from(f32::EPSILON) * xs.iter().fold(1.0_f64, |m, x| m.max(x.abs()));
            let mut largest = xs[0];
            for x in &mut xs {
                if *x < largest - tolerance {
                    return Err(invalid("graph subpath folds back in data x"));
                }
                largest = largest.max(*x);
                *x = largest;
            }
            let [left, right] = x_range.map_or([xs[0], xs[xs.len() - 1]], |[l, r]| {
                [l.max(xs[0]), r.min(xs[xs.len() - 1])]
            });
            if right <= left {
                continue;
            }
            let curves = (points.len() - 1) / 2;
            let mut first = 0;
            while first + 1 < curves && xs[2 * first + 2] <= left {
                first += 1;
            }
            let mut last = first;
            while last + 1 < curves && xs[2 * last + 2] < right {
                last += 1;
            }
            let a = area_parameter(self, &points, &xs, first, left, tolerance)?;
            let b = area_parameter(self, &points, &xs, last, right, tolerance)?;
            let needed = result
                .num_points()
                .checked_add(2 * (last - first + 1) + 7)
                .and_then(|n| n.checked_add(usize::from(result.num_points() != 0)))
                .ok_or(SamplingError::CapacityOverflow {
                    context: "graph area output",
                })?;
            self.sampling_budget
                .ensure_total("graph area output", needed)?;
            let mut clipped = Vec::with_capacity(2 * (last - first + 1) + 1);
            for index in first..=last {
                let p = [
                    points[2 * index],
                    points[2 * index + 1],
                    points[2 * index + 2],
                ];
                let part = bezier::partial_quadratic(
                    &p,
                    if index == first { a } else { 0.0 },
                    if index == last { b } else { 1.0 },
                );
                if clipped.is_empty() {
                    clipped.push(part[0]);
                }
                clipped.extend_from_slice(&part[1..]);
            }
            let start = clipped[0];
            let baseline_end = record_point(self.c2p(&[right, 0.0]))?;
            let baseline_start = record_point(self.c2p(&[left, 0.0]))?;
            let mut region = QuadPath::from_points(clipped)
                .map_err(|_| invalid("invalid clipped quadratic path"))?;
            region
                .add_line_to(baseline_end, false)
                .and_then(|p| p.add_line_to(baseline_start, false))
                .and_then(|p| p.add_line_to(start, false))
                .map_err(|_| invalid("could not close integral region"))?;
            for &point in region.points() {
                record_point(point)?;
            }
            result
                .add_subpath(region.points())
                .map_err(|_| invalid("could not join integral components"))?;
        }
        Ok(VMobject::from_path(&result).with_style(style))
    }
}

// The fixed iteration bound is independent of scene frame count. Evaluation
// and restriction remain Chisel's primitives, never a resampled polyline.
fn area_parameter(
    frame: &CoordinateFrame,
    points: &[Vec3],
    xs: &[f64],
    index: usize,
    x: f64,
    tolerance: f64,
) -> Result<f64, CoordsError> {
    let start = xs[2 * index];
    let stop = xs[2 * index + 2];
    if x <= start {
        return Ok(0.0);
    }
    if x >= stop {
        return Ok(1.0);
    }
    let (mut low, mut high, mut best, mut error) = (0.0, 1.0, 0.0, (start - x).abs());
    for _ in 0..64 {
        let middle = (low + high) / 2.0;
        if middle == low || middle == high {
            break;
        }
        let point = bezier::quadratic_point(
            points[2 * index],
            points[2 * index + 1],
            points[2 * index + 2],
            middle,
        );
        let value = frame.p2c(point)[0];
        if !value.is_finite() {
            return Err(invalid("area boundary conversion is not finite"));
        }
        if (value - x).abs() < error {
            best = middle;
            error = (value - x).abs();
        }
        if value == x {
            return Ok(middle);
        }
        if value < x {
            low = middle;
        } else {
            high = middle;
        }
    }
    if error > tolerance {
        return Err(invalid(
            "area boundary cannot be resolved at record precision",
        ));
    }
    Ok(best)
}

impl LiveCoordinateSystem {
    /// Build Riemann bins in one snapshot of the currently registered axes.
    pub fn get_riemann_rectangles(
        &self,
        stage: &Stage,
        function: &dyn Fn(f64) -> f64,
        x_range: Option<[f64; 2]>,
        dx: Option<f64>,
        input_sample_type: &str,
    ) -> Result<VMobject, CoordsError> {
        self.snapshot(stage)?
            .get_riemann_rectangles(function, x_range, dx, input_sample_type)
    }

    /// Styled bins in the current chart; invalid axes refuse before the callback.
    #[allow(clippy::too_many_arguments)]
    pub fn riemann_rectangles_styled(
        &self,
        stage: &Stage,
        function: &dyn Fn(f64) -> f64,
        x_range: Option<[f64; 2]>,
        dx: Option<f64>,
        input_sample_type: &str,
        config: &RiemannConfig,
    ) -> Result<VMobject, CoordsError> {
        self.snapshot(stage)?.riemann_rectangles_styled(
            function,
            x_range,
            dx,
            input_sample_type,
            config,
        )
    }

    /// Clip a detached world-space graph in the current chart and close every
    /// continuous component to its own numeric baseline.
    pub fn get_area_under_graph(
        &self,
        stage: &Stage,
        graph: &VMobject,
        x_range: Option<[f64; 2]>,
    ) -> Result<VMobject, CoordsError> {
        self.snapshot(stage)?.get_area_under_graph(graph, x_range)
    }

    /// Styled integral region, using one current frame throughout clipping.
    pub fn area_under_graph_styled(
        &self,
        stage: &Stage,
        graph: &VMobject,
        x_range: Option<[f64; 2]>,
        color: Srgb,
        opacity: f64,
    ) -> Result<VMobject, CoordsError> {
        self.snapshot(stage)?
            .area_under_graph_styled(graph, x_range, color, opacity)
    }
}
