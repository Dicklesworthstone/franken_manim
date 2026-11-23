//! Hard native discs become bounded quadratic ellipse paths. The rasterizer
//! and this adapter share their world-space billboard axes; a projective
//! circle is converted to its ellipse before approximation, never by merely
//! projecting polynomial control points.

use fmn_core::types::Vec3;
use fmn_mobject::{Entry, Mob, Mobject, Stage, StageError};

use super::{GeometryBudget, ProjectedFamily, SvgProjectionError, unsupported};
use crate::{Camera, three_d::dot_billboard_axes};

// Half the 0.1 px silhouette budget goes to the quadratic approximation,
// half to the f32 record boundary. This is a geometry bound, not a claim that
// an SVG viewer uses Lumen's interior smoothstep antialiasing band.
const HALF_ERROR_PX: f64 = 0.05;

fn record<const N: usize>(
    entry: &Entry,
    row: usize,
    name: &str,
) -> Result<[f32; N], SvgProjectionError> {
    entry
        .buffer
        .read(row, name)
        .and_then(|values| values.try_into().ok())
        .ok_or_else(|| unsupported("invalid dot-cloud record schema"))
}

pub(super) fn validate(entry: &Entry) -> Result<(), SvgProjectionError> {
    let uniforms = entry.uniforms();
    if !uniforms.glow_factor.is_finite()
        || !uniforms.anti_alias_width.is_finite()
        || uniforms.anti_alias_width < 0.0
        || !uniforms.is_fixed_in_frame.is_finite()
    {
        return Err(unsupported("invalid dot-cloud uniforms"));
    }
    if uniforms.glow_factor > 0.0 {
        return Err(unsupported("dot glow requires a radial paint profile"));
    }
    for row in 0..entry.buffer.len() {
        let point = record::<3>(entry, row, "point")?;
        let [radius] = record::<1>(entry, row, "radius")?;
        let rgba = record::<4>(entry, row, "rgba")?;
        if !point
            .iter()
            .chain(rgba.iter())
            .all(|value| value.is_finite())
            || !radius.is_finite()
            || radius < 0.0
            || !entry
                .placement()
                .apply_point(point.map(f64::from))
                .iter()
                .all(|v| v.is_finite())
        {
            return Err(unsupported(
                "dot centers, colors and nonnegative radii must be finite",
            ));
        }
    }
    Ok(())
}

struct Ellipse {
    center: [f64; 2],
    axes: [[f64; 2]; 2],
}

impl Ellipse {
    fn project(
        camera: &Camera,
        center: Vec3,
        radius: f64,
        fixed: f64,
    ) -> Result<Self, SvgProjectionError> {
        let [_, right, up] = dot_billboard_axes(camera.location(), center, radius);
        let projection = camera.projection();
        let origin = projection.project(center, fixed).clip;
        let delta = |axis: Vec3| {
            let offset = std::array::from_fn(|i| center[i] + axis[i]);
            let clip = projection.project(offset, fixed).clip;
            std::array::from_fn::<_, 4, _>(|i| clip[i] - origin[i])
        };
        let a = delta(right);
        let b = delta(up);
        if !origin
            .iter()
            .chain(a.iter())
            .chain(b.iter())
            .all(|v| v.is_finite())
        {
            return Err(unsupported("invalid dot camera projection"));
        }

        // For a disc u²+v² <= 1, the minimum of d0 + da*u + db*v
        // is d0 - sqrt(da²+db²). Admit the entire depth extent, not just
        // the center or sampled boundary. X/Y clipping belongs to the SVG
        // viewport, exactly as it does for the existing vector path route.
        let minimum = |d0: f64, da: f64, db: f64| d0 - (da * da + db * db).sqrt();
        if minimum(origin[3], a[3], b[3]) <= 0.0
            || [-1.0, 1.0].into_iter().any(|sign| {
                minimum(
                    origin[3] + sign * origin[2],
                    a[3] + sign * a[2],
                    b[3] + sign * b[2],
                ) < 0.0
            })
        {
            return Err(unsupported(
                "dot discs crossing camera depth or horizon planes",
            ));
        }

        // Write the screen map as p(u) = p0 + B*u/(1 + g·u).
        // Its ellipse has center p0 - B*g/h and axes B*K, where
        // h = 1 - g·g and K = I/sqrt(h) + g*gᵀ/[h*(1+sqrt(h))].
        // K² = I/h + g*gᵀ/h², the ellipse covariance in unit-disc space.
        // This avoids fitting sampled points or cancelling conic eigenvalues.
        let w = origin[3];
        let g = [a[3] / w, b[3] / w];
        let h = 1.0 - (g[0] * g[0] + g[1] * g[1]);
        if !h.is_finite() || h <= 0.0 {
            return Err(unsupported("degenerate projected dot ellipse"));
        }
        let (width, height) = camera.pixel_shape();
        let scales = [f64::from(width) / 2.0, f64::from(height) / 2.0];
        let p0: [f64; 2] = std::array::from_fn(|i| origin[i] / w * scales[i]);
        let basis: [[f64; 2]; 2] = std::array::from_fn(|i| {
            [
                (a[i] / w * scales[i]) - p0[i] * g[0],
                (b[i] / w * scales[i]) - p0[i] * g[1],
            ]
        });
        let bg: [f64; 2] = std::array::from_fn(|i| basis[i][0] * g[0] + basis[i][1] * g[1]);
        let sqrt_h = h.sqrt();
        let center = std::array::from_fn(|i| p0[i] - bg[i] / h);
        let axes = std::array::from_fn(|axis| {
            std::array::from_fn(|i| {
                basis[i][axis] / sqrt_h + bg[i] * g[axis] / (h * (1.0 + sqrt_h))
            })
        });
        let ellipse = Self { center, axes };
        if !ellipse
            .center
            .iter()
            .chain(ellipse.axes.iter().flatten())
            .all(|v| v.is_finite())
        {
            return Err(unsupported(
                "projected dot coordinates exceed native record precision",
            ));
        }
        Ok(ellipse)
    }

