//! Durable continuations for prepared, closed native plays.
//!
//! A continuation owns data describing interpolation AND the remaining native
//! lifecycle. Interpolation purity alone is insufficient: a user wrapper may
//! delegate interpolation while adding an effectful finish hook. Actual dynamic
//! types are checked through Any; callbacks, groups and unknown rates fall back.
//! Stage contents live in the enclosing SceneState, whose identity the caller
//! must bind to this document. Handles rebind only after that state is restored.

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use fmn_hash::{Limits, Reader, Schema, SerialError, UnknownPolicy, Writer};
use fmn_mobject::{Mob, Stage};

use crate::creation::PartialGeometry;
use crate::transform::TargetPrep;
use crate::{
    AnimConfig, AnimError, AnimState, Animation, OpenSegment, PathFunc, RateFunc,
    RationalFrameClock, RevealBounds, Rotating, ShowPartial, Transform, VFade,
};

const SCHEMA: Schema = Schema::new(*b"FMPC", 1, 1, 0);
const DECLARATION_SCHEMA: Schema = Schema::new(*b"FMPC", 2, 1, 0);
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_ITEMS: usize = 1_000_000;
const MAX_RATE_DEPTH: usize = 32;

/// A malformed, resource-bounded or semantically invalid native continuation.
#[derive(Debug)]
pub enum Error {
    /// Versioned document framing or allocation failure.
    Serial(SerialError),
    /// Existing animation or rational-clock validation failure.
    Animation(AnimError),
    /// The document is inconsistent with its prepared native state.
    Invalid(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serial(error) => write!(f, "play continuation: {error}"),
            Self::Animation(error) => write!(f, "play continuation: {error}"),
            Self::Invalid(reason) => write!(f, "invalid play continuation: {reason}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Serial(error) => Some(error),
            Self::Animation(error) => Some(error),
            Self::Invalid(_) => None,
        }
    }
}

impl From<SerialError> for Error {
    fn from(value: SerialError) -> Self {
        Self::Serial(value)
    }
}

impl From<AnimError> for Error {
    fn from(value: AnimError) -> Self {
        Self::Animation(value)
    }
}

/// Timing overrides read without executing a factory or animation hook.
#[derive(Default)]
pub struct DeclarationOverrides<'a> {
    /// Optional second-based duration.
    pub run_time: Option<f64>,
    /// Optional exact frame duration.
    pub run_time_frames: Option<u64>,
    /// Optional closed rate function.
    pub rate_func: Option<&'a RateFunc>,
    /// Optional family lag.
    pub lag_ratio: Option<f64>,
}

fn limits(max_bytes: usize) -> Limits {
    let max_bytes = max_bytes.min(MAX_BYTES);
    Limits {
        max_total: max_bytes,
        max_field: max_bytes,
    }
}

fn native_kind(animation: &dyn Animation) -> Option<u8> {
    let any: &dyn Any = animation;
    if any.is::<Transform>() {
        Some(1)
    } else if any.is::<Rotating>() {
        Some(2)
    } else if any.is::<VFade>() {
        Some(3)
    } else if any.is::<ShowPartial>() {
        Some(4)
    } else {
        None
    }
}

fn clone_native(animation: &dyn Animation) -> Option<Box<dyn Animation>> {
    let any: &dyn Any = animation;
    if let Some(value) = any.downcast_ref::<Transform>() {
        Some(Box::new(value.clone()))
    } else if let Some(value) = any.downcast_ref::<Rotating>() {
        Some(Box::new(value.clone()))
    } else if let Some(value) = any.downcast_ref::<VFade>() {
        Some(Box::new(value.clone()))
    } else if let Some(value) = any.downcast_ref::<ShowPartial>() {
        Some(Box::new(value.clone()))
    } else {
        None
    }
}

