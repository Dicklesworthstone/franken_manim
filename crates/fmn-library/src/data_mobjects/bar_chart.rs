//! Stable chart handles and begin-time value targets over Choreo's Transform.

use std::collections::HashSet;

use fmn_anim::{AnimConfig, AnimError, AnimState, Animation, AnimationSignature, Transform};
use fmn_core::constants::MED_LARGE_BUFF;
use fmn_core::types::Vec3;
use fmn_mobject::{IdBuildHasher, Mob, Stage};
use fmn_text::FontBook;

use super::{BarChart, DataMobjectError};
use crate::{Rectangle, VMobject};

/// Maximum bars and value-axis intervals admitted by the native chart.
pub const MAX_BAR_CHART_BARS: usize = 4096;
// Four straight quadratic edges share anchors, including the closing anchor.
const BAR_POINTS: usize = 9;

fn invalid(reason: &'static str) -> DataMobjectError {
    DataMobjectError::InvalidChart(reason)
}

fn finite_point(point: Vec3) -> bool {
    point
        .iter()
        .all(|value| value.is_finite() && value.abs() <= f64::from(f32::MAX))
}

fn record_point(point: Vec3) -> Vec3 {
    point.map(|value| f64::from(value as f32))
}

fn positive(value: f64, name: &'static str) -> Result<(), DataMobjectError> {
    if !value.is_finite() || value <= 0.0 || value > f64::from(f32::MAX) {
        Err(invalid(name))
    } else {
        Ok(())
    }
}

fn bound_count(count: usize, context: &'static str) -> Result<(), DataMobjectError> {
    if count > MAX_BAR_CHART_BARS {
        Err(DataMobjectError::ChartLimit {
            context,
            requested: count,
            max: MAX_BAR_CHART_BARS,
        })
    } else {
        Ok(())
    }
}

fn animation_error(error: DataMobjectError) -> AnimError {
    match error {
        DataMobjectError::Animation(error) => error,
        error => AnimError::UpdateFailed {
            operation: "BarChart::change_bar_values",
            detail: error.to_string(),
        },
    }
}

#[derive(Clone, Debug)]
struct RectangleTemplate {
    uv: [[f64; 2]; BAR_POINTS],
    left: usize,
    right: usize,
}

impl RectangleTemplate {
    fn new() -> Result<Self, DataMobjectError> {
        let rectangle = Rectangle::new()
            .width(1.0)
            .height(1.0)
            .build()
            .map_err(|error| DataMobjectError::Geometry(error.to_string()))?;
        let points = rectangle.points();
        if points.len() != BAR_POINTS {
            return Err(invalid("native rectangle point topology changed"));
        }
        let left = points
            .iter()
            .position(|point| point[0] == -0.5 && point[1] == -0.5)
            .ok_or_else(|| invalid("native rectangle has no bottom-left anchor"))?;
        let right = points
            .iter()
            .position(|point| point[0] == 0.5 && point[1] == -0.5)
            .ok_or_else(|| invalid("native rectangle has no bottom-right anchor"))?;
        Ok(Self {
            uv: std::array::from_fn(|index| [points[index][0] + 0.5, points[index][1] + 0.5]),
            left,
            right,
        })
    }
}

impl BarChart {
    pub(super) fn validated_scale(&self) -> Result<f64, DataMobjectError> {
        if self.values.is_empty() {
            return Err(DataMobjectError::NoValues);
        }
        bound_count(self.values.len(), "bars")?;
        bound_count(self.n_ticks, "value-axis intervals")?;
        positive(self.height, "height must be finite and positive")?;
        positive(self.width, "width must be finite and positive")?;
        if self.values.iter().any(|value| !value.is_finite()) {
            return Err(invalid("bar values must be finite"));
        }
        let scale = self.max_value.unwrap_or_else(|| {
            let magnitude = self
                .values
                .iter()
                .map(|value| value.abs())
                .fold(0.0, f64::max);
            if magnitude == 0.0 { 1.0 } else { magnitude }
        });
        // Scale is a semantic value, not a coordinate; a large finite scale
        // remains useful when its ratios produce representable geometry.
        if !scale.is_finite() || scale <= 0.0 {
            return Err(invalid("max_value must be finite and positive"));
        }
        if self.values.iter().any(|value| {
            let height = (value / scale) * self.height;
            !height.is_finite() || height.abs() > f64::from(f32::MAX)
        }) {
            return Err(invalid("bar heights must fit finite f32 coordinates"));
        }
        Ok(scale)
    }

