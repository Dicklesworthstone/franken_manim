//! Durable continuations for prepared, closed native plays.
//!
//! A continuation owns data describing interpolation AND the remaining native
//! lifecycle. Interpolation purity alone is insufficient: a user wrapper may
//! delegate interpolation while adding an effectful finish hook. Actual dynamic
//! types are checked through Any; callbacks, just-in-time compositions and
//! unknown rates fall back. Closed eager groups retain their nested lifecycle.
//! Stage contents live in the enclosing SceneState, whose identity the caller
//! must bind to this document. Handles rebind only after that state is restored.

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use fmn_hash::{Limits, Reader, Schema, SerialError, UnknownPolicy, Writer};
use fmn_mobject::path_sampling::PathObservation;
use fmn_mobject::{Mob, Stage};

use crate::composition::GroupContinuationControls;
use crate::creation::{IntRound, PartialGeometry};
use crate::transform::TargetPrep;
use crate::{
    AnimConfig, AnimError, AnimState, Animation, AnimationGroup, DrawBorderThenFill, FadeTransform,
    FramePosition, MoveAlongPath, OpenSegment, PathFunc, RateFunc, RationalFrameClock, RevealBounds,
    Rotating, ShowIncreasingSubsets, ShowPartial, Transform, VFade, VShowPassingFlash, WiggleOutThenIn,
};

// Minor 4 adds captured path motion and cross-fade lifecycle carriers.
// Existing native kind payloads retain their layout and remain readable. Writers
// retain minor 2/3 for older trees so their declaration digests stay stable.
const SCHEMA: Schema = Schema::new(*b"FMPC", 1, 1, 4);
const DECLARATION_SCHEMA: Schema = Schema::new(*b"FMPC", 2, 1, 4);
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_ITEMS: usize = 1_000_000;
const MAX_RATE_DEPTH: usize = 32;
const MAX_NATIVE_DEPTH: usize = 32;

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
#[derive(Clone, Copy, Default)]
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
    } else if any.is::<DrawBorderThenFill>() {
        Some(5)
    } else if any.is::<AnimationGroup>() {
        Some(6)
    } else if any.is::<ShowIncreasingSubsets>() {
        Some(7)
    } else if any.is::<WiggleOutThenIn>() {
        Some(8)
    } else if any.is::<VShowPassingFlash>() {
        Some(9)
    } else if any.is::<MoveAlongPath>() {
        Some(10)
    } else if any.is::<FadeTransform>() {
        Some(11)
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
    } else if let Some(value) = any.downcast_ref::<DrawBorderThenFill>() {
        Some(Box::new(value.clone()))
    } else if let Some(value) = any.downcast_ref::<ShowIncreasingSubsets>() {
        Some(Box::new(value.clone()))
    } else if let Some(value) = any.downcast_ref::<WiggleOutThenIn>() {
        Some(Box::new(value.clone()))
    } else if let Some(value) = any.downcast_ref::<VShowPassingFlash>() {
        Some(Box::new(value.clone()))
    } else if let Some(value) = any.downcast_ref::<MoveAlongPath>() {
        Some(Box::new(value.clone()))
    } else if let Some(value) = any.downcast_ref::<FadeTransform>() {
        Some(Box::new(value.clone()))
    } else if let Some(value) = any.downcast_ref::<AnimationGroup>() {
        let children = value
            .animations()
            .iter()
            .map(|child| clone_native(child.as_ref()))
            .collect::<Option<Vec<_>>>()?;
        Some(Box::new(value.clone_for_continuation(children)))
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

fn group_controls_supported(controls: GroupContinuationControls) -> bool {
    controls.layout_lag_ratio.is_finite()
        && controls.duration_scale.is_finite()
        && controls.duration_scale >= 0.0
        && controls.timing_run_time.is_finite()
}

// Complete this bounded, concrete-type walk before invoking any trait hook.
// A group may contain an arbitrary user wrapper, including a wrapper that
// advertises pure interpolation but adds effects to begin or finish.
fn native_tree_supported(animation: &dyn Animation, depth: usize, remaining: &mut usize) -> bool {
    if depth > MAX_NATIVE_DEPTH || *remaining == 0 || native_kind(animation).is_none() {
        return false;
    }
    *remaining -= 1;
    if !config_supported(&animation.state().config) {
        return false;
    }
    if let Some(motion) = (animation as &dyn Any).downcast_ref::<MoveAlongPath>()
        && !motion.captured_path().is_some_and(captured_path_valid)
    {
        return false;
    }
    if let Some(group) = (animation as &dyn Any).downcast_ref::<AnimationGroup>() {
        group_controls_supported(group.continuation_controls())
            && group
                .animations()
                .iter()
                .all(|child| native_tree_supported(child.as_ref(), depth + 1, remaining))
    } else {
        true
    }
}

fn native_play_supported(animations: &[Box<dyn Animation>]) -> bool {
    let mut remaining = MAX_ITEMS;
    animations
        .iter()
        .all(|animation| native_tree_supported(animation.as_ref(), 0, &mut remaining))
}

// Call only after native_play_supported has bounded the concrete carrier tree.
fn carrier_minor(animation: &dyn Animation) -> u16 {
    if let Some(group) = (animation as &dyn Any).downcast_ref::<AnimationGroup>() {
        group
            .animations()
            .iter()
            .map(|child| carrier_minor(child.as_ref()))
            .max()
            .unwrap_or(2)
    } else {
        match native_kind(animation) {
            Some(10 | 11) => 4,
            Some(7..=9) => 3,
            _ => 2,
        }
    }
}

fn emission_schema(schema: Schema, animations: &[Box<dyn Animation>]) -> Schema {
    Schema {
        minor: animations
            .iter()
            .map(|animation| carrier_minor(animation.as_ref()))
            .max()
            .unwrap_or(2),
        ..schema
    }
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
    } else if let Some(value) = any.downcast_ref::<DrawBorderThenFill>() {
        if !border_parameters_valid(value)
            || value.outline.is_none()
            || value.crossed.len() > value.state.families().len()
        {
            return false;
        }
        let reveal = ShowPartial::new(value.state.mobject(), RevealBounds::Creation);
        if value
            .all_mobjects()
            .into_iter()
            .any(|mob| reveal.validate_geometry(stage, mob).is_err())
        {
            return false;
        }
        // This is an unordered phase set. Reversing a rate curve removes and
        // later re-adds members, so insertion order is not a family invariant.
        // Sort bounded copies to avoid quadratic validation on long text.
        let mut crossed = value.crossed.clone();
        if crossed
            .iter()
            .any(|&mob| stage.snapshot_handle(mob).is_none())
        {
            return false;
        }
        crossed.sort_unstable_by_key(|&mob| stage.snapshot_handle(mob));
        if crossed.windows(2).any(|pair| pair[0] == pair[1]) {
            return false;
        }
        let mut family = stage.family(value.state.mobject());
        family.sort_unstable_by_key(|&mob| stage.snapshot_handle(mob));
        crossed.iter().all(|&mob| {
            family
                .binary_search_by_key(&stage.snapshot_handle(mob), |&member| {
                    stage.snapshot_handle(member)
                })
                .is_ok()
        })
    } else if let Some(value) = any.downcast_ref::<ShowIncreasingSubsets>() {
        subsets_parameters_valid(stage, value)
    } else if let Some(value) = any.downcast_ref::<WiggleOutThenIn>() {
        wiggle_parameters_valid(value)
    } else if let Some(value) = any.downcast_ref::<VShowPassingFlash>() {
        flash_parameters_valid(value) && flash_profiles_valid(stage, value)
    } else if let Some(value) = any.downcast_ref::<MoveAlongPath>() {
        value.captured_path().is_some_and(captured_path_valid)
    } else if let Some(value) = any.downcast_ref::<FadeTransform>() {
        fade_parameters_valid(stage, value)
    } else {
        false
    }
}

fn captured_path_valid(path: &PathObservation) -> bool {
    path.point_count() > 0
        && path.point_count() <= MAX_ITEMS
        && path.world_points().iter().flatten().all(|value| value.is_finite())
        && path.total_length().is_finite()
        && path.total_length() >= 0.0
}

