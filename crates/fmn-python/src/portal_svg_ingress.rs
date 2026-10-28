//! Bounded SVG paint ingress. Chisel owns XML, styles, booleans and arclength;
//! this bridge admits overrides, checks native-record precision, and installs
//! the completed family before the ordinary Python sizing hooks run.
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyList};

use crate::{BridgeMobject, install_native_tree, native_error, srgb_from_py};
use fmn_library::VMobject;
use fmn_library::svg::{
    DEFAULT_SVG_SCENE_TOLERANCE, SvgDocument, SvgPaintError, SvgPaintOverrides,
    svg_document_for_size_with_tolerance, svg_document_with_paints,
};
use fmn_mobject::Placement;

#[pyfunction]
fn _build_svg_paints<'py>(
    slf: &Bound<'py, BridgeMobject>,
    factory: &Bound<'py, PyAny>,
    source: &str,
    options: &Bound<'py, PyDict>,
) -> PyResult<Bound<'py, PyList>> {
    // The native parser enforces its own byte budget before parsing, too.
    if source.len() > 1_048_576 {
        return Err(PyValueError::new_err("SVG input exceeds 1048576 bytes"));
    }
    let mut overrides = SvgPaintOverrides::default();
    let mut height = None;
    let mut width = None;
    let mut should_center = None;
    let mut constructor = false;
    for (key, value) in options.iter() {
        match key.extract::<String>()?.as_str() {
            "fill_color" => overrides.fill_color = Some(srgb_from_py(&value)?),
            "stroke_color" => overrides.stroke_color = Some(srgb_from_py(&value)?),
            "fill_opacity" => overrides.fill_opacity = Some(value.extract()?),
            "stroke_opacity" => overrides.stroke_opacity = Some(value.extract()?),
            "stroke_width" => overrides.stroke_width = Some(value.extract()?),
            name @ ("height" | "width") => {
                // The constructor supplies both keys, including None for an
                // unsized class. Raw factory calls supply neither key and
                // leave the returned parts in SVG viewport coordinates.
                constructor = true;
                let size = value.extract::<Option<f64>>()?;
                if size.is_some_and(|size| !size.is_finite() || size <= 0.0) {
                    return Err(PyValueError::new_err(format!(
                        "SVG {name} must be finite and positive"
                    )));
                }
                if name == "height" {
                    height = size;
                } else {
                    width = size;
                }
            }
            "should_center" => {
                // Inspect only an ordinary bool. Authored truthiness belongs
                // to the usual constructor hook, never this admission pass.
                should_center = if value.is_instance_of::<PyBool>() {
                    Some(value.extract::<bool>()?)
                } else {
                    None
                };
            }
            name => {
                return Err(PyTypeError::new_err(format!(
                    "unknown SVG paint override: {name}"
                )));
            }
        }
    }
    let (prepared, kinds) = slf.py().detach(|| {
        let document = if height.is_some() || width.is_some() {
            // Half the scene-space budget belongs to cubic conversion; the
            // other half covers native f32 records and their normalization.
            svg_document_for_size_with_tolerance(
                source.as_bytes(),
                height,
                width,
                0.5 * DEFAULT_SVG_SCENE_TOLERANCE,
            )
            .map_err(native_error)?
        } else {
            // An explicitly unsized import keeps the viewport-space parser
            // contract. There is no final scene-space size to bound.
            SvgDocument::parse(source.as_bytes())
                .map_err(SvgPaintError::Document)
                .map_err(native_error)?
        };
        // One child per pointful shape, in document order: the element each
        // came from decides the Reference class the portal gives it (fm-5rlu).
        let kinds: Vec<&'static str> = document
            .shapes
            .iter()
            .filter(|shape| shape.path.has_points())
            .map(|shape| shape.kind.name())
            .collect();
        let prepared = svg_document_with_paints(&document, overrides).map_err(native_error)?;
        let prepared =
            prepare_native_precision(prepared, height, width, should_center, constructor)
                .map_err(PyValueError::new_err)?;
        Ok::<_, PyErr>((prepared, kinds))
    })?;
    let children = install_native_tree(slf, factory, prepared)?;
    slf.as_any().setattr("_svg_child_kinds", kinds)?;
    Ok(children)
}

pub(super) fn install(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(_build_svg_paints, module)?)
}

const MIN_IMPORT_SCALE: f64 = 1e-8;
const NONFINITE_RECORD: &str =
    "SVG precision: imported points must remain finite in native f32 records";
const LOST_PATH_TOPOLOGY: &str =
    "SVG precision: f32 records cannot preserve shared-anchor path topology";
const NORMALIZATION_REFUSED: &str = "SVG precision: normalization is not finite or representable";
const MINIMUM_SCALE_REFUSED: &str =
    "SVG precision: requested size requires a scale below the native minimum (1e-8)";
const COLLAPSED_AXIS: &str =
    "SVG precision: native records collapse a requested normalization axis";