    pub(super) fn bars_index(&self) -> usize {
        2 + self.n_ticks.max(1)
            + 1
            + if self.include_x_ticks {
                self.values.len() + 1
            } else {
                0
            }
            + if self.label_y_axis {
                self.n_ticks.max(1)
            } else {
                0
            }
    }

    /// Install a chart and retain its semantic component handles. Add the
    /// returned [`BarChartBinding::mobject`] to the scene separately.
    ///
    /// All values, layout bounds and record-space geometry are validated before
    /// the caller's Stage is changed. Initial positive charts preserve the
    /// detached constructor's established geometry exactly.
    ///
    /// # Errors
    /// Invalid controls, excessive bar/tick counts, Scribe errors, or geometry
    /// that loses its chart basis when stored in the authoritative f32 records.
    pub fn add_to(
        &self,
        stage: &mut Stage,
        book: &FontBook,
    ) -> Result<BarChartBinding, DataMobjectError> {
        let scale = self.validated_scale()?;
        let built = self.build(book)?;
        let template = RectangleTemplate::new()?;
        let index = self.bars_index();
        // build() preflights the exact f32 coordinates and component topology.
        let root = stage.add(built);
        let children = stage.get(root).expect("just installed chart").submobjects();
        let y_axis = children[1];
        let bars = stage
            .get(children[index])
            .expect("just installed bars")
            .submobjects()
            .to_vec();
        Ok(BarChartBinding {
            root,
            y_axis,
            bars,
            scale,
            height_fraction: self.height / (self.height + MED_LARGE_BUFF),
            template,
            scratch: Vec::with_capacity(BAR_POINTS * 3),
            planned: Vec::new(),
        })
    }
}

/// Validate after the chart's final centering, in the f32 coordinate system
/// Stage::add will actually store. This keeps installation failure atomic.
pub(super) fn validate_detached_chart(
    chart: &VMobject,
    bars_index: usize,
) -> Result<(), DataMobjectError> {
    let template = RectangleTemplate::new()?;
    let children = chart.children();
    let axis = children
        .get(1)
        .ok_or_else(|| invalid("missing chart y-axis"))?;
    let bars = children
        .get(bars_index)
        .ok_or_else(|| invalid("missing chart bars"))?;
    let start = axis
        .points()
        .first()
        .copied()
        .ok_or_else(|| invalid("empty chart y-axis"))?;
    let end = axis
        .points()
        .last()
        .copied()
        .ok_or_else(|| invalid("empty chart y-axis"))?;
    let delta = std::array::from_fn(|i| record_point(end)[i] - record_point(start)[i]);
    for bar in bars.children() {
        if bar.points().len() != BAR_POINTS {
            return Err(invalid("bar rectangle topology changed"));
        }
        let left = record_point(bar.points()[template.left]);
        let right = record_point(bar.points()[template.right]);
        validate_basis(left, right, delta)?;
    }
    let mut pending = vec![chart];
    while let Some(member) = pending.pop() {
        if member.points().iter().any(|point| !finite_point(*point)) {
            return Err(invalid("chart coordinates must fit finite f32 records"));
        }
        pending.extend(member.children());
    }
    Ok(())
}

/// Stable bar identities and a live chart basis.
///
/// Updates use the y-axis's current start/end vector and each bar's current
/// bottom edge. Rotation, reflection, shear, nonuniform scale and independent
/// bar width/position edits therefore remain meaningful, including after the
/// bar passes through zero height. Labels and child annotations retain their
/// authored geometry. Styles, updater bindings, scene membership and fixed-size
/// live record views survive updates.
///
/// Clones refer to the same arena handles. Use [`Self::from_handles`] to adopt
/// a copied or externally assembled chart after its handles have been rebound.
#[derive(Debug)]
pub struct BarChartBinding {
    root: Mob,
    y_axis: Mob,
    bars: Vec<Mob>,
    scale: f64,
    height_fraction: f64,
    template: RectangleTemplate,
    scratch: Vec<f32>,
    planned: Vec<[Vec3; BAR_POINTS]>,
}

impl Clone for BarChartBinding {
    fn clone(&self) -> Self {
        Self {
            root: self.root,
            y_axis: self.y_axis,
            bars: self.bars.clone(),
            scale: self.scale,
            height_fraction: self.height_fraction,
            template: self.template.clone(),
            scratch: Vec::with_capacity(BAR_POINTS * 3),
            planned: Vec::new(),
        }
    }
}