fn rate_supported(rate: &RateFunc, depth: usize) -> bool {
    if depth > MAX_RATE_DEPTH {
        return false;
    }
    match rate {
        RateFunc::Catalog(_) => true,
        RateFunc::Base(function) => fmn_core::rate::tag_of(*function).is_some(),
        RateFunc::Sampled(values) => {
            values.len() <= MAX_ITEMS && values.iter().all(|v| v.is_finite())
        }
        RateFunc::Squish { inner, a, b } => {
            a.is_finite() && b.is_finite() && rate_supported(inner, depth + 1)
        }
        RateFunc::Scaled { inner, proportion } => {
            proportion.is_finite() && rate_supported(inner, depth + 1)
        }
        RateFunc::Preparing(_) | RateFunc::Exact(_) => false,
    }
}

fn config_supported(config: &AnimConfig) -> bool {
    config.run_time.is_finite()
        && config.lag_ratio.is_finite()
        && config.final_alpha_value.is_finite()
        && config
            .time_span
            .is_none_or(|(start, end)| start.is_finite() && end.is_finite())
        && rate_supported(&config.rate_func, 0)
}

fn native_parameters_valid(stage: &Stage, animation: &dyn Animation) -> bool {
    let any: &dyn Any = animation;
    if let Some(value) = any.downcast_ref::<Transform>() {
        value.path.validate().is_ok()
    } else if let Some(value) = any.downcast_ref::<Rotating>() {
        value.angle.is_finite()
            && value.axis.iter().all(|value| value.is_finite())
            && value
                .about_point
                .is_none_or(|point| point.iter().all(|value| value.is_finite()))
            && value
                .about_edge
                .is_none_or(|edge| edge.iter().all(|value| value.is_finite()))
    } else if let Some(value) = any.downcast_ref::<VFade>() {
        value
            .validate_geometry(stage, value.state.mobject())
            .is_ok()
            && value
                .state
                .starting_mobject()
                .is_some_and(|starting| value.validate_geometry(stage, starting).is_ok())
    } else if let Some(value) = any.downcast_ref::<ShowPartial>() {
        let finite_bounds = match value.bounds {
            RevealBounds::Creation => true,
            RevealBounds::PassingFlash { time_width } => time_width.is_finite(),
        };
        finite_bounds
            && value
                .validate_geometry(stage, value.state.mobject())
                .is_ok()
            && value
                .state
                .starting_mobject()
                .is_some_and(|starting| value.validate_geometry(stage, starting).is_ok())
    } else {
        false
    }
}

// Execution parameters retain their binary64 identity, including signed zero.
// They are bit fields rather than canonicalized geometric values.
fn put_float(writer: &mut Writer, value: f64) {
    writer.put_u64(value.to_bits());
}
fn get_float(reader: &mut Reader<'_>) -> Result<f64, Error> {
    let value = f64::from_bits(reader.get_u64()?);
    if !value.is_finite() {
        return Err(Error::Invalid("non-finite native parameter"));
    }
    Ok(value)
}
fn put_vec(writer: &mut Writer, value: [f64; 3]) {
    for component in value {
        put_float(writer, component);
    }
}
fn get_vec(reader: &mut Reader<'_>) -> Result<[f64; 3], Error> {
    Ok([get_float(reader)?, get_float(reader)?, get_float(reader)?])
}
fn put_optional_vec(writer: &mut Writer, value: Option<[f64; 3]>) {
    writer.put_bool(value.is_some());
    if let Some(value) = value {
        put_vec(writer, value);
    }
}
fn get_optional_vec(reader: &mut Reader<'_>) -> Result<Option<[f64; 3]>, Error> {
    if reader.get_bool()? {
        Ok(Some(get_vec(reader)?))
    } else {
        Ok(None)
    }
}
fn put_count(writer: &mut Writer, count: usize) -> Result<(), Error> {
    if count > MAX_ITEMS {
        return Err(Error::Invalid("native collection exceeds its bound"));
    }
    writer.put_u32(u32::try_from(count).map_err(|_| Error::Invalid("native count overflow"))?);
    Ok(())
}