const CURVE_PRECISION_REFUSED: &str = "SVG precision: f32 records cannot preserve the requested curve tolerance; reduce SVG coordinate offsets or requested size";

#[derive(Clone, Copy)]
struct ImportBounds {
    min: [f64; 3],
    max: [f64; 3],
}

impl ImportBounds {
    fn empty() -> Self {
        Self {
            min: [f64::INFINITY; 3],
            max: [f64::NEG_INFINITY; 3],
        }
    }

    fn include(&mut self, point: [f64; 3]) {
        for (axis, value) in point.into_iter().enumerate() {
            self.min[axis] = self.min[axis].min(value);
            self.max[axis] = self.max[axis].max(value);
        }
    }

    fn span(self, axis: usize) -> f64 {
        self.max[axis] - self.min[axis]
    }
}

/// The ordinary SVG constructor flips about its center, optionally centers,
/// then fits height and width in that order. Records become f32 both before
/// these hooks and when orientation inspection bakes the flip, so the actual
/// normalization needs the bounds after that intermediate materialization.
#[derive(Clone, Copy)]
struct ImportNormalization {
    center: [f64; 3],
    scale: f64,
    peak_scale: f64,
}

impl ImportNormalization {
    fn new(
        bounds: ImportBounds,
        height: Option<f64>,
        width: Option<f64>,
    ) -> Result<Self, &'static str> {
        let center: [f64; 3] =
            std::array::from_fn(|axis| 0.5 * (bounds.min[axis] + bounds.max[axis]));
        let spans: [f64; 3] = std::array::from_fn(|axis| bounds.span(axis));
        if center
            .iter()
            .chain(spans.iter())
            .any(|value| !value.is_finite())
        {
            return Err(NORMALIZATION_REFUSED);
        }
        let mut scale: f64 = 1.0;
        let mut peak_scale: f64 = 1.0;
        for (size, axis) in [(height, 1), (width, 0)] {
            let Some(size) = size else {
                continue;
            };
            let span = spans[axis] * scale;
            if !span.is_finite() || (span == 0.0 && spans[axis] != 0.0) {
                return Err(NORMALIZATION_REFUSED);
            }
            if span == 0.0 {
                // rescale_to_fit intentionally leaves a zero extent alone.
                continue;
            }
            let factor = size / span;
            if !factor.is_finite() || factor <= 0.0 {
                return Err(NORMALIZATION_REFUSED);
            }
            if factor < MIN_IMPORT_SCALE {
                // Mobject.scale clamps each call separately. Treating the
                // requested factor as applied would give a false bound.
                return Err(MINIMUM_SCALE_REFUSED);
            }
            scale *= factor;
            if !scale.is_finite() || scale <= 0.0 {
                return Err(NORMALIZATION_REFUSED);
            }
            peak_scale = peak_scale.max(scale);
        }
        Ok(Self {
            center,
            scale,
            peak_scale,
        })
    }

    fn point(self, point: [f64; 3], centered: bool) -> [f64; 3] {
        std::array::from_fn(|axis| {
            // flip(RIGHT) is a half turn: x stays, y/z change sign.
            let sign = if axis == 0 { 1.0 } else { -1.0 };
            let relative = sign * (point[axis] - self.center[axis]) * self.scale;
            if centered {
                relative
            } else {
                self.center[axis] + relative
            }
        })
    }

    fn point_after_flip(self, point: [f64; 3], centered: bool) -> [f64; 3] {
        if !centered && self.scale == 1.0 {
            // An uncentered, unsized constructor applies no further map.
            // Do not introduce cancellation through a fictitious pivot.
            return point;
        }
        std::array::from_fn(|axis| {
            let relative = (point[axis] - self.center[axis]) * self.scale;
            if centered {
                relative
            } else {
                self.center[axis] + relative
            }
        })
    }
}

fn visit_import_paths(
    family: &VMobject,
    visitor: &mut impl FnMut(&[[f64; 3]]) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    visitor(family.points())?;
    for child in family.children() {
        visit_import_paths(child, visitor)?;
    }
    Ok(())
}

fn visit_import_path_pairs(
    source: &VMobject,
    candidate: &VMobject,
    visitor: &mut impl FnMut(&[[f64; 3]], &[[f64; 3]]) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    if source.points().len() != candidate.points().len()
        || source.children().len() != candidate.children().len()
    {
        return Err(LOST_PATH_TOPOLOGY);
    }
    visitor(source.points(), candidate.points())?;
    for (original, proposed) in source.children().iter().zip(candidate.children()) {
        visit_import_path_pairs(original, proposed, visitor)?;
    }
    Ok(())
}

fn record_point(point: [f64; 3]) -> Result<[f64; 3], &'static str> {
    if point.iter().any(|value| !value.is_finite()) {
        return Err(NONFINITE_RECORD);
    }
    // This is the conversion performed by VMobject -> Mobject, without
    // installing anything in the Stage or invoking an authored Python hook.
    #[allow(clippy::cast_possible_truncation)]
    let rounded = point.map(|value| f64::from(value as f32));
    if rounded.iter().any(|value| !value.is_finite()) {
        return Err(NONFINITE_RECORD);
    }
    Ok(rounded)
}

