//! The functional-map mechanism — family 5 (§9.4, fm-cye), ported from
//! the pinned Reference's `animation/movement.py`:
//!
//! - [`Homotopy`]: per zipped pair, restore the pointlike data from the
//!   start, then map every point through `(x, y, z, t) ↦ (x', y', z')` at
//!   `t = alpha` (movement.py:37 — `match_points` + `apply_function`,
//!   whose default pivot is the true origin). [`smoothed_homotopy`] adds
//!   the `make_smooth` re-handling pass (movement.py:52);
//!   [`complex_homotopy`] is the `(z, t) ↦ w` plane map over `(x, y)`
//!   with `z` carried through (movement.py:56).
//! - [`PhaseFlow`] (movement.py:75): forward-Euler advection
//!   `p ↦ p + Δt·f(p)` with `Δt = virtual_time·(α − α_prev)` — the state
//!   is *path-dependent by design* (the Reference's `last_alpha` memo),
//!   so its segments are conservatively stateful. The Reference's
//!   overridden `interpolate_mobject` never consults `rate_func` or
//!   `time_span`; ported verbatim (the `linear` default makes it moot).
//! - [`MoveAlongPath`] (movement.py:104): `move_to` the point `α` of the
//!   way along a path — **by true arc length** (BN-03, the W2 layer),
//!   where the Reference rides `quick_point_from_proportion`'s
//!   equal-curve-length approximation. Constant speed under the original
//!   name is the Behavior-Noted improvement this program exists for.
//!   [`MoveAlongPath::from_observation`] opts into an immutable world-space
//!   path and absolute begin-state poses, allowing exact frame-parallel work
//!   while sharing one retained path and arc-length table across animations.
//!
//! User-supplied closures are not provable, so every closure-carrying
//! animation here keeps the default Unclassified signature and the R20
//! demotion — frame-parallel eligibility is never guessed.

use std::sync::Arc;

use fmn_core::types::Vec3;
use fmn_mobject::path_sampling::PathObservation;
use fmn_mobject::stage::{surface_sample_is_present, surface_validity};
use fmn_mobject::{Mob, Mobject, Placement, Stage, StageError};

use crate::animation::{
    AnimConfig, AnimError, AnimState, Animation, AnimationSignature, RateFunc, sub_alpha,
    time_spanned_alpha,
};

mod path_cache;

// -------------------------------------------------------------- Homotopy

type HomotopyMap = dyn Fn(f64, f64, f64, f64) -> Result<Vec3, AnimError>;

/// Homotopy (movement.py:17): animate a point map over the family.
///
/// The original constructor preserves live, in-place callback observations.
/// The fallible constructor opts into atomic publication of each complete
/// sample, allowing a scientific map to refuse its domain with a typed error.
pub struct Homotopy {
    state: AnimState,
    homotopy: Box<HomotopyMap>,
    smoothed: bool,
    atomic: bool,
    error: Option<AnimError>,
    scratch: Option<HomotopyScratch>,
}