struct Budget {
    remaining: usize,
}
impl Budget {
    fn charge<T>(&mut self, count: usize) -> Result<(), Error> {
        let bytes = count
            .checked_mul(std::mem::size_of::<T>())
            .ok_or(Error::Invalid("allocation size overflow"))?;
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or(Error::Invalid("decoded native allocation budget exceeded"))?;
        Ok(())
    }
    fn vector<T>(&mut self, count: usize) -> Result<Vec<T>, Error> {
        self.charge::<T>(count)?;
        let mut out = Vec::new();
        out.try_reserve_exact(count)
            .map_err(|_| SerialError::AllocationFailed {
                needed: count.saturating_mul(std::mem::size_of::<T>()),
            })?;
        Ok(out)
    }
}

fn charge_carrier(budget: &mut Budget, kind: u8) -> Result<(), Error> {
    match kind {
        1 => budget.charge::<Transform>(1),
        2 => budget.charge::<Rotating>(1),
        3 => budget.charge::<VFade>(1),
        4 => budget.charge::<ShowPartial>(1),
        _ => Err(Error::Invalid("unknown native animation kind")),
    }
}

fn charge_rate(budget: &mut Budget, rate: &RateFunc) -> Result<(), Error> {
    budget.charge::<RateFunc>(1)?;
    match rate {
        RateFunc::Sampled(values) => {
            budget.charge::<f64>(values.len())?;
            budget.charge::<f64>(values.len())?;
        }
        RateFunc::Squish { inner, .. } | RateFunc::Scaled { inner, .. } => {
            charge_rate(budget, inner)?;
        }
        _ => {}
    }
    Ok(())
}

// Admission uses the same allocation charges as decode, including temporary
// Vec-to-Arc copies. A checkpoint must never exceed its own restore budget.
fn capture_budget(animations: &[Box<dyn Animation>]) -> Result<(), Error> {
    let mut budget = Budget {
        remaining: MAX_BYTES,
    };
    budget.charge::<Box<dyn Animation>>(animations.len())?;
    for animation in animations {
        let kind = native_kind(animation.as_ref())
            .ok_or(Error::Invalid("unsupported native animation"))?;
        charge_carrier(&mut budget, kind)?;
        let state = animation.state();
        charge_rate(&mut budget, &state.config.rate_func)?;
        budget.charge::<u8>(state.config.name.len())?;
        budget.charge::<Vec<Mob>>(state.families().len())?;
        budget.charge::<Vec<Mob>>(state.families().len())?;
        for row in state.families() {
            budget.charge::<Mob>(row.len())?;
        }
    }
    Ok(())
}

fn get_count(reader: &mut Reader<'_>, minimum_bytes: usize) -> Result<usize, Error> {
    let count =
        usize::try_from(reader.get_u32()?).map_err(|_| Error::Invalid("native count overflow"))?;
    if count > MAX_ITEMS || count > reader.remaining() / minimum_bytes.max(1) {
        return Err(Error::Invalid(
            "native count exceeds the remaining document",
        ));
    }
    Ok(count)
}

fn put_mob(writer: &mut Writer, stage: &Stage, mob: Mob) -> Result<(), Error> {
    let (slot, generation) = stage
        .snapshot_handle(mob)
        .ok_or(Error::Invalid("dead or foreign prepared handle"))?;
    writer.put_u32(slot).put_u32(generation);
    Ok(())
}
fn get_mob(reader: &mut Reader<'_>, stage: &Stage) -> Result<Mob, Error> {
    stage
        .resolve_snapshot_handle(reader.get_u32()?, reader.get_u32()?)
        .ok_or(Error::Invalid(
            "prepared handle is absent from the restored snapshot",
        ))
}