fn shared_anchor_break(curve: [[f64; 3]; 3]) -> bool {
    // QuadPath::subpath_end_indices' fixed Reference disambiguation rule.
    // Its threshold is intentionally unrelated to the approximation budget.
    curve[0] == curve[1] && (0..3).any(|axis| (curve[1][axis] - curve[2][axis]).abs() > 1e-4)
}

fn retain_curve_topology(
    original: [[f64; 3]; 3],
    rounded: [[f64; 3]; 3],
) -> Result<(), &'static str> {
    let was_break = shared_anchor_break(original);
    if was_break != shared_anchor_break(rounded) {
        return Err(LOST_PATH_TOPOLOGY);
    }
    if !was_break && original[0] != original[2] && rounded[0] == rounded[2] {
        // A surviving handle cannot recover two distinct erased anchors;
        // accepting it would silently turn an open curve into a closed loop.
        return Err(LOST_PATH_TOPOLOGY);
    }
    if !was_break
        && (original[0] != original[1] || original[1] != original[2])
        && rounded[0] == rounded[1]
        && rounded[1] == rounded[2]
    {
        return Err(LOST_PATH_TOPOLOGY);
    }
    if !was_break
        && original[1] != original[0]
        && original[1] != original[2]
        && rounded[0] != rounded[2]
        && (rounded[1] == rounded[0] || rounded[1] == rounded[2])
    {
        // The converter deliberately separates stationary handles. Losing
        // that separation can introduce a break now or on point reversal.
        return Err(LOST_PATH_TOPOLOGY);
    }
    Ok(())
}

fn normalized_record_error(
    point: [f64; 3],
    rounded: [f64; 3],
    original: ImportNormalization,
    native: ImportNormalization,
    centered: bool,
) -> Result<f64, &'static str> {
    let expected = original.point(point, centered);
    let actual = native.point_after_flip(rounded, centered);
    // get_points()/writable record access bakes the final placement back into
    // f32. Bound both representations: a later bake must not turn an admitted
    // retained curve into a visibly different one.
    let baked = record_point(actual)?;
    let mut largest_difference: f64 = 0.0;
    let mut coordinate_scale: f64 = 1.0;
    let mut differences = [0.0; 3];
    for axis in 0..3 {
        if !expected[axis].is_finite() || !actual[axis].is_finite() {
            return Err(NORMALIZATION_REFUSED);
        }
        differences[axis] = (expected[axis] - actual[axis])
            .abs()
            .max((expected[axis] - baked[axis]).abs());
        if !differences[axis].is_finite() {
            return Err(NORMALIZATION_REFUSED);
        }
        largest_difference = largest_difference.max(differences[axis].abs());
        coordinate_scale = coordinate_scale
            .max(expected[axis].abs())
            .max(actual[axis].abs())
            .max(baked[axis].abs())
            .max(point[axis].abs() * original.peak_scale)
            .max(rounded[axis].abs() * native.peak_scale);
    }
    if !coordinate_scale.is_finite() {
        return Err(NORMALIZATION_REFUSED);
    }
    let distance = if largest_difference == 0.0 {
        0.0
    } else {
        largest_difference
            * differences
                .iter()
                .map(|difference| (difference / largest_difference).powi(2))
                .sum::<f64>()
                .sqrt()
    };
    // Leave outward room for f64 bound arithmetic and the Stage's equivalent
    // sequence of affine placements. A large intermediate height followed by
    // a small width still incurs the intermediate rounding, hence peak_scale.
    let error = distance + 128.0 * f64::EPSILON * coordinate_scale;
    if !error.is_finite() {
        return Err(NORMALIZATION_REFUSED);
    }
    Ok(error)
}

#[derive(Clone, Copy)]
struct RecordProjection {
    flip: Placement,
    normalization: ImportNormalization,
    source_bounds: ImportBounds,
}

impl RecordProjection {
    fn new(
        original: &VMobject,
        candidate: &VMobject,
        height: Option<f64>,
        width: Option<f64>,
        constructor: bool,
    ) -> Result<Self, &'static str> {
        let mut source_bounds = ImportBounds::empty();
        let mut bounds = ImportBounds::empty();
        visit_import_path_pairs(original, candidate, &mut |points, proposed| {
            for (&source, &point) in points.iter().zip(proposed) {
                source_bounds.include(source);
                bounds.include(record_point(point)?);
            }
            Ok(())
        })?;
        let flip = if constructor {
            Placement::about(
                fmn_library::rotation_matrix(std::f64::consts::PI, [1.0, 0.0, 0.0]),
                ImportNormalization::new(bounds, None, None)?.center,
            )
        } else {
            Placement::IDENTITY
        };
        let mut flipped = ImportBounds::empty();
        visit_import_paths(candidate, &mut |points| {
            for &point in points {
                flipped.include(record_point(flip.apply_point(record_point(point)?))?);
            }
            Ok(())
        })?;
        Ok(Self {
            flip,
            normalization: ImportNormalization::new(flipped, height, width)?,
            source_bounds,
        })
    }

    fn flipped(self, point: [f64; 3]) -> Result<[f64; 3], &'static str> {
        record_point(self.flip.apply_point(record_point(point)?))
    }
}