impl BarChartBinding {
    /// Adopt explicit chart components through the same checks as native value
    /// updates. This is also the binding seam for the optional Python portal.
    /// `height_fraction` excludes the lower padding from the y-axis extent;
    /// native charts use `height / (height + MED_LARGE_BUFF)`.
    ///
    /// # Errors
    /// Invalid scale/fraction, excessive or aliased components, stale or foreign
    /// handles, unreachable bars/axis, or unsupported rectangle records.
    pub fn from_handles(
        stage: &Stage,
        root: Mob,
        y_axis: Mob,
        bars: &[Mob],
        max_value: f64,
        height_fraction: f64,
    ) -> Result<Self, DataMobjectError> {
        bound_count(bars.len(), "bars")?;
        if !max_value.is_finite() || max_value <= 0.0 {
            return Err(invalid("max_value must be finite and positive"));
        }
        if !height_fraction.is_finite() || height_fraction <= 0.0 || height_fraction > 1.0 {
            return Err(invalid("height_fraction must be finite and in (0, 1]"));
        }
        let binding = Self {
            root,
            y_axis,
            bars: bars.to_vec(),
            scale: max_value,
            height_fraction,
            template: RectangleTemplate::new()?,
            scratch: Vec::with_capacity(BAR_POINTS * 3),
            planned: Vec::new(),
        };
        binding.validate(stage)?;
        binding.height_vector(stage)?;
        Ok(binding)
    }

    /// Installed family root.
    #[must_use]
    pub fn mobject(&self) -> Mob {
        self.root
    }

    /// Live y-axis whose direction and extent define value heights.
    #[must_use]
    pub fn y_axis(&self) -> Mob {
        self.y_axis
    }

    /// Bars in value-series order.
    #[must_use]
    pub fn bars(&self) -> &[Mob] {
        &self.bars
    }

    /// One stable bar handle, if its index exists.
    #[must_use]
    pub fn bar(&self, index: usize) -> Option<Mob> {
        self.bars.get(index).copied()
    }

    /// Positive value corresponding to the chart's full live height.
    #[must_use]
    pub fn max_value(&self) -> f64 {
        self.scale
    }

    fn validate(&self, stage: &Stage) -> Result<(), DataMobjectError> {
        if !stage.contains(self.root) {
            return Err(AnimError::StaleHandle(self.root).into());
        }
        let family: HashSet<_, IdBuildHasher> = stage.family(self.root).into_iter().collect();
        let mut seen = HashSet::<Mob, IdBuildHasher>::default();
        seen.insert(self.root);
        for &mob in std::iter::once(&self.y_axis).chain(&self.bars) {
            let entry = stage.get(mob).ok_or(AnimError::StaleHandle(mob))?;
            if !seen.insert(mob) || !family.contains(&mob) {
                return Err(invalid(
                    "chart components must be distinct members of its family",
                ));
            }
            if entry.buffer.schema().pointlike_keys() != ["point"]
                || entry.buffer.schema().field_width("point") != Some(3)
            {
                return Err(invalid(
                    "chart components require only the three-lane point field",
                ));
            }
            if mob != self.y_axis
                && (entry.buffer.len() != BAR_POINTS
                    || entry.buffer.schema().field_width("joint_angle") != Some(1))
            {
                return Err(invalid(
                    "bar updates require the original rectangular point topology",
                ));
            }
        }
        Ok(())
    }

    fn height_vector(&self, stage: &Stage) -> Result<Vec3, DataMobjectError> {
        let entry = stage
            .get(self.y_axis)
            .ok_or(AnimError::StaleHandle(self.y_axis))?;
        if entry.buffer.len() < 2 {
            return Err(invalid("chart y-axis must have distinct endpoints"));
        }
        let start = entry
            .buffer
            .read(0, "point")
            .ok_or_else(|| invalid("missing y-axis points"))?;
        let end = entry
            .buffer
            .read(entry.buffer.len() - 1, "point")
            .ok_or_else(|| invalid("missing y-axis points"))?;
        let placement = entry.placement();
        let start = placement.apply_point(std::array::from_fn(|i| f64::from(start[i])));
        let end = placement.apply_point(std::array::from_fn(|i| f64::from(end[i])));
        let height = std::array::from_fn(|i| (end[i] - start[i]) * self.height_fraction);
        if !finite_point(start)
            || !finite_point(end)
            || !finite_point(height)
            || height.iter().all(|coordinate| *coordinate == 0.0)
        {
            return Err(invalid("chart y-axis must define a finite nonzero height"));
        }
        Ok(height)
    }