fn put_rate(writer: &mut Writer, rate: &RateFunc) -> Result<(), Error> {
    match rate {
        RateFunc::Catalog(value) => {
            writer.put_u8(0).put_u8(value.tag());
        }
        RateFunc::Base(function) => {
            let tag = fmn_core::rate::tag_of(*function)
                .ok_or(Error::Invalid("rate has no durable native identity"))?;
            writer.put_u8(0).put_u8(tag);
        }
        RateFunc::Sampled(values) => {
            writer.put_u8(1);
            put_count(writer, values.len())?;
            for &value in values.iter() {
                put_float(writer, value);
            }
        }
        RateFunc::Squish { inner, a, b } => {
            writer.put_u8(2);
            put_float(writer, *a);
            put_float(writer, *b);
            put_rate(writer, inner)?;
        }
        RateFunc::Scaled { inner, proportion } => {
            writer.put_u8(3);
            put_float(writer, *proportion);
            put_rate(writer, inner)?;
        }
        RateFunc::Preparing(_) | RateFunc::Exact(_) => {
            return Err(Error::Invalid("rate has no supported durable carrier"));
        }
    }
    Ok(())
}

fn get_rate(reader: &mut Reader<'_>, budget: &mut Budget, depth: usize) -> Result<RateFunc, Error> {
    if depth > MAX_RATE_DEPTH {
        return Err(Error::Invalid("rate nesting exceeds its bound"));
    }
    budget.charge::<RateFunc>(1)?;
    Ok(match reader.get_u8()? {
        0 => {
            crate::rate_from_tag(reader.get_u8()?).ok_or(Error::Invalid("unknown catalog rate"))?
        }
        1 => {
            let count = get_count(reader, 8)?;
            let mut values = budget.vector::<f64>(count)?;
            budget.charge::<f64>(count)?; // Vec-to-Arc allocation may briefly coexist.
            for _ in 0..count {
                values.push(get_float(reader)?);
            }
            RateFunc::Sampled(Arc::from(values))
        }
        2 => {
            let a = get_float(reader)?;
            let b = get_float(reader)?;
            RateFunc::Squish {
                inner: Box::new(get_rate(reader, budget, depth + 1)?),
                a,
                b,
            }
        }
        3 => {
            let proportion = get_float(reader)?;
            RateFunc::Scaled {
                inner: Box::new(get_rate(reader, budget, depth + 1)?),
                proportion,
            }
        }
        _ => return Err(Error::Invalid("unknown native rate kind")),
    })
}

fn put_config(writer: &mut Writer, config: &AnimConfig) -> Result<(), Error> {
    put_float(writer, config.run_time);
    writer.put_bool(config.run_time_frames.is_some());
    if let Some(frames) = config.run_time_frames {
        writer.put_u64(frames);
    }
    writer.put_bool(config.time_span.is_some());
    if let Some((start, end)) = config.time_span {
        put_float(writer, start);
        put_float(writer, end);
    }
    put_float(writer, config.lag_ratio);
    put_rate(writer, &config.rate_func)?;
    writer.put_str(&config.name).put_bool(config.remover);
    put_float(writer, config.final_alpha_value);
    writer.put_bool(config.suspend_mobject_updating);
    Ok(())
}

fn get_config(reader: &mut Reader<'_>, budget: &mut Budget) -> Result<AnimConfig, Error> {
    let run_time = get_float(reader)?;
    let run_time_frames = if reader.get_bool()? {
        Some(reader.get_u64()?)
    } else {
        None
    };
    let time_span = if reader.get_bool()? {
        Some((get_float(reader)?, get_float(reader)?))
    } else {
        None
    };
    let lag_ratio = get_float(reader)?;
    let rate_func = get_rate(reader, budget, 0)?;
    let raw_name = reader.get_str()?;
    budget.charge::<u8>(raw_name.len())?;
    let mut name = String::new();
    name.try_reserve_exact(raw_name.len())
        .map_err(|_| SerialError::AllocationFailed {
            needed: raw_name.len(),
        })?;
    name.push_str(raw_name);
    let remover = reader.get_bool()?;
    let final_alpha_value = get_float(reader)?;
    let suspend_mobject_updating = reader.get_bool()?;
    Ok(AnimConfig {
        run_time,
        run_time_frames,
        time_span,
        lag_ratio,
        rate_func,
        name,
        remover,
        final_alpha_value,
        suspend_mobject_updating,
    })
}