impl std::fmt::Debug for Homotopy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Homotopy")
            .field("state", &self.state)
            .field("smoothed", &self.smoothed)
            .field("atomic", &self.atomic)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl Homotopy {
    /// Homotopy(homotopy, mobject), with the Reference's default run time of 3.
    ///
    /// Each live member is restored from its starting copy before the callback.
    /// Captured record views therefore observe the historical in-place order.
    /// A failed sample stops immediately; completed earlier member writes and
    /// arbitrary callback side effects are not rolled back.
    #[must_use]
    pub fn new(homotopy: impl Fn(f64, f64, f64, f64) -> Vec3 + 'static, mobject: Mob) -> Self {
        let mut animation = Self::try_new(move |x, y, z, t| Ok(homotopy(x, y, z, t)), mobject);
        animation.atomic = false;
        animation
    }

    /// A fallible point map with atomic geometry publication.
    ///
    /// The callback receives begin-state coordinates and the pipeline's alpha.
    /// All members are prepared before any live geometry is published, so
    /// captured views observe the previous published sample during evaluation.
    /// The first callback or geometry error preserves that complete sample and
    /// stops later calls until a new begin. External callback effects are not
    /// rolled back. This remains effectful and never enters pure-frame workers.
    #[must_use]
    pub fn try_new(
        homotopy: impl Fn(f64, f64, f64, f64) -> Result<Vec3, AnimError> + 'static,
        mobject: Mob,
    ) -> Self {
        let config = AnimConfig {
            name: "Homotopy".to_owned(),
            run_time: 3.0,
            ..AnimConfig::default()
        };
        Self {
            state: AnimState::new(mobject, config),
            homotopy: Box::new(homotopy),
            smoothed: false,
            atomic: true,
            error: None,
            scratch: None,
        }
    }

    /// Replace the animation config.
    #[must_use]
    pub fn with_config(mut self, config: AnimConfig) -> Self {
        self.state.config = config;
        self
    }

    fn apply_rows(
        &mut self,
        stage: &mut Stage,
        rows: &[Vec<Mob>],
        mut alpha: impl FnMut(usize) -> Result<f64, AnimError>,
    ) -> Result<(), AnimError> {
        if !self.atomic {
            for (index, row) in rows.iter().enumerate() {
                let time = alpha(index)?;
                validate_homotopy_time(time)?;
                let (member, start) = homotopy_pair(stage, row)?;
                let starting = PointGeometry::starting(stage, start)?;
                starting.publish(stage, member)?;
                map_homotopy_member(stage, member, self.homotopy.as_ref(), time)?;
                if self.smoothed {
                    stage.make_family_smooth(member)?;
                }
            }
            return Ok(());
        }

        for row in rows {
            homotopy_pair(stage, row)?;
        }
        let mut scratch = match self.scratch.take() {
            Some(mut scratch) if scratch.matches(rows) => {
                scratch.reset(stage)?;
                scratch
            }
            _ => HomotopyScratch::new(stage, rows)?,
        };
        // Atomic construction is an explicit opt-in for plain Homotopy.
        // The existing smoothed/complex constructors keep the live path above.
        for (index, row) in rows.iter().enumerate() {
            let time = alpha(index)?;
            validate_homotopy_time(time)?;
            let staged = scratch.members[index].1;
            let starting = PointGeometry::starting(stage, row[1])?;
            starting.publish(&mut scratch.stage, staged)?;
            map_homotopy_member(&mut scratch.stage, staged, self.homotopy.as_ref(), time)?;
        }
        let prepared = scratch
            .members
            .iter()
            .map(|&(original, staged)| {
                PointGeometry::finished(&scratch.stage, staged).map(|geometry| (original, geometry))
            })
            .collect::<Result<Vec<_>, AnimError>>()?;
        // Same-schema scratch writes already proved every resize and column
        // write. No callback or topology mutation occurs during publication.
        for (original, geometry) in prepared {
            geometry.publish(stage, original)?;
        }
        self.scratch = Some(scratch);
        Ok(())
    }
}

impl Animation for Homotopy {
    fn state(&self) -> &AnimState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut AnimState {
        &mut self.state
    }

    fn setup(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        self.error = None;
        self.scratch = None;
        let mobject = self.state.mobject();
        if !stage.contains(mobject) {
            return Err(AnimError::StaleHandle(mobject));
        }
        Ok(())
    }

    fn interpolate(&mut self, stage: &mut Stage, alpha: f64) {
        if self.deferred_error().is_some() || stage.updater_error().is_some() {
            return;
        }
        let mobject = self.state.mobject();
        if !stage.contains(mobject) {
            self.error = Some(AnimError::StaleHandle(mobject));
            return;
        }
        let config = &self.state.config;
        let spanned = time_spanned_alpha(alpha, config.run_time, config.time_span);
        if !alpha.is_finite() || !spanned.is_finite() || !config.lag_ratio.is_finite() {
            self.error = Some(AnimError::InvalidPath(
                "Homotopy sample timing must be finite",
            ));
            return;
        }
        let lag_ratio = config.lag_ratio;
        let rate_func = config.rate_func.clone();
        let rows = Arc::clone(&self.state.families);
        self.error = self
            .apply_rows(stage, &rows, |index| {
                let time = sub_alpha(spanned, index, rows.len(), lag_ratio, &rate_func);
                if let Some(error) = rate_func.error() {
                    return Err(error);
                }
                Ok(time)
            })
            .err();
    }

    fn interpolate_submobject(&mut self, stage: &mut Stage, mobs: &[Mob], sub_alpha: f64) {
        if self.deferred_error().is_none() && stage.updater_error().is_none() {
            self.error = self
                .apply_rows(stage, &[mobs.to_vec()], |_| Ok(sub_alpha))
                .err();
        }
    }