fn adjacent_record(value: f64, positive: bool) -> f64 {
    #[allow(clippy::cast_possible_truncation)]
    let value = value as f32;
    f64::from(if positive {
        value.next_up()
    } else {
        value.next_down()
    })
}

fn record_spacing(value: f64) -> Result<f64, &'static str> {
    let value = record_point([value, 0.0, 0.0])?[0];
    Ok(
        [adjacent_record(value, false), adjacent_record(value, true)]
            .into_iter()
            .filter(|neighbor| neighbor.is_finite())
            .map(|neighbor| (neighbor - value).abs())
            .fold(0.0, f64::max),
    )
}

fn collapsed_handle(curve: [[f64; 3]; 3]) -> Option<usize> {
    if curve[1] == curve[0] {
        Some(0)
    } else if curve[1] == curve[2] {
        Some(2)
    } else {
        None
    }
}

fn repair_record_family(
    original: &VMobject,
    candidate: VMobject,
    projection: RecordProjection,
    should_center: Option<bool>,
    constructor: bool,
    growth: f64,
) -> Result<VMobject, &'static str> {
    let mut points = candidate
        .points()
        .iter()
        .copied()
        .map(record_point)
        .collect::<Result<Vec<_>, _>>()?;
    for offset in (0..points.len().saturating_sub(2)).step_by(2) {
        let source = &original.points()[offset..offset + 3];
        let source_curve = [source[0], source[1], source[2]];
        if shared_anchor_break(source_curve) || source[1] == source[0] || source[1] == source[2] {
            continue;
        }
        let curve = [points[offset], points[offset + 1], points[offset + 2]];
        let mut collision = collapsed_handle(curve);
        let mut minimum = [0.0_f64; 3];
        if constructor {
            let flipped = [
                projection.flipped(curve[0])?,
                projection.flipped(curve[1])?,
                projection.flipped(curve[2])?,
            ];
            collision = collision.or_else(|| collapsed_handle(flipped));
            for centered in [true, false] {
                if should_center.is_some_and(|value| value != centered) {
                    continue;
                }
                let placed =
                    flipped.map(|point| projection.normalization.point_after_flip(point, centered));
                let baked = [
                    record_point(placed[0])?,
                    record_point(placed[1])?,
                    record_point(placed[2])?,
                ];
                collision = collision.or_else(|| collapsed_handle(baked));
                for index in 0..3 {
                    for axis in 0..2 {
                        minimum[axis] = minimum[axis]
                            .max(record_spacing(curve[index][axis])?)
                            .max(record_spacing(flipped[index][axis])?)
                            .max(
                                record_spacing(placed[index][axis])?
                                    / projection.normalization.scale,
                            );
                    }
                }
            }
        }
        let Some(anchor) = collision else {
            continue;
        };
        // Request a representable separation in the records actually installed
        // before hooks. A later-phase collision is repaired here, never merely
        // in the admission simulation. The source stays untouched for the
        // complete original-versus-candidate error comparison below.
        points[offset + 1] = curve[anchor];
        minimum.iter_mut().for_each(|step| *step *= 2.0 * growth);
        fmn_library::repair_projected_path_handles(
            source,
            &mut points[offset..offset + 3],
            minimum,
            adjacent_record,
            |direction| {
                std::array::from_fn(|axis| {
                    if axis < 2 && projection.source_bounds.span(axis) > 0.0 {
                        direction[axis]
                    } else {
                        0.0
                    }
                })
            },
        )
        .map_err(|_| LOST_PATH_TOPOLOGY)?;
    }
    let mut index = 0;
    let mut failure = None;
    let repaired = candidate.with_points(points).map_children(|child| {
        let source = &original.children()[index];
        index += 1;
        match repair_record_family(
            source,
            child,
            projection,
            should_center,
            constructor,
            growth,
        ) {
            Ok(child) => child,
            Err(error) => {
                failure = Some(error);
                VMobject::new()
            }
        }
    });
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(repaired)
}