    fn plan_values(&mut self, stage: &Stage, values: &[f64]) -> Result<(), DataMobjectError> {
        self.validate(stage)?;
        if values.len() > self.bars.len() {
            return Err(DataMobjectError::BarValueCountMismatch {
                bars: self.bars.len(),
                values: values.len(),
            });
        }
        if values.iter().any(|value| !value.is_finite()) {
            return Err(invalid("bar values must be finite"));
        }
        let height = self.height_vector(stage)?;
        self.planned.clear();
        self.planned
            .try_reserve(values.len())
            .map_err(|_| invalid("bar target allocation failed"))?;
        for (&mob, &value) in self.bars.iter().zip(values) {
            let entry = stage.get(mob).ok_or(AnimError::StaleHandle(mob))?;
            if !entry.buffer.read_column_into("point", &mut self.scratch) {
                return Err(invalid("missing bar points"));
            }
            let placement = entry.placement();
            let points: [Vec3; BAR_POINTS] = std::array::from_fn(|index| {
                placement.apply_point(std::array::from_fn(|axis| {
                    f64::from(self.scratch[index * 3 + axis])
                }))
            });
            if points.iter().any(|point| !finite_point(*point)) {
                return Err(invalid("bar points must fit finite f32 coordinates"));
            }
            let left = points[self.template.left];
            let right = points[self.template.right];
            validate_basis(left, right, height)?;
            let target_height: Vec3 = height.map(|coordinate| coordinate * (value / self.scale));
            // Keep the established portal association: L + u*(R-L) + v*(H*r).
            let target = std::array::from_fn(|index| {
                std::array::from_fn(|axis| {
                    left[axis]
                        + self.template.uv[index][0] * (right[axis] - left[axis])
                        + self.template.uv[index][1] * target_height[axis]
                })
            });
            if target.iter().any(|point| !finite_point(*point)) {
                return Err(invalid(
                    "bar height must produce finite f32-representable geometry",
                ));
            }
            // A lazy f64 placement may distinguish baseline anchors that
            // collapse together when written to the authoritative f32 records.
            // Refuse before publishing any bar, preserving the basis needed by
            // subsequent zero-to-nonzero updates.
            validate_basis(
                record_point(target[self.template.left]),
                record_point(target[self.template.right]),
                height,
            )?;
            self.planned.push(target);
        }
        Ok(())
    }

    /// Update a prefix of bars. An empty prefix changes no records.
    ///
    /// All selected targets are checked before the first live write. A bar's
    /// existing height is never divided by, so zero and signed transitions are
    /// ordinary cases. The scale and axis are still validated for an empty
    /// update. Fixed point cardinality preserves active record views.
    ///
    /// # Errors
    /// A stale/restructured binding, invalid series, unsupported records,
    /// collapsed chart basis, or nonrepresentable target coordinate. Failures
    /// leave all live chart records unchanged.
    pub fn change_bar_values(
        &mut self,
        stage: &mut Stage,
        values: &[f64],
    ) -> Result<(), DataMobjectError> {
        self.plan_values(stage, values)?;
        for (&mob, target) in self.bars.iter().zip(&self.planned) {
            // Preflight proved live handles, fixed representable record sizes,
            // and the native rectangle's shared-anchor topology. No callback
            // runs between those checks and these ordinary fixed-size writes.
            stage.set_points(mob, target)?;
        }
        Ok(())
    }
}

fn validate_basis(left: Vec3, right: Vec3, height: Vec3) -> Result<(), DataMobjectError> {
    let width: Vec3 = std::array::from_fn(|i| right[i] - left[i]);
    let width_scale = width.iter().copied().map(f64::abs).fold(0.0, f64::max);
    let height_scale = height.iter().copied().map(f64::abs).fold(0.0, f64::max);
    if !finite_point(left)
        || !finite_point(right)
        || !finite_point(height)
        || width_scale == 0.0
        || height_scale == 0.0
    {
        return Err(invalid(
            "chart width and height must define a nondegenerate basis",
        ));
    }
    let a = width.map(|value| value / width_scale);
    let b = height.map(|value| value / height_scale);
    let cross = [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ];
    if cross.iter().all(|coordinate| *coordinate == 0.0) {
        return Err(invalid("chart width and height must be independent"));
    }
    Ok(())
}