    fn deferred_error(&self) -> Option<AnimError> {
        self.error
            .clone()
            .or_else(|| self.state.config.rate_func.error())
    }

    fn teardown(&mut self, _stage: &mut Stage) {
        self.scratch = None;
    }
}

fn homotopy_pair(stage: &Stage, row: &[Mob]) -> Result<(Mob, Mob), AnimError> {
    let [member, start] = *row else {
        return Err(AnimError::UnalignedFamilies);
    };
    for mob in [member, start] {
        if !stage.contains(mob) {
            return Err(AnimError::StaleHandle(mob));
        }
    }
    Ok((member, start))
}

fn validate_homotopy_time(time: f64) -> Result<(), AnimError> {
    if !time.is_finite() {
        return Err(AnimError::InvalidPath(
            "Homotopy rate must return a finite proportion",
        ));
    }
    Ok(())
}

#[allow(clippy::cast_possible_truncation)]
fn validate_mapped_point(point: Vec3) -> Result<(), AnimError> {
    if point.iter().any(|value| !(*value as f32).is_finite()) {
        return Err(
            StageError::InvalidPointMap("mapped points must fit finite f32 records").into(),
        );
    }
    Ok(())
}

fn map_homotopy_member(
    stage: &mut Stage,
    member: Mob,
    homotopy: &HomotopyMap,
    time: f64,
) -> Result<(), AnimError> {
    let points = stage.get_points(member).ok_or(StageError::SchemaMismatch)?;
    let mut mapped = Vec::with_capacity(points.len());
    for point in points {
        if point.iter().any(|coordinate| !coordinate.is_finite()) {
            return Err(StageError::InvalidPointMap("Homotopy input points must be finite").into());
        }
        let point = homotopy(point[0], point[1], point[2], time)?;
        validate_mapped_point(point)?;
        mapped.push(point);
    }
    stage.set_points(member, &mapped)?;
    Ok(())
}

/// Pointlike data only; styles remain in the destination's live record buffer.
struct PointGeometry {
    len: usize,
    placement: Placement,
    columns: Vec<(String, Vec<f32>)>,
}

impl PointGeometry {
    fn starting(stage: &Stage, mob: Mob) -> Result<Self, AnimError> {
        let entry = stage.get(mob).ok_or(AnimError::StaleHandle(mob))?;
        if entry.buffer.schema().field_width("point") != Some(3) {
            return Err(StageError::SchemaMismatch.into());
        }
        let mut columns = Vec::new();
        for key in entry.buffer.schema().pointlike_keys() {
            if entry.buffer.schema().field_width(key) != Some(3) {
                return Err(StageError::SchemaMismatch.into());
            }
            let values = entry
                .buffer
                .read_column(key)
                .ok_or(StageError::SchemaMismatch)?;
            columns.push((key.clone(), values));
        }
        Ok(Self {
            len: entry.buffer.len(),
            placement: entry.placement(),
            columns,
        })
    }

    fn finished(stage: &Stage, mob: Mob) -> Result<Self, AnimError> {
        let entry = stage.get(mob).ok_or(AnimError::StaleHandle(mob))?;
        let validity = surface_validity(&entry.buffer)?;
        let mut keys = vec!["point"];
        for key in entry.buffer.schema().pointlike_keys() {
            if !keys.contains(&key.as_str()) {
                keys.push(key);
            }
        }
        if entry.buffer.schema().field_width("joint_angle").is_some()
            && !keys.contains(&"joint_angle")
        {
            keys.push("joint_angle");
        }
        let mut columns = Vec::new();
        for key in keys {
            let width = entry
                .buffer
                .schema()
                .field_width(key)
                .ok_or(StageError::SchemaMismatch)?;
            if width == 0 {
                return Err(StageError::SchemaMismatch.into());
            }
            let values = entry
                .buffer
                .read_column(key)
                .ok_or(StageError::SchemaMismatch)?;
            if values
                .chunks_exact(width)
                .enumerate()
                .any(|(index, record)| {
                    surface_sample_is_present(validity.as_deref(), index)
                        && record.iter().any(|value| !value.is_finite())
                })
            {
                return Err(StageError::InvalidPointMap(
                    "mapped geometry must fit finite f32 records",
                )
                .into());
            }
            columns.push((key.to_owned(), values));
        }
        Ok(Self {
            len: entry.buffer.len(),
            placement: entry.placement(),
            columns,
        })
    }