fn fade_source(stage: &Stage, value: &FadeTransform) -> Option<Mob> {
    let children = stage.get(value.state.mobject())?.submobjects();
    let [source, _] = children else {
        return None;
    };
    Some(*source)
}

fn fade_parameters_valid(stage: &Stage, value: &FadeTransform) -> bool {
    let Some(source) = fade_source(stage, value) else {
        return false;
    };
    let Some(saved) = stage.saved_state(source) else {
        return false;
    };
    value.dim_to_match < 3
        && saved != source
        && stage.contains(saved)
        && stage.contains(value.to_add_on_completion)
        && [value.state.starting_mobject(), value.ending]
            .into_iter()
            .all(|mob| {
                mob.and_then(|mob| stage.get(mob))
                    .is_some_and(|entry| entry.submobjects().len() == 2)
            })
}

// A reveal intentionally detaches members from its live root. Validate the
// complete begin family through the immutable starting copy, not the currently
// visible prefix. Hidden descendants still own lifetime flags and may reappear.
fn subsets_parameters_valid(stage: &Stage, value: &ShowIncreasingSubsets) -> bool {
    let state = &value.state;
    let Some(starting) = state.starting_mobject() else {
        return false;
    };
    let family = stage.family(starting);
    let rows = state.families();
    if rows.len() != family.len()
        || rows
            .first()
            .is_none_or(|row| row.as_slice() != [state.mobject(), starting])
        || rows.iter().zip(&family).any(|(row, &member)| {
            row.len() != 2
                || row[1] != member
                || !stage.contains(row[0])
                || !stage.is_animating(row[0])
                || (state.config.suspend_mobject_updating && !stage.is_updating_suspended(row[0]))
        })
    {
        return false;
    }
    let mut by_start: Vec<_> = rows.iter().map(|row| (row[1], row[0])).collect();
    by_start.sort_unstable_by_key(|&(_, original)| stage.snapshot_handle(original));
    if by_start.windows(2).any(|pair| pair[0].1 == pair[1].1) {
        return false;
    }
    by_start.sort_unstable_by_key(|&(start, _)| stage.snapshot_handle(start));
    if by_start.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return false;
    }
    let original_for = |start| {
        by_start
            .binary_search_by_key(&stage.snapshot_handle(start), |&(candidate, _)| {
                stage.snapshot_handle(candidate)
            })
            .ok()
            .map(|index| by_start[index].1)
    };
    for row in rows {
        let Some(original) = stage.get(row[0]) else {
            return false;
        };
        let Some(start) = stage.get(row[1]) else {
            return false;
        };
        let declared_children = if row[0] == state.mobject() {
            let visible = original.submobjects();
            if value.one_by_one {
                if visible.len() > 1
                    || visible
                        .first()
                        .is_some_and(|member| !value.all_submobs.contains(member))
                {
                    return false;
                }
            } else if !value.all_submobs.starts_with(visible) {
                return false;
            }
            value.all_submobs.as_slice()
        } else {
            original.submobjects()
        };
        if declared_children.len() != start.submobjects().len()
            || declared_children
                .iter()
                .zip(start.submobjects())
                .any(|(&original, &start)| original_for(start) != Some(original))
        {
            return false;
        }
    }
    true
}

fn wiggle_parameters_valid(value: &WiggleOutThenIn) -> bool {
    value.scale_value.is_finite()
        && value.rotation_angle.is_finite()
        && value.n_wiggles.is_finite()
        && value
            .scale_about_point
            .is_none_or(|point| point.iter().all(|value| value.is_finite()))
        && value
            .rotate_about_point
            .is_none_or(|point| point.iter().all(|value| value.is_finite()))
}

fn flash_parameters_valid(value: &VShowPassingFlash) -> bool {
    value.time_width.is_finite() && value.time_width > 0.0 && value.taper_width.is_finite()
}

// Profiles are begin-time data, not the current (gaussian-windowed) widths.
// Bind them to the original geometry before allowing a restored carrier to
// index or publish them. This also excludes forged, duplicate or omitted rows.
fn flash_profiles_valid(stage: &Stage, value: &VShowPassingFlash) -> bool {
    let mut profiles = value.widths.iter();
    for row in value.state.families() {
        if row.len() != 2 {
            return false;
        }
        let (Some(original), Some(start)) = (stage.get(row[0]), stage.get(row[1])) else {
            return false;
        };
        let Some(start_widths) = start.buffer.read_column("stroke_width") else {
            continue;
        };
        let Some((member, profile)) = profiles.next() else {
            return false;
        };
        if *member != row[0]
            || profile.len() != start_widths.len()
            || original.buffer.len() != start.buffer.len()
            || original.buffer.schema() != start.buffer.schema()
        {
            return false;
        }
        let n = start_widths.len();
        if start_widths
            .iter()
            .zip(profile)
            .enumerate()
            .any(|(i, (&width, &saved))| {
                let x = if n > 1 {
                    i as f64 / (n as f64 - 1.0)
                } else {
                    0.0
                };
                let expected = f64::from(width) * value.taper_kernel(x);
                !saved.is_finite() || saved.to_bits() != expected.to_bits()
            })
        {
            return false;
        }
    }
    profiles.next().is_none()
}

fn border_parameters_valid(value: &DrawBorderThenFill) -> bool {
    value.stroke_width.is_finite()
        && value.stroke_width >= 0.0
        && value.stroke_width <= f64::from(f32::MAX)
        && value
            .stroke_color
            .is_none_or(|color| color.iter().all(|component| component.is_finite()))
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
    native_items: usize,
}
impl Budget {
    fn native_node(&mut self, depth: usize) -> Result<(), Error> {
        if depth > MAX_NATIVE_DEPTH || self.native_items >= MAX_ITEMS {
            return Err(Error::Invalid(
                "native nesting or node count exceeds its bound",
            ));
        }
        self.native_items += 1;
        Ok(())
    }

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
        5 => budget.charge::<DrawBorderThenFill>(1),
        6 => budget.charge::<AnimationGroup>(1),
        7 => budget.charge::<ShowIncreasingSubsets>(1),
        8 => budget.charge::<WiggleOutThenIn>(1),
        9 => budget.charge::<VShowPassingFlash>(1),
        10 => budget.charge::<MoveAlongPath>(1),
        11 => budget.charge::<FadeTransform>(1),
        _ => Err(Error::Invalid("unknown native animation kind")),
    }
}

fn charge_path_table(budget: &mut Budget, points: usize) -> Result<(), Error> {
    // A path with n = 2c + 1 records builds c lengths and c + 1 cumulative
    // lengths. Account for that derived table and conservative Arc/Vec headers
    // before reconstructing the Chisel geometry.
    budget.charge::<f64>(points)?;
    budget.charge::<usize>(24)
}

fn charge_group_timing(budget: &mut Budget, children: usize) -> Result<(), Error> {
    // A cached interval has two bounded exact positions. Re-derivation and
    // semantic validation can briefly retain both tables plus duration and
    // arithmetic temporaries. Charge their worst-case integer storage before
    // resolving any timing, even when ordinary seconds need much less.
    let positions = children
        .checked_mul(6)
        .and_then(|count| count.checked_add(8))
        .ok_or(Error::Invalid("group timing allocation overflow"))?;
    budget.charge::<FramePosition>(positions)?;
    let words = positions
        .checked_mul(FramePosition::MAX_RATIO_BYTES)
        .ok_or(Error::Invalid("group timing allocation overflow"))?;
    budget.charge::<u8>(words)
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
        native_items: 0,
    };
    budget.charge::<Box<dyn Animation>>(animations.len())?;
    for animation in animations {
        capture_native_budget(animation.as_ref(), &mut budget, 0)?;
    }
    Ok(())
}