/// Animate a prefix of bar values through the existing closed Transform law.
///
/// Target geometry is built at begin from the live chart, including preceding
/// succession members. No chart callback executes during interpolation or on a
/// frame worker. Ordinary Transform timing, data locks, updater suspension and
/// cleanup remain authoritative; source bar handles and annotations survive.
#[derive(Debug)]
pub struct ChangeBarValues {
    binding: BarChartBinding,
    values: Vec<f64>,
    inner: Transform,
}

impl ChangeBarValues {
    /// Validate a value animation without allocating or mutating any Stage entry.
    ///
    /// # Errors
    /// The same input and binding errors as [`BarChartBinding::change_bar_values`].
    /// Inputs are checked again at begin, since the chart may have changed.
    pub fn new(
        stage: &Stage,
        chart: &BarChartBinding,
        values: &[f64],
    ) -> Result<Self, DataMobjectError> {
        let mut binding = chart.clone();
        binding.plan_values(stage, values)?;
        Ok(Self {
            binding,
            values: values.to_vec(),
            inner: Transform::new(chart.root, chart.root).with_config(AnimConfig {
                name: "ChangeBarValues".into(),
                ..AnimConfig::default()
            }),
        })
    }

    /// Set the ordinary animation controls and rate function.
    #[must_use]
    pub fn with_config(mut self, config: AnimConfig) -> Self {
        self.inner = self.inner.with_config(config);
        self
    }
}

impl Animation for ChangeBarValues {
    fn state(&self) -> &AnimState {
        self.inner.state()
    }
    fn state_mut(&mut self) -> &mut AnimState {
        self.inner.state_mut()
    }

    fn validate_begin(&self, stage: &Stage) -> Result<(), AnimError> {
        let mut binding = self.binding.clone();
        binding
            .plan_values(stage, &self.values)
            .map_err(animation_error)?;
        Transform::new(self.binding.root, self.binding.root)
            .with_config(self.state().config.clone())
            .validate_begin(stage)
    }

    fn setup(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        Transform::new(self.binding.root, self.binding.root)
            .with_config(self.state().config.clone())
            .validate_begin(stage)?;
        self.binding
            .plan_values(stage, &self.values)
            .map_err(animation_error)?;
        let copied = stage.copy_family_mapped(self.binding.root)?;
        let target = copied.root();
        let mut replacement =
            Transform::new(self.binding.root, target).with_config(self.state().config.clone());
        replacement.state_mut().copy_timing_from(self.state());
        let result = (|| {
            for (&source, points) in self.binding.bars.iter().zip(&self.binding.planned) {
                let copy = copied.get(source).ok_or(AnimError::StaleHandle(source))?;
                stage.set_points(copy, points)?;
            }
            replacement.setup(stage)
        })();
        // Transform may make another endpoint copy to preserve an authored
        // target during geometry baking. This prototype is private, so retire
        // it when it is not the actual interpolation endpoint. Keep ordinary
        // starting/target copies alive for Succession's initialized replay.
        if result.is_err() || replacement.target_copy() != Some(target) {
            for &(_, member) in copied.pairs().iter().rev() {
                if stage.contains(member) {
                    let _ = stage.delete(member);
                }
            }
        }
        result?;
        self.inner = replacement;
        Ok(())
    }

    fn preflight_mobjects(&self) -> Vec<Mob> {
        vec![self.binding.root]
    }
    fn create_starting_mobject(&self, stage: &mut Stage) -> Result<Mob, AnimError> {
        self.inner.create_starting_mobject(stage)
    }
    fn after_begin(&mut self, stage: &mut Stage) {
        self.inner.after_begin(stage);
    }
    fn teardown(&mut self, stage: &mut Stage) {
        self.inner.teardown(stage);
    }
    fn clean_up_from_scene(&mut self, stage: &mut Stage) {
        self.inner.clean_up_from_scene(stage);
    }
    fn all_mobjects(&self) -> Vec<Mob> {
        self.inner.all_mobjects()
    }
    fn interpolate_submobject(&mut self, stage: &mut Stage, mobs: &[Mob], alpha: f64) {
        self.inner.interpolate_submobject(stage, mobs, alpha);
    }
    fn effect_signature(&self) -> AnimationSignature {
        self.inner.effect_signature()
    }
    fn freeze_interpolation(&self) -> Option<fmn_anim::pure_frame::FrozenAnimation> {
        self.inner.freeze_interpolation()
    }
}