    fn publish(&self, stage: &mut Stage, mob: Mob) -> Result<(), AnimError> {
        let entry = stage.get_mut(mob).ok_or(AnimError::StaleHandle(mob))?;
        if entry.buffer.schema().field_width("point") != Some(3)
            || self.columns.iter().any(|(key, values)| {
                entry.buffer.schema().field_width(key).is_none_or(|width| {
                    width == 0
                        || values.len() / width != self.len
                        || !values.len().is_multiple_of(width)
                })
            })
        {
            return Err(StageError::SchemaMismatch.into());
        }
        if entry.buffer.len() != self.len {
            entry
                .buffer
                .resize_preserving_order(self.len)
                .map_err(StageError::Record)?;
        }
        for (key, values) in &self.columns {
            if entry.buffer.read_column(key).as_deref() != Some(values.as_slice())
                && !entry.buffer.write_range(key, 0, values)
            {
                return Err(StageError::SchemaMismatch.into());
            }
        }
        stage.set_placement(mob, self.placement)?;
        stage.refresh_bounding_box(mob, false, true);
        Ok(())
    }
}

/// Only explicit atomic maps use this scratch arena. Entries and capacity are
/// reused across samples; no unrelated scene, graph, or updater data is copied.
struct HomotopyScratch {
    stage: Stage,
    members: Vec<(Mob, Mob)>,
}

impl HomotopyScratch {
    fn new(stage: &Stage, rows: &[Vec<Mob>]) -> Result<Self, AnimError> {
        let mut scratch = Self {
            stage: Stage::new(),
            members: Vec::with_capacity(rows.len()),
        };
        for row in rows {
            let original = row[0];
            let entry = stage
                .get(original)
                .ok_or(AnimError::StaleHandle(original))?;
            let staged = scratch
                .stage
                .add(Mobject::from_buffer(entry.buffer.snapshot_clone()));
            scratch.stage.set_placement(staged, entry.placement())?;
            scratch.members.push((original, staged));
        }
        Ok(scratch)
    }

    fn matches(&self, rows: &[Vec<Mob>]) -> bool {
        self.members
            .iter()
            .map(|pair| pair.0)
            .eq(rows.iter().map(|row| row[0]))
    }

    fn reset(&mut self, stage: &Stage) -> Result<(), AnimError> {
        for &(original, staged) in &self.members {
            let entry = stage
                .get(original)
                .ok_or(AnimError::StaleHandle(original))?;
            self.stage
                .get_mut(staged)
                .ok_or(StageError::StaleHandle)?
                .buffer = entry.buffer.snapshot_clone();
            self.stage.set_placement(staged, entry.placement())?;
        }
        Ok(())
    }
}

/// SmoothedVectorizedHomotopy: the original live map followed by the
/// approximate-smooth re-handling pass after each application.
#[must_use]
pub fn smoothed_homotopy(
    homotopy: impl Fn(f64, f64, f64, f64) -> Vec3 + 'static,
    mobject: Mob,
) -> Homotopy {
    let mut anim = Homotopy::new(homotopy, mobject);
    anim.state.config.name = "SmoothedVectorizedHomotopy".to_owned();
    anim.smoothed = true;
    anim
}

/// ComplexHomotopy: map the xy plane, carrying z through.
#[must_use]
pub fn complex_homotopy(
    map: impl Fn(f64, f64, f64) -> (f64, f64) + 'static,
    mobject: Mob,
) -> Homotopy {
    let mut anim = Homotopy::new(
        move |x, y, z, t| {
            let (re, im) = map(x, y, t);
            [re, im, z]
        },
        mobject,
    );
    anim.state.config.name = "ComplexHomotopy".to_owned();
    anim
}

// ------------------------------------------------------------- PhaseFlow

/// Forward-Euler advection between raw consecutive alphas.
///
/// The original constructor retains live per-member publication; try_new
/// explicitly opts into atomic publication of each whole family sample.
pub struct PhaseFlow {
    state: AnimState,
    function: Box<dyn Fn(Vec3) -> Result<Vec3, AnimError>>,
    virtual_time: f64,
    last_alpha: Option<f64>,
    atomic: bool,
    error: Option<AnimError>,
}