fn put_state(
    writer: &mut Writer,
    stage: &Stage,
    state: &AnimState,
    prepared: bool,
) -> Result<(), Error> {
    put_mob(writer, stage, state.mobject())?;
    put_config(writer, &state.config)?;
    if prepared {
        put_mob(
            writer,
            stage,
            state
                .starting_mobject()
                .ok_or(Error::Invalid("native animation has not begun"))?,
        )?;
        writer.put_bool(state.mobject_was_updating());
        put_count(writer, state.families().len())?;
        for row in state.families() {
            for &mob in row {
                put_mob(writer, stage, mob)?;
            }
        }
    }
    Ok(())
}

fn get_state(
    reader: &mut Reader<'_>,
    stage: &Stage,
    budget: &mut Budget,
    fps: u32,
    width: usize,
) -> Result<AnimState, Error> {
    let mob = get_mob(reader, stage)?;
    let config = get_config(reader, budget)?;
    let starting = get_mob(reader, stage)?;
    let was_updating = reader.get_bool()?;
    let count = get_count(reader, width * 8)?;
    let mut families = budget.vector::<Vec<Mob>>(count)?;
    budget.charge::<Vec<Mob>>(count)?;
    for _ in 0..count {
        let mut row = budget.vector::<Mob>(width)?;
        for _ in 0..width {
            row.push(get_mob(reader, stage)?);
        }
        families.push(row);
    }
    let duration = config.duration_on_grid(fps)?;
    let mut state = AnimState::new(mob, config);
    state.starting_mobject = Some(starting);
    state.families = Arc::from(families);
    state.mobject_was_updating = was_updating;
    // Reconstruct the exact authored duration; do not round through seconds.
    state.frame_units = duration.is_frame_native().then(|| duration.frame_units());
    state.prepared_duration = Some(duration);
    Ok(state)
}

fn put_path(writer: &mut Writer, path: PathFunc) {
    match path {
        PathFunc::Straight => {
            writer.put_u8(0);
        }
        PathFunc::Arc { angle, axis } => {
            writer.put_u8(1);
            put_float(writer, angle);
            put_vec(writer, axis);
        }
    }
}
fn get_path(reader: &mut Reader<'_>) -> Result<PathFunc, Error> {
    Ok(match reader.get_u8()? {
        0 => PathFunc::Straight,
        1 => PathFunc::Arc {
            angle: get_float(reader)?,
            axis: get_vec(reader)?,
        },
        _ => return Err(Error::Invalid("unknown native path")),
    })
}

// Preparation has already run for a continuation. Its recipe is needed only
// for the fresh factory's declaration identity, never for resumed execution.
fn put_preparations(writer: &mut Writer, transform: &Transform) {
    writer.put_bool(transform.start_prep.is_some());
    if let Some(prep) = &transform.start_prep {
        writer
            .put_bool(prep.opacity_zero)
            .put_bool(prep.scale.is_some());
        if let Some(scale) = prep.scale {
            put_float(writer, scale);
        }
        put_optional_vec(writer, prep.shift);
        put_optional_vec(writer, prep.move_to);
        writer.put_bool(prep.color.is_some());
        if let Some(color) = prep.color {
            for value in color {
                writer.put_u32(value.to_bits());
            }
        }
    }
    match &transform.target_prep {
        None => {
            writer.put_u8(0);
        }
        Some(TargetPrep::Indicate { scale, color }) => {
            writer.put_u8(1);
            put_float(writer, *scale);
            for value in color {
                writer.put_u32(value.to_bits());
            }
        }
        Some(TargetPrep::ReversePoints) => {
            writer.put_u8(2);
        }
        Some(TargetPrep::FadeOut { shift, scale }) => {
            writer.put_u8(3);
            put_vec(writer, *shift);
            put_float(writer, *scale);
        }
        Some(TargetPrep::Color(color)) => {
            writer.put_u8(4);
            for value in color {
                writer.put_u32(value.to_bits());
            }
        }
        Some(TargetPrep::Scale(scale)) => {
            writer.put_u8(5);
            put_float(writer, *scale);
        }
        Some(TargetPrep::Matrix(matrix)) => {
            writer.put_u8(6);
            for &row in matrix {
                put_vec(writer, row);
            }
        }
    }
}