fn capture_native_budget(
    animation: &dyn Animation,
    budget: &mut Budget,
    depth: usize,
) -> Result<(), Error> {
    budget.native_node(depth)?;
    let kind = native_kind(animation).ok_or(Error::Invalid("unsupported native animation"))?;
    charge_carrier(budget, kind)?;
    let state = animation.state();
    charge_rate(budget, &state.config.rate_func)?;
    budget.charge::<u8>(state.config.name.len())?;
    budget.charge::<Vec<Mob>>(state.families().len())?;
    budget.charge::<Vec<Mob>>(state.families().len())?;
    for row in state.families() {
        budget.charge::<Mob>(row.len())?;
    }
    let any = animation as &dyn Any;
    if let Some(value) = any.downcast_ref::<DrawBorderThenFill>() {
        budget.charge::<Mob>(value.crossed.len())?;
        // Phase membership validation sorts a copy and the live family.
        budget.charge::<Mob>(value.crossed.len())?;
        budget.charge::<Mob>(state.families().len())?;
    } else if let Some(value) = any.downcast_ref::<ShowIncreasingSubsets>() {
        budget.charge::<Mob>(value.all_submobs.len())?;
        // The starting family's DFS and the sorted reverse handle map.
        budget.charge::<Mob>(state.families().len())?;
        budget.charge::<(Mob, Mob)>(state.families().len())?;
    } else if let Some(value) = any.downcast_ref::<VShowPassingFlash>() {
        budget.charge::<(Mob, Vec<f64>)>(value.widths.len())?;
        for (_, profile) in &value.widths {
            budget.charge::<f64>(profile.len())?;
            budget.charge::<f32>(profile.len())?;
        }
    } else if let Some(value) = any.downcast_ref::<MoveAlongPath>() {
        let path = value
            .captured_path()
            .ok_or(Error::Invalid("live path motion has no closed continuation"))?;
        budget.charge::<[f64; 3]>(path.point_count())?;
        charge_path_table(budget, path.point_count())?;
    } else if let Some(group) = any.downcast_ref::<AnimationGroup>() {
        charge_group_timing(budget, group.animations().len())?;
        charge_rate(budget, &state.config.rate_func)?;
        budget.charge::<u8>(state.config.name.len())?;
        budget.charge::<Box<dyn Animation>>(group.animations().len())?;
        for child in group.animations() {
            capture_native_budget(child.as_ref(), budget, depth + 1)?;
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
    let mut remaining = MAX_ITEMS;
    put_native_node(writer, stage, animation, prepared, 0, &mut remaining)
}

fn put_native_node(
    writer: &mut Writer,
    stage: &Stage,
    animation: &dyn Animation,
    prepared: bool,
    depth: usize,
    remaining: &mut usize,
) -> Result<(), Error> {
    if depth > MAX_NATIVE_DEPTH || *remaining == 0 {
        return Err(Error::Invalid(
            "native nesting or node count exceeds its bound",
        ));
    }
    *remaining -= 1;
    let kind = native_kind(animation).ok_or(Error::Invalid("unsupported native animation"))?;
    writer.put_u8(kind);
    let any: &dyn Any = animation;
    if let Some(group) = any.downcast_ref::<AnimationGroup>() {
        // Eager groups own no starting copy or zipped family table. Each
        // child's independently prepared state carries those handles.
        put_state(writer, stage, group.state(), false)?;
        let controls = group.continuation_controls();
        if !group_controls_supported(controls) {
            return Err(Error::Invalid("invalid native group controls"));
        }
        put_float(writer, controls.layout_lag_ratio);
        put_float(writer, controls.duration_scale);
        put_float(writer, controls.timing_run_time);
        writer.put_bool(controls.run_time_override);
        put_count(writer, group.animations().len())?;
        for child in group.animations() {
            put_native_node(
                writer,
                stage,
                child.as_ref(),
                prepared,
                depth + 1,
                remaining,
            )?;
        }
        return Ok(());
    }
    put_state(writer, stage, animation.state(), prepared)?;
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
    } else if let Some(value) = any.downcast_ref::<DrawBorderThenFill>() {
        if !border_parameters_valid(value) {
            return Err(Error::Invalid("invalid border/fill stroke settings"));
        }
        put_float(writer, value.stroke_width);
        writer.put_bool(value.stroke_color.is_some());
        if let Some(color) = value.stroke_color {
            for component in color {
                writer.put_u32(component.to_bits());
            }
        }
        if prepared {
            put_mob(
                writer,
                stage,
                value
                    .outline
                    .ok_or(Error::Invalid("missing prepared outline"))?,
            )?;
            put_count(writer, value.crossed.len())?;
            for &mob in &value.crossed {
                put_mob(writer, stage, mob)?;
            }
        }
    } else if let Some(value) = any.downcast_ref::<ShowIncreasingSubsets>() {
        put_count(writer, value.all_submobs.len())?;
        for &mob in &value.all_submobs {
            put_mob(writer, stage, mob)?;
        }
        writer
            .put_u8(match value.int_round {
                IntRound::Round => 0,
                IntRound::Ceil => 1,
            })
            .put_bool(value.one_by_one);
    } else if let Some(value) = any.downcast_ref::<WiggleOutThenIn>() {
        if !wiggle_parameters_valid(value) {
            return Err(Error::Invalid("invalid wiggle parameters"));
        }
        put_float(writer, value.scale_value);
        put_float(writer, value.rotation_angle);
        put_float(writer, value.n_wiggles);
        put_optional_vec(writer, value.scale_about_point);
        put_optional_vec(writer, value.rotate_about_point);
    } else if let Some(value) = any.downcast_ref::<VShowPassingFlash>() {
        if !flash_parameters_valid(value) {
            return Err(Error::Invalid("invalid passing-flash parameters"));
        }
        put_float(writer, value.time_width);
        put_float(writer, value.taper_width);
        if prepared {
            put_count(writer, value.widths.len())?;
            for (member, profile) in &value.widths {
                put_mob(writer, stage, *member)?;
                put_count(writer, profile.len())?;
                for &width in profile {
                    put_float(writer, width);
                }
            }
        }
    } else if let Some(value) = any.downcast_ref::<MoveAlongPath>() {
        let path = value
            .captured_path()
            .filter(|path| captured_path_valid(path))
            .ok_or(Error::Invalid("invalid or live path motion"))?;
        put_count(writer, path.point_count())?;
        for &point in path.world_points() {
            put_vec(writer, point);
        }
    } else if let Some(value) = any.downcast_ref::<FadeTransform>() {
        let dim = u8::try_from(value.dim_to_match)
            .ok()
            .filter(|&dim| dim < 3)
            .ok_or(Error::Invalid("invalid cross-fade matching dimension"))?;
        put_mob(writer, stage, value.to_add_on_completion)?;
        writer
            .put_bool(value.stretch)
            .put_u8(dim)
            .put_bool(value.pieces);
        if prepared {
            put_mob(
                writer,
                stage,
                value.ending.ok_or(Error::Invalid("missing cross-fade ending"))?,
            )?;
            let saved = fade_source(stage, value)
                .and_then(|source| stage.saved_state(source))
                .ok_or(Error::Invalid("missing cross-fade saved source"))?;
            put_mob(writer, stage, saved)?;
        }
    }
    Ok(())
}

fn get_native(
    reader: &mut Reader<'_>,
    stage: &Stage,
    budget: &mut Budget,
    fps: u32,
) -> Result<Box<dyn Animation>, Error> {
    get_native_node(reader, stage, budget, fps, 0)
}

fn get_native_node(
    reader: &mut Reader<'_>,
    stage: &Stage,
    budget: &mut Budget,
    fps: u32,
    depth: usize,
) -> Result<Box<dyn Animation>, Error> {
    budget.native_node(depth)?;
    let kind = reader.get_u8()?;
    if !(1..=11).contains(&kind) {
        return Err(Error::Invalid("unknown native animation kind"));
    }
    if kind >= 7 && reader.version().1 < 3 {
        return Err(Error::Invalid(
            "native animation kind requires FMPC minor 3",
        ));
    }
    if kind >= 10 && reader.version().1 < 4 {
        return Err(Error::Invalid(
            "native animation kind requires FMPC minor 4",
        ));
    }
    charge_carrier(budget, kind)?;
    if kind == 6 {
        let mob = get_mob(reader, stage)?;
        let config = get_config(reader, budget)?;
        let controls = GroupContinuationControls {
            layout_lag_ratio: get_float(reader)?,
            duration_scale: get_float(reader)?,
            timing_run_time: get_float(reader)?,
            run_time_override: reader.get_bool()?,
        };
        if !group_controls_supported(controls) {
            return Err(Error::Invalid("invalid native group controls"));
        }
        let count = get_count(reader, 1)?;
        if count > MAX_ITEMS - budget.native_items {
            return Err(Error::Invalid("native node count exceeds its bound"));
        }
        charge_group_timing(budget, count)?;
        charge_rate(budget, &config.rate_func)?;
        budget.charge::<u8>(config.name.len())?;
        let mut children = budget.vector::<Box<dyn Animation>>(count)?;
        for _ in 0..count {
            children.push(get_native_node(reader, stage, budget, fps, depth + 1)?);
        }
        return Ok(Box::new(AnimationGroup::from_continuation(
            AnimState::new(mob, config),
            children,
            controls,
            fps,
        )?));
    }
    let width = if matches!(kind, 1 | 5 | 11) { 3 } else { 2 };
    let state = get_state(reader, stage, budget, fps, width)?;
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
        5 => {
            let mut value = DrawBorderThenFill::new(state.mobject());
            value.state = state;
            value.stroke_width = get_float(reader)?;
            value.stroke_color = if reader.get_bool()? {
                Some([
                    f32::from_bits(reader.get_u32()?),
                    f32::from_bits(reader.get_u32()?),
                    f32::from_bits(reader.get_u32()?),
                ])
            } else {
                None
            };
            if !border_parameters_valid(&value) {
                return Err(Error::Invalid("invalid border/fill stroke settings"));
            }
            value.outline = Some(get_mob(reader, stage)?);
            let count = get_count(reader, 8)?;
            if count > value.state.families().len() {
                return Err(Error::Invalid("border/fill phase count exceeds its family"));
            }
            value.crossed = budget.vector::<Mob>(count)?;
            budget.charge::<Mob>(count)?;
            budget.charge::<Mob>(value.state.families().len())?;
            for _ in 0..count {
                value.crossed.push(get_mob(reader, stage)?);
            }
            Box::new(value)
        }
        7 => {
            let count = get_count(reader, 8)?;
            if count > state.families().len().saturating_sub(1) {
                return Err(Error::Invalid(
                    "reveal child count exceeds its prepared family",
                ));
            }
            let mut all_submobs = budget.vector::<Mob>(count)?;
            budget.charge::<Mob>(state.families().len())?;
            budget.charge::<(Mob, Mob)>(state.families().len())?;
            for _ in 0..count {
                all_submobs.push(get_mob(reader, stage)?);
            }
            let int_round = match reader.get_u8()? {
                0 => IntRound::Round,
                1 => IntRound::Ceil,
                _ => return Err(Error::Invalid("unknown reveal rounding rule")),
            };
            Box::new(ShowIncreasingSubsets {
                state,
                all_submobs,
                int_round,
                one_by_one: reader.get_bool()?,
            })
        }
        8 => {
            let mut value = WiggleOutThenIn::new(state.mobject());
            value.state = state;
            value.scale_value = get_float(reader)?;
            value.rotation_angle = get_float(reader)?;
            value.n_wiggles = get_float(reader)?;
            value.scale_about_point = get_optional_vec(reader)?;
            value.rotate_about_point = get_optional_vec(reader)?;
            Box::new(value)
        }
        9 => {
            let mut value = VShowPassingFlash::new(state.mobject());
            value.state = state;
            value.time_width = get_float(reader)?;
            value.taper_width = get_float(reader)?;
            if !flash_parameters_valid(&value) {
                return Err(Error::Invalid("invalid passing-flash parameters"));
            }
            let count = get_count(reader, 12)?;
            if count > value.state.families().len() {
                return Err(Error::Invalid(
                    "flash profile count exceeds its prepared family",
                ));
            }
            value.widths = budget.vector::<(Mob, Vec<f64>)>(count)?;
            for _ in 0..count {
                let member = get_mob(reader, stage)?;
                let count = get_count(reader, 8)?;
                let mut profile = budget.vector::<f64>(count)?;
                budget.charge::<f32>(count)?;
                for _ in 0..count {
                    profile.push(get_float(reader)?);
                }
                value.widths.push((member, profile));
            }
            Box::new(value)
        }
        10 => {
            let count = get_count(reader, 24)?;
            if count == 0 || count % 2 == 0 {
                return Err(Error::Invalid("invalid captured path point count"));
            }
            charge_path_table(budget, count)?;
            let mut points = budget.vector::<[f64; 3]>(count)?;
            for _ in 0..count {
                points.push(get_vec(reader)?);
            }
            let path = PathObservation::from_world_points(points)
                .map_err(|_| Error::Invalid("invalid captured path geometry"))?;
            if !captured_path_valid(&path) {
                return Err(Error::Invalid("invalid captured path arc length"));
            }
            let mut value = MoveAlongPath::from_observation(state.mobject(), path);
            *value.state_mut() = state;
            Box::new(value)
        }
        11 => {
            let target = get_mob(reader, stage)?;
            let stretch = reader.get_bool()?;
            let dim_to_match = usize::from(reader.get_u8()?);
            if dim_to_match >= 3 {
                return Err(Error::Invalid("invalid cross-fade matching dimension"));
            }
            let pieces = reader.get_bool()?;
            let ending = get_mob(reader, stage)?;
            let saved = get_mob(reader, stage)?;
            let value = FadeTransform {
                state,
                ending: Some(ending),
                to_add_on_completion: target,
                stretch,
                dim_to_match,
                pieces,
                interpolation_scratch: Default::default(),
            };
            if fade_source(stage, &value).and_then(|source| stage.saved_state(source)) != Some(saved) {
                return Err(Error::Invalid("cross-fade saved source differs"));
            }
            Box::new(value)
        }
        _ => return Err(Error::Invalid("unknown native animation kind")),
    })
}