impl std::fmt::Debug for PhaseFlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhaseFlow")
            .field("state", &self.state)
            .field("virtual_time", &self.virtual_time)
            .field("last_alpha", &self.last_alpha)
            .field("atomic", &self.atomic)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl PhaseFlow {
    /// PhaseFlow(function, mobject): run time 3, linear rate, and virtual time
    /// equal to the construction-time run time when unspecified.
    ///
    /// Callbacks observe the original live member order. An invalid sample
    /// stops immediately without rolling back previously completed members.
    #[must_use]
    pub fn new(
        function: impl Fn(Vec3) -> Vec3 + 'static,
        mobject: Mob,
        virtual_time: Option<f64>,
    ) -> Self {
        let mut animation = Self::try_new(move |point| Ok(function(point)), mobject, virtual_time);
        animation.atomic = false;
        animation
    }

    /// A fallible field with atomic publication of the complete family.
    ///
    /// During evaluation, captured views observe the previous published sample.
    /// The first field or geometry error leaves that sample unchanged and stops
    /// further callbacks until a new begin. External effects are not rolled back.
    #[must_use]
    pub fn try_new(
        function: impl Fn(Vec3) -> Result<Vec3, AnimError> + 'static,
        mobject: Mob,
        virtual_time: Option<f64>,
    ) -> Self {
        let run_time = 3.0;
        let config = AnimConfig {
            name: "PhaseFlow".to_owned(),
            run_time,
            rate_func: RateFunc::linear(),
            ..AnimConfig::default()
        };
        Self {
            state: AnimState::new(mobject, config),
            function: Box::new(function),
            virtual_time: virtual_time.unwrap_or(run_time),
            last_alpha: None,
            atomic: true,
            error: None,
        }
    }

    /// Replace config without changing the construction-time virtual time.
    #[must_use]
    pub fn with_config(mut self, config: AnimConfig) -> Self {
        self.state.config = config;
        self
    }
}

impl Animation for PhaseFlow {
    fn state(&self) -> &AnimState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut AnimState {
        &mut self.state
    }

    fn setup(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        self.error = None;
        self.last_alpha = None;
        let mobject = self.state.mobject();
        if !stage.contains(mobject) {
            return Err(AnimError::StaleHandle(mobject));
        }
        if !self.virtual_time.is_finite() {
            return Err(AnimError::InvalidPath(
                "PhaseFlow virtual time must be finite",
            ));
        }
        Ok(())
    }

    /// Raw alpha, no rate/lag/time-span; the first call only records last_alpha.
    fn interpolate(&mut self, stage: &mut Stage, alpha: f64) {
        if self.deferred_error().is_some() || stage.updater_error().is_some() {
            return;
        }
        let mobject = self.state.mobject();
        if !stage.contains(mobject) {
            self.error = Some(AnimError::StaleHandle(mobject));
            return;
        }
        if !alpha.is_finite() {
            self.error = Some(AnimError::InvalidPath("PhaseFlow alpha must be finite"));
            return;
        }
        if let Some(last) = self.last_alpha {
            let dt = self.virtual_time * (alpha - last);
            if !dt.is_finite() {
                self.error = Some(AnimError::InvalidPath("PhaseFlow time step must be finite"));
                return;
            }
            let field = &self.function;
            let map = |point: Vec3| -> Result<Vec3, AnimError> {
                if point.iter().any(|coordinate| !coordinate.is_finite()) {
                    return Err(StageError::InvalidPointMap(
                        "PhaseFlow input points must be finite",
                    )
                    .into());
                }
                let velocity = field(point)?;
                if velocity.iter().any(|coordinate| !coordinate.is_finite()) {
                    return Err(StageError::InvalidPointMap(
                        "PhaseFlow field must return finite vectors",
                    )
                    .into());
                }
                let mapped = [
                    point[0] + dt * velocity[0],
                    point[1] + dt * velocity[1],
                    point[2] + dt * velocity[2],
                ];
                validate_mapped_point(mapped)?;
                Ok(mapped)
            };
            let result = if self.atomic {
                stage.try_apply_points_function_with(mobject, map, Some([0.0; 3]), None)
            } else {
                stage.try_apply_points_function_in_order_with(mobject, map, Some([0.0; 3]), None)
            };
            if let Err(error) = result {
                self.error = Some(error);
                return;
            }
        }
        self.last_alpha = Some(alpha);
    }

    fn deferred_error(&self) -> Option<AnimError> {
        self.error
            .clone()
            .or_else(|| self.state.config.rate_func.error())
    }

    fn interpolate_submobject(&mut self, _stage: &mut Stage, _mobs: &[Mob], _sub_alpha: f64) {
        // Interpolate is overridden and never zips families.
    }
}