fn put_native(
    writer: &mut Writer,
    stage: &Stage,
    animation: &dyn Animation,
    prepared: bool,
) -> Result<(), Error> {
    let kind = native_kind(animation).ok_or(Error::Invalid("unsupported native animation"))?;
    writer.put_u8(kind);
    put_state(writer, stage, animation.state(), prepared)?;
    let any: &dyn Any = animation;
    if let Some(value) = any.downcast_ref::<Transform>() {
        put_mob(writer, stage, value.target)?;
        put_path(writer, value.path);
        writer.put_bool(value.replace_in_scene);
        if prepared {
            put_mob(
                writer,
                stage,
                value
                    .target_copy
                    .ok_or(Error::Invalid("missing prepared target"))?,
            )?;
        } else {
            put_preparations(writer, value);
        }
    } else if let Some(value) = any.downcast_ref::<Rotating>() {
        put_float(writer, value.angle);
        put_vec(writer, value.axis);
        put_optional_vec(writer, value.about_point);
        put_optional_vec(writer, value.about_edge);
    } else if let Some(value) = any.downcast_ref::<VFade>() {
        writer.put_bool(value.reversed);
    } else if let Some(value) = any.downcast_ref::<ShowPartial>() {
        match value.bounds {
            RevealBounds::Creation => {
                writer.put_u8(0);
            }
            RevealBounds::PassingFlash { time_width } => {
                writer.put_u8(1);
                put_float(writer, time_width);
            }
        }
        match value.geometry {
            PartialGeometry::QuadraticPath => {
                writer.put_u8(0);
            }
            PartialGeometry::SurfaceGrid { resolution, axis } => {
                writer
                    .put_u8(1)
                    .put_u64(resolution.0 as u64)
                    .put_u64(resolution.1 as u64)
                    .put_u64(axis as u64);
            }
        }
        writer
            .put_bool(value.should_match_start)
            .put_bool(value.restore_on_teardown);
    }
    Ok(())
}

fn get_native(
    reader: &mut Reader<'_>,
    stage: &Stage,
    budget: &mut Budget,
    fps: u32,
) -> Result<Box<dyn Animation>, Error> {
    let kind = reader.get_u8()?;
    if !(1..=4).contains(&kind) {
        return Err(Error::Invalid("unknown native animation kind"));
    }
    charge_carrier(budget, kind)?;
    let state = get_state(reader, stage, budget, fps, if kind == 1 { 3 } else { 2 })?;
    Ok(match kind {
        1 => {
            let target = get_mob(reader, stage)?;
            let path = get_path(reader)?;
            let replace = reader.get_bool()?;
            let target_copy = get_mob(reader, stage)?;
            let mut value = Transform::new(state.mobject(), target);
            value.state = state;
            value.path = path;
            value.replace_in_scene = replace;
            value.target_copy = Some(target_copy);
            Box::new(value)
        }
        2 => {
            let mut value = Rotating::new(state.mobject());
            value.state = state;
            value.angle = get_float(reader)?;
            value.axis = get_vec(reader)?;
            value.about_point = get_optional_vec(reader)?;
            value.about_edge = get_optional_vec(reader)?;
            Box::new(value)
        }
        3 => {
            let mut value = crate::v_fade_in(state.mobject());
            value.state = state;
            value.reversed = reader.get_bool()?;
            Box::new(value)
        }
        4 => {
            let bounds = match reader.get_u8()? {
                0 => RevealBounds::Creation,
                1 => RevealBounds::PassingFlash {
                    time_width: get_float(reader)?,
                },
                _ => return Err(Error::Invalid("unknown reveal bounds")),
            };
            let geometry = match reader.get_u8()? {
                0 => PartialGeometry::QuadraticPath,
                1 => {
                    let rows = usize::try_from(reader.get_u64()?)
                        .map_err(|_| Error::Invalid("surface dimension overflow"))?;
                    let columns = usize::try_from(reader.get_u64()?)
                        .map_err(|_| Error::Invalid("surface dimension overflow"))?;
                    let axis = usize::try_from(reader.get_u64()?)
                        .map_err(|_| Error::Invalid("surface axis overflow"))?;
                    if axis > 1
                        || rows.checked_mul(columns).is_none()
                        || (if axis == 0 { rows } else { columns }) < 2
                    {
                        return Err(Error::Invalid("invalid prepared surface reveal"));
                    }
                    PartialGeometry::SurfaceGrid {
                        resolution: (rows, columns),
                        axis,
                    }
                }
                _ => return Err(Error::Invalid("unknown reveal geometry")),
            };
            let mut value = ShowPartial::new(state.mobject(), bounds);
            value.state = state;
            value.geometry = geometry;
            value.should_match_start = reader.get_bool()?;
            value.restore_on_teardown = reader.get_bool()?;
            Box::new(value)
        }
        _ => return Err(Error::Invalid("unknown native animation kind")),
    })
}