    fn component_count(&self, remaining_points: usize) -> Result<usize, SvgProjectionError> {
        // The Frobenius norm bounds the maximum stretch of both ellipse
        // axes. A tangent-control quadratic over angle 2u has maximum radial
        // excess (cos(u) + sec(u))/2 - 1 = (1-cos(u))²/(2*cos(u)).
        // Start with BN-09's 16-component full circle and refine only when
        // its screen-space error requires it. Every allocation is budgeted.
        let stretch = self
            .axes
            .iter()
            .flatten()
            .map(|v| v * v)
            .sum::<f64>()
            .sqrt();
        let mut components = 16usize;
        loop {
            if components > fmn_geom::bezier::MAX_ARC_COMPONENTS
                || 2 * components + 1 > remaining_points
            {
                return Err(SvgProjectionError::GeometryLimit);
            }
            let cosine = fmn_dmath::cos(std::f64::consts::PI / components as f64);
            let excess = (1.0 - cosine) * (1.0 - cosine) / (2.0 * cosine);
            if stretch * excess <= HALF_ERROR_PX {
                return Ok(components);
            }
            components *= 2;
        }
    }

    fn points(&self, components: usize) -> Result<Vec<Vec3>, SvgProjectionError> {
        let mut points =
            fmn_geom::bezier::quadratic_points_for_arc(std::f64::consts::TAU, components)
                .map_err(|_| SvgProjectionError::GeometryLimit)?;
        for point in &mut points {
            let unit = *point;
            for (i, value) in point[..2].iter_mut().enumerate() {
                *value = self.center[i] + self.axes[0][i] * unit[0] + self.axes[1][i] * unit[1];
            }
            let dx = point[0] - f64::from(point[0] as f32);
            let dy = point[1] - f64::from(point[1] as f32);
            if !point[..2]
                .iter()
                .all(|v| v.is_finite() && v.abs() <= f64::from(f32::MAX))
                || (dx * dx + dy * dy).sqrt() > HALF_ERROR_PX
            {
                return Err(unsupported(
                    "projected dot coordinates exceed native record precision",
                ));
            }
        }
        // Exact closure survives both the f32 record boundary and SVG parse.
        let last = points.len() - 1;
        points[last] = points[0];
        Ok(points)
    }
}

pub(super) fn project(
    stage: &Stage,
    source: Mob,
    camera: &Camera,
    budget: &mut GeometryBudget,
    projected: &mut Stage,
) -> Result<ProjectedFamily, SvgProjectionError> {
    let entry = stage.get(source).ok_or(StageError::StaleHandle)?;
    let mut family = ProjectedFamily {
        roots: Vec::new(),
        points: 0,
    };
    for row in 0..entry.buffer.len() {
        let [radius] = record::<1>(entry, row, "radius")?;
        let rgba = record::<4>(entry, row, "rgba")?;
        if radius == 0.0 || rgba[3] <= 0.0 {
            continue;
        }
        let center = entry
            .placement()
            .apply_point(record::<3>(entry, row, "point")?.map(f64::from));
        let ellipse = Ellipse::project(
            camera,
            center,
            f64::from(radius),
            entry.uniforms().is_fixed_in_frame,
        )?;
        let components = ellipse.component_count(budget.limits.max_points - budget.points)?;
        let point_count = 2 * components + 1;
        budget.charge(point_count, 1)?;
        let points = ellipse.points(components)?;
        let mut object = Mobject::vector_from_points(&points);
        for point in 0..point_count {
            if !object.buffer.write(point, "fill_rgba", &rgba) {
                return Err(unsupported("invalid projected dot paint records"));
            }
        }
        family.roots.try_reserve(1)?;
        family.roots.push(projected.add(object));
        family.points += point_count;
    }
    Ok(family)
}

#[cfg(test)]
mod tests;