// ---------------------------------------------------------- MoveAlongPath

/// `MoveAlongPath` (movement.py:104): `move_to` the point `α` of the way
/// along `path` — constant-speed by true arc length (BN-03).
///
/// [`Self::new`] observes a live path and preserves incremental placement.
/// [`Self::from_observation`] follows a captured world-space path and restores
/// the moving family's begin geometry before every sample. The captured mode
/// supports independent native frame jobs when the ordinary updater, view,
/// rate-function and output-alias checks admit the segment.
#[derive(Debug, Clone)]
pub struct MoveAlongPath {
    state: AnimState,
    path: MotionPath,
    path_cache: path_cache::PathCache,
    error: Option<AnimError>,
}

#[derive(Debug, Clone)]
enum MotionPath {
    Live(Mob),
    Captured(PathObservation),
}

impl MoveAlongPath {
    /// `MoveAlongPath(mobject, path)`, observing live path edits each sample.
    ///
    /// Placement is incremental, including its floating-point operation order,
    /// so this mode retains the ordinary serial animation driver.
    #[must_use]
    pub fn new(mobject: Mob, path: Mob) -> Self {
        Self::with_path_source(mobject, MotionPath::Live(path))
    }

    /// Follow an immutable, shared world-space path using absolute poses.
    ///
    /// Capture with [`fmn_mobject::path_sampling::PathSampler::observe`].
    /// Cloned observations share their geometry and true-arc-length table;
    /// source edits or removal never change an already captured path. Each
    /// sample restores the moving family's begin point data and placement,
    /// then centers it on the sampled point. This makes arbitrary frame order
    /// bit-identical to serial playback of this mode. Styles remain live.
    ///
    /// Use [`Self::new`] when path edits or incremental geometry changes must
    /// remain observable. The two modes have distinct, explicit contracts.
    ///
    /// ```
    /// use fmn_anim::MoveAlongPath;
    /// use fmn_mobject::{Mobject, Stage, StageError};
    /// use fmn_mobject::path_sampling::PathSampler;
    ///
    /// let mut stage = Stage::new();
    /// let path = stage.add(Mobject::vector_from_points(&[
    ///     [-2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [2.0, 0.0, 0.0],
    /// ]));
    /// let captured = PathSampler::default().observe(&stage, path)?;
    /// let moving = stage.add(Mobject::from_points(&[[0.0, 0.0, 0.0]]));
    /// let animation = MoveAlongPath::from_observation(moving, captured);
    /// # let _ = animation;
    /// # Ok::<(), StageError>(())
    /// ```
    #[must_use]
    pub fn from_observation(mobject: Mob, path: PathObservation) -> Self {
        Self::with_path_source(mobject, MotionPath::Captured(path))
    }

    fn with_path_source(mobject: Mob, path: MotionPath) -> Self {
        let config = AnimConfig {
            name: "MoveAlongPath".to_owned(),
            ..AnimConfig::default()
        };
        Self {
            state: AnimState::new(mobject, config),
            path,
            path_cache: path_cache::PathCache::default(),
            error: None,
        }
    }

    /// Successful world-space path/table builds performed by this animation.
    ///
    /// Repeated samples of an unchanged path reuse one observation. Point,
    /// storage, or placement changes rebuild it; writable point views force a
    /// fresh observation each time. Style-only edits do not normally rebuild.
    /// This diagnostic is not serialized and resets when the animation is
    /// cloned. Finishing or aborting releases cached geometry but keeps the
    /// count for inspection. It is a work counter, not a timing estimate.
    /// Captured-path mode reports zero: its shared observation was prepared by
    /// the caller's sampler and never materialized by this animation.
    #[must_use]
    pub fn path_materializations(&self) -> u64 {
        self.path_cache.materializations()
    }

    /// Replace the animation config.
    #[must_use]
    pub fn with_config(mut self, config: AnimConfig) -> Self {
        self.state.config = config;
        self
    }