fn prepared_valid(stage: &Stage, animation: &dyn Animation, fps: u32) -> Result<bool, Error> {
    if !config_supported(&animation.state().config) || !native_parameters_valid(stage, animation) {
        return Ok(false);
    }
    animation.validate_begin(stage)?;
    let state = animation.state();
    let Some(starting) = state.starting_mobject() else {
        return Ok(false);
    };
    if !stage.contains(starting)
        || !stage.is_animating(state.mobject())
        || (state.mobject_was_updating() && !state.config.suspend_mobject_updating)
        || (state.config.suspend_mobject_updating && !stage.is_updating_suspended(state.mobject()))
    {
        return Ok(false);
    }
    let duration = state.config.duration_on_grid(fps)?;
    if state.prepared_duration() != Some(&duration)
        || state.frame_units() != duration.is_frame_native().then(|| duration.frame_units())
    {
        return Ok(false);
    }
    let family_roots = animation.all_mobjects();
    let width = if native_kind(animation) == Some(1) {
        3
    } else {
        2
    };
    if family_roots.len() != width {
        return Ok(false);
    }
    let families: Vec<_> = family_roots.iter().map(|&mob| stage.family(mob)).collect();
    let count = families.iter().map(Vec::len).min().unwrap_or(0);
    if state.families().len() != count
        || state.families().iter().enumerate().any(|(index, row)| {
            row.len() != width
                || row
                    .iter()
                    .zip(&families)
                    .any(|(&mob, family)| mob != family[index])
        })
    {
        return Ok(false);
    }
    Ok(true)
}