fn prepare_native_precision(
    original: VMobject,
    height: Option<f64>,
    width: Option<f64>,
    should_center: Option<bool>,
    constructor: bool,
) -> Result<VMobject, &'static str> {
    match admit_native_precision(
        &original,
        &original,
        height,
        width,
        should_center,
        constructor,
    ) {
        Ok(()) => return Ok(original),
        Err(LOST_PATH_TOPOLOGY) => {}
        Err(error) => return Err(error),
    }
    let mut candidate = original.clone();
    // One normal-grid step is normally enough. Recompute normalization after
    // repair; a fixed retry cap prevents hostile data from expanding work.
    for attempt in 0..6 {
        let projection = RecordProjection::new(&original, &candidate, height, width, constructor)?;
        candidate = repair_record_family(
            &original,
            candidate,
            projection,
            should_center,
            constructor,
            f64::from(1_u32 << attempt),
        )?;
        if height.is_none() && width.is_none() {
            // There is no final scene-space scale for an unsized import.
            // Still bound a representation repair in its raw viewport units.
            let budget = 0.5 * fmn::geometry::DEFAULT_SVG_TOLERANCE;
            visit_import_path_pairs(&original, &candidate, &mut |before, after| {
                for (&source, &point) in before.iter().zip(after) {
                    let delta = std::array::from_fn::<_, 3, _>(|axis| point[axis] - source[axis]);
                    if delta[0].hypot(delta[1]).hypot(delta[2]) > budget {
                        return Err(LOST_PATH_TOPOLOGY);
                    }
                }
                Ok(())
            })?;
        }
        match admit_native_precision(
            &original,
            &candidate,
            height,
            width,
            should_center,
            constructor,
        ) {
            Ok(()) => return Ok(candidate),
            Err(LOST_PATH_TOPOLOGY) => {}
            Err(error) => return Err(error),
        }
    }
    Err(LOST_PATH_TOPOLOGY)
}