    fn sample_path(&mut self, stage: &Stage, alpha: f64) -> Result<Vec3, AnimError> {
        let point = match &self.path {
            MotionPath::Live(mob) => {
                let Some(path) = stage.get(*mob) else {
                    self.path_cache.clear();
                    return Err(AnimError::StaleHandle(*mob));
                };
                if path.buffer.is_empty() {
                    self.path_cache.clear();
                    return Err(AnimError::EmptyMobject);
                }
                self.path_cache.sample(stage, *mob, alpha)
            }
            MotionPath::Captured(path) => path.sample(alpha),
        }
        .map_err(AnimError::Stage)?;
        if point.iter().any(|coordinate| !coordinate.is_finite()) {
            self.path_cache.clear();
            return Err(AnimError::InvalidPath(
                "MoveAlongPath sampled a non-finite path point",
            ));
        }
        Ok(point)
    }

    fn restore_begin_geometry(&self, stage: &mut Stage) -> Result<(), AnimError> {
        // Validate every prepared row before writing: an invalid family must
        // not leave the earlier members restored and the later ones untouched.
        for row in self.state.families() {
            let [output, starting] = row.as_slice() else {
                return Err(AnimError::UnalignedFamilies);
            };
            for &mob in [output, starting] {
                if !stage.contains(mob) {
                    return Err(AnimError::StaleHandle(mob));
                }
            }
        }
        for row in self.state.families() {
            stage.match_points(row[0], row[1]);
        }
        Ok(())
    }
}

impl Animation for MoveAlongPath {
    fn state(&self) -> &AnimState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut AnimState {
        &mut self.state
    }

    fn effect_signature(&self) -> AnimationSignature {
        match &self.path {
            MotionPath::Live(_) => AnimationSignature::Unclassified,
            MotionPath::Captured(_) => AnimationSignature::Pure,
        }
    }

    fn freeze_interpolation(&self) -> Option<crate::pure_frame::FrozenAnimation> {
        if matches!(self.path, MotionPath::Captured(_)) && self.error.is_none() {
            crate::pure_frame::FrozenAnimation::path_motion(self.clone())
        } else {
            None
        }
    }

    /// A pointless path is the typed refusal (the Reference indexes into
    /// an empty array at the first frame).
    fn setup(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        self.error = None;
        let mobject = self.state.mobject();
        if !stage.contains(mobject) {
            self.path_cache.clear();
            return Err(AnimError::StaleHandle(mobject));
        }
        self.sample_path(stage, 0.0)?;
        Ok(())
    }

    fn preflight_mobjects(&self) -> Vec<Mob> {
        let mut mobs = vec![self.state.mobject()];
        if let MotionPath::Live(path) = &self.path {
            mobs.push(*path);
        }
        mobs
    }

    /// movement.py:115 with the BN-03 improvement: the rate function
    /// applies to the raw alpha (the Reference's override bypasses lag and
    /// time-span), and the sample point is taken by true arc length.
    fn interpolate(&mut self, stage: &mut Stage, alpha: f64) {
        if self.error.is_some() || stage.updater_error().is_some() {
            return;
        }
        let mobject = self.state.mobject();
        if !stage.contains(mobject) {
            self.error = Some(AnimError::StaleHandle(mobject));
            return;
        }
        let rated = self.state.config.rate_func.eval(alpha);
        // An exact-rate lookup records its own failure while returning a
        // finite placeholder. Never let that placeholder move the object.
        if let Some(error) = self.state.config.rate_func.error() {
            self.error = Some(error);
            return;
        }
        if !rated.is_finite() {
            self.error = Some(AnimError::InvalidPath(
                "MoveAlongPath rate function returned a non-finite proportion",
            ));
            return;
        }
        match self.sample_path(stage, rated) {
            Ok(point) => {
                if matches!(self.path, MotionPath::Captured(_)) {
                    if self.state.starting_mobject().is_none() {
                        return;
                    }
                    if let Err(error) = self.restore_begin_geometry(stage) {
                        self.error = Some(error);
                        return;
                    }
                }
                stage.move_to(mobject, point, [0.0, 0.0, 0.0]);
            }
            Err(error) => self.error = Some(error),
        }
    }

    /// A malformed, removed or non-finite live path refuses the frame. Keep
    /// its first error until the next begin instead of publishing the last
    /// valid position as though the animation had completed successfully.
    fn deferred_error(&self) -> Option<AnimError> {
        self.error
            .clone()
            .or_else(|| self.state.config.rate_func.error())
    }

    fn teardown(&mut self, _stage: &mut Stage) {
        self.path_cache.clear();
    }

    fn interpolate_submobject(&mut self, _stage: &mut Stage, _mobs: &[Mob], _sub_alpha: f64) {
        // Unreachable: interpolate is overridden and never zips families.
    }
}