/// Serialize a completed-frame boundary of a prepared native play.
/// Unknown dynamic animation types/rates, current effects, aliases or live
/// views return None without mutating the scene. The enclosing SceneState
/// must retain the prepared starting/target entries and locked data columns.
///
/// # Errors
/// A partial frame, mismatched clock, invalid native state or exceeded budget.
pub fn capture(
    stage: &Stage,
    clock: &RationalFrameClock,
    animations: &[Box<dyn Animation>],
    open: &OpenSegment,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, Error> {
    if !open.is_at_frame_boundary() {
        return Err(Error::Invalid("finish the pending frame first"));
    }
    if animations.is_empty()
        || !open.report().purity.is_pure()
        || animations
            .iter()
            .any(|animation| native_kind(animation.as_ref()).is_none())
        || !crate::classify_play(stage, animations).is_pure()
        || crate::pure_frame::freeze_animations(stage, animations).is_err()
    {
        return Ok(None);
    }
    for animation in animations {
        if !prepared_valid(stage, animation.as_ref(), clock.fps())? {
            return Ok(None);
        }
    }
    if capture_budget(animations).is_err() {
        return Ok(None);
    }
    let grid_clock = RationalFrameClock::new(clock.fps()).map_err(AnimError::Clock)?;
    let duration = crate::frame::play_duration(animations, clock.fps())?;
    let grid = grid_clock
        .segment_duration(&duration)
        .map_err(AnimError::Clock)?;
    if grid != open.segment() {
        return Ok(None);
    }
    OpenSegment::resume_closed(clock, grid, open.report().base_frame, open.stepped())?;
    let mut writer = Writer::with_limits(SCHEMA, limits(max_bytes));
    writer
        .put_u32(clock.fps())
        .put_i64(open.report().base_frame)
        .put_i64(open.stepped());
    put_count(&mut writer, animations.len())?;
    for animation in animations {
        put_native(&mut writer, stage, animation.as_ref(), true)?;
    }
    Ok(Some(writer.finish()?))
}

/// Decode prepared native carriers against an already restored matching Stage.
/// No stage writes, factory work, begin/finish calls or zero-dt passes occur.
///
/// # Errors
/// Invalid framing, budget, generation, native carrier, purity or frame cursor.
pub fn restore(
    stage: &Stage,
    clock: &RationalFrameClock,
    bytes: &[u8],
) -> Result<(Vec<Box<dyn Animation>>, OpenSegment), Error> {
    let mut reader = Reader::open(bytes, SCHEMA, limits(MAX_BYTES), UnknownPolicy::Strict)?;
    let fps = reader.get_u32()?;
    if fps != clock.fps() {
        return Err(Error::Invalid("restored frame rate differs"));
    }
    let base_frame = reader.get_i64()?;
    let stepped = reader.get_i64()?;
    let count = get_count(&mut reader, 1)?;
    if count == 0 {
        return Err(Error::Invalid("empty prepared play"));
    }
    let mut budget = Budget {
        remaining: MAX_BYTES,
    };
    let mut animations = budget.vector::<Box<dyn Animation>>(count)?;
    for _ in 0..count {
        animations.push(get_native(&mut reader, stage, &mut budget, fps)?);
    }
    reader.finish()?;
    for animation in &animations {
        if !prepared_valid(stage, animation.as_ref(), fps)? {
            return Err(Error::Invalid(
                "prepared families or timing are inconsistent",
            ));
        }
    }
    if !crate::classify_play(stage, &animations).is_pure()
        || crate::pure_frame::freeze_animations(stage, &animations).is_err()
    {
        return Err(Error::Invalid(
            "prepared play has effects, aliases or live views",
        ));
    }
    let grid_clock = RationalFrameClock::new(fps).map_err(AnimError::Clock)?;
    let duration = crate::frame::play_duration(&animations, fps)?;
    let grid = grid_clock
        .segment_duration(&duration)
        .map_err(AnimError::Clock)?;
    let open = OpenSegment::resume_closed(clock, grid, base_frame, stepped)?;
    Ok((animations, open))
}

/// Bind a fresh factory's supported native declarations, including preparation
/// recipes and effective timing overrides. This clones only known native data;
/// it never invokes arbitrary trait hooks, rate functions, preparation or begin.
///
/// # Errors
/// A dead/foreign declared handle or bounded document encoding failure.
pub fn declaration_bytes(
    stage: &Stage,
    animations: &[Box<dyn Animation>],
    overrides: DeclarationOverrides<'_>,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, Error> {
    if animations.is_empty()
        || (overrides.run_time.is_some() && overrides.run_time_frames.is_some())
    {
        return Ok(None);
    }
    let mut writer = Writer::with_limits(DECLARATION_SCHEMA, limits(max_bytes));
    put_count(&mut writer, animations.len())?;
    for animation in animations {
        let Some(mut owned) = clone_native(animation.as_ref()) else {
            return Ok(None);
        };
        owned.update_rate_info(
            overrides.run_time,
            overrides.rate_func.cloned(),
            overrides.lag_ratio,
        );
        if let Some(frames) = overrides.run_time_frames {
            owned.state_mut().config.run_time_frames = Some(frames);
        }
        if !config_supported(&owned.state().config) {
            return Ok(None);
        }
        put_native(&mut writer, stage, owned.as_ref(), false)?;
    }
    Ok(Some(writer.finish()?))
}