fn prepared_valid(stage: &Stage, animation: &dyn Animation, fps: u32) -> Result<bool, Error> {
    if native_kind(animation).is_none() || !config_supported(&animation.state().config) {
        return Ok(false);
    }
    if let Some(group) = (animation as &dyn Any).downcast_ref::<AnimationGroup>() {
        if !group.continuation_lifecycle_valid(stage) {
            return Ok(false);
        }
        for child in group.animations() {
            if !prepared_valid(stage, child.as_ref(), fps)? {
                return Ok(false);
            }
        }
        if !group.continuation_timing_valid(fps) {
            return Ok(false);
        }
        animation.validate_begin(stage)?;
        return Ok(true);
    }
    if !native_parameters_valid(stage, animation) {
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
    if (animation as &dyn Any).is::<ShowIncreasingSubsets>() {
        // Its changing root family was checked against the complete starting
        // graph above. Comparing with today's visible family would discard
        // the hidden members that finish/abort still have to release.
        return Ok(true);
    }
    let family_roots = animation.all_mobjects();
    let width = if matches!(native_kind(animation), Some(1 | 5 | 11)) {
        3
    } else {
        2
    };
    if family_roots.len() != width {
        return Ok(false);
    }
    let families: Vec<_> = family_roots.iter().map(|&mob| stage.family(mob)).collect();
    let count = families.iter().map(Vec::len).min().unwrap_or(0);
    Ok(state.families().len() == count
        && state.families().iter().enumerate().all(|(index, row)| {
            row.len() == width
                && row
                    .iter()
                    .zip(&families)
                    .all(|(&mob, family)| mob == family[index])
        }))
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
        || !native_play_supported(animations)
        || capture_budget(animations).is_err()
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
    let grid_clock = RationalFrameClock::new(clock.fps()).map_err(AnimError::Clock)?;
    let duration = crate::frame::play_duration(animations, clock.fps())?;
    let grid = grid_clock
        .segment_duration(&duration)
        .map_err(AnimError::Clock)?;
    if grid != open.segment() {
        return Ok(None);
    }
    OpenSegment::resume_closed(clock, grid, open.report().base_frame, open.stepped())?;
    let mut writer = Writer::with_limits(emission_schema(SCHEMA, animations), limits(max_bytes));
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
        native_items: 0,
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
        || !native_play_supported(animations)
        || capture_budget(animations).is_err()
    {
        return Ok(None);
    }
    let mut writer = Writer::with_limits(
        emission_schema(DECLARATION_SCHEMA, animations),
        limits(max_bytes),
    );
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

fn declaration_is_fresh(animation: &dyn Animation) -> bool {
    let state = animation.state();
    if state.starting_mobject().is_some()
        || !state.families().is_empty()
        || state.mobject_was_updating()
        || state.prepared_duration().is_some()
        || state.frame_units().is_some()
    {
        return false;
    }
    let any = animation as &dyn Any;
    if let Some(group) = any.downcast_ref::<AnimationGroup>() {
        group.declaration_is_fresh()
            && group
                .animations()
                .iter()
                .all(|child| declaration_is_fresh(child.as_ref()))
    } else if let Some(transform) = any.downcast_ref::<Transform>() {
        transform.target_copy.is_none()
    } else if let Some(drawing) = any.downcast_ref::<DrawBorderThenFill>() {
        drawing.outline.is_none() && drawing.crossed.is_empty()
    } else if let Some(flash) = any.downcast_ref::<VShowPassingFlash>() {
        flash.widths.is_empty()
    } else if let Some(fade) = any.downcast_ref::<FadeTransform>() {
        fade.ending.is_none()
    } else {
        true
    }
}

/// Bind an entirely unprepared closed declaration and its exact planned frame
/// count. A future program segment can retain the fresh factory's carrier only
/// when it has no hidden begin, interpolation or timing state. No preparation,
/// rate evaluation, lifecycle hook or Stage mutation occurs here.
///
/// # Errors
/// The declaration's encoding errors or invalid exact timing.
pub fn fresh_declaration(
    stage: &Stage,
    animations: &[Box<dyn Animation>],
    overrides: DeclarationOverrides<'_>,
    fps: u32,
    max_bytes: usize,
) -> Result<Option<(Vec<u8>, i64)>, Error> {
    if !native_play_supported(animations)
        || !animations
            .iter()
            .all(|animation| declaration_is_fresh(animation.as_ref()))
    {
        return Ok(None);
    }
    let Some(bytes) = declaration_bytes(stage, animations, overrides, max_bytes)? else {
        return Ok(None);
    };
    let mut owned = Vec::with_capacity(animations.len());
    for animation in animations {
        let Some(mut clone) = clone_native(animation.as_ref()) else {
            return Ok(None);
        };
        clone.update_rate_info(
            overrides.run_time,
            overrides.rate_func.cloned(),
            overrides.lag_ratio,
        );
        if let Some(frames) = overrides.run_time_frames {
            clone.state_mut().config.run_time_frames = Some(frames);
        }
        owned.push(clone);
    }
    let duration = crate::frame::play_duration(&owned, fps)?;
    let frames = RationalFrameClock::new(fps)
        .map_err(AnimError::Clock)?
        .segment_duration(&duration)
        .map_err(AnimError::Clock)?
        .n_frames();
    Ok(Some((bytes, frames)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_core::rng::RngRoot;
    use fmn_mobject::{Mobject, Snapshot};

    fn prepared_border() -> (Stage, DrawBorderThenFill) {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::vector_from_points(&[
            [0.0, 0.0, 0.0],
            [0.5, 0.0, 0.0],
            [1.0, 0.0, 0.0],
        ]));
        stage.add_to_scene(mob).unwrap();
        let mut animation = DrawBorderThenFill::new(mob);
        animation.prepare_timing(30).unwrap();
        animation.begin(&mut stage).unwrap();
        animation.interpolate(&mut stage, 0.75);
        (stage, animation)
    }

    fn decode_prepared(stage: &Stage, value: &dyn Animation) -> Result<bool, Error> {
        let mut writer = Writer::with_limits(SCHEMA, limits(MAX_BYTES));
        put_native(&mut writer, stage, value, true)?;
        let bytes = writer.finish()?;
        let mut reader = Reader::open(&bytes, SCHEMA, limits(MAX_BYTES), UnknownPolicy::Strict)?;
        let mut budget = Budget {
            remaining: MAX_BYTES,
            native_items: 0,
        };
        let decoded = get_native(&mut reader, stage, &mut budget, 30)?;
        reader.finish()?;
        prepared_valid(stage, decoded.as_ref(), 30)
    }

    #[test]
    fn border_continuation_rejects_forged_phase_members_and_geometry() {
        let (mut stage, animation) = prepared_border();
        assert!(decode_prepared(&stage, &animation).unwrap());
        let mut forged = animation.clone();
        forged.crossed = vec![animation.outline.unwrap()];
        assert!(!decode_prepared(&stage, &forged).unwrap());
        forged.crossed = vec![animation.state.mobject(); 2];
        assert!(decode_prepared(&stage, &forged).is_err());
        forged = animation.clone();
        forged.outline = Some(stage.add(Mobject::from_points(&[[0.0, 0.0, 0.0]])));
        assert!(!decode_prepared(&stage, &forged).unwrap());
        let foreign = Stage::new().add(Mobject::new());
        forged = animation.clone();
        forged.crossed = vec![foreign];
        assert!(decode_prepared(&stage, &forged).is_err());
    }

    #[test]
    fn border_continuation_binds_finite_representable_stroke_settings() {
        let (stage, animation) = prepared_border();
        for width in [f64::NAN, f64::INFINITY, -1.0, f64::from(f32::MAX) * 2.0] {
            let forged = animation.clone().with_stroke_width(width);
            assert!(decode_prepared(&stage, &forged).is_err());
        }
        let forged = animation
            .clone()
            .with_stroke_color(Some([1.0, f32::NAN, 0.0]));
        assert!(decode_prepared(&stage, &forged).is_err());
        let declared: Vec<Box<dyn Animation>> =
            vec![Box::new(DrawBorderThenFill::new(animation.state.mobject()))];
        let first = declaration_bytes(
            &stage,
            &declared,
            DeclarationOverrides::default(),
            MAX_BYTES,
        )
        .unwrap()
        .unwrap();
        let changed: Vec<Box<dyn Animation>> = vec![Box::new(
            DrawBorderThenFill::new(animation.state.mobject()).with_stroke_width(3.0),
        )];
        let second =
            declaration_bytes(&stage, &changed, DeclarationOverrides::default(), MAX_BYTES)
                .unwrap()
                .unwrap();
        assert_ne!(first, second);
    }

    fn indication_family() -> (Stage, Mob) {
        let mut stage = Stage::new();
        let root = stage.add(Mobject::vector_from_points(&[]));
        for x in [0.0, 2.0, 4.0] {
            let child = stage.add(Mobject::vector_from_points(&[
                [x, 0.0, 0.0],
                [x + 0.5, 1.0, 0.0],
                [x + 1.0, 0.0, 0.0],
            ]));
            stage
                .get_mut(child)
                .unwrap()
                .buffer
                .write_range("stroke_width", 0, &[2.0, 4.0, 3.0]);
            stage.attach(root, child).unwrap();
            let grandchild = stage.add(Mobject::from_points(&[[x, -1.0, 0.0]]));
            stage.attach(child, grandchild).unwrap();
        }
        stage.add_to_scene(root).unwrap();
        (stage, root)
    }

    fn motion_scene(kind: u8, nested: bool, remover: bool) -> (Stage, Vec<Box<dyn Animation>>) {
        let (mut stage, root) = indication_family();
        let mut animation: Box<dyn Animation> = if kind == 0 {
            let path = stage.add(Mobject::vector_from_points(&[
                [-3.0, 0.0, 0.0],
                [-1.0, 3.0, 0.0],
                [1.0, 0.0, 0.0],
                [2.0, -2.0, 0.0],
                [4.0, 1.0, 0.0],
            ]));
            stage.shift(path, [0.100_000_000_003, 0.200_000_000_007, 0.0]);
            let captured = fmn_mobject::path_sampling::PathSampler::default()
                .observe(&stage, path)
                .unwrap();
            // The continuation owns the exact observation, not a dependency
            // on whatever now occupies the original path's arena slot.
            stage.shift(path, [100.0, 100.0, 0.0]);
            stage.delete(path).unwrap();
            Box::new(MoveAlongPath::from_observation(root, captured))
        } else {
            let target = stage.copy_family(root).unwrap();
            stage.shift(target, [2.75, -1.25, 0.0]);
            let fade = if kind == 1 {
                crate::fade_transform(&mut stage, root, target)
            } else {
                crate::fade_transform_pieces(&mut stage, root, target)
            }
            .unwrap()
            .with_stretch(false)
            .with_dim_to_match(0);
            Box::new(fade)
        };
        animation.state_mut().config.run_time_frames = Some(13);
        animation.state_mut().config.rate_func =
            RateFunc::Catalog(fmn_core::rate::CatalogRate::ThereAndBack);
        animation.state_mut().config.final_alpha_value = 0.375;
        animation.state_mut().config.remover = remover;
        animation.state_mut().config.suspend_mobject_updating = true;
        if nested {
            animation = Box::new(AnimationGroup::new(&mut stage, vec![animation]).unwrap());
            animation = Box::new(AnimationGroup::new(&mut stage, vec![animation]).unwrap());
        }
        (stage, vec![animation])
    }

    #[test]
    fn captured_motion_and_crossfades_resume_every_remaining_sample_and_cleanup() {
        let rng = RngRoot::from_seed(17);
        for kind in 0..3 {
            for nested in [false, true] {
                for remover in [false, true] {
                    for boundary in [0, 1, 6, 12, 13] {
                        let (mut stage, mut animations) = motion_scene(kind, nested, remover);
                        let mut clock = RationalFrameClock::new(30).unwrap();
                        clock.advance_frames(5).unwrap();
                        stage.set_time_from_clock(clock.now().to_f64());
                        let mut open = crate::open_play(&mut stage, &clock, &mut animations).unwrap();
                        crate::advance_play(
                            &mut stage,
                            &mut clock,
                            &rng,
                            &mut animations,
                            &mut open,
                            boundary,
                            &mut |_| {},
                        )
                        .unwrap();
                        let checkpoint = capture(&stage, &clock, &animations, &open, MAX_BYTES)
                            .unwrap()
                            .expect("closed motion carrier");
                        let snapshot = stage.snapshot().to_bytes().unwrap();
                        let mut resumed_stage = Stage::new();
                        let decoded = Snapshot::from_bytes(&snapshot, &resumed_stage).unwrap();
                        resumed_stage.restore(&decoded.snapshot);
                        resumed_stage.set_time_from_clock(clock.now().to_f64());
                        let mut resumed_clock = clock;
                        let (mut resumed, mut resumed_open) =
                            restore(&resumed_stage, &resumed_clock, &checkpoint).unwrap();
                        // Decode cannot run begin, rebuild ghosts, or save a
                        // new source. All prepared state survives byte-for-byte.
                        assert_eq!(resumed_stage.snapshot().to_bytes().unwrap(), snapshot);
                        assert_eq!(
                            capture(
                                &resumed_stage,
                                &resumed_clock,
                                &resumed,
                                &resumed_open,
                                MAX_BYTES
                            )
                            .unwrap()
                            .unwrap(),
                            checkpoint
                        );
                        let mut serial_frames = Vec::new();
                        let mut resumed_frames = Vec::new();
                        crate::advance_play(
                            &mut stage,
                            &mut clock,
                            &rng,
                            &mut animations,
                            &mut open,
                            13,
                            &mut |packet| {
                                serial_frames.push((
                                    packet.metadata(),
                                    packet.state().to_bytes().unwrap(),
                                ));
                            },
                        )
                        .unwrap();
                        crate::advance_play(
                            &mut resumed_stage,
                            &mut resumed_clock,
                            &rng,
                            &mut resumed,
                            &mut resumed_open,
                            13,
                            &mut |packet| {
                                resumed_frames.push((
                                    packet.metadata(),
                                    packet.state().to_bytes().unwrap(),
                                ));
                            },
                        )
                        .unwrap();
                        assert_eq!(
                            serial_frames, resumed_frames,
                            "kind={kind}, nested={nested}, remover={remover}, boundary={boundary}"
                        );
                        crate::finish_open_play(&mut stage, &mut animations, open).unwrap();
                        crate::finish_open_play(&mut resumed_stage, &mut resumed, resumed_open)
                            .unwrap();
                        assert_eq!(
                            stage.snapshot().to_bytes().unwrap(),
                            resumed_stage.snapshot().to_bytes().unwrap(),
                            "cleanup kind={kind}, nested={nested}, remover={remover}, boundary={boundary}"
                        );
                        assert_eq!(clock.now(), resumed_clock.now());
                    }
                }
            }
        }
    }

    #[test]
    fn motion_declarations_bind_captured_points_and_crossfade_controls() {
        let (mut stage, root) = indication_family();
        let target = stage.copy_family(root).unwrap();
        stage.shift(target, [2.0, -1.0, 0.0]);
        let first = PathObservation::from_world_points(vec![[1.000_000_000_001, -0.0, 0.0]])
            .unwrap();
        let changed = PathObservation::from_world_points(vec![[1.000_000_000_002, -0.0, 0.0]])
            .unwrap();
        let fade = crate::fade_transform(&mut stage, root, target).unwrap();
        let variants: Vec<Box<dyn Animation>> = vec![
            Box::new(MoveAlongPath::from_observation(root, first)),
            Box::new(MoveAlongPath::from_observation(root, changed)),
            Box::new(fade.clone()),
            Box::new(fade.clone().with_stretch(false)),
            Box::new(fade.with_dim_to_match(2)),
        ];
        let before = stage.snapshot().to_bytes().unwrap();
        let mut declarations = Vec::new();
        for animation in variants {
            let (bytes, frames) = fresh_declaration(
                &stage,
                &[animation],
                DeclarationOverrides::default(),
                30,
                MAX_BYTES,
            )
            .unwrap()
            .unwrap();
            assert_eq!(frames, 30);
            let reader = Reader::open(
                &bytes,
                DECLARATION_SCHEMA,
                limits(MAX_BYTES),
                UnknownPolicy::Strict,
            )
            .unwrap();
            assert_eq!(reader.version(), (1, 4));
            assert!(!declarations.contains(&bytes));
            declarations.push(bytes);
            assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
        }
        assert!(
            declaration_bytes(
                &stage,
                &[Box::new(MoveAlongPath::new(root, target))],
                DeclarationOverrides::default(),
                MAX_BYTES,
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn captured_motion_decode_enforces_geometry_version_and_memory_bounds() {
        let (mut stage, mut animations) = motion_scene(0, false, false);
        let clock = RationalFrameClock::new(30).unwrap();
        let _open = crate::open_play(&mut stage, &clock, &mut animations).unwrap();
        let before = stage.snapshot().to_bytes().unwrap();
        for case in 0..7 {
            let schema = if case == 0 {
                Schema::new(*b"FMPC", 1, 1, 3)
            } else {
                SCHEMA
            };
            let mut writer = Writer::with_limits(schema, limits(MAX_BYTES));
            if case < 2 {
                put_native(&mut writer, &stage, animations[0].as_ref(), true).unwrap();
            } else {
                writer.put_u8(10);
                put_state(&mut writer, &stage, animations[0].state(), true).unwrap();
                match case {
                    2 => {
                        writer.put_u32(0);
                    }
                    3 => {
                        writer.put_u32(2);
                        put_vec(&mut writer, [0.0; 3]);
                        put_vec(&mut writer, [1.0; 3]);
                    }
                    4 => {
                        writer.put_u32(u32::MAX);
                    }
                    5 => {
                        writer.put_u32(3);
                        for point in [[0.0; 3], [f64::NAN, 0.0, 0.0], [1.0; 3]] {
                            put_vec(&mut writer, point);
                        }
                    }
                    _ => {
                        writer.put_u32(3);
                        for point in [
                            [-f64::MAX, 0.0, 0.0],
                            [0.0, f64::MAX, 0.0],
                            [f64::MAX, 0.0, 0.0],
                        ] {
                            put_vec(&mut writer, point);
                        }
                    }
                }
            }
            let bytes = writer.finish().unwrap();
            let mut reader =
                Reader::open(&bytes, SCHEMA, limits(MAX_BYTES), UnknownPolicy::Strict).unwrap();
            let mut budget = Budget {
                remaining: if case == 1 { 16 } else { MAX_BYTES },
                native_items: 0,
            };
            assert!(
                get_native(&mut reader, &stage, &mut budget, 30).is_err(),
                "case={case}"
            );
            assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
        }
    }

    #[test]
    fn crossfade_decode_binds_saved_source_and_refuses_invalid_lifecycle() {
        let (mut stage, root) = indication_family();
        let target = stage.copy_family(root).unwrap();
        stage.shift(target, [3.0, -1.0, 0.0]);
        let mut animation = crate::fade_transform_pieces(&mut stage, root, target).unwrap();
        animation.prepare_timing(30).unwrap();
        animation.begin(&mut stage).unwrap();
        animation.interpolate(&mut stage, 0.4);
        assert!(decode_prepared(&stage, &animation).unwrap());
        let before = stage.snapshot().to_bytes().unwrap();
        let mut foreign_stage = Stage::new();
        let foreign = foreign_stage.add(Mobject::new());
        for case in 0..4 {
            let mut forged = animation.clone();
            match case {
                0 => forged.dim_to_match = 3,
                1 => forged.ending = Some(target),
                2 => forged.to_add_on_completion = foreign,
                _ => forged.ending = None,
            }
            assert!(
                !matches!(decode_prepared(&stage, &forged), Ok(true)),
                "case={case}"
            );
            assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
        }
        let mut writer = Writer::with_limits(SCHEMA, limits(MAX_BYTES));
        put_native(&mut writer, &stage, &animation, true).unwrap();
        let bytes = writer.finish().unwrap();
        let saved = stage.saved_state(root).unwrap();
        let different = stage.copy_family(root).unwrap();
        stage.link_saved_state(root, different).unwrap();
        let before = stage.snapshot().to_bytes().unwrap();
        let mut reader =
            Reader::open(&bytes, SCHEMA, limits(MAX_BYTES), UnknownPolicy::Strict).unwrap();
        let mut budget = Budget {
            remaining: MAX_BYTES,
            native_items: 0,
        };
        assert!(get_native(&mut reader, &stage, &mut budget, 30).is_err());
        assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
        stage.link_saved_state(root, saved).unwrap();
    }

    #[test]
    fn reveal_continuation_rejects_forged_hidden_inventory_and_family_links() {
        let (mut stage, root) = indication_family();
        let mut animation = crate::show_increasing_subsets(&stage, root).unwrap();
        animation.state.config.suspend_mobject_updating = true;
        animation.prepare_timing(30).unwrap();
        animation.begin(&mut stage).unwrap();
        assert!(stage.get(root).unwrap().submobjects().is_empty());
        assert!(decode_prepared(&stage, &animation).unwrap());
        let before = stage.snapshot().to_bytes().unwrap();
        for case in 0..5 {
            let mut forged = animation.clone();
            match case {
                0 => forged.all_submobs.swap(0, 1),
                1 => forged.all_submobs[1] = forged.all_submobs[0],
                2 => {
                    forged.all_submobs.pop();
                }
                3 => forged.all_submobs[0] = root,
                _ => {
                    let mut rows = forged.state.families().to_vec();
                    rows[1][0] = rows[2][0];
                    forged.state.families = Arc::from(rows);
                }
            }
            assert!(!decode_prepared(&stage, &forged).unwrap(), "case={case}");
            assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
        }
        let hidden = animation.all_submobs[2];
        stage.set_animating_status(hidden, false, false);
        assert!(!decode_prepared(&stage, &animation).unwrap());
        stage.set_animating_status(hidden, true, false);
        let grandchild = stage.get(hidden).unwrap().submobjects()[0];
        stage.detach(hidden, grandchild);
        assert!(!decode_prepared(&stage, &animation).unwrap());
    }

    #[test]
    fn passing_flash_continuation_binds_exact_profiles_to_the_prepared_geometry() {
        let (mut stage, root) = indication_family();
        let mut animation = VShowPassingFlash::new(root)
            .with_time_width(0.7)
            .with_taper_width(0.2);
        animation.prepare_timing(30).unwrap();
        animation.begin(&mut stage).unwrap();
        animation.interpolate(&mut stage, 0.4);
        assert!(decode_prepared(&stage, &animation).unwrap());
        let before = stage.snapshot().to_bytes().unwrap();
        for case in 0..5 {
            let mut forged = animation.clone();
            match case {
                0 => forged.widths[1].0 = root,
                1 => forged.widths[1].1[1] += 0.125,
                2 => {
                    forged.widths.pop();
                }
                3 => forged.widths[1].1.push(1.0),
                _ => forged.widths.swap(0, 1),
            }
            assert!(!decode_prepared(&stage, &forged).unwrap(), "case={case}");
            assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
        }
        let mut nonfinite = animation.clone();
        nonfinite.widths[1].1[1] = f64::NAN;
        assert!(decode_prepared(&stage, &nonfinite).is_err());
        for time_width in [0.0, -0.5, f64::INFINITY] {
            assert!(
                decode_prepared(&stage, &animation.clone().with_time_width(time_width)).is_err()
            );
        }
    }

    #[test]
    fn new_carrier_declarations_bind_parameters_without_preparing_the_scene() {
        let (stage, root) = indication_family();
        let before = stage.snapshot().to_bytes().unwrap();
        for kind in 0..3 {
            let first: Box<dyn Animation> = match kind {
                0 => Box::new(crate::show_increasing_subsets(&stage, root).unwrap()),
                1 => Box::new(WiggleOutThenIn::new(root)),
                _ => Box::new(VShowPassingFlash::new(root)),
            };
            let changed: Box<dyn Animation> = match kind {
                0 => Box::new(crate::show_submobjects_one_by_one(&stage, root).unwrap()),
                1 => Box::new(WiggleOutThenIn::new(root).with_rotate_about_point([1.0, 2.0, 3.0])),
                _ => Box::new(VShowPassingFlash::new(root).with_taper_width(0.2)),
            };
            let overrides = DeclarationOverrides {
                run_time_frames: Some(7),
                ..DeclarationOverrides::default()
            };
            let (first, frames) = fresh_declaration(&stage, &[first], overrides, 30, MAX_BYTES)
                .unwrap()
                .unwrap();
            let (changed, changed_frames) =
                fresh_declaration(&stage, &[changed], overrides, 30, MAX_BYTES)
                    .unwrap()
                    .unwrap();
            assert_eq!((frames, changed_frames), (7, 7));
            assert_ne!(first, changed, "kind={kind}");
            assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
        }
        for nonfinite in [f64::NAN, f64::INFINITY] {
            let animation: Box<dyn Animation> =
                Box::new(WiggleOutThenIn::new(root).with_n_wiggles(nonfinite));
            assert!(
                declaration_bytes(
                    &stage,
                    &[animation],
                    DeclarationOverrides::default(),
                    MAX_BYTES
                )
                .is_err()
            );
        }
        let mut stale = VShowPassingFlash::new(root);
        stale.widths.push((root, Vec::new()));
        assert!(
            fresh_declaration(
                &stage,
                &[Box::new(stale)],
                DeclarationOverrides::default(),
                30,
                MAX_BYTES
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn new_carrier_decode_enforces_version_counts_and_allocation_limits() {
        let (mut stage, root) = indication_family();
        let mut reveal = crate::show_increasing_subsets(&stage, root).unwrap();
        reveal.prepare_timing(30).unwrap();
        reveal.begin(&mut stage).unwrap();
        let before = stage.snapshot().to_bytes().unwrap();
        for case in 0..4 {
            let schema = if case == 0 {
                Schema::new(*b"FMPC", 1, 1, 2)
            } else {
                SCHEMA
            };
            let mut writer = Writer::with_limits(schema, limits(MAX_BYTES));
            if case < 2 {
                put_native(&mut writer, &stage, &reveal, true).unwrap();
            } else {
                writer.put_u8(if case == 2 { 7 } else { 9 });
                put_state(&mut writer, &stage, &reveal.state, true).unwrap();
                if case == 3 {
                    put_float(&mut writer, 0.3);
                    put_float(&mut writer, 0.05);
                }
                writer.put_u32(u32::MAX);
            }
            let bytes = writer.finish().unwrap();
            let mut reader =
                Reader::open(&bytes, SCHEMA, limits(MAX_BYTES), UnknownPolicy::Strict).unwrap();
            let mut budget = Budget {
                remaining: if case == 1 { 16 } else { MAX_BYTES },
                native_items: 0,
            };
            assert!(
                get_native(&mut reader, &stage, &mut budget, 30).is_err(),
                "case={case}"
            );
        }
        assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
    }

    #[test]
    fn legacy_only_trees_keep_declaration_and_checkpoint_bytes_stable() {
        for nested in [false, true] {
            for new_carrier in [false, true] {
                let (mut stage, root) = indication_family();
                let mut animation: Box<dyn Animation> = if new_carrier {
                    Box::new(WiggleOutThenIn::new(root))
                } else {
                    Box::new(Rotating::new(root))
                };
                if nested {
                    animation = Box::new(AnimationGroup::new(&mut stage, vec![animation]).unwrap());
                    animation = Box::new(AnimationGroup::new(&mut stage, vec![animation]).unwrap());
                }
                let mut animations = vec![animation];
                let declaration = declaration_bytes(
                    &stage,
                    &animations,
                    DeclarationOverrides::default(),
                    MAX_BYTES,
                )
                .unwrap()
                .unwrap();
                let expected_minor = if new_carrier { 3 } else { 2 };
                let reader = Reader::open(
                    &declaration,
                    DECLARATION_SCHEMA,
                    limits(MAX_BYTES),
                    UnknownPolicy::Strict,
                )
                .unwrap();
                assert_eq!(reader.version(), (1, expected_minor));
                if !new_carrier {
                    let legacy = Schema::new(*b"FMPC", 2, 1, 2);
                    let mut writer = Writer::with_limits(legacy, limits(MAX_BYTES));
                    writer.put_u32(1);
                    put_native(&mut writer, &stage, animations[0].as_ref(), false).unwrap();
                    assert_eq!(declaration, writer.finish().unwrap());
                }

                let clock = RationalFrameClock::new(30).unwrap();
                let open = crate::open_play(&mut stage, &clock, &mut animations).unwrap();
                let checkpoint = capture(&stage, &clock, &animations, &open, MAX_BYTES)
                    .unwrap()
                    .unwrap();
                let reader = Reader::open(
                    &checkpoint,
                    SCHEMA,
                    limits(MAX_BYTES),
                    UnknownPolicy::Strict,
                )
                .unwrap();
                assert_eq!(reader.version(), (1, expected_minor));
                if !new_carrier {
                    let legacy = Schema::new(*b"FMPC", 1, 1, 2);
                    let mut writer = Writer::with_limits(legacy, limits(MAX_BYTES));
                    writer.put_u32(30).put_i64(0).put_i64(0).put_u32(1);
                    put_native(&mut writer, &stage, animations[0].as_ref(), true).unwrap();
                    assert_eq!(checkpoint, writer.finish().unwrap());
                }
            }
        }
    }

    #[test]
    fn older_native_continuation_documents_remain_readable() {
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::vector_from_points(&[
            [0.0, 0.0, 0.0],
            [0.5, 0.0, 0.0],
            [1.0, 0.0, 0.0],
        ]));
        stage.add_to_scene(mob).unwrap();
        let mut animations: Vec<Box<dyn Animation>> = vec![Box::new(crate::v_fade_in(mob))];
        let clock = RationalFrameClock::new(30).unwrap();
        let open = crate::open_play(&mut stage, &clock, &mut animations).unwrap();
        let legacy = Schema::new(*b"FMPC", 1, 1, 0);
        let mut writer = Writer::with_limits(legacy, limits(MAX_BYTES));
        writer.put_u32(30).put_i64(0).put_i64(0).put_u32(1);
        put_native(&mut writer, &stage, animations[0].as_ref(), true).unwrap();
        let bytes = writer.finish().unwrap();
        let (restored, restored_open) = restore(&stage, &clock, &bytes).unwrap();
        let current = capture(&stage, &clock, &animations, &open, MAX_BYTES)
            .unwrap()
            .unwrap();
        assert_eq!(
            capture(&stage, &clock, &restored, &restored_open, MAX_BYTES)
                .unwrap()
                .unwrap(),
            current
        );
        assert!(Reader::open(&current, legacy, limits(MAX_BYTES), UnknownPolicy::Strict).is_err());
    }

    fn put_raw_group(
        writer: &mut Writer,
        stage: &Stage,
        state: &AnimState,
        controls: GroupContinuationControls,
        count: u32,
    ) {
        writer.put_u8(6);
        put_state(writer, stage, state, false).unwrap();
        put_float(writer, controls.layout_lag_ratio);
        put_float(writer, controls.duration_scale);
        put_float(writer, controls.timing_run_time);
        writer.put_bool(controls.run_time_override).put_u32(count);
    }

    #[test]
    fn nested_group_decode_enforces_depth_and_allocation_bounds() {
        let (stage, animation) = prepared_border();
        let state = AnimState::new(animation.state.mobject(), AnimConfig::default());
        let controls = GroupContinuationControls {
            layout_lag_ratio: 0.0,
            duration_scale: 1.0,
            timing_run_time: 1.0,
            run_time_override: false,
        };
        let before = stage.snapshot().to_bytes().unwrap();
        for (depth, allowance) in [(MAX_NATIVE_DEPTH + 1, MAX_BYTES), (1, 4096)] {
            let mut writer = Writer::with_limits(SCHEMA, limits(MAX_BYTES));
            for _ in 0..depth {
                put_raw_group(&mut writer, &stage, &state, controls, 1);
            }
            put_native(&mut writer, &stage, &animation, true).unwrap();
            let bytes = writer.finish().unwrap();
            let mut reader =
                Reader::open(&bytes, SCHEMA, limits(MAX_BYTES), UnknownPolicy::Strict).unwrap();
            let mut budget = Budget {
                remaining: allowance,
                native_items: 0,
            };
            assert!(get_native(&mut reader, &stage, &mut budget, 30).is_err());
        }
        assert_eq!(stage.snapshot().to_bytes().unwrap(), before);
    }

    #[test]
    fn group_decode_refuses_nonfinite_controls_inconsistent_timing_and_forged_counts() {
        let mut stage = Stage::new();
        let mut group = AnimationGroup::new(&mut stage, Vec::new())
            .unwrap()
            .with_run_time(0.2);
        group.prepare_timing(30).unwrap();
        group.begin(&mut stage).unwrap();
        for case in 0..5 {
            let mut controls = group.continuation_controls();
            match case {
                0 => controls.layout_lag_ratio = f64::INFINITY,
                1 => controls.duration_scale = f64::NAN,
                2 => controls.duration_scale = -1.0,
                3 => controls.timing_run_time = 0.3,
                _ => {}
            }
            let mut writer = Writer::with_limits(SCHEMA, limits(MAX_BYTES));
            put_raw_group(
                &mut writer,
                &stage,
                group.state(),
                controls,
                if case == 4 { u32::MAX } else { 0 },
            );
            let bytes = writer.finish().unwrap();
            let mut reader =
                Reader::open(&bytes, SCHEMA, limits(MAX_BYTES), UnknownPolicy::Strict).unwrap();
            let mut budget = Budget {
                remaining: MAX_BYTES,
                native_items: 0,
            };
            assert!(get_native(&mut reader, &stage, &mut budget, 30).is_err());
        }
    }

    #[test]
    fn unknown_nested_children_are_refused_without_calling_their_state_hook() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct Unknown {
            state: AnimState,
            refuse_hooks: Arc<AtomicBool>,
        }
        impl Animation for Unknown {
            fn state(&self) -> &AnimState {
                assert!(!self.refuse_hooks.load(Ordering::SeqCst));
                &self.state
            }
            fn state_mut(&mut self) -> &mut AnimState {
                panic!("declaration must not invoke a nested mutable state hook")
            }
            fn interpolate_submobject(&mut self, _: &mut Stage, _: &[Mob], _: f64) {
                panic!("declaration must not invoke interpolation")
            }
        }
        let mut stage = Stage::new();
        let mob = stage.add(Mobject::new());
        let refuse_hooks = Arc::new(AtomicBool::new(false));
        let unknown = Unknown {
            state: AnimState::new(mob, AnimConfig::default()),
            refuse_hooks: refuse_hooks.clone(),
        };
        let group = AnimationGroup::new(&mut stage, vec![Box::new(unknown)]).unwrap();
        refuse_hooks.store(true, Ordering::SeqCst);
        assert!(
            declaration_bytes(
                &stage,
                &[Box::new(group)],
                DeclarationOverrides::default(),
                MAX_BYTES,
            )
            .unwrap()
            .is_none()
        );
    }
}