/// Refuse a false scene-space guarantee before any native nodes are installed.
///
/// A quadratic is a convex combination of its controls, so bounding every
/// normalized control displacement bounds the entire curve displacement.
/// Comparing with each representation's own bounds also captures f32 changes
/// to the centering and fitted scale, which raw coordinate error alone misses.
fn admit_native_precision(
    family: &VMobject,
    candidate: &VMobject,
    height: Option<f64>,
    width: Option<f64>,
    should_center: Option<bool>,
    constructor: bool,
) -> Result<(), &'static str> {
    let mut original_bounds = ImportBounds::empty();
    let mut native_bounds = ImportBounds::empty();
    let mut any_points = false;
    visit_import_path_pairs(family, candidate, &mut |points, proposed| {
        if !points.is_empty() && points.len().is_multiple_of(2) {
            return Err(LOST_PATH_TOPOLOGY);
        }
        for (before, after) in points
            .windows(3)
            .step_by(2)
            .zip(proposed.windows(3).step_by(2))
        {
            retain_curve_topology(
                [before[0], before[1], before[2]],
                [
                    record_point(after[0])?,
                    record_point(after[1])?,
                    record_point(after[2])?,
                ],
            )?;
        }
        for (&point, &proposed) in points.iter().zip(proposed) {
            if point.iter().any(|value| !value.is_finite()) {
                return Err(NONFINITE_RECORD);
            }
            let rounded = record_point(proposed)?;
            original_bounds.include(point);
            native_bounds.include(rounded);
            any_points = true;
        }
        Ok(())
    })?;
    if !any_points || !constructor {
        // Public factory parts stay in raw SVG coordinates. Initial record
        // admission is sufficient until their caller chooses a placement.
        return Ok(());
    }

    // ensure_positive_orientation() asks get_num_points()/get_points() for
    // every member, baking flip(RIGHT) into f32 before centering and sizing.
    // Reuse Marionette's exact affine construction, including the tiny z
    // component of a floating-point half-turn: it matters for path topology.
    let flip = Placement::about(
        fmn_library::rotation_matrix(std::f64::consts::PI, [1.0, 0.0, 0.0]),
        ImportNormalization::new(native_bounds, None, None)?.center,
    );
    let flip_point =
        |point| -> Result<[f64; 3], &'static str> { Ok(flip.apply_point(record_point(point)?)) };
    let flipped_record = |point| record_point(flip_point(point)?);
    let mut flipped_bounds = ImportBounds::empty();
    visit_import_path_pairs(family, candidate, &mut |points, proposed| {
        for (source, curve) in points
            .windows(3)
            .step_by(2)
            .zip(proposed.windows(3).step_by(2))
        {
            let reflected = [
                flipped_record(curve[0])?,
                flipped_record(curve[1])?,
                flipped_record(curve[2])?,
            ];
            retain_curve_topology([source[0], source[1], source[2]], reflected)?;
        }
        for &point in proposed {
            flipped_bounds.include(flipped_record(point)?);
        }
        Ok(())
    })?;
    for (size, axis) in [(height, 1), (width, 0)] {
        if size.is_some() && original_bounds.span(axis) > 0.0 && flipped_bounds.span(axis) == 0.0 {
            return Err(COLLAPSED_AXIS);
        }
    }
    let original = ImportNormalization::new(original_bounds, height, width)?;
    let native = ImportNormalization::new(flipped_bounds, height, width)?;
    let budget = 0.5 * DEFAULT_SVG_SCENE_TOLERANCE;
    visit_import_path_pairs(family, candidate, &mut |points, proposed| {
        for centered in [true, false] {
            if should_center.is_some_and(|value| value != centered) {
                continue;
            }
            for (source, curve) in points
                .windows(3)
                .step_by(2)
                .zip(proposed.windows(3).step_by(2))
            {
                let placed = [
                    native.point_after_flip(flipped_record(curve[0])?, centered),
                    native.point_after_flip(flipped_record(curve[1])?, centered),
                    native.point_after_flip(flipped_record(curve[2])?, centered),
                ];
                // Compare to original structure: an affine f64 cancellation
                // can erase a handle before the following f32 conversion.
                let source = [source[0], source[1], source[2]];
                retain_curve_topology(source, placed)?;
                retain_curve_topology(
                    source,
                    [
                        record_point(placed[0])?,
                        record_point(placed[1])?,
                        record_point(placed[2])?,
                    ],
                )?;
            }
        }
        if height.is_none() && width.is_none() {
            // Unsized constructors still pass every finite/topology check,
            // including centering and its later writable-record bake. Only
            // the final scene-space approximation promise is inapplicable.
            return Ok(());
        }
        for (&point, &proposed) in points.iter().zip(proposed) {
            let rounded = flipped_record(proposed)?;
            for centered in [true, false] {
                if should_center.is_some_and(|value| value != centered) {
                    continue;
                }
                if normalized_record_error(point, rounded, original, native, centered)? > budget {
                    return Err(CURVE_PRECISION_REFUSED);
                }
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admit_constructor(
        family: &VMobject,
        height: Option<f64>,
        width: Option<f64>,
        should_center: Option<bool>,
    ) -> Result<(), &'static str> {
        admit_native_precision(family, family, height, width, should_center, true)
    }

    fn offset_curve(offset: f64) -> VMobject {
        VMobject::from_points(vec![
            [offset, 0.0, 0.0],
            [offset + 5.1, 6.2, 0.0],
            [offset + 16.0, 16.0, 0.0],
        ])
    }

    #[test]
    fn record_precision_keeps_ordinary_icons_and_rejects_large_offsets_before_install() {
        assert_eq!(
            admit_constructor(&offset_curve(0.0), Some(4.0), None, Some(true)),
            Ok(())
        );
        assert_eq!(
            admit_constructor(&offset_curve(1e6), Some(4.0), None, Some(true)),
            Err(CURVE_PRECISION_REFUSED)
        );
        assert_eq!(
            admit_constructor(&offset_curve(1e6), None, None, Some(true)),
            Ok(()),
            "an unsized import does not promise a final scene-space bound"
        );
    }

    #[test]
    fn record_precision_includes_the_change_to_normalization_scale() {
        let curve = VMobject::from_points(vec![
            [0.0, 0.0, 0.0],
            [500_000.0, 0.5, 0.0],
            [1_000_000.0, 1.000_000_03, 0.0],
        ]);
        assert_eq!(
            admit_constructor(&curve, Some(4.0), None, Some(true)),
            Err(CURVE_PRECISION_REFUSED),
            "tiny y rounding changes the height fit enough to move x visibly"
        );
    }

    #[test]
    fn record_precision_uses_family_bounds_and_checks_unknown_centering_both_ways() {
        let family = VMobject::new().with_children([
            offset_curve(1e6),
            VMobject::from_points(vec![[0.0, 1e6, 0.0]]),
        ]);
        assert_eq!(
            admit_constructor(&family, Some(4.0), None, Some(true)),
            Ok(()),
            "the whole family is normalized together"
        );
        let translated = VMobject::from_points(vec![
            [1_000_000.02, 0.0, 0.0],
            [1_500_000.02, 500_000.0, 0.0],
            [2_000_000.02, 1_000_000.0, 0.0],
        ]);
        assert_eq!(
            admit_constructor(&translated, Some(4.0), None, Some(true)),
            Ok(()),
            "centering cancels a common coordinate offset"
        );
        for should_center in [Some(false), None] {
            assert_eq!(
                admit_constructor(&translated, Some(4.0), None, should_center),
                Err(CURVE_PRECISION_REFUSED),
                "unknown truthiness must also admit the uncentered outcome"
            );
        }
    }

    #[test]
    fn record_precision_preserves_break_markers_but_refuses_erased_real_handles() {
        let marker =
            VMobject::from_points(vec![[0.0, 0.0, 0.0], [0.0, 0.0, 0.0], [10.0, 10.0, 0.0]]);
        assert_eq!(admit_constructor(&marker, None, None, None), Ok(()));
        let erased_start = VMobject::from_points(vec![
            [0.0, 0.0, 0.0],
            [f64::from_bits(1), 0.0, 0.0],
            [1.0, 1.0, 0.0],
        ]);
        let erased_end = VMobject::from_points(vec![
            [0.0, 0.0, 0.0],
            [1.0 + f64::EPSILON, 1.0, 0.0],
            [1.0, 1.0, 0.0],
        ]);
        let erased_curve = VMobject::from_points(vec![
            [0.0, 0.0, 0.0],
            [1e-50, 1e-50, 0.0],
            [2e-50, 2e-50, 0.0],
        ]);
        for curve in [erased_start, erased_end, erased_curve] {
            assert_eq!(
                admit_constructor(&curve, None, None, None),
                Err(LOST_PATH_TOPOLOGY)
            );
        }
    }

    #[test]
    fn record_precision_refuses_erased_anchors_even_when_the_handle_survives() {
        let curve = VMobject::from_points(vec![
            [100_000_000.0, 0.0, 0.0],
            [100_000_008.0, 1.0, 0.0],
            [100_000_001.0, 0.0, 0.0],
        ]);
        assert_eq!(
            admit_native_precision(&curve, &curve, None, None, None, false),
            Err(LOST_PATH_TOPOLOGY)
        );
        assert_eq!(
            prepare_native_precision(curve, None, None, None, false),
            Err(LOST_PATH_TOPOLOGY),
            "moving a handle cannot repair erased endpoint identity"
        );
    }

    #[test]
    fn record_precision_refuses_nonfinite_and_unrepresentable_points() {
        for value in [f64::NAN, f64::INFINITY, f64::MAX, -f64::MAX] {
            let curve = VMobject::from_points(vec![[value, 0.0, 0.0]]);
            assert_eq!(
                admit_constructor(&curve, None, None, None),
                Err(NONFINITE_RECORD)
            );
        }
    }

    #[test]
    fn record_precision_covers_the_final_placement_bake() {
        let exact = VMobject::from_points(vec![[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [3.0, 7.0, 0.0]]);
        assert_eq!(
            admit_constructor(&exact, Some(4.0), None, Some(true)),
            Ok(())
        );
        assert_eq!(
            admit_constructor(&exact, Some(1e6), None, Some(true)),
            Err(CURVE_PRECISION_REFUSED),
            "initial records are exact, but final normalized f32 controls are not"
        );
        let fragile_handle =
            VMobject::from_points(vec![[0.0, 0.0, 0.0], [1e-8, 0.0, 0.0], [1.0, 1.0, 0.0]]);
        assert_eq!(
            admit_constructor(&fragile_handle, Some(4.0), None, Some(true)),
            Err(LOST_PATH_TOPOLOGY),
            "centering followed by f32 bake must not manufacture a path break"
        );
    }

    #[test]
    fn record_precision_covers_the_orientation_bake_before_sizing() {
        let curve = VMobject::from_points(vec![
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [2.0, 33_554_432.0, 0.0],
        ]);
        assert_eq!(
            admit_constructor(&curve, Some(4.0), None, Some(true)),
            Ok(())
        );
        assert_eq!(
            admit_constructor(&curve, Some(65_536.0), None, Some(true)),
            Err(CURVE_PRECISION_REFUSED),
            "exact initial and ideal final records still lose a control during the flip bake"
        );
        let fragile_handle = VMobject::from_points(vec![
            [0.0, 0.0, 0.0],
            [0.0, 1.0 / 64.0, 0.0],
            [2.0, 33_554_432.0, 0.0],
        ]);
        assert_eq!(
            admit_constructor(&fragile_handle, None, None, Some(true)),
            Err(LOST_PATH_TOPOLOGY),
            "an unsized constructor must not turn its real first handle into a subpath break"
        );
    }

    #[test]
    fn record_precision_checks_unsized_centering_without_transforming_raw_parts() {
        let family = VMobject::new().with_children([
            VMobject::from_points(vec![[0.0, 0.0, 0.0], [0.125, 0.0, 0.0], [2.0, 0.0, 0.0]]),
            VMobject::from_points(vec![[33_554_432.0, 0.0, 0.0]]),
        ]);
        assert_eq!(
            admit_constructor(&family, None, None, Some(true)),
            Err(LOST_PATH_TOPOLOGY),
            "an unsized constructor still centers before writable point access"
        );
        assert_eq!(
            admit_constructor(&family, None, None, Some(false)),
            Ok(()),
            "without centering or sizing, the constructor leaves these points unchanged"
        );
        assert_eq!(
            admit_native_precision(&family, &family, None, None, None, false),
            Ok(()),
            "raw factory calls do not run the constructor's placement phases"
        );
        let fragile_flip = VMobject::from_points(vec![
            [0.0, 0.0, 0.0],
            [0.0, 1.0 / 64.0, 0.0],
            [2.0, 33_554_432.0, 0.0],
        ]);
        assert_eq!(
            admit_native_precision(&fragile_flip, &fragile_flip, None, None, None, false),
            Ok(()),
            "a later authored flip is outside an unframed factory's admission"
        );
    }

    #[test]
    fn record_precision_repairs_stationary_cubics_through_real_native_bakes() {
        for data in ["M0 0C0 0 1 1 1 1", "M0 0C0 0 1 1 1 0", "M0 0C0 1 1 1 1 1"] {
            for transform in ["", "translate(1 1)"] {
                for width in [None, Some(9.0)] {
                    let source =
                        format!(r#"<svg><path transform="{transform}" d="{data}"/></svg>"#);
                    let document = svg_document_for_size_with_tolerance(
                        source.as_bytes(),
                        Some(4.0),
                        width,
                        0.5 * DEFAULT_SVG_SCENE_TOLERANCE,
                    )
                    .unwrap();
                    let original =
                        svg_document_with_paints(&document, SvgPaintOverrides::default()).unwrap();
                    let (min, max) = original.extent().unwrap();
                    let expected =
                        ImportNormalization::new(ImportBounds { min, max }, Some(4.0), width)
                            .unwrap();
                    let repaired = prepare_native_precision(
                        original.clone(),
                        Some(4.0),
                        width,
                        Some(true),
                        true,
                    )
                    .unwrap_or_else(|error| panic!("{data} / {transform} / {width:?}: {error}"));
                    let mut stage = fmn_mobject::Stage::new();
                    let root = stage.add(repaired);
                    let leaf = stage
                        .family(root)
                        .into_iter()
                        .find(|&member| {
                            stage
                                .get_points(member)
                                .is_some_and(|points| !points.is_empty())
                        })
                        .unwrap();
                    stage.rotate(root, std::f64::consts::PI, [1.0, 0.0, 0.0], None, None);
                    stage.bake_family_placements(root).unwrap();
                    stage.center(root);
                    stage.set_height(root, 4.0, false);
                    if let Some(width) = width {
                        stage.set_width(root, width, false);
                    }
                    stage.bake_family_placements(root).unwrap();
                    let points = stage.get_points(leaf).unwrap();
                    let path = fmn_library::QuadPath::from_points(points.clone()).unwrap();
                    assert_eq!(
                        path.subpaths().len(),
                        1,
                        "stationary endpoint became a break: {source}"
                    );
                    for curve in points.windows(3).step_by(2) {
                        assert_ne!(curve[1], curve[0]);
                        assert_ne!(curve[1], curve[2]);
                    }
                    for (&control, &actual) in original.children()[0].points().iter().zip(&points) {
                        let ideal = expected.point(control, true);
                        let distance = (ideal[0] - actual[0])
                            .hypot(ideal[1] - actual[1])
                            .hypot(ideal[2] - actual[2]);
                        assert!(
                            distance <= 0.5 * DEFAULT_SVG_SCENE_TOLERANCE,
                            "repair exceeded record budget: {distance}, {source}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn record_precision_refuses_size_clamping_and_collapsed_requested_axes() {
        let ordinary = offset_curve(0.0);
        assert_eq!(
            admit_constructor(&ordinary, Some(1e12), Some(4.0), None),
            Err(MINIMUM_SCALE_REFUSED),
            "width fitting must not silently clamp after an enormous height fit"
        );
        assert_eq!(
            admit_constructor(&ordinary, Some(1e-12), None, None),
            Err(MINIMUM_SCALE_REFUSED)
        );
        let intermediate = VMobject::from_points(vec![
            [5.0, 5.0, 0.0],
            [13.125, 13.125, 0.0],
            [21.25, 21.25, 0.0],
        ]);
        assert_eq!(
            admit_constructor(&intermediate, Some(1e16), Some(1e8), None),
            Err(CURVE_PRECISION_REFUSED),
            "a later reduction cannot erase arithmetic at an enormous intermediate scale"
        );
        let narrow =
            VMobject::from_points(vec![[0.0, 0.0, 0.0], [1e-50, 1.0, 0.0], [2e-50, 2.0, 0.0]]);
        assert_eq!(
            admit_constructor(&narrow, None, Some(4.0), None),
            Err(COLLAPSED_AXIS)
        );
        assert_eq!(
            admit_constructor(&VMobject::new(), Some(4.0), None, None),
            Ok(())
        );
        let horizontal =
            VMobject::from_points(vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]]);
        assert_eq!(
            admit_constructor(&horizontal, Some(4.0), None, None),
            Ok(()),
            "a zero source height keeps the normal positional no-op"
        );
    }

    #[test]
    fn production_svg_subclass_lifecycle() {
        crate::with_python_test_module("SVG subclass lifecycle", |py, _module, globals| {
            let source = std::ffi::CString::new(concat!(
                include_str!("../tests/svg_lifecycle.py"),
                "\n_suite = unittest.defaultTestLoader.loadTestsFromTestCase(SvgLifecycleTests)\n",
                "_result = unittest.TextTestRunner(verbosity=2).run(_suite)\n",
                "assert _result.wasSuccessful(), 'native SVG lifecycle acceptance failed'\n",
            ))
            .unwrap();
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .expect("native SVG subclass lifecycle");
        });
    }
}
